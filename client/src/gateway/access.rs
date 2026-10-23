//! 管理台的直接访问控制（不经过看看服务器、直接连到这台机器端口的访问）。
//!
//! 管理台默认监听 `0.0.0.0:1234`，但按来源 IP 放行：
//! - **这台机器自己**（127.0.0.1 / ::1，或连接两端是同一个 IP）：总是放行，不需要访问码；
//!   在 Docker 等环境里运行时，宿主机的浏览器是经网关（如 172.17.0.1）连进来的，可以在启动时用
//!   `--local-from` / `LOOKLOOK_LOCAL_FROM` 指定也当作这台机器的 IP 或网段（见 [`set_local_from`]），默认没有；
//! - **局域网**（192.168.x.x、10.x.x.x、172.16–31.x.x、100.64/10、169.254/16、fc00::/7、fe80::/10）：
//!   打开“允许局域网访问”后放行；
//! - **白名单**：用户自己添加的 IP、网段或域名（例如公司固定公网 IP；动态 IP 用 DDNS 域名，按解析结果放行，见 [`Resolved`]）；
//! - 其他一律拒绝。
//!
//! 局域网和白名单访问在启用访问码后还要输入访问码。访问码只保存在本机数据库（`access` 键），
//! 不上传；浏览器里保存的是由它派生的 Cookie，换访问码后旧 Cookie 全部失效。输错次数按 IP 限制。
//!
//! 所有来源都检查 Host：只接受 localhost、IP 地址、局域网名字（单段名、`.local`、`.lan`、`.home.arpa`、
//! `.internal`）和用户设置的打开地址，挡住 DNS 重绑定（恶意网站把自己的域名解析到本机来读取终端）。

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Mutex, OnceLock, RwLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::settings::{parse_open_host, OpenHost};
use crate::store::Store;

const KEY: &str = "access";
pub const COOKIE: &str = "ll_access";
/// 访问码表单提交地址（不和管理台、终端路由冲突）
pub const FORM_PATH: &str = "/__looklook/access";
pub const MAX_ALLOWED: usize = 32;
const MAX_FAILS: u32 = 10;
const FAIL_WINDOW: Duration = Duration::from_secs(15 * 60);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct AccessConfig {
    /// 允许局域网里的其他设备打开管理台
    pub allow_lan: bool,
    /// 额外允许的 IP、网段或域名（白名单），如 `203.0.113.7`、`203.0.113.0/24`、`home.example.com`
    pub allowed_ips: Vec<String>,
    /// 局域网 / 白名单访问是否需要访问码
    pub code_enabled: bool,
    /// 访问码（只存在本机数据库）
    pub code: String,
    /// 启动时打开的地址（主机部分）；为空表示 127.0.0.1。看看装在 NAS 等别的机器上时使用。
    pub open_host: String,
}

impl Default for AccessConfig {
    fn default() -> Self {
        Self { allow_lan: false, allowed_ips: Vec::new(), code_enabled: false, code: generate_code(), open_host: String::new() }
    }
}

impl AccessConfig {
    pub fn load(store: &Store) -> Self {
        let mut c: Self = store.get(KEY).ok().flatten().unwrap_or_default();
        if c.code.trim().is_empty() {
            c.code = generate_code();
        }
        // 存进去之前都校验过；万一不合法（手改数据库等）就丢掉那一项。
        c.allowed_ips.retain(|s| parse_entry(s).is_ok());
        if parse_open_host(&c.open_host).is_err() {
            c.open_host.clear();
        }
        c
    }

    /// 白名单里的域名。
    pub fn domains(&self) -> impl Iterator<Item = &str> {
        self.allowed_ips.iter().map(String::as_str).filter(|s| matches!(parse_entry(s), Ok(Entry::Domain(_))))
    }

    pub fn save(&self, store: &Store) -> anyhow::Result<()> {
        store.set(KEY, self)
    }

