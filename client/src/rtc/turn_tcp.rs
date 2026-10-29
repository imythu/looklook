//! 经 TCP 用 TURN：这台电脑到线路的 UDP 不通（公司、学校网络，或安全软件拦了 UDP）时，远程控制台改经 TCP 连中转。
//!
//! libjuice 只会经 UDP 用 TURN，这里在本机回环地址上开一个 UDP 端口冒充 TURN 服务器，把收到的每个 STUN / ChannelData
//! 消息原样经 TCP 转给线路上的 coturn（同一端口 3478 也收 TCP），回来的消息再按原路发回：
//!
//! - libjuice 的每个本地端口（一条 PeerConnection 一个）对应一条 TCP 连接，coturn 按 TCP 连接认 TURN 分配；
//! - TCP 上的分帧（RFC 8656 §12.5）：STUN 消息 20 字节头加长度；ChannelData 4 字节头加长度，补齐到 4 的倍数，
//!   转回 UDP 时去掉补齐的字节；
//! - 一段时间没有消息就关掉 TCP 连接（TURN 分配随之释放）。连接正在用时 ICE 的连通性检查会一直经过这里。
//!
//! 什么时候用：线路测速（`relay_watch`）发现到某条线路 UDP 不通、TCP 通时 [`set_tcp`]，之后新建的 PeerConnection
//! 多一个指向本地转发的 TURN 地址（`peer::ice_urls`）；UDP 恢复后不再加，已经起的转发留着给还在用的连接。
//! TURN 分配的应答里带的映射地址是 TCP 连接的，libjuice 不把经回环地址 TURN 拿到的映射地址当作 srflx 候选
//! （third_party 的 libjuice 补丁）。

use std::collections::HashMap;
use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UdpSocket};
use tokio::sync::mpsc;

/// 连线路的期限（期间 libjuice 发来的消息先排队）。
const CONNECT_LIMIT: Duration = Duration::from_secs(5);
/// 这么久没有 libjuice 发来的消息就关掉 TCP 连接。
const IDLE: Duration = Duration::from_secs(5 * 60);
/// 每条连接排队的消息数，超过就丢（和 UDP 丢包一样，libjuice 会重传）。
const QUEUE: usize = 256;

struct Route {
    local: SocketAddr,
    on: bool,
}

/// 线路（`主机:端口`，小写）→ 本地转发。
static ROUTES: Mutex<Option<HashMap<String, Route>>> = Mutex::new(None);

fn routes() -> std::sync::MutexGuard<'static, Option<HashMap<String, Route>>> {
    ROUTES.lock().unwrap_or_else(|e| e.into_inner())
}

/// 记下测速结论：到线路 `stun`（`主机:端口`）是否改经 TCP 用 TURN。第一次打开时起本地转发（要在 tokio 运行时里调用）。
pub fn set_tcp(stun: &str, on: bool) {
    let key = stun.to_ascii_lowercase();
    let mut g = routes();
    let map = g.get_or_insert_with(HashMap::new);
    if let Some(r) = map.get_mut(&key) {
        if r.on != on {
            tracing::info!(relay = %key, on, "远程控制台经 TCP 连线路");
        }
        r.on = on;
        return;
    }
    if !on {
        return;
    }
    match start(&key) {
        Ok(local) => {
            tracing::info!(relay = %key, %local, "到线路的 UDP 不通，远程控制台改经 TCP 连线路");
            map.insert(key, Route { local, on });
        }
        Err(e) => tracing::warn!(relay = %key, error = %e, "开不了经 TCP 连线路的本地转发"),
    }
}

/// `turn:` 地址里的 `主机:端口` 该经 TCP 时，本地转发的地址。
pub fn local_for(hostport: &str) -> Option<SocketAddr> {
    routes().as_ref()?.get(&hostport.to_ascii_lowercase()).filter(|r| r.on).map(|r| r.local)
}

/// 在回环地址上开 UDP 端口，转发到 `target`（`主机:端口`，每条新连接都重新解析）。
fn start(target: &str) -> io::Result<SocketAddr> {
    let std = std::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))?;
    std.set_nonblocking(true)?;
    let sock = Arc::new(UdpSocket::from_std(std)?);
    let local = sock.local_addr()?;
    tokio::spawn(serve(sock, target.to_string()));
    Ok(local)
}

async fn serve(sock: Arc<UdpSocket>, target: String) {
    let mut sessions: HashMap<SocketAddr, mpsc::Sender<Vec<u8>>> = HashMap::new();
    let mut buf = vec![0u8; 65536];
    loop {
        let (n, src) = match sock.recv_from(&mut buf).await {
            Ok(r) => r,
            // Windows：发回给已关掉的 libjuice 端口后，下一次收会报 ConnectionReset
            Err(e) if e.kind() == io::ErrorKind::ConnectionReset => continue,
            Err(e) => {
                tracing::debug!(error = %e, "TURN 本地转发收消息失败");
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
        };
        if sessions.get(&src).is_none_or(|tx| tx.is_closed()) {
            sessions.retain(|_, tx| !tx.is_closed());
            let (tx, rx) = mpsc::channel(QUEUE);
            tokio::spawn(session(sock.clone(), src, target.clone(), rx));
            sessions.insert(src, tx);
        }
        let _ = sessions[&src].try_send(buf[..n].to_vec());
    }
}

/// 一个 libjuice 端口（`src`）的 TCP 连接：两个方向原样转发，连接断了或闲置太久就结束。
async fn session(sock: Arc<UdpSocket>, src: SocketAddr, target: String, mut rx: mpsc::Receiver<Vec<u8>>) {
    let tcp = async {
        let to = crate::relay::resolve(&target).await.map_err(|e| io::Error::other(format!("解析失败：{e}")))?;
        tokio::time::timeout(CONNECT_LIMIT, TcpStream::connect(to)).await.map_err(|_| io::Error::from(io::ErrorKind::TimedOut))?
    };
    let tcp = match tcp.await {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(relay = %target, error = %e, "经 TCP 连不上线路的 TURN");
            return;
        }
    };
    let _ = tcp.set_nodelay(true);
    tracing::debug!(relay = %target, %src, "经 TCP 连上线路的 TURN");
    let (mut rd, mut wr) = tcp.into_split();
    let up = async {
        while let Ok(Some(m)) = tokio::time::timeout(IDLE, rx.recv()).await {
            wr.write_all(&to_stream(m)).await?;
        }
        Ok::<_, io::Error>("idle")
    };
    let down = async {
        let e = loop {
            match read_message(&mut rd).await {
                Ok(m) => drop(sock.send_to(&m, src).await),
                Err(e) => break e,
            }
        };
        Err::<&str, _>(e)
    };
    let end = tokio::select! { r = up => r, r = down => r };
    tracing::debug!(relay = %target, %src, end = ?end, "经 TCP 连线路的 TURN 连接结束");
}

