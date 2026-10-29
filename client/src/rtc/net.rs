//! 这台电脑的 WebRTC 网络设置（服务端 docs/07）：传输端口范围、公网 IPv4 / IPv6、IPv6 开关。
//!
//! - 端口范围：每条 PeerConnection 在范围里绑一个 UDP 端口，用完了改用随机端口（[`super::peer::Pc`]）；
//! - 公网地址：等候选收集完，在签名前写进本机 SDP（[`add_public`]），对端连得进来就直连，连不进来 ICE 照常走中转；
//!   也可以填域名（DDNS），后台定时解析（[`resolve_loop`]），写 SDP 时用最近一次的结果，不在建连时等 DNS；
//! - 检测（[`detect`]）：电脑自己去问中转的 STUN 和 cip.cc、ip.me 等服务，只用来辅助填写，不自动保存。
//!
//! 存在本机数据库（`kv.rtc_net`），不在电脑之间同步；改了只对新建的连接生效。

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::RwLock;
use std::time::Duration;

use axum::extract::State;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::error::LocalError;
use crate::gateway::App;
use crate::settings::{parse_open_host, OpenHost};
use crate::store::Store;

const KEY: &str = "rtc_net";
/// 出厂默认的端口范围：32 个端口，够 16 条浏览器连接加本机连其他电脑（docs/07 §3）。
pub const DEFAULT_PORTS: (u16, u16) = (50000, 50031);
/// libdatachannel 只在起始端口大于 1024 时才把范围交给 libjuice（`icetransport.cpp`）。
const MIN_PORT: u16 = 1025;
const MIN_SPAN: u16 = 8;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct NetConfig {
    pub port_begin: u16,
    pub port_end: u16,
    /// IPv4 或域名（规范写法：域名小写、IPv6 不带方括号）。早先只存 IP，存的也是这样的字符串。
    pub public_v4: Option<String>,
    /// IPv6 或域名。
    pub public_v6: Option<String>,
    /// 关掉时只绑 IPv4（`0.0.0.0`），也不写公网 IPv6。
    pub ipv6: bool,
}

impl NetConfig {
    pub const DEFAULT: Self = Self { port_begin: DEFAULT_PORTS.0, port_end: DEFAULT_PORTS.1, public_v4: None, public_v6: None, ipv6: true };

    pub fn load(store: &Store) -> Self {
        let c: Self = store.get(KEY).ok().flatten().unwrap_or_default();
        if c.check().is_ok() {
            c
        } else {
            Self { port_begin: DEFAULT_PORTS.0, port_end: DEFAULT_PORTS.1, ..c }
        }
    }

    pub fn save(&self, store: &Store) -> anyhow::Result<()> {
        store.set(KEY, self)
    }

    /// 端口范围和地址是否可用；错误是 (字段, 界面 `error:rule.*` 的规则名)。
    fn check(&self) -> Result<(), (&'static str, &'static str)> {
        if self.port_begin < MIN_PORT || self.port_end < self.port_begin {
            return Err(("rtc_ports", "rtc_ports"));
        }
        if self.port_end - self.port_begin + 1 < MIN_SPAN {
            return Err(("rtc_ports", "rtc_ports_span"));
        }
        if let Some(s) = &self.public_v4 {
            parse_addr(s, false).map_err(|r| ("public_v4", r))?;
        }
        if let Some(s) = &self.public_v6 {
            parse_addr(s, true).map_err(|r| ("public_v6", r))?;
        }
        Ok(())
    }