    /// 打开地址；`None` 表示用 127.0.0.1。
    pub fn open_host(&self) -> Option<OpenHost> {
        if self.open_host.trim().is_empty() {
            return None;
        }
        parse_open_host(&self.open_host).ok()
    }

    /// 浏览器 Cookie 里保存的值：由访问码派生，不直接保存访问码。
    pub fn cookie_value(&self) -> String {
        let mut h = Sha256::new();
        h.update(b"looklook-access\0");
        h.update(normalize_code(&self.code).as_bytes());
        data_encoding::HEXLOWER.encode(&h.finalize())
    }

    pub fn code_matches(&self, input: &str) -> bool {
        let (a, b) = (normalize_code(input), normalize_code(&self.code));
        !a.is_empty() && ct_eq(&a, &b)
    }
}

/// 访问来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Peer {
    /// 这台机器自己
    Local,
    /// 局域网（已允许）
    Lan,
    /// 白名单（已允许）
    Listed,
    /// 不允许
    Denied,
}

impl Peer {
    pub fn needs_code(self, cfg: &AccessConfig) -> bool {
        matches!(self, Peer::Lan | Peer::Listed) && cfg.code_enabled
    }
}

/// 启动时指定、也当作这台机器的来源（`--local-from`），只设置一次。
static LOCAL_FROM: OnceLock<Vec<(IpAddr, u8)>> = OnceLock::new();

pub fn set_local_from(nets: Vec<(IpAddr, u8)>) {
    let _ = LOCAL_FROM.set(nets);
}

/// 来源是不是启动时指定也当作这台机器的 IP 或网段。
pub fn local_from(ip: IpAddr) -> bool {
    LOCAL_FROM.get().is_some_and(|nets| nets.iter().any(|(net, len)| in_net(ip, *net, *len)))
}

/// 判断来源。`same_host`：连接两端是同一个 IP（从这台机器用自己的局域网地址打开），也算本机。
/// `resolved`：白名单里的域名当前解析出来的 IP（[`Resolved::ips`]）。
pub fn classify(ip: IpAddr, cfg: &AccessConfig, same_host: bool, resolved: &[IpAddr]) -> Peer {
    let ip = ip.to_canonical();
    if ip.is_loopback() || same_host {
        return Peer::Local;
    }
    if cfg.allowed_ips.iter().filter_map(|s| parse_net(s).ok()).any(|(net, len)| in_net(ip, net, len)) || resolved.iter().any(|r| r.to_canonical() == ip) {
        return Peer::Listed;
    }
    if cfg.allow_lan && is_lan(ip) {
        return Peer::Lan;
    }
    Peer::Denied
}

pub fn is_lan(ip: IpAddr) -> bool {
    match ip.to_canonical() {
        IpAddr::V4(v) => {
            let o = v.octets();
            v.is_private() || v.is_link_local() || (o[0] == 100 && (64..128).contains(&o[1]))
        }
        IpAddr::V6(v) => {
            let s = v.segments()[0];
            (s & 0xfe00) == 0xfc00 || (s & 0xffc0) == 0xfe80
        }
    }
}

/// 解析白名单的一项：单个 IP 或 `IP/前缀长度`。错误是界面 `error:rule.*` 的规则名。
pub fn parse_net(input: &str) -> Result<(IpAddr, u8), &'static str> {
    let s = input.trim();
    if s.is_empty() {
        return Err("ip_format");
    }
    let (addr, len) = match s.split_once('/') {
        Some((a, l)) => (a, Some(l)),
        None => (s, None),
    };
    let addr = addr.trim_start_matches('[').trim_end_matches(']');
    let ip: IpAddr = addr.parse().map_err(|_| "ip_format")?;
    let max = if ip.is_ipv4() { 32 } else { 128 };
    let len = match len {
        None => max,
        Some(l) => l.parse::<u8>().ok().filter(|n| *n <= max).ok_or("ip_prefix")?,
    };
    // 0.0.0.0/0 之类等于对全网开放，不允许；太宽的网段也不允许。
    if len < if ip.is_ipv4() { 8 } else { 32 } || ip.is_unspecified() {
        return Err("ip_too_wide");
    }
    if ip.is_loopback() {
        return Err("ip_loopback");
    }
    Ok((ip, len))
}

