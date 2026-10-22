//! libdatachannel（`datachannel` crate）的封装：回调线程上的事件转成 tokio 通道。
//!
//! 回调里只往无界通道里发消息，不做别的：`rtcDelete*` 会等正在执行的回调结束，回调里阻塞就会死锁。
//! 通道（[`Channel`]）要先于它所属的连接（[`Pc`]）释放，见 `super::run_peer`。

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

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
    gather: Arc<Mutex<GatherLog>>,
}

/// 候选收集的经过（电脑端没拿到 srflx / relay 候选时记进日志）：每个候选与距开始的毫秒数、收集结束的时间。
struct GatherLog {
    start: Instant,
    cands: Vec<(u128, String)>,
    done: Option<u128>,
}

impl GatherLog {
    fn new() -> Arc<Mutex<Self>> {
        Arc::new(Mutex::new(Self { start: Instant::now(), cands: Vec::new(), done: None }))
    }

    /// `host 172.17.0.2@3ms srflx 203.0.113.5@120ms done@450ms`（还没结束时末尾为 `pending@…`）。
    fn describe(&self) -> String {
        let mut out: Vec<String> = self.cands.iter().map(|(ms, c)| format!("{c}@{ms}ms")).collect();
        out.push(match self.done {
            Some(ms) => format!("done@{ms}ms"),
            None => format!("pending@{}ms", self.start.elapsed().as_millis()),
        });
        out.join(" ")
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
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
        let mut g = lock(&self.gather);
        let ms = g.start.elapsed().as_millis();
        let c = cand_parts(&cand.candidate).map_or_else(|| cand.candidate.clone(), |(a, t)| format!("{t} {a}"));
        g.cands.push((ms, c));
    }