    /// 按请求修改（没给的项不变；地址给空字符串表示清除），校验通过才返回。
    pub fn updated(&self, u: &NetUpdate) -> Result<Self, (&'static str, &'static str)> {
        let mut c = self.clone();
        if let Some(b) = u.port_begin {
            c.port_begin = b;
        }
        if let Some(e) = u.port_end {
            c.port_end = e;
        }
        if let Some(s) = &u.public_v4 {
            c.public_v4 = match s.trim() {
                "" => None,
                s => Some(parse_addr(s, false).map_err(|r| ("public_v4", r))?.to_string()),
            };
        }
        if let Some(s) = &u.public_v6 {
            c.public_v6 = match s.trim() {
                "" => None,
                s => Some(parse_addr(s, true).map_err(|r| ("public_v6", r))?.to_string()),
            };
        }
        if let Some(v) = u.ipv6 {
            c.ipv6 = v;
        }
        c.check()?;
        Ok(c)
    }

    pub fn in_range(&self, port: u16) -> bool {
        (self.port_begin..=self.port_end).contains(&port)
    }
}

impl Default for NetConfig {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// 填的公网地址：IP，或者按域名解析。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Addr {
    Ip(IpAddr),
    Name(String),
}

impl std::fmt::Display for Addr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Addr::Ip(ip) => ip.fmt(f),
            Addr::Name(n) => f.write_str(n),
        }
    }
}

/// 解析填的地址：这一族的 IP（IPv6 可带方括号），或者域名。有字母、没有冒号的才当域名，
/// `1.2.3` 之类的笔误仍然报格式不对。错误是界面 `error:rule.*` 的规则名。
pub fn parse_addr(input: &str, v6: bool) -> Result<Addr, &'static str> {
    let s = input.trim();
    let format = if v6 { "ipv6" } else { "ipv4" };
    if s.chars().any(|c| c.is_ascii_alphabetic()) && !s.contains(':') {
        return match parse_open_host(s) {
            Ok(OpenHost::Name(n)) if n == "localhost" || n.ends_with(".localhost") => Err("host_unusable"),
            Ok(OpenHost::Name(n)) => Ok(Addr::Name(n)),
            _ => Err(format),
        };
    }
    let ip: IpAddr = match v6 {
        false => IpAddr::V4(s.parse().map_err(|_| format)?),
        true => IpAddr::V6(s.trim_matches(['[', ']']).parse().map_err(|_| format)?),
    };
    check_public(ip)?;
    Ok(Addr::Ip(ip))
}

/// 回环、未指定、组播、链路本地、广播地址填了也连不到。私有地址和 CGNAT 允许（Tailscale、ZeroTier 这类组网）。
fn check_public(ip: IpAddr) -> Result<(), &'static str> {
    let bad = ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_multicast()
        || match ip {
            IpAddr::V4(v) => v.is_link_local() || v.is_broadcast() || v.octets()[0] == 0,
            IpAddr::V6(v) => (v.segments()[0] & 0xffc0) == 0xfe80 || v.to_ipv4_mapped().is_some(),
        };
    if bad {
        Err("host_unusable")
    } else {
        Ok(())
    }
}

/// `PUT /api/rtc/net` 与命令行的修改。
#[derive(Debug, Default, Deserialize, Serialize)]
pub struct NetUpdate {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port_begin: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port_end: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_v4: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_v6: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ipv6: Option<bool>,
}

/// 当前生效的设置：建连时读（[`super::peer::Pc`]），启动时与修改后更新。
static CURRENT: RwLock<NetConfig> = RwLock::new(NetConfig::DEFAULT);

pub fn current() -> NetConfig {
    CURRENT.read().unwrap_or_else(|e| e.into_inner()).clone()
}

pub fn set_current(c: NetConfig) {
    *CURRENT.write().unwrap_or_else(|e| e.into_inner()) = c;
}

/// 填的公网地址在 SDP 里的 foundation 前缀（启动页按它认出“走的是填的地址”）。
pub const PUBLIC_FOUNDATION: &str = "llpub";