/// 白名单的一项。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    Net(IpAddr, u8),
    /// 域名（小写、不带结尾的点），按解析出来的 IP 放行
    Domain(String),
}

/// 解析白名单的一项：IP、网段或域名。
pub fn parse_entry(input: &str) -> Result<Entry, &'static str> {
    match parse_net(input) {
        Ok((ip, len)) => Ok(Entry::Net(ip, len)),
        // 有字母、不是 IPv6 的才当域名，`203.0.113` 之类的笔误仍然报 IP 格式不对。
        Err("ip_format") if input.chars().any(|c| c.is_ascii_alphabetic()) && !input.contains(':') => match parse_open_host(input) {
            Ok(OpenHost::Name(n)) if n == "localhost" || n.ends_with(".localhost") => Err("ip_loopback"),
            Ok(OpenHost::Name(n)) => Ok(Entry::Domain(n)),
            _ => Err("ip_format"),
        },
        Err(e) => Err(e),
    }
}

/// 规范写法。
pub fn format_entry(e: &Entry) -> String {
    match e {
        Entry::Net(ip, len) => format_net((*ip, *len)),
        Entry::Domain(n) => n.clone(),
    }
}

/// 规范写法（去掉多余空格，单个 IP 不带 /32）。
pub fn format_net(net: (IpAddr, u8)) -> String {
    let max = if net.0.is_ipv4() { 32 } else { 128 };
    if net.1 == max {
        net.0.to_string()
    } else {
        format!("{}/{}", net.0, net.1)
    }
}

fn in_net(ip: IpAddr, net: IpAddr, len: u8) -> bool {
    match (ip.to_canonical(), net) {
        (IpAddr::V4(a), IpAddr::V4(b)) => {
            let mask = if len == 0 { 0 } else { u32::MAX << (32 - len as u32) };
            u32::from(a) & mask == u32::from(b) & mask
        }
        (IpAddr::V6(a), IpAddr::V6(b)) => {
            let mask = if len == 0 { 0 } else { u128::MAX << (128 - len as u32) };
            u128::from(a) & mask == u128::from(b) & mask
        }
        _ => false,
    }
}

/// 请求头 Host 是否可以接受（见模块说明）。`name` 已去掉端口和方括号。
pub fn host_allowed(name: &str, open_host: Option<&OpenHost>) -> bool {
    let n = name.trim_end_matches('.').to_ascii_lowercase();
    if n.is_empty() {
        return false;
    }
    if n == "localhost" || n.ends_with(".localhost") || n.parse::<IpAddr>().is_ok() {
        return true;
    }
    if open_host.is_some_and(|h| h.matches(&n)) {
        return true;
    }
    // 局域网名字：这些不能在公网注册，攻击者没法把它们解析到你的机器上。
    let lan_name = !n.contains('.') || [".local", ".lan", ".home.arpa", ".internal"].iter().any(|s| n.ends_with(s));
    lan_name && parse_open_host(&n).is_ok()
}

/// 访问码：12 位，去掉容易看错的字符，分成三组，例如 `K7MQ-2XRA-9WTD`。
pub fn generate_code() -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHJKMNPQRSTUVWXYZ23456789";
    let bytes = crate::util::random_bytes::<12>();
    let chars: Vec<char> = bytes.iter().map(|b| ALPHABET[*b as usize % ALPHABET.len()] as char).collect();
    chars.chunks(4).map(|c| c.iter().collect::<String>()).collect::<Vec<_>>().join("-")
}

/// 自己设置的访问码：8–64 个字符，不能有空格等控制字符。
pub fn check_custom_code(code: &str) -> Result<(), &'static str> {
    let n = normalize_code(code).chars().count();
    if !(8..=64).contains(&n) || code.chars().any(|c| c.is_control()) {
        return Err("code_length");
    }
    Ok(())
}