    fn on_gathering_state_change(&mut self, state: GatheringState) {
        if state == GatheringState::Complete {
            let mut g = lock(&self.gather);
            g.done = Some(g.start.elapsed().as_millis());
            drop(g);
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

#[cfg(test)]
fn classify(local: &str, remote: &str) -> Option<PathInfo> {
    let (la, lt) = cand_parts(local)?;
    let (ra, rt) = cand_parts(remote)?;
    Some(path_info(la, lt, ra, rt))
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

fn path_info(la: &str, lt: &str, ra: &str, rt: &str) -> PathInfo {
    let kind = if lt == "relay" || rt == "relay" {
        PathKind::Relay
    } else if private_addr(la) && private_addr(ra) {
        PathKind::Lan
    } else {
        PathKind::P2p
    };
    PathInfo { kind, local: format!("{la} {lt}"), remote: format!("{ra} {rt}") }
}

/// SDP 里地址为 `addr`（`ip:端口`，libjuice 的写法，IPv6 不带方括号）的候选的类型。
fn cand_type_at<'a>(sdp: &'a str, addr: &str) -> Option<&'a str> {
    let (ip, port) = addr.rsplit_once(':')?;
    let ip = ip.trim_matches(['[', ']']);
    sdp.lines().filter(|l| l.starts_with("a=candidate")).find_map(|l| {
        let t: Vec<&str> = l.split_whitespace().collect();
        let typ = t.iter().position(|x| *x == "typ").and_then(|i| t.get(i + 1))?;
        (t.get(4)?.eq_ignore_ascii_case(ip) && *t.get(5)? == port).then_some(*typ)
    })
}

/// 一条 PeerConnection（应答方：浏览器连进来；发起方：本机 App 连同一账户的其他电脑，见 `super::outbound`）。
pub struct Pc {
    pc: Box<RtcPeerConnection<PcHandler>>,
    pub state: watch::Receiver<PcState>,
    gathered: watch::Receiver<bool>,
    reflexive: watch::Receiver<usize>,
    pub incoming: mpsc::UnboundedReceiver<Channel>,
    gather: Arc<Mutex<GatherLog>>,
    /// STUN / TURN 服务器（`host:port`，不含凭证），排查用。
    servers: Vec<String>,
    /// 对端 SDP 里的候选概况（应答方才有），排查用。
    remote: String,
    /// offer 里的候选（`mid`, `candidate:…`），本机候选收集完再交给 libjuice（见 [`split_candidates`]）。
    deferred: Vec<(String, String)>,
    /// 对端的完整 SDP（含候选），[`Self::path`] 按地址查候选类型用。
    remote_sdp: String,
}

impl Pc {
    /// 按浏览器的 offer 建立连接（libdatachannel 随即自动生成 answer 并开始收集候选）。
    pub fn answer(ice: &[IceServer], offer_sdp: &str) -> Result<Self> {
        let mut me = Self::new(ice)?;
        let (sdp, deferred) = split_candidates(offer_sdp);
        let offer: SessionDescription =
            serde_json::from_value(serde_json::json!({ "type": "offer", "sdp": sdp })).map_err(|e| anyhow!("offer 无法解析：{e}"))?;
        me.pc.set_remote_description(&offer).map_err(|e| anyhow!("设置 offer 失败：{e:?}"))?;
        me.remote = cand_summary(offer_sdp);
        me.remote_sdp = offer_sdp.to_string();
        me.deferred = deferred;
        Ok(me)
    }

    /// 作为发起方建立连接：开一条 `ctl` 通道（libdatachannel 随即生成 offer 并开始收集候选）。
    pub fn offer(ice: &[IceServer]) -> Result<(Self, Channel)> {
        let mut me = Self::new(ice)?;
        let ctl = me.channel("ctl")?;
        Ok((me, ctl))
    }

    fn new(ice: &[IceServer]) -> Result<Self> {
        // libdatachannel / libjuice 自己的警告（STUN / TURN 失败、发送失败等）进日志；只设置一次
        datachannel::configure_logging(tracing::Level::WARN);
        let urls = ice_urls(ice);
        let cfg = RtcConfig::new(&urls);
        let (state_tx, state) = watch::channel(PcState::New);
        let (gathered_tx, gathered) = watch::channel(false);
        let (reflexive_tx, reflexive) = watch::channel(0);
        let (incoming_tx, incoming) = mpsc::unbounded_channel();
        let gather = GatherLog::new();
        let h = PcHandler { incoming: incoming_tx, pending: None, state: state_tx, gathered: gathered_tx, reflexive: reflexive_tx, gather: gather.clone() };
        let pc = RtcPeerConnection::new(&cfg, h).map_err(|e| anyhow!("创建 PeerConnection 失败：{e:?}"))?;
        let servers = urls.iter().map(|u| server_host(u)).collect();
        Ok(Self { pc, state, gathered, reflexive, incoming, gather, servers, remote: String::new(), deferred: Vec::new(), remote_sdp: String::new() })
    }

    /// 连上以后实际走的路；还没选出候选对时为 None。
    ///
    /// 不用 `selected_candidate_pair`：datachannel 0.16 给两端分配同样大小的缓冲区，短的那个末尾多出 NUL，
    /// 每次都转换失败（还记一条 ERROR）。这里取选中的两端地址，到两边的 SDP 里查候选类型；
    /// 对端 SDP 里没有的地址是对端经未公布的地址连过来的（prflx）。
    pub fn path(&self) -> Option<PathInfo> {
        let (la, ra) = (self.pc.local_address()?, self.pc.remote_address()?);
        let local = self.pc.local_description().and_then(|d| serde_json::to_value(&d).ok()).and_then(|v| v["sdp"].as_str().map(str::to_owned));
        let lt = local.as_deref().and_then(|sdp| cand_type_at(sdp, &la)).unwrap_or("host");
        let rt = cand_type_at(&self.remote_sdp, &ra).unwrap_or("prflx");
        let host = |a: &str| a.rsplit_once(':').map_or(a, |(h, _)| h).trim_matches(['[', ']']).to_string();
        Some(path_info(&host(&la), lt, &host(&ra), rt))
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
        self.remote_sdp = sdp.to_string();
        let answer: SessionDescription = serde_json::from_value(serde_json::json!({ "type": "answer", "sdp": sdp })).map_err(|e| anyhow!("answer 无法解析：{e}"))?;
        self.pc.set_remote_description(&answer).map_err(|e| anyhow!("设置 answer 失败：{e:?}"))
    }

    /// 发起方：等候选收集完，返回 offer SDP（规则同 [`Self::local_answer`]）。
    pub async fn local_offer(&mut self, limit: Duration) -> Result<String> {
        self.local_sdp("offer", limit).await
    }

    /// 等候选收集完（不用 trickle），返回 answer SDP：收集结束、或者有了 srflx / relay 候选再等 [`REFLEXIVE_GRACE`]，
    /// 最多 `limit`。
    /// 之后才把 offer 里的候选交给 libjuice。
    pub async fn local_answer(&mut self, limit: Duration) -> Result<String> {
        let sdp = self.local_sdp("answer", limit).await?;
        for (mid, candidate) in std::mem::take(&mut self.deferred) {
            if let Err(e) = self.pc.add_remote_candidate(&IceCandidate { candidate, mid }) {
                tracing::debug!(error = ?e, "添加对端候选失败");
            }
        }
        Ok(sdp)
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
        let sdp = v["sdp"].as_str().map(str::to_owned).ok_or_else(|| anyhow!("{kind} 缺少 sdp"))?;
        if !sdp.contains(" typ srflx") && !sdp.contains(" typ relay") && !self.servers.is_empty() {
            self.warn_no_reflexive(kind).await;
        }
        Ok(sdp)
    }

    /// 本机没拿到 srflx / relay 候选：对端多半连不上（浏览器报 RTC_DEVICE_NO_ROUTE）。记下收集经过、
    /// 服务器地址与系统解析结果（Clash/mihomo 的 fake-ip 会解析成 198.18.x），以及对端的候选概况。
    async fn warn_no_reflexive(&self, kind: &str) {
        let mut resolved = Vec::new();
        for s in &self.servers {
            if resolved.iter().any(|(h, _): &(String, String)| h == s) {
                continue;
            }
            let ips = match tokio::time::timeout(Duration::from_secs(1), tokio::net::lookup_host(s.as_str())).await {
                Ok(Ok(a)) => a.map(|a| a.ip().to_string()).collect::<Vec<_>>().join("|"),
                Ok(Err(e)) => format!("error: {e}"),
                Err(_) => "timeout".into(),
            };
            resolved.push((s.clone(), ips));
        }
        let resolved = resolved.iter().map(|(h, ip)| format!("{h}={ip}")).collect::<Vec<_>>().join(" ");
        let gather = lock(&self.gather).describe();
        tracing::warn!(kind, gather, servers = resolved, remote = self.remote, "WebRTC 本机没拿到公网/中继候选，对端可能连不上");
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

/// 把 offer 里的候选拿出来，等本机候选收集完再加（[`Pc::local_answer`]）。
///
/// 电脑上开着 Clash/mihomo 的 TUN 时，同一个本地 UDP 端口发出的第一个包决定这个端口之后走直连还是走代理。
/// 电脑版 Chrome 把本机地址藏成 mDNS 名，offer 里能用的只有它的公网 srflx：libjuice 一拿到就先向它发连通性检查，
/// 这个包按规则走了代理，之后同一端口发往中转的 STUN / TURN 请求也跟着走不通，answer 里只剩 host 候选，
/// 浏览器报 RTC_DEVICE_NO_ROUTE。先和中转通信，端口就按中转的规则（直连）走。
fn split_candidates(sdp: &str) -> (String, Vec<(String, String)>) {
    let mut out = String::with_capacity(sdp.len());
    let mut cands = Vec::new();
    let mut mid = String::new();
    for line in sdp.split_inclusive('\n') {
        let l = line.trim_end();
        if let Some(m) = l.strip_prefix("a=mid:") {
            mid = m.to_string();
        }
        if let Some(c) = l.strip_prefix("a=") {
            if c.starts_with("candidate:") {
                cands.push((mid.clone(), c.to_string()));
                continue;
            }
            if c == "end-of-candidates" {
                continue;
            }
        }
        out.push_str(line);
    }
    (out, cands)
}

/// `stun:host:port`、`turn:user:pass@host:port?transport=udp` → `host:port`（不含凭证）。
fn server_host(url: &str) -> String {
    let rest = url.split_once(':').map_or(url, |(_, r)| r);
    let rest = rest.rsplit_once('@').map_or(rest, |(_, r)| r);
    rest.split('?').next().unwrap_or(rest).to_string()
}

/// SDP 里的候选概况：`host×2(mdns×2) srflx×1 relay×1 ipv6×0`，没有候选时为 `none`。
fn cand_summary(sdp: &str) -> String {
    let (mut host, mut mdns, mut srflx, mut relay, mut prflx, mut v6) = (0, 0, 0, 0, 0, 0);
    for (addr, typ) in sdp.lines().filter(|l| l.starts_with("a=candidate")).filter_map(cand_parts) {
        match typ {
            "host" => host += 1,
            "srflx" => srflx += 1,
            "relay" => relay += 1,
            _ => prflx += 1,
        }
        if addr.ends_with(".local") {
            mdns += 1;
        } else if addr.contains(':') {
            v6 += 1;
        }
    }
    if host + srflx + relay + prflx == 0 {
        return "none".into();
    }
    format!("host×{host}(mdns×{mdns}) srflx×{srflx} relay×{relay} other×{prflx} ipv6×{v6}")
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
    fn diagnostics_helpers() {
        assert_eq!(server_host("stun:r3.x:3478"), "r3.x:3478");
        assert_eq!(server_host("turn:1700000000%3A1:p%2Fw@r3.x:3478?transport=udp"), "r3.x:3478");
        assert_eq!(server_host("stun:[2001:db8::1]:3478"), "[2001:db8::1]:3478");
        let sdp = "v=0\r\na=candidate:1 1 UDP 1 3f1c-aa.local 5000 typ host\r\na=candidate:2 1 UDP 1 2001:db8::1 5001 typ host\r\n\
                   a=candidate:3 1 UDP 1 203.0.113.5 5002 typ srflx raddr 0.0.0.0 rport 0\r\n";
        assert_eq!(cand_summary(sdp), "host×2(mdns×1) srflx×1 relay×0 other×0 ipv6×1");
        assert_eq!(cand_summary("v=0\r\n"), "none");
    }

    #[test]
    fn candidate_type_by_address() {
        let sdp = "a=candidate:1 1 UDP 1 192.168.1.2 5000 typ host\r\na=candidate:2 1 UDP 1 2001:db8::1 5001 typ srflx raddr 0.0.0.0 rport 0\r\n\
                   a=candidate:3 1 UDP 1 203.0.113.9 6000 typ relay raddr 1.2.3.4 rport 7\r\n";
        assert_eq!(cand_type_at(sdp, "192.168.1.2:5000"), Some("host"));
        assert_eq!(cand_type_at(sdp, "2001:db8::1:5001"), Some("srflx"));
        assert_eq!(cand_type_at(sdp, "203.0.113.9:6000"), Some("relay"));
        assert_eq!(cand_type_at(sdp, "192.168.1.2:5001"), None);
    }

    #[test]
    fn offer_candidates_are_deferred() {
        let sdp = "v=0\r\nm=application 9 UDP/DTLS/SCTP webrtc-datachannel\r\na=mid:0\r\na=ice-ufrag:x\r\n\
                   a=candidate:1 1 udp 1 3f1c-aa.local 5000 typ host\r\na=candidate:2 1 udp 1 203.0.113.5 5002 typ srflx\r\n\
                   a=end-of-candidates\r\na=sctp-port:5000\r\n";
        let (rest, cands) = split_candidates(sdp);
        assert_eq!(rest, "v=0\r\nm=application 9 UDP/DTLS/SCTP webrtc-datachannel\r\na=mid:0\r\na=ice-ufrag:x\r\na=sctp-port:5000\r\n");
        assert_eq!(
            cands,
            vec![
                ("0".to_string(), "candidate:1 1 udp 1 3f1c-aa.local 5000 typ host".to_string()),
                ("0".to_string(), "candidate:2 1 udp 1 203.0.113.5 5002 typ srflx".to_string()),
            ]
        );
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
