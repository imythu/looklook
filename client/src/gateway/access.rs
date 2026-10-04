//! 管理台的直接访问控制（不经过看看服务器、直接连到这台机器端口的访问）。
//!
//! 管理台默认监听 `0.0.0.0:1234`，但按来源 IP 放行：
//! - **这台机器自己**（127.0.0.1 / ::1，或连接两端是同一个 IP）：总是放行，不需要访问码；
//! - **局域网**（192.168.x.x、10.x.x.x、172.16–31.x.x、100.64/10、169.254/16、fc00::/7、fe80::/10）：
//!   打开“允许局域网访问”后放行；
//! - **白名单**：用户自己添加的 IP 或网段（例如公司固定公网 IP）；
//! - 其他一律拒绝。
//!
//! 局域网和白名单访问在启用访问码后还要输入访问码。访问码只保存在本机数据库（`access` 键），
//! 不上传；浏览器里保存的是由它派生的 Cookie，换访问码后旧 Cookie 全部失效。输错次数按 IP 限制。
//!
//! 所有来源都检查 Host：只接受 localhost、IP 地址、局域网名字（单段名、`.local`、`.lan`、`.home.arpa`、
//! `.internal`）和用户设置的打开地址，挡住 DNS 重绑定（恶意网站把自己的域名解析到本机来读取终端）。

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;
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
    /// 额外允许的 IP 或网段（白名单），如 `203.0.113.7`、`203.0.113.0/24`
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
        c.allowed_ips.retain(|s| parse_net(s).is_ok());
        if parse_open_host(&c.open_host).is_err() {
            c.open_host.clear();
        }
        c
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

/// 判断来源。`same_host`：连接两端是同一个 IP（从这台机器用自己的局域网地址打开），也算本机。
pub fn classify(ip: IpAddr, cfg: &AccessConfig, same_host: bool) -> Peer {
    let ip = ip.to_canonical();
    if ip.is_loopback() || same_host {
        return Peer::Local;
    }
    if cfg.allowed_ips.iter().filter_map(|s| parse_net(s).ok()).any(|(net, len)| in_net(ip, net, len)) {
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
        assert_eq!(classify(ip("127.0.0.1"), &c, false), Peer::Local);
        assert_eq!(classify(ip("::ffff:127.0.0.1"), &c, false), Peer::Local);
        assert_eq!(classify(ip("192.168.1.20"), &c, true), Peer::Local);
        assert_eq!(classify(ip("192.168.1.30"), &c, false), Peer::Denied);
        assert_eq!(classify(ip("203.0.113.7"), &c, false), Peer::Denied);
        c.allow_lan = true;
        for lan in ["192.168.1.30", "10.1.2.3", "172.20.0.5", "100.100.1.1", "169.254.3.4", "fd00::5", "fe80::1"] {
            assert_eq!(classify(ip(lan), &c, false), Peer::Lan, "{lan}");
        }
        assert_eq!(classify(ip("172.32.0.1"), &c, false), Peer::Denied);
        assert_eq!(classify(ip("203.0.113.7"), &c, false), Peer::Denied);
        c.allowed_ips = vec!["203.0.113.0/24".into(), "2001:db8::1".into()];
        assert_eq!(classify(ip("203.0.113.7"), &c, false), Peer::Listed);
        assert_eq!(classify(ip("203.0.114.7"), &c, false), Peer::Denied);
        assert_eq!(classify(ip("2001:db8::1"), &c, false), Peer::Listed);
        assert!(!Peer::Local.needs_code(&AccessConfig { code_enabled: true, ..c.clone() }));
        assert!(Peer::Lan.needs_code(&AccessConfig { code_enabled: true, ..c.clone() }));
        assert!(!Peer::Lan.needs_code(&c));
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
