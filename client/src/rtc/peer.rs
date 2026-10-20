//! libdatachannel（`datachannel` crate）的封装：回调线程上的事件转成 tokio 通道。
//!
//! 回调里只往无界通道里发消息，不做别的：`rtcDelete*` 会等正在执行的回调结束，回调里阻塞就会死锁。
//! 通道（[`Channel`]）要先于它所属的连接（[`Pc`]）释放，见 `super::run_peer`。

use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use datachannel::{
    ConnectionState, DataChannelHandler, DataChannelInfo, GatheringState, IceCandidate, PeerConnectionHandler, RtcConfig,
    RtcDataChannel, RtcPeerConnection, SessionDescription,
};
use looklook_protocol::rtc::IceServer;
use tokio::sync::{mpsc, watch};

/// 发送缓冲超过这么多就暂停发送，降到 [`LOW_WATER`] 以下再继续（docs/06 §6.2）。
pub const HIGH_WATER: usize = 1024 * 1024;
pub const LOW_WATER: usize = 256 * 1024;

/// 拿到第一个经 STUN / TURN 得到的候选（srflx / relay）后再等这么久，让同一台服务器上的 TURN 中继候选也赶上，
/// 然后就回 answer，不等收集全部结束（连不上的 STUN / TURN 地址要等到超时才算结束）。
const REFLEXIVE_GRACE: Duration = Duration::from_millis(400);

/// 连接状态（`datachannel::ConnectionState` 不能复制，转成这个）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PcState {
    New,
    Connecting,
    Connected,
    Disconnected,
    Failed,
    Closed,
}

impl From<&ConnectionState> for PcState {
    fn from(s: &ConnectionState) -> Self {
        match s {
            ConnectionState::New => PcState::New,
            ConnectionState::Connecting => PcState::Connecting,
            ConnectionState::Connected => PcState::Connected,
            ConnectionState::Disconnected => PcState::Disconnected,
            ConnectionState::Failed => PcState::Failed,
            ConnectionState::Closed => PcState::Closed,
        }
    }
}

#[derive(Debug)]
pub enum DcEvent {
    Open,
    Msg(Vec<u8>),
    /// 发送缓冲降到了 [`LOW_WATER`] 以下。
    Low,
    Closed,
}

pub struct DcHandler(mpsc::UnboundedSender<DcEvent>);

impl DataChannelHandler for DcHandler {
    fn on_open(&mut self) {
        let _ = self.0.send(DcEvent::Open);
    }
    fn on_closed(&mut self) {
        let _ = self.0.send(DcEvent::Closed);
    }
    fn on_error(&mut self, err: &str) {
        tracing::debug!(error = err, "DataChannel 出错");
        let _ = self.0.send(DcEvent::Closed);
    }
    fn on_message(&mut self, msg: &[u8]) {
        let _ = self.0.send(DcEvent::Msg(msg.to_vec()));
    }
    fn on_buffered_amount_low(&mut self) {
        let _ = self.0.send(DcEvent::Low);
    }
}

/// 一条 DataChannel（浏览器开的，或者本机作为发起方开的）。释放它就关闭通道（libdatachannel 先发完已排队的数据）。
pub struct Channel {
    dc: Box<RtcDataChannel<DcHandler>>,
    rx: mpsc::UnboundedReceiver<DcEvent>,
    pub label: String,
}

impl Channel {
    pub fn send(&mut self, msg: &[u8]) -> Result<()> {
        self.dc.send(msg).map_err(|e| anyhow!("DataChannel 发送失败：{e:?}"))
    }

    /// 发送缓冲超过高水位，应等 [`DcEvent::Low`] 再发。
    pub fn congested(&self) -> bool {
        self.dc.buffered_amount() > HIGH_WATER
    }

    /// 下一个事件；通道关闭后为 None（之后一直是 None）。
    pub async fn event(&mut self) -> Option<DcEvent> {
        match self.rx.recv().await {
            Some(DcEvent::Closed) | None => {
                self.rx.close();
                None
            }
            ev => ev,
        }
    }

    /// 等通道打开（本机开的通道）；先关闭或超时都算失败。
    pub async fn opened(&mut self, limit: Duration) -> Result<()> {
        let wait = async {
            loop {
                match self.rx.recv().await {
                    Some(DcEvent::Open) => return Ok(()),
                    Some(DcEvent::Closed) | None => bail!("通道没能打开"),
                    Some(_) => {}
                }
            }
        };
        tokio::time::timeout(limit, wait).await.map_err(|_| anyhow!("通道打开超时"))?
    }

