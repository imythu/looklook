//! 可续接的终端 WebSocket（服务端 docs/06 §15）。
//!
//! 手机切后台、网络切换时，浏览器常常要换一条新的 PeerConnection（libjuice 不支持 ICE restart）。原来终端的
//! WebSocket 跟着通道一起断，ttyd 前端重连、tmux 重画整屏，中间显示“正在重连”。现在：
//!
//! - 打开 WebSocket 时浏览器带 `"resume": true`，这边回的 `open` 帧带续接令牌 `resume`；
//! - 到 ttyd 的 WebSocket 由一个独立的会话任务（[`run`]）持有，不属于哪条通道。通道只是它当前的“接入”：
//!   通道断了（没收到浏览器的 `close`），会话保留 [`PARK`]，期间 ttyd 的输出照常读进缓冲；
//! - 浏览器连上新的 PeerConnection 后开一条通道，首帧 `{"t":"resume","k":令牌,"rx":收到的消息数}`。
//!   会话从第 `rx` 条消息起重发（已交给旧通道、可能没送到的输出保留最近 [`RETAIN`]），回 `{"t":"resumed","rx":收到的消息数}`，
//!   浏览器据此重发它那边没送到的输入。缓冲里已经没有第 `rx` 条时续接失败，回 `close`，浏览器照常重连；
//! - 令牌只在同一个子站会话（`sid`）里有效；吊销、会话到期时连同保留中的会话一起关闭。
//!
//! 不带 `resume` 的 WebSocket（本机 App 连别的电脑、旧版启动页）走同一套代码，只是通道断了会话就结束。

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::Duration;

use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{mpsc, oneshot};
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;
use tokio_util::sync::CancellationToken;

/// 通道断开后保留会话的时间（浏览器那边放弃得更早一些）。
pub const PARK: Duration = Duration::from_secs(60);
/// 已交给通道的输出保留这么多，供续接时重发（发送缓冲高水位 1 MiB，再留些余量）。
const RETAIN: usize = 2 * 1024 * 1024;
/// 还没交给通道的输出超过这么多就先不读 ttyd（背压到 ttyd 与 tmux）。
const BACKLOG: usize = 4 * 1024 * 1024;
/// 会话到接入之间的队列长度（消息数）。
const QUEUE: usize = 16;

/// 会话 → 接入。
#[derive(Debug)]
pub enum Out {
    Msg(bool, Bytes),
    Close(u16),
}

/// 接入 → 会话。
#[derive(Debug)]
pub enum In {
    Msg(Message),
    Close,
}

/// 一条通道接到会话上：会话从第 `rx` 条输出起发给 `out`；`reply` 回会话已收到的输入条数，续接不了时为 None。
pub struct Attach {
    rx: u64,
    out: mpsc::Sender<Out>,
    inp: mpsc::UnboundedReceiver<In>,
    reply: oneshot::Sender<Option<u64>>,
}

/// 接入一侧的两端：`out` 收会话的输出，`inp` 把浏览器的输入交给会话。
pub struct Port {
    pub out: mpsc::Receiver<Out>,
    pub inp: mpsc::UnboundedSender<In>,
}

fn port() -> (Port, mpsc::Sender<Out>, mpsc::UnboundedReceiver<In>) {
    let (out_tx, out) = mpsc::channel(QUEUE);
    let (inp, inp_rx) = mpsc::unbounded_channel();
    (Port { out, inp }, out_tx, inp_rx)
}

struct Entry {
    sid: String,
    attach: mpsc::Sender<Attach>,
    cancel: CancellationToken,
}