/// 比较时不区分大小写，忽略空格和短横线（手机上输入更方便）。
fn normalize_code(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace() && *c != '-').flat_map(char::to_uppercase).collect()
}

fn ct_eq(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// 这台机器的局域网 IP（只要真正的局域网网段，跳过代理软件的虚拟网卡等），用来告诉用户在别的设备上输入什么地址。
pub fn own_lan_ips() -> Vec<IpAddr> {
    let mut ips: Vec<IpAddr> = local_ip_address::list_afinet_netifas()
        .unwrap_or_default()
        .into_iter()
        .map(|(_, ip)| ip)
        .filter(|ip| is_lan(*ip) && !matches!(ip, IpAddr::V6(v) if (v.segments()[0] & 0xffc0) == 0xfe80))
        .collect();
    // IPv4 在前，家用网络里最常见的 192.168 排最前
    ips.sort_by_key(|ip| (ip.is_ipv6(), !matches!(ip, IpAddr::V4(v) if v.octets()[0] == 192), *ip));
    ips.dedup();
    ips
}

/// 白名单里域名的解析结果。后台每分钟解析一次（[`Resolved::run`]）；
/// 有不在名单里的来源时最多每 15 秒再补解析一次，DDNS 的 IP 刚变时不用等太久。
#[derive(Default)]
pub struct Resolved {
    ips: RwLock<HashMap<String, Vec<IpAddr>>>,
    last: Mutex<Option<Instant>>,
}

const RESOLVE_EVERY: Duration = Duration::from_secs(60);
const RESOLVE_RETRY: Duration = Duration::from_secs(15);
const RESOLVE_TIMEOUT: Duration = Duration::from_secs(3);

impl Resolved {
    /// 这些域名当前解析出来的所有 IP。
    pub fn ips(&self, cfg: &AccessConfig) -> Vec<IpAddr> {
        let m = self.ips.read().unwrap_or_else(|e| e.into_inner());
        cfg.domains().filter_map(|d| m.get(d)).flatten().copied().collect()
    }

    /// 每个域名解析出来的 IP（界面显示用）。
    pub fn view(&self, cfg: &AccessConfig) -> HashMap<String, Vec<String>> {
        let m = self.ips.read().unwrap_or_else(|e| e.into_inner());
        cfg.domains().map(|d| (d.to_string(), m.get(d).map(|v| v.iter().map(IpAddr::to_string).collect()).unwrap_or_default())).collect()
    }

    /// 重新解析所有域名。解析失败时保留上次的结果（DNS 偶尔出错不至于把自己挡在外面）。
    pub async fn refresh(&self, cfg: &AccessConfig) {
        *self.last.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now());
        let domains: Vec<String> = cfg.domains().map(str::to_string).collect();
        let lookups = domains.iter().map(|d| async move {
            let r = tokio::time::timeout(RESOLVE_TIMEOUT, tokio::net::lookup_host((d.as_str(), 0))).await;
            let ips: Option<Vec<IpAddr>> = match r {
                Ok(Ok(addrs)) => Some(addrs.map(|a| a.ip().to_canonical()).collect()),
                _ => None,
            };
            if ips.is_none() {
                tracing::debug!(domain = %d, "白名单域名解析失败");
            }
            (d.clone(), ips)
        });
        let results = futures_util::future::join_all(lookups).await;
        let mut m = self.ips.write().unwrap_or_else(|e| e.into_inner());
        m.retain(|k, _| domains.contains(k));
        for (d, ips) in results {
            if let Some(mut ips) = ips {
                ips.sort();
                ips.dedup();
                m.insert(d, ips);
            }
        }
    }

    /// 来源不在名单里时调用：距上次解析超过 15 秒就再解析一次，返回是否解析了。
    pub async fn refresh_if_stale(&self, cfg: &AccessConfig) -> bool {
        if cfg.domains().next().is_none() {
            return false;
        }
        {
            let mut last = self.last.lock().unwrap_or_else(|e| e.into_inner());
            if last.is_some_and(|t| t.elapsed() < RESOLVE_RETRY) {
                return false;
            }
            *last = Some(Instant::now());
        }
        self.refresh(cfg).await;
        true
    }

    /// 后台定时解析；`cfg` 每次取当前的访问设置。
    pub async fn run(self: std::sync::Arc<Self>, cfg: impl Fn() -> AccessConfig) {
        loop {
            let c = cfg();
            if c.domains().next().is_some() {
                self.refresh(&c).await;
            }
            tokio::time::sleep(RESOLVE_EVERY).await;
        }
    }
}