/// 把填的公网地址作为 srflx 候选写进本机 SDP（docs/07 §4.3）。端口取本机 host 候选的端口（POLL 模式下一条连接一个
/// socket，host 候选端口都相同）；端口不在设置的范围里（范围用完、改用了随机端口）时不写，随机端口没有放行。
/// 类型写 srflx：优先级低于局域网 host，高于 relay。
pub fn add_public(sdp: &str, c: &NetConfig) -> String {
    let lines: Vec<&str> = sdp.split_inclusive('\n').collect();
    let cands: Vec<Vec<&str>> =
        lines.iter().filter_map(|l| l.trim_end().strip_prefix("a=candidate:")).map(|c| c.split_whitespace().collect::<Vec<_>>()).collect();
    let hosts: Vec<(IpAddr, u16)> = cands
        .iter()
        .filter(|t| t.get(6) == Some(&"typ") && t.get(7) == Some(&"host"))
        .filter_map(|t| Some((t.get(4)?.parse().ok()?, t.get(5)?.parse().ok()?)))
        .collect();
    let Some(&(_, port)) = hosts.first() else { return sdp.to_string() };
    if !c.in_range(port) {
        return sdp.to_string();
    }
    let mut want: Vec<IpAddr> = Vec::new();
    if let Some(v4) = c.public_v4.as_deref().and_then(|s| public_ip(s, false)) {
        want.push(v4);
    }
    if let (true, Some(v6)) = (c.ipv6, c.public_v6.as_deref().and_then(|s| public_ip(s, true))) {
        want.push(v6);
    }
    let present = |ip: &IpAddr| cands.iter().any(|t| t.get(4).and_then(|a| a.parse::<IpAddr>().ok()).as_ref() == Some(ip) && t.get(5) == Some(&port.to_string().as_str()));
    let mut extra = String::new();
    for ip in want.iter().filter(|ip| !present(ip)) {
        let (fam, local_pref, base) = match ip {
            IpAddr::V4(_) => (4, 65534u32, hosts.iter().find(|(h, _)| h.is_ipv4()).map_or("0.0.0.0".to_string(), |(h, _)| h.to_string())),
            IpAddr::V6(_) => (6, 65535u32, hosts.iter().find(|(h, _)| h.is_ipv6()).map_or("::".to_string(), |(h, _)| h.to_string())),
        };
        let priority = (100u32 << 24) | (local_pref << 8) | 255;
        extra.push_str(&format!("a=candidate:{PUBLIC_FOUNDATION}{fam} 1 UDP {priority} {ip} {port} typ srflx raddr {base} rport {port}\r\n"));
    }
    if extra.is_empty() {
        return sdp.to_string();
    }
    // 放在最后一个候选后面（在 a=end-of-candidates 之前）
    let last = lines.iter().rposition(|l| l.starts_with("a=candidate:")).unwrap_or(lines.len() - 1);
    let mut out = String::with_capacity(sdp.len() + extra.len());
    for (i, l) in lines.iter().enumerate() {
        out.push_str(l);
        if i == last {
            if !l.ends_with('\n') {
                out.push_str("\r\n");
            }
            out.push_str(&extra);
        }
    }
    out
}

// ---------- 域名解析 ----------

/// 填的域名最近一次解析出来的地址：(域名, 是否 IPv6) → 地址。
static RESOLVED: RwLock<Option<HashMap<(String, bool), IpAddr>>> = RwLock::new(None);

const RESOLVE_EVERY: Duration = Duration::from_secs(60);
const RESOLVE_TIMEOUT: Duration = Duration::from_secs(3);

/// 填的地址现在对应的 IP：IP 直接用；域名用最近一次解析的结果，还没解析出来时为 None（不写公网候选）。
/// 设置里存的都是规范写法，IP 保存时已经校验过，这里不再检查。
pub fn public_ip(s: &str, v6: bool) -> Option<IpAddr> {
    match s.parse::<IpAddr>() {
        Ok(ip) => (ip.is_ipv6() == v6).then_some(ip),
        Err(_) => RESOLVED.read().unwrap_or_else(|e| e.into_inner()).as_ref()?.get(&(s.to_string(), v6)).copied(),
    }
}