    /// 下一条消息（跳过其他事件）；通道关闭后为 None。
    pub async fn recv(&mut self) -> Option<Vec<u8>> {
        loop {
            if let DcEvent::Msg(m) = self.event().await? {
                return Some(m);
            }
        }
    }
}

struct PcHandler {
    incoming: mpsc::UnboundedSender<Channel>,
    /// 对端开的通道：`data_channel_handler` 与 `on_data_channel` 成对、依次调用，中间暂存。
    pending: Option<(String, mpsc::UnboundedReceiver<DcEvent>)>,
    state: watch::Sender<PcState>,
    gathered: watch::Sender<bool>,
    /// 已得到的 srflx / relay 候选数。
    reflexive: watch::Sender<usize>,
}

impl PeerConnectionHandler for PcHandler {
    type DCH = DcHandler;

    fn data_channel_handler(&mut self, info: DataChannelInfo) -> DcHandler {
        let (tx, rx) = mpsc::unbounded_channel();
        self.pending = Some((info.label, rx));
        DcHandler(tx)
    }

    fn on_data_channel(&mut self, mut dc: Box<RtcDataChannel<DcHandler>>) {
        if let Some((label, rx)) = self.pending.take() {
            let _ = dc.set_buffered_amount_low_threshold(LOW_WATER);
            let _ = self.incoming.send(Channel { dc, rx, label });
        }
    }

    fn on_connection_state_change(&mut self, state: ConnectionState) {
        let _ = self.state.send(PcState::from(&state));
    }

    fn on_candidate(&mut self, cand: IceCandidate) {
        if cand.candidate.contains(" typ srflx") || cand.candidate.contains(" typ relay") {
            self.reflexive.send_modify(|n| *n += 1);
        }
    }

    fn on_gathering_state_change(&mut self, state: GatheringState) {
        if state == GatheringState::Complete {
            let _ = self.gathered.send(true);
        }
    }
}

/// 连接实际走的路（界面电脑菜单里的色点，见 src-ui/shared/paths.js）：两边都是内网地址为局域网直连，
/// 有一边是中继为中转，其余（含两边都有公网 IPv6 直接连上）为公网直连。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PathKind {
    Lan,
    P2p,
    Relay,
}

/// 选中的那对候选：路径类型与两边的“地址 类型”（日志与界面详情用）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct PathInfo {
    pub kind: PathKind,
    pub local: String,
    pub remote: String,
}

/// `candidate:1 1 UDP 2122317823 192.168.1.2 42009 typ host …` → (地址, 类型)。
fn cand_parts(c: &str) -> Option<(&str, &str)> {
    let t: Vec<&str> = c.trim().trim_start_matches("a=").split_whitespace().collect();
    let typ = t.iter().position(|x| *x == "typ").and_then(|i| t.get(i + 1))?;
    Some((t.get(4)?, typ))
}

/// 内网地址：RFC 1918、IPv6 ULA / 链路本地，以及浏览器隐藏真实 IP 用的 mDNS 名（`xxxx.local`）。
fn private_addr(a: &str) -> bool {
    if a.ends_with(".local") {
        return true;
    }
    match a.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(v)) => v.is_private(),
        Ok(std::net::IpAddr::V6(v)) => (v.segments()[0] & 0xfe00) == 0xfc00 || (v.segments()[0] & 0xffc0) == 0xfe80,
        Err(_) => false,
    }
}

pub fn classify(local: &str, remote: &str) -> Option<PathInfo> {
    let (la, lt) = cand_parts(local)?;
    let (ra, rt) = cand_parts(remote)?;
    let kind = if lt == "relay" || rt == "relay" {
        PathKind::Relay
    } else if private_addr(la) && private_addr(ra) {
        PathKind::Lan
    } else {
        PathKind::P2p
    };
    Some(PathInfo { kind, local: format!("{la} {lt}"), remote: format!("{ra} {rt}") })
}

/// 一条 PeerConnection（应答方：浏览器连进来；发起方：本机 App 连同一账户的其他电脑，见 `super::outbound`）。
pub struct Pc {
    pc: Box<RtcPeerConnection<PcHandler>>,
    pub state: watch::Receiver<PcState>,
    gathered: watch::Receiver<bool>,
    reflexive: watch::Receiver<usize>,
    pub incoming: mpsc::UnboundedReceiver<Channel>,
}