/// 访问码输错次数限制：每个 IP 15 分钟内最多错 10 次。
#[derive(Default)]
pub struct Limiter(Mutex<HashMap<IpAddr, (u32, Instant)>>);

impl Limiter {
    pub fn blocked(&self, ip: IpAddr) -> bool {
        let mut m = self.0.lock().unwrap_or_else(|e| e.into_inner());
        m.retain(|_, (_, t)| t.elapsed() < FAIL_WINDOW);
        m.get(&ip).is_some_and(|(n, _)| *n >= MAX_FAILS)
    }

    pub fn fail(&self, ip: IpAddr) {
        let mut m = self.0.lock().unwrap_or_else(|e| e.into_inner());
        m.entry(ip).or_insert((0, Instant::now())).0 += 1;
    }

    pub fn clear(&self, ip: IpAddr) {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).remove(&ip);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn classify_sources() {
        let mut c = AccessConfig::default();
        assert_eq!(classify(ip("127.0.0.1"), &c, false, &[]), Peer::Local);
        assert_eq!(classify(ip("::ffff:127.0.0.1"), &c, false, &[]), Peer::Local);
        assert_eq!(classify(ip("192.168.1.20"), &c, true, &[]), Peer::Local);
        assert_eq!(classify(ip("192.168.1.30"), &c, false, &[]), Peer::Denied);
        assert_eq!(classify(ip("203.0.113.7"), &c, false, &[]), Peer::Denied);
        c.allow_lan = true;
        for lan in ["192.168.1.30", "10.1.2.3", "172.20.0.5", "100.100.1.1", "169.254.3.4", "fd00::5", "fe80::1"] {
            assert_eq!(classify(ip(lan), &c, false, &[]), Peer::Lan, "{lan}");
        }
        assert_eq!(classify(ip("172.32.0.1"), &c, false, &[]), Peer::Denied);
        assert_eq!(classify(ip("203.0.113.7"), &c, false, &[]), Peer::Denied);
        c.allowed_ips = vec!["203.0.113.0/24".into(), "2001:db8::1".into()];
        assert_eq!(classify(ip("203.0.113.7"), &c, false, &[]), Peer::Listed);
        assert_eq!(classify(ip("203.0.114.7"), &c, false, &[]), Peer::Denied);
        assert_eq!(classify(ip("2001:db8::1"), &c, false, &[]), Peer::Listed);
        let ddns = AccessConfig { allowed_ips: vec!["home.example.com".into()], ..AccessConfig::default() };
        assert_eq!(classify(ip("198.51.100.9"), &ddns, false, &[]), Peer::Denied);
        assert_eq!(classify(ip("198.51.100.9"), &ddns, false, &[ip("198.51.100.9")]), Peer::Listed);
        assert_eq!(classify(ip("::ffff:198.51.100.9"), &ddns, false, &[ip("198.51.100.9")]), Peer::Listed);
        assert!(!Peer::Local.needs_code(&AccessConfig { code_enabled: true, ..c.clone() }));
        assert!(Peer::Lan.needs_code(&AccessConfig { code_enabled: true, ..c.clone() }));
        assert!(!Peer::Lan.needs_code(&c));
    }