/// 解析结果里能用的地址：这一族、能当公网地址。代理软件 fake-ip 模式给的 198.18.0.0/15 不是真实地址，也不要。
fn usable(ip: IpAddr, v6: bool) -> bool {
    ip.is_ipv6() == v6 && check_public(ip).is_ok() && !matches!(ip, IpAddr::V4(v) if v.octets()[0] == 198 && (v.octets()[1] & 0xfe) == 18)
}

/// 重新解析设置里的域名。解析失败时保留上次的结果（DNS 偶尔出错不至于丢掉直连）。
pub async fn resolve(c: &NetConfig) {
    let mut names: Vec<(String, bool)> = Vec::new();
    for (s, v6) in [(&c.public_v4, false), (&c.public_v6, true)] {
        if let Some(Ok(Addr::Name(n))) = s.as_deref().map(|s| parse_addr(s, v6)) {
            names.push((n, v6));
        }
    }
    let lookups = names.iter().map(|(n, v6)| async move {
        let r = tokio::time::timeout(RESOLVE_TIMEOUT, tokio::net::lookup_host((n.as_str(), 0))).await;
        let ip = match r {
            Ok(Ok(addrs)) => addrs.map(|a| a.ip().to_canonical()).find(|ip| usable(*ip, *v6)),
            _ => None,
        };
        if ip.is_none() {
            tracing::debug!(domain = %n, v6, "直连公网地址的域名没解析出可用地址");
        }
        ((n.clone(), *v6), ip)
    });
    let results = futures_util::future::join_all(lookups).await;
    let mut m = RESOLVED.write().unwrap_or_else(|e| e.into_inner());
    let m = m.get_or_insert_with(HashMap::new);
    m.retain(|k, _| names.contains(k));
    for (k, ip) in results {
        if let Some(ip) = ip {
            if m.insert(k.clone(), ip) != Some(ip) {
                tracing::info!(domain = %k.0, %ip, "直连公网地址的域名解析结果");
            }
        }
    }
}

/// 后台每分钟解析一次当前设置里的域名（没有域名时什么也不做）。
pub async fn resolve_loop() {
    loop {
        resolve(&current()).await;
        tokio::time::sleep(RESOLVE_EVERY).await;
    }
}

// ---------- 检测公网地址 ----------