/// 可续接的会话：令牌 → 会话。
#[derive(Default)]
pub struct Terms {
    m: Mutex<HashMap<String, Entry>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Terms {
    /// 关闭这些子站会话的终端会话。
    pub fn revoke(&self, sids: &[String]) {
        lock(&self.m).values().filter(|e| sids.contains(&e.sid)).for_each(|e| e.cancel.cancel());
    }

    pub fn close_all(&self) {
        lock(&self.m).values().for_each(|e| e.cancel.cancel());
    }

    pub fn sids(&self) -> Vec<String> {
        lock(&self.m).values().map(|e| e.sid.clone()).collect()
    }

    pub fn count(&self) -> usize {
        lock(&self.m).len()
    }

    /// 续接：令牌对得上、属于同一个子站会话时，返回接入的一端与会话已收到的输入条数。
    pub async fn resume(&self, sid: &str, key: &str, rx: u64) -> Option<(Port, u64)> {
        let attach = {
            let m = lock(&self.m);
            let e = m.get(key).filter(|e| e.sid == sid)?;
            e.attach.clone()
        };
        let (port, out, inp) = port();
        let (reply, got) = oneshot::channel();
        attach.send(Attach { rx, out, inp, reply }).await.ok()?;
        Some((port, got.await.ok()??))
    }
}

/// 起一个会话任务，返回第一条接入的一端与续接令牌（`resumable` 为 false 时没有令牌，接入断了会话就结束）。
/// `guard` 与会话同生共死（内存里那条 HTTP 连接的服务端任务）。会话在 `expires` 时结束。
pub fn spawn<S, G>(terms: &std::sync::Arc<Terms>, sid: &str, ws: WebSocketStream<S>, guard: G, resumable: bool, expires: Instant) -> (Port, Option<String>)
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    G: Send + 'static,
{
    let (port, out, inp) = port();
    let cancel = CancellationToken::new();
    let (attach_tx, attach_rx) = mpsc::channel(1);
    let key = resumable.then(|| {
        let key = looklook_protocol::signing::b64(&crate::util::random_bytes::<18>());
        lock(&terms.m).insert(key.clone(), Entry { sid: sid.to_string(), attach: attach_tx, cancel: cancel.clone() });
        key
    });
    let terms = terms.clone();
    let k = key.clone();
    tokio::spawn(async move {
        let park = if resumable { PARK } else { Duration::ZERO };
        run(ws, Some((out, inp)), attach_rx, cancel, park, expires).await;
        drop(guard);
        if let Some(k) = k {
            lock(&terms.m).remove(&k);
        }
    });
    (port, key)
}

/// 输出缓冲：第 `first` 条起的消息；`sent` 之前的已交给通道（保留最近 [`RETAIN`] 供重发）。
#[derive(Default)]
struct Outbox {
    q: VecDeque<(bool, Bytes)>,
    first: u64,
    sent: u64,
    /// `sent` 之前、之后的字节数。
    held: usize,
    backlog: usize,
}

impl Outbox {
    fn next(&self) -> u64 {
        self.first + self.q.len() as u64
    }

    fn push(&mut self, text: bool, b: Bytes) {
        self.backlog += b.len();
        self.q.push_back((text, b));
    }

    fn peek(&self) -> Option<(bool, Bytes)> {
        self.q.get((self.sent - self.first) as usize).cloned()
    }

    fn advance(&mut self) {
        if let Some((_, b)) = self.peek() {
            self.sent += 1;
            self.held += b.len();
            self.backlog -= b.len();
        }
        while self.held > RETAIN && self.first < self.sent {
            let (_, b) = self.q.pop_front().expect("held");
            self.first += 1;
            self.held -= b.len();
        }
    }