    #[test]
    fn local_from_nets() {
        assert!(!local_from(ip("172.17.0.1")));
        set_local_from(vec![parse_net("172.17.0.1").unwrap(), parse_net("192.168.65.0/24").unwrap()]);
        assert!(local_from(ip("172.17.0.1")));
        assert!(local_from(ip("::ffff:192.168.65.3")));
        assert!(!local_from(ip("172.17.0.2")));
    }

    #[test]
    fn whitelist_entries() {
        assert_eq!(parse_net(" 203.0.113.7 "), Ok((ip("203.0.113.7"), 32)));
        assert_eq!(format_net(parse_net("203.0.113.0/24").unwrap()), "203.0.113.0/24");
        assert_eq!(format_net(parse_net("2001:db8::1").unwrap()), "2001:db8::1");
        assert_eq!(parse_net("203.0.113"), Err("ip_format"));
        assert_eq!(parse_net("example.com"), Err("ip_format"));
        assert_eq!(parse_net("203.0.113.0/33"), Err("ip_prefix"));
        assert_eq!(parse_net("0.0.0.0/0"), Err("ip_too_wide"));
        assert_eq!(parse_net("10.0.0.0/4"), Err("ip_too_wide"));
        assert_eq!(parse_net("127.0.0.1"), Err("ip_loopback"));
        assert_eq!(parse_entry(" 4.NAS.Example.org. "), Ok(Entry::Domain("4.nas.example.org".into())));
        assert_eq!(parse_entry("nas"), Ok(Entry::Domain("nas".into())));
        assert_eq!(parse_entry("203.0.113.0/24"), Ok(Entry::Net(ip("203.0.113.0"), 24)));
        assert_eq!(parse_entry("203.0.113"), Err("ip_format"));
        assert_eq!(parse_entry("http://example.com"), Err("ip_format"));
        assert_eq!(parse_entry("example.com/24"), Err("ip_format"));
        assert_eq!(parse_entry("bad_name.com"), Err("ip_format"));
        assert_eq!(parse_entry("localhost"), Err("ip_loopback"));
        let cfg = AccessConfig { allowed_ips: vec!["203.0.113.7".into(), "home.example.com".into()], ..AccessConfig::default() };
        assert_eq!(cfg.domains().collect::<Vec<_>>(), ["home.example.com"]);
    }

    #[test]
    fn hosts() {
        for ok in ["localhost", "127.0.0.1", "192.168.1.20", "fd00::1", "nas", "nas.local", "box.lan", "x.home.arpa"] {
            assert!(host_allowed(ok, None), "{ok}");
        }
        for bad in ["evil.example", "127.0.0.1.evil.example", "nas.local.evil.example", ""] {
            assert!(!host_allowed(bad, None), "{bad}");
        }
        let mine = parse_open_host("nas.example.com").unwrap();
        assert!(host_allowed("NAS.example.com", Some(&mine)));
    }

    #[test]
    fn codes() {
        let c = generate_code();
        assert_eq!(c.len(), 14);
        let cfg = AccessConfig { code: "K7MQ-2XRA-9WTD".into(), ..AccessConfig::default() };
        assert!(cfg.code_matches("k7mq2xra9wtd"));
        assert!(cfg.code_matches(" K7MQ 2XRA 9WTD "));
        assert!(!cfg.code_matches("K7MQ-2XRA-9WTE"));
        assert!(!cfg.code_matches(""));
        let other = AccessConfig { code: "K7MQ-2XRA-9WTE".into(), ..cfg.clone() };
        assert_ne!(cfg.cookie_value(), other.cookie_value());
        assert!(check_custom_code("short").is_err());
        assert!(check_custom_code("long enough").is_ok());
    }

    #[test]
    fn limiter() {
        let l = Limiter::default();
        let a = ip("192.168.1.30");
        for _ in 0..MAX_FAILS {
            assert!(!l.blocked(a));
            l.fail(a);
        }
        assert!(l.blocked(a));
        assert!(!l.blocked(ip("192.168.1.31")));
        l.clear(a);
        assert!(!l.blocked(a));
    }
}