/// 一个来源的结果。
#[derive(Debug, Clone, Serialize)]
pub struct Probe {
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ip: Option<IpAddr>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Detected {
    pub v4: Vec<Probe>,
    pub v6: Vec<Probe>,
    /// 多数来源一致的地址（一样多时取靠前的来源）。
    pub suggest_v4: Option<Ipv4Addr>,
    pub suggest_v6: Option<Ipv6Addr>,
}

const V4_SITES: &[(&str, &str)] = &[("cip.cc", "http://cip.cc/"), ("ip.me", "https://ip.me/"), ("4.ipw.cn", "https://4.ipw.cn/"), ("ipify", "https://api.ipify.org/")];
const V6_SITES: &[(&str, &str)] = &[("ip.me", "https://ip.me/"), ("6.ipw.cn", "https://6.ipw.cn/"), ("ipify", "https://api6.ipify.org/"), ("ident.me", "https://v6.ident.me/")];
const DETECT_TIMEOUT: Duration = Duration::from_secs(4);

/// 查这台电脑的出口地址。`stun`：当前中转的 STUN 地址（`主机:端口`，没登录时为 None），作为 IPv4 的第一个来源。
/// 不走系统代理（经代理查到的是代理的出口）；系统开了 TUN 模式代理时绕不开，所以各来源的结果都列出来。
pub async fn detect(stun: Option<String>) -> Detected {
    let relay = async {
        let stun = stun?;
        Some(match crate::relay::stun_mapped(&stun).await {
            Ok(ip) => Probe { source: "relay".into(), ip: Some(ip), error: None },
            Err(e) => Probe { source: "relay".into(), ip: None, error: Some(e) },
        })
    };
    let v4 = futures_util::future::join_all(V4_SITES.iter().map(|(name, url)| site(name, url, false)));
    let v6 = futures_util::future::join_all(V6_SITES.iter().map(|(name, url)| site(name, url, true)));
    let (relay, mut v4, v6) = tokio::join!(relay, v4, v6);
    if let Some(r) = relay {
        v4.insert(0, r);
    }
    let suggest_v4 = majority(&v4).and_then(|ip| match ip {
        IpAddr::V4(v) => Some(v),
        _ => None,
    });
    let suggest_v6 = majority(&v6).and_then(|ip| match ip {
        IpAddr::V6(v) => Some(v),
        _ => None,
    });
    Detected { v4, v6, suggest_v4, suggest_v6 }
}

fn majority(probes: &[Probe]) -> Option<IpAddr> {
    let ips: Vec<IpAddr> = probes.iter().filter_map(|p| p.ip).collect();
    let mut best: Option<(IpAddr, usize)> = None;
    for ip in &ips {
        let n = ips.iter().filter(|x| *x == ip).count();
        if best.is_none_or(|(_, m)| n > m) {
            best = Some((*ip, n));
        }
    }
    best.map(|(ip, _)| ip)
}

/// 经指定地址族问一个网站：自己解析域名、只留这一族的地址交给 reqwest（`local_address` 不能限定地址族）。
async fn site(name: &str, url: &str, v6: bool) -> Probe {
    let r = tokio::time::timeout(DETECT_TIMEOUT, ask(url, v6)).await.unwrap_or_else(|_| Err("timeout".into()));
    match r {
        Ok(ip) => Probe { source: name.into(), ip: Some(ip), error: None },
        Err(e) => Probe { source: name.into(), ip: None, error: Some(e) },
    }
}

async fn ask(url: &str, v6: bool) -> Result<IpAddr, String> {
    let u = reqwest::Url::parse(url).map_err(|e| e.to_string())?;
    let host = u.host_str().ok_or("url")?.to_string();
    let port = u.port_or_known_default().unwrap_or(443);
    let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), port)).await.map_err(|_| "dns".to_string())?.filter(|a| a.is_ipv6() == v6).collect();
    if addrs.is_empty() {
        return Err("no_address".into());
    }
    let client = reqwest::Client::builder()
        .no_proxy()
        .resolve_to_addrs(&host, &addrs)
        .timeout(DETECT_TIMEOUT)
        .user_agent("curl/8.5.0")
        .build()
        .map_err(|e| e.to_string())?;
    let resp = client.get(u).header("Accept", "text/plain").send().await.map_err(|e| if e.is_connect() { "connect".to_string() } else { "network".to_string() })?;
    if !resp.status().is_success() {
        return Err(format!("http_{}", resp.status().as_u16()));
    }
    let body = resp.text().await.map_err(|_| "network".to_string())?;
    first_ip(&body, v6).ok_or_else(|| "no_ip".into())
}

/// 响应里第一个属于这一族的 IP（cip.cc 返回 `IP\t: 1.2.3.4` 加几行说明，其他服务只返回地址）。
fn first_ip(body: &str, v6: bool) -> Option<IpAddr> {
    body.split(|c: char| !(c.is_ascii_hexdigit() || c == '.' || c == ':'))
        .filter(|t| !t.is_empty())
        .filter_map(|t| t.trim_matches(':').parse::<IpAddr>().ok())
        .find(|ip| ip.is_ipv6() == v6 && check_public(*ip).is_ok())
}

// ---------- 接口 ----------