    /// 从第 `rx` 条起重发；缓冲里已经没有就为 false。
    fn rewind(&mut self, rx: u64) -> bool {
        if rx < self.first || rx > self.next() {
            return false;
        }
        self.sent = rx;
        let split = (rx - self.first) as usize;
        self.held = self.q.iter().take(split).map(|(_, b)| b.len()).sum();
        self.backlog = self.q.iter().skip(split).map(|(_, b)| b.len()).sum();
        true
    }
}

type Cur = (mpsc::Sender<Out>, mpsc::UnboundedReceiver<In>);

async fn recv_in(cur: &mut Option<Cur>) -> Option<In> {
    match cur {
        Some((_, inp)) => inp.recv().await,
        None => std::future::pending().await,
    }
}

async fn reserve(out: Option<mpsc::Sender<Out>>) -> Option<mpsc::OwnedPermit<Out>> {
    match out {
        Some(out) => out.reserve_owned().await.ok(),
        None => std::future::pending().await,
    }
}

async fn run<S>(mut ws: WebSocketStream<S>, mut cur: Option<Cur>, mut attach_rx: mpsc::Receiver<Attach>, cancel: CancellationToken, park: Duration, expires: Instant)
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut ob = Outbox::default();
    let mut received: u64 = 0;
    // ttyd 那边结束了：把缓冲里的输出发完再把关闭码交给浏览器
    let mut ended: Option<u16> = None;
    let far = || Instant::now() + Duration::from_secs(86_400 * 365);
    let parked = tokio::time::sleep_until(far());
    let lifetime = tokio::time::sleep_until(expires);
    tokio::pin!(parked, lifetime);
    let why = loop {
        let pending = ob.sent < ob.next() || ended.is_some();
        let out = cur.as_ref().filter(|_| pending).map(|(out, _)| out.clone());
        tokio::select! {
            _ = cancel.cancelled() => break "revoked",
            _ = &mut lifetime => break "session_expired",
            _ = &mut parked, if cur.is_none() => break "park_timeout",
            // 不可续接的会话没有登记，发送端一开始就没了
            a = attach_rx.recv(), if !park.is_zero() => {
                let Some(a) = a else { break "closed" };
                if !ob.rewind(a.rx) {
                    let _ = a.reply.send(None);
                    break "resume_gap";
                }
                let _ = a.reply.send(Some(received));
                cur = Some((a.out, a.inp));
                parked.as_mut().reset(far());
            }
            m = recv_in(&mut cur), if cur.is_some() => match m {
                Some(In::Msg(m)) => {
                    if ws.send(m).await.is_err() {
                        ended = ended.or(Some(1006));
                    }
                    received += 1;
                }
                Some(In::Close) => {
                    let _ = ws.close(None).await;
                    return;
                }
                None => {
                    cur = None;
                    if park.is_zero() {
                        break "detached";
                    }
                    parked.as_mut().reset(Instant::now() + park);
                }
            },
            p = reserve(out), if pending && cur.is_some() => match p {
                Some(p) => match ob.peek() {
                    Some((text, b)) => {
                        p.send(Out::Msg(text, b));
                        ob.advance();
                    }
                    None => {
                        p.send(Out::Close(ended.unwrap_or(1006)));
                        return;
                    }
                },
                // 接入已经没了：下一轮 recv_in 会看到
                None => {}
            },
            m = ws.next(), if ended.is_none() && ob.backlog < BACKLOG => match m {
                Some(Ok(Message::Text(t))) => ob.push(true, Bytes::from(t)),
                Some(Ok(Message::Binary(b))) => ob.push(false, b),
                Some(Ok(Message::Close(c))) => ended = Some(c.map(|c| u16::from(c.code)).unwrap_or(1000)),
                Some(Ok(_)) => {}
                Some(Err(_)) | None => ended = Some(1006),
            },
        }
    };
    tracing::debug!(why, "终端会话结束");
    if let Some((out, _)) = cur {
        let _ = out.try_send(Out::Close(1006));
    }
    let _ = ws.close(None).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outbox_keeps_recent_output_for_rewind() {
        let mut ob = Outbox::default();
        let chunk = Bytes::from(vec![0u8; 512 * 1024]);
        for _ in 0..8 {
            ob.push(false, chunk.clone());
        }
        assert_eq!(ob.backlog, 4 * 1024 * 1024);
        for _ in 0..6 {
            ob.advance();
        }
        // 已发 6 条（3 MiB），只保留最近 2 MiB（4 条）
        assert_eq!((ob.first, ob.sent, ob.held, ob.backlog), (2, 6, RETAIN, 1024 * 1024));
        assert!(!ob.rewind(1), "too old");
        assert!(!ob.rewind(9), "from the future");
        assert!(ob.rewind(3));
        assert_eq!((ob.sent, ob.held, ob.backlog), (3, 512 * 1024, 5 * 512 * 1024));
        assert!(ob.rewind(8));
        assert_eq!(ob.peek(), None);
    }
}