/// UDP 上来的消息 → TCP 上的写法：ChannelData 补齐到 4 的倍数（STUN 消息本来就是）。
fn to_stream(mut m: Vec<u8>) -> Vec<u8> {
    if m.first().is_some_and(|b| b >> 6 == 1) {
        m.resize(m.len().div_ceil(4) * 4, 0);
    }
    m
}

/// 从 TCP 读一条消息（去掉 ChannelData 的补齐）。前两位既不是 STUN（00）也不是 ChannelData（01）时出错。
async fn read_message(rd: &mut (impl AsyncRead + Unpin)) -> io::Result<Vec<u8>> {
    let mut head = [0u8; 4];
    rd.read_exact(&mut head).await?;
    let len = u16::from_be_bytes([head[2], head[3]]) as usize;
    let (total, keep) = match head[0] >> 6 {
        0 => (20 + len, 20 + len),
        1 => (4 + len.div_ceil(4) * 4, 4 + len),
        _ => return Err(io::Error::new(io::ErrorKind::InvalidData, "不是 STUN / ChannelData")),
    };
    let mut m = vec![0u8; total];
    m[..4].copy_from_slice(&head);
    rd.read_exact(&mut m[4..]).await?;
    m.truncate(keep);
    Ok(m)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stun(len: u16) -> Vec<u8> {
        let mut m = vec![0x00, 0x01];
        m.extend_from_slice(&len.to_be_bytes());
        m.extend_from_slice(&[0x21, 0x12, 0xa4, 0x42]);
        m.extend_from_slice(&[9; 12]);
        m.extend(std::iter::repeat_n(7, len as usize));
        m
    }

    fn channel_data(payload: &[u8]) -> Vec<u8> {
        let mut m = vec![0x40, 0x01];
        m.extend_from_slice(&(payload.len() as u16).to_be_bytes());
        m.extend_from_slice(payload);
        m
    }

    #[tokio::test]
    async fn frames_messages_on_the_stream() {
        assert_eq!(to_stream(channel_data(b"abcde")).len(), 12, "补齐到 4 的倍数");
        assert_eq!(to_stream(channel_data(b"abcd")).len(), 8);
        assert_eq!(to_stream(stun(8)).len(), 28);
        let msgs = [stun(0), channel_data(b"hello"), stun(12), channel_data(b""), channel_data(b"xyz")];
        let stream: Vec<u8> = msgs.iter().cloned().flat_map(to_stream).collect();
        let mut rd = &stream[..];
        for m in &msgs {
            assert_eq!(&read_message(&mut rd).await.unwrap(), m);
        }
        assert!(read_message(&mut rd).await.is_err(), "读完了");
        let mut bad = &[0x80u8, 0, 0, 0][..];
        assert_eq!(read_message(&mut bad).await.unwrap_err().kind(), io::ErrorKind::InvalidData);
    }

    async fn echo(sock: &UdpSocket, local: SocketAddr, m: Vec<u8>) {
        sock.send_to(&m, local).await.unwrap();
        let mut buf = [0u8; 2048];
        let (n, from) = tokio::time::timeout(Duration::from_secs(3), sock.recv_from(&mut buf)).await.expect("收到回应").unwrap();
        assert_eq!(from, local);
        assert_eq!(&buf[..n], &m[..]);
    }

    /// 经本地转发到一个回显的“TURN 服务器”：每个本地端口一条 TCP 连接，消息原样往返。
    #[tokio::test]
    async fn forwards_each_port_over_its_own_connection() {
        let server = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let target = server.local_addr().unwrap().to_string();
        let accepted = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let n = accepted.clone();
        tokio::spawn(async move {
            loop {
                let (mut s, _) = server.accept().await.unwrap();
                n.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                tokio::spawn(async move {
                    while let Ok(m) = read_message(&mut s).await {
                        s.write_all(&to_stream(m)).await.unwrap();
                    }
                });
            }
        });
        set_tcp(&target, true);
        let local = local_for(&target).expect("开了本地转发");
        assert_eq!(local_for(&target.to_uppercase()), Some(local), "不分大小写");
        let a = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let b = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        echo(&a, local, stun(8)).await;
        echo(&a, local, channel_data(b"hello")).await;
        echo(&b, local, channel_data(b"world!!")).await;
        assert_eq!(accepted.load(std::sync::atomic::Ordering::SeqCst), 2);
        set_tcp(&target, false);
        assert_eq!(local_for(&target), None);
        set_tcp(&target, true);
        assert_eq!(local_for(&target), Some(local), "再打开时用原来的转发");
    }
}