/// 网卡上的公网 IPv6（GUA），它们本来就会作为 host 候选，界面上提示“一般不用填”。
fn nic_v6() -> Vec<Ipv6Addr> {
    let mut v: Vec<Ipv6Addr> = local_ip_address::list_afinet_netifas()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(_, ip)| match ip {
            IpAddr::V6(v) if (v.segments()[0] & 0xe000) == 0x2000 => Some(v),
            _ => None,
        })
        .collect();
    v.sort();
    v.dedup();
    v
}

/// 接口与命令行显示的样子。
pub fn view(c: &NetConfig) -> Value {
    let mut v = json!({
        "port_begin": c.port_begin,
        "port_end": c.port_end,
        "public_v4": c.public_v4.clone().unwrap_or_default(),
        "public_v6": c.public_v6.clone().unwrap_or_default(),
        "ipv6": c.ipv6,
        "default_ports": [DEFAULT_PORTS.0, DEFAULT_PORTS.1],
        "nic_v6": nic_v6(),
    });
    // 填的是域名时加上它现在解析出的地址（还没解析出来是 null）；填的是 IP 或没填时没有这一项
    for (k, s, v6) in [("resolved_v4", &c.public_v4, false), ("resolved_v6", &c.public_v6, true)] {
        if let Some(s) = s.as_deref().filter(|s| matches!(parse_addr(s, v6), Ok(Addr::Name(_)))) {
            v[k] = json!(public_ip(s, v6));
        }
    }
    v
}

/// `GET /api/rtc/net`
pub async fn get_net(State(app): State<App>) -> Json<Value> {
    Json(view(&NetConfig::load(&app.store)))
}

/// `PUT /api/rtc/net`：远程打开的管理台也能改（改错了最多直连不了，中转仍然可用）。
pub async fn put_net(State(app): State<App>, Json(u): Json<NetUpdate>) -> Result<Json<Value>, LocalError> {
    let c = NetConfig::load(&app.store).updated(&u).map_err(|(f, r)| LocalError::invalid(f, r))?;
    c.save(&app.store)?;
    set_current(c.clone());
    // 新填的域名马上解析一次，界面能立刻显示解析结果
    resolve(&c).await;
    tracing::info!(ports = %format!("{}-{}", c.port_begin, c.port_end), public_v4 = ?c.public_v4, public_v6 = ?c.public_v6, ipv6 = c.ipv6, "WebRTC 网络设置已修改");
    Ok(Json(view(&c)))
}

/// `POST /api/rtc/net/detect`：只返回结果，不保存。
pub async fn post_detect(State(app): State<App>) -> Json<Detected> {
    Json(detect(relay_stun(&app)).await)
}