impl Pc {
    /// 按浏览器的 offer 建立连接（libdatachannel 随即自动生成 answer 并开始收集候选）。
    pub fn answer(ice: &[IceServer], offer_sdp: &str) -> Result<Self> {
        let cfg = RtcConfig::new(&ice_urls(ice));
        let (state_tx, state) = watch::channel(PcState::New);
        let (gathered_tx, gathered) = watch::channel(false);
        let (reflexive_tx, reflexive) = watch::channel(0);
        let (incoming_tx, incoming) = mpsc::unbounded_channel();
        let h = PcHandler { incoming: incoming_tx, pending: None, state: state_tx, gathered: gathered_tx, reflexive: reflexive_tx };
        let mut pc = RtcPeerConnection::new(&cfg, h).map_err(|e| anyhow!("创建 PeerConnection 失败：{e:?}"))?;
        let offer: SessionDescription = serde_json::from_value(serde_json::json!({ "type": "offer", "sdp": offer_sdp }))
            .map_err(|e| anyhow!("offer 无法解析：{e}"))?;
        pc.set_remote_description(&offer).map_err(|e| anyhow!("设置 offer 失败：{e:?}"))?;
        Ok(Self { pc, state, gathered, reflexive, incoming })
    }

    /// 作为发起方建立连接：开一条 `ctl` 通道（libdatachannel 随即生成 offer 并开始收集候选）。
    pub fn offer(ice: &[IceServer]) -> Result<(Self, Channel)> {
        let cfg = RtcConfig::new(&ice_urls(ice));
        let (state_tx, state) = watch::channel(PcState::New);
        let (gathered_tx, gathered) = watch::channel(false);
        let (reflexive_tx, reflexive) = watch::channel(0);
        let (incoming_tx, incoming) = mpsc::unbounded_channel();
        let h = PcHandler { incoming: incoming_tx, pending: None, state: state_tx, gathered: gathered_tx, reflexive: reflexive_tx };
        let pc = RtcPeerConnection::new(&cfg, h).map_err(|e| anyhow!("创建 PeerConnection 失败：{e:?}"))?;
        let mut me = Self { pc, state, gathered, reflexive, incoming };
        let ctl = me.channel("ctl")?;
        Ok((me, ctl))
    }

    /// 连上以后实际走的路；还没选出候选对时为 None。
    pub fn path(&self) -> Option<PathInfo> {
        let p = self.pc.selected_candidate_pair()?;
        classify(&p.local, &p.remote)
    }

    /// 本机开一条通道（发起方）。
    pub fn channel(&mut self, label: &str) -> Result<Channel> {
        let (tx, rx) = mpsc::unbounded_channel();
        let mut dc = self.pc.create_data_channel(label, DcHandler(tx)).map_err(|e| anyhow!("创建 DataChannel 失败：{e:?}"))?;
        let _ = dc.set_buffered_amount_low_threshold(LOW_WATER);
        Ok(Channel { dc, rx, label: label.to_string() })
    }

    /// 发起方：设置对端的 answer。
    pub fn set_answer(&mut self, sdp: &str) -> Result<()> {
        let answer: SessionDescription = serde_json::from_value(serde_json::json!({ "type": "answer", "sdp": sdp })).map_err(|e| anyhow!("answer 无法解析：{e}"))?;
        self.pc.set_remote_description(&answer).map_err(|e| anyhow!("设置 answer 失败：{e:?}"))
    }

    /// 发起方：等候选收集完，返回 offer SDP（规则同 [`Self::local_answer`]）。
    pub async fn local_offer(&mut self, limit: Duration) -> Result<String> {
        self.local_sdp("offer", limit).await
    }

    /// 等候选收集完（不用 trickle），返回 answer SDP：收集结束、或者有了 srflx / relay 候选再等 [`REFLEXIVE_GRACE`]，
    /// 最多 `limit`。
    pub async fn local_answer(&mut self, limit: Duration) -> Result<String> {
        self.local_sdp("answer", limit).await
    }

    async fn local_sdp(&mut self, kind: &str, limit: Duration) -> Result<String> {
        let (mut gathered, mut reflexive) = (self.gathered.clone(), self.reflexive.clone());
        let enough = async move {
            tokio::select! {
                _ = gathered.wait_for(|g| *g) => {}
                _ = async { let _ = reflexive.wait_for(|n| *n > 0).await; tokio::time::sleep(REFLEXIVE_GRACE).await } => {}
            }
        };
        let _ = tokio::time::timeout(limit, enough).await;
        let d = self.pc.local_description().ok_or_else(|| anyhow!("没有生成 {kind}"))?;
        let v = serde_json::to_value(&d)?;
        if v["type"] != kind {
            bail!("本地描述不是 {kind}：{}", v["type"]);
        }
        v["sdp"].as_str().map(str::to_owned).ok_or_else(|| anyhow!("{kind} 缺少 sdp"))
    }
}