/// 当前中转的 STUN 地址：中转主机名 + TURN 端口（与线路测速相同）。
fn relay_stun(app: &App) -> Option<String> {
    let s = app.account.session()?;
    let host = s.relay.server_addr.rsplit_once(':').map_or(s.relay.server_addr.as_str(), |(h, _)| h).to_string();
    Some(format!("{host}:{}", looklook_protocol::rtc::TURN_PORT))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SDP: &str = "v=0\r\nm=application 50003 UDP/DTLS/SCTP webrtc-datachannel\r\na=mid:0\r\n\
        a=candidate:1 1 UDP 2122317823 192.168.1.2 50003 typ host\r\n\
        a=candidate:2 1 UDP 2122317567 2001:db8::2 50003 typ host\r\n\
        a=candidate:3 1 UDP 1686052607 198.51.100.4 61000 typ srflx raddr 192.168.1.2 rport 50003\r\n\
        a=end-of-candidates\r\na=sctp-port:5000\r\n";

    fn cfg(v4: Option<&str>, v6: Option<&str>) -> NetConfig {
        NetConfig { public_v4: v4.map(str::to_string), public_v6: v6.map(str::to_string), ..NetConfig::DEFAULT }
    }

    #[test]
    fn public_candidates_go_after_the_last_candidate() {
        let out = add_public(SDP, &cfg(Some("203.0.113.7"), Some("2001:db8:1::7")));
        let v4 = "a=candidate:llpub4 1 UDP 1694498559 203.0.113.7 50003 typ srflx raddr 192.168.1.2 rport 50003\r\n";
        let v6 = "a=candidate:llpub6 1 UDP 1694498815 2001:db8:1::7 50003 typ srflx raddr 2001:db8::2 rport 50003\r\n";
        assert!(out.contains(&format!("rport 50003\r\n{v4}{v6}a=end-of-candidates\r\n")), "{out}");
        // 只填一个
        let out = add_public(SDP, &cfg(None, Some("2001:db8:1::7")));
        assert!(out.contains(v6) && !out.contains("llpub4"));
        assert_eq!(add_public(SDP, &cfg(None, None)), SDP);
    }

    #[test]
    fn public_candidates_are_skipped_when_useless() {
        // IPv6 关了
        let off = NetConfig { ipv6: false, ..cfg(None, Some("2001:db8:1::7")) };
        assert_eq!(add_public(SDP, &off), SDP);
        // 端口不在范围里（范围用完、改用了随机端口）
        let other = NetConfig { port_begin: 40000, port_end: 40031, ..cfg(Some("203.0.113.7"), None) };
        assert_eq!(add_public(SDP, &other), SDP);
        // 已经有同样的地址和端口（网卡上就是这个公网地址）
        assert_eq!(add_public(SDP, &cfg(Some("192.168.1.2"), None)), SDP);
        // 没有 host 候选
        assert_eq!(add_public("v=0\r\na=mid:0\r\n", &cfg(Some("203.0.113.7"), None)), "v=0\r\na=mid:0\r\n");
    }

    #[test]
    fn domains_use_the_last_resolved_address() {
        let c = cfg(Some("ddns-test.example.com"), Some("ddns-test.example.com"));
        // 还没解析出来：不写
        assert_eq!(add_public(SDP, &c), SDP);
        assert_eq!(view(&c)["resolved_v4"], Value::Null);
        assert!(view(&cfg(Some("203.0.113.7"), None)).get("resolved_v4").is_none(), "填的是 IP 时没有这一项");
        RESOLVED.write().unwrap().get_or_insert_with(HashMap::new).extend([
            (("ddns-test.example.com".to_string(), false), "203.0.113.9".parse().unwrap()),
            (("ddns-test.example.com".to_string(), true), "2001:db8:1::9".parse().unwrap()),
        ]);
        let out = add_public(SDP, &c);
        assert!(out.contains("llpub4 1 UDP 1694498559 203.0.113.9 50003 typ srflx") && out.contains(" 2001:db8:1::9 50003 typ srflx"), "{out}");
        assert_eq!(view(&c)["resolved_v4"], "203.0.113.9");
        // fake-ip、回环这类解析结果不用
        assert!(!usable("198.18.0.5".parse().unwrap(), false) && !usable("127.0.0.1".parse().unwrap(), false));
        assert!(usable("198.20.0.5".parse().unwrap(), false) && !usable("2001:db8::1".parse().unwrap(), false));
    }

    #[test]
    fn updates_are_validated() {
        let c = NetConfig::DEFAULT;
        let u = |j: Value| serde_json::from_value::<NetUpdate>(j).unwrap();
        let ok = c.updated(&u(json!({"port_begin": 40000, "port_end": 40015, "public_v4": " 203.0.113.7 ", "public_v6": "[2001:db8::7]"}))).unwrap();
        assert_eq!((ok.port_begin, ok.port_end), (40000, 40015));
        assert_eq!(ok.public_v4.as_deref(), Some("203.0.113.7"));
        assert_eq!(ok.public_v6.as_deref(), Some("2001:db8::7"), "去掉方括号");
        // 域名（DDNS）：存小写、去掉末尾的点；两个框都能填
        let d = c.updated(&u(json!({"public_v4": " Home.Example.com. ", "public_v6": "v6.example.com"}))).unwrap();
        assert_eq!((d.public_v4.as_deref(), d.public_v6.as_deref()), (Some("home.example.com"), Some("v6.example.com")));
        assert_eq!(ok.updated(&u(json!({"public_v4": ""}))).unwrap().public_v4, None, "空字符串清除");
        assert_eq!(ok.updated(&u(json!({"ipv6": false}))).unwrap().public_v6, ok.public_v6, "没给的项不变");
        // 共享的 100.64/10、Tailscale 这类地址允许
        assert!(c.updated(&u(json!({"public_v4": "100.101.102.103"}))).is_ok());
        for (j, want) in [
            (json!({"port_begin": 1024}), ("rtc_ports", "rtc_ports")),
            (json!({"port_begin": 50010, "port_end": 50000}), ("rtc_ports", "rtc_ports")),
            (json!({"port_end": 50003}), ("rtc_ports", "rtc_ports_span")),
            (json!({"public_v4": "2001:db8::1"}), ("public_v4", "ipv4")),
            (json!({"public_v4": "1.2.3"}), ("public_v4", "ipv4")),
            (json!({"public_v6": "203.0.113.7"}), ("public_v6", "ipv6")),
            (json!({"public_v4": "127.0.0.1"}), ("public_v4", "host_unusable")),
            (json!({"public_v4": "169.254.1.1"}), ("public_v4", "host_unusable")),
            (json!({"public_v6": "fe80::1"}), ("public_v6", "host_unusable")),
            (json!({"public_v6": "::ffff:1.2.3.4"}), ("public_v6", "host_unusable")),
            (json!({"public_v4": "localhost"}), ("public_v4", "host_unusable")),
            (json!({"public_v4": "http://home.example.com"}), ("public_v4", "ipv4")),
            (json!({"public_v4": "home.example.com:50000"}), ("public_v4", "ipv4")),
            (json!({"public_v6": "bad_name.example.com"}), ("public_v6", "ipv6")),
        ] {
            assert_eq!(c.updated(&u(j.clone())).unwrap_err(), want, "{j}");
        }
    }

    #[test]
    fn stored_config_falls_back_to_default_ports() {
        let store = Store::memory().unwrap();
        assert_eq!(NetConfig::load(&store), NetConfig::DEFAULT);
        store.set(KEY, &json!({"port_begin": 80, "port_end": 90, "public_v4": "203.0.113.7"})).unwrap();
        let c = NetConfig::load(&store);
        assert_eq!((c.port_begin, c.port_end), DEFAULT_PORTS);
        assert_eq!(c.public_v4.as_deref(), Some("203.0.113.7"), "别的项保留");
    }

    #[test]
    fn detection_helpers() {
        let cip = "IP\t: 203.0.113.7\n地址\t: 中国  北京\n运营商\t: 电信\n\n数据二\t: 北京市 | 电信\n\nURL\t: http://www.cip.cc/203.0.113.7\n";
        assert_eq!(first_ip(cip, false), Some("203.0.113.7".parse().unwrap()));
        assert_eq!(first_ip("2001:db8:1::7\n", true), Some("2001:db8:1::7".parse().unwrap()));
        assert_eq!(first_ip("2001:db8:1::7\n", false), None);
        assert_eq!(first_ip("<html>nope</html>", false), None);
        let p = |ip: Option<&str>| Probe { source: "x".into(), ip: ip.map(|s| s.parse().unwrap()), error: None };
        let a = Some("203.0.113.7");
        let b = Some("198.51.100.4");
        assert_eq!(majority(&[p(b), p(a), p(None), p(a)]), a.map(|s| s.parse().unwrap()));
        assert_eq!(majority(&[p(b), p(a)]), b.map(|s| s.parse().unwrap()), "一样多时取靠前的");
        assert_eq!(majority(&[p(None)]), None);
    }
}