/// `host:port` 里的主机名按客户端自己的规则解析（`*.localhost`、`LOOKLOOK_RESOLVE`，见 `platform`），
/// 其他主机名原样交给 libjuice 自己解析。
fn resolve_host(hostport: &str) -> String {
    let Some((host, port)) = hostport.rsplit_once(':') else { return hostport.to_string() };
    match crate::platform::resolve_override(host).or_else(|| crate::platform::localhost_addr(host)) {
        Some(std::net::IpAddr::V6(ip)) => format!("[{ip}]:{port}"),
        Some(ip) => format!("{ip}:{port}"),
        None => hostport.to_string(),
    }
}

/// 浏览器格式的 ICE 服务器 → libdatachannel 的 URL：`stun:host:port`、`turn:user:pass@host:port?transport=udp`。
/// libjuice 只支持 UDP 的 TURN，`transport=tcp` 的地址跳过（docs/06 §11）。
pub fn ice_urls(servers: &[IceServer]) -> Vec<String> {
    let enc = |s: &str| {
        s.bytes()
            .map(|b| match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => (b as char).to_string(),
                _ => format!("%{b:02X}"),
            })
            .collect::<String>()
    };
    let mut out = Vec::new();
    for s in servers {
        for url in &s.urls {
            if let Some(rest) = url.strip_prefix("stun:") {
                out.push(format!("stun:{}", resolve_host(rest)));
            } else if let Some(rest) = url.strip_prefix("turn:") {
                let (hostport, query) = rest.split_once('?').unwrap_or((rest, ""));
                let hostport = resolve_host(hostport);
                let udp = query.split('&').all(|kv| kv != "transport=tcp");
                if !udp {
                    continue;
                }
                let (Some(u), Some(p)) = (&s.username, &s.credential) else { continue };
                out.push(format!("turn:{}:{}@{hostport}?transport=udp", enc(u), enc(p)));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ice_url_conversion() {
        assert_eq!(resolve_host("r1.looklook.localhost:3478"), "127.0.0.1:3478");
        let s = looklook_protocol::rtc::ice_servers("r2.x", b"k", "1", 1_700_000_000);
        let urls = ice_urls(&s);
        assert_eq!(urls.len(), 2, "{urls:?}");
        assert_eq!(urls[0], "stun:r2.x:3478");
        assert!(urls[1].starts_with("turn:1700000000%3A1:"), "{}", urls[1]);
        assert!(urls[1].ends_with("@r2.x:3478?transport=udp"));
        // base64 密码里的 + / = 都要编码
        let p = s[1].credential.clone().unwrap();
        assert!(!urls[1].contains(&p) || !p.contains(['+', '/', '=']));
    }

    #[test]
    fn path_kinds() {
        let c = |a: &str, t: &str| format!("a=candidate:1 1 UDP 2122317823 {a} 42009 typ {t}");
        let k = |l: String, r: String| classify(&l, &r).unwrap().kind;
        assert_eq!(k(c("192.168.1.2", "host"), c("192.168.1.9", "prflx")), PathKind::Lan);
        assert_eq!(k(c("10.0.0.2", "host"), c("3f1c-aa.local", "host")), PathKind::Lan);
        assert_eq!(k(c("203.0.113.5", "srflx"), c("198.51.100.7", "srflx")), PathKind::P2p);
        assert_eq!(k(c("2001:db8::1", "host"), c("2001:db8:1::2", "host")), PathKind::P2p);
        assert_eq!(k(c("192.168.1.2", "host"), c("203.0.113.9", "relay")), PathKind::Relay);
        assert!(classify("garbage", "x").is_none());
    }

    /// 本机候选地址不含 Clash/mihomo 的 TUN / fake-ip 地址（198.18.0.0/15），私有 IPv4 排在最前（third_party 的 libjuice 补丁）。
    #[tokio::test]
    async fn host_candidates_skip_tun() {
        let (mut pc, _ch) = Pc::offer(&[]).unwrap();
        let sdp = pc.local_offer(Duration::from_secs(5)).await.unwrap();
        let hosts: Vec<&str> = sdp.lines().filter(|l| l.starts_with("a=candidate") && l.contains(" typ host")).collect();
        eprintln!("{hosts:#?}");
        assert!(!hosts.iter().any(|l| l.contains(" 198.18.") || l.contains(" 198.19.")), "{hosts:?}");
    }
}
