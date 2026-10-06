//! 远程打开 → 改走局域网（docs/LAN_SWITCH.md）。
//!
//! https 的远程页面不能嵌入或探测 `http://192.168.x.x`（混合内容），所以局域网连接是**新开一个页面**：
//!
//! 1. 远程页面经中转 `GET /api/lan`：这台电脑的局域网地址、浏览器是否看起来在同一网络
//!    （浏览器的公网 IP 与平台看到的这台电脑的公网 IP 相同；IPv6 比较 /64）；
//! 2. 用户点“改用局域网”（或开了“自动切换”）：`POST /api/lan/ticket` 拿一张一次性票据（60 秒），
//!    浏览器打开 `http://{局域网IP}:{端口}/__looklook/lan?t=票据&to=/t/{id}`；
//! 3. 直接入口兑换票据：只接受局域网来源，票据用一次就作废；成功后发 `ll_lan` Cookie（24 小时，绑定来源 IP），
//!    之后这个浏览器从局域网打开管理台不受“允许局域网访问”开关和访问码限制——它已经用看看账号登录过远程页面了。
//!
//! 票据只能从远程入口（带平台会话凭证）申请，局域网里的其他人拿不到；Cookie 只在内存里，看看重启后失效，
//! 回到远程页面再切一次即可。用户可以在“设置 → 访问控制”里关掉这个功能（`lan_switch`）。

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};

pub const REDEEM_PATH: &str = "/__looklook/lan";
pub const COOKIE: &str = "ll_lan";
const TICKET_TTL: Duration = Duration::from_secs(60);
const PASS_TTL: Duration = Duration::from_secs(86_400);
const MAX_TICKETS: usize = 32;
const MAX_PASSES: usize = 64;

#[derive(Default)]
pub struct Lan {
    tickets: Mutex<HashMap<String, Instant>>,
    /// Cookie 值 → (来源 IP, 发放时间)
    passes: Mutex<HashMap<String, (IpAddr, Instant)>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Lan {
    pub fn issue(&self) -> String {
        let now = Instant::now();
        let t = crate::util::random_secret();
        let mut m = lock(&self.tickets);
        m.retain(|_, at| now.duration_since(*at) < TICKET_TTL);
        if m.len() >= MAX_TICKETS {
            m.clear();
        }
        m.insert(t.clone(), now);
        t
    }

    /// 兑换票据（一次性）：返回发给浏览器的 Cookie 值。
    pub fn redeem(&self, ticket: &str, ip: IpAddr) -> Option<String> {
        let at = lock(&self.tickets).remove(ticket)?;
        if at.elapsed() >= TICKET_TTL {
            return None;
        }
        let now = Instant::now();
        let pass = crate::util::random_secret();
        let mut p = lock(&self.passes);
        p.retain(|_, (_, at)| now.duration_since(*at) < PASS_TTL);
        if p.len() >= MAX_PASSES {
            if let Some(oldest) = p.iter().min_by_key(|(_, (_, at))| *at).map(|(k, _)| k.clone()) {
                p.remove(&oldest);
            }
        }
        p.insert(pass.clone(), (ip.to_canonical(), now));
        Some(pass)
    }

    pub fn pass_ok(&self, value: &str, ip: IpAddr) -> bool {
        lock(&self.passes).get(value).is_some_and(|(owner, at)| *owner == ip.to_canonical() && at.elapsed() < PASS_TTL)
    }
}

/// 浏览器和这台电脑看起来在同一个网络：公网 IP 相同（IPv6 同一个 /64）。
pub fn same_network(browser: IpAddr, own: IpAddr) -> bool {
    match (browser.to_canonical(), own.to_canonical()) {
        (IpAddr::V4(a), IpAddr::V4(b)) => a == b,
        (IpAddr::V6(a), IpAddr::V6(b)) => a.segments()[..4] == b.segments()[..4],
        _ => false,
    }
}

/// 兑换后跳去的页面：只接受本站的相对路径。
pub fn safe_next(to: &str) -> &str {
    Some(to).filter(|n| n.starts_with('/') && !n.starts_with("//") && !n.starts_with("/\\")).unwrap_or("/")
}

/// `GET /__looklook/lan?t=&to=`：票据对就发 Cookie 并跳到 `to`；不对给出说明页（回远程页面再切一次）。
pub fn redeem_response(lan: &Lan, ticket: &str, to: &str, ip: IpAddr) -> Response {
    let Some(pass) = lan.redeem(ticket, ip) else {
        tracing::info!(%ip, "局域网票据无效或已过期");
        return super::assets::message_page(StatusCode::FORBIDDEN, "lan_expired");
    };
    tracing::info!(%ip, "远程页面切换到局域网");
    let mut r = Redirect::to(safe_next(to)).into_response();
    // Lax：从远程页面跳过来是跨站的顶层导航，Strict 的 Cookie 在这次跳转后的第一个请求里带不上。
    let c = format!("{COOKIE}={pass}; HttpOnly; SameSite=Lax; Path=/; Max-Age={}", PASS_TTL.as_secs());
    r.headers_mut().insert(header::SET_COOKIE, HeaderValue::from_str(&c).expect("cookie"));
    r.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn tickets_are_single_use_and_passes_bound_to_ip() {
        let l = Lan::default();
        let t = l.issue();
        assert!(l.redeem("nope", ip("192.168.1.30")).is_none());
        let pass = l.redeem(&t, ip("192.168.1.30")).unwrap();
        assert!(l.redeem(&t, ip("192.168.1.30")).is_none(), "一次性");
        assert!(l.pass_ok(&pass, ip("192.168.1.30")));
        assert!(l.pass_ok(&pass, ip("::ffff:192.168.1.30")));
        assert!(!l.pass_ok(&pass, ip("192.168.1.31")), "换了设备不行");
        assert!(!l.pass_ok("x", ip("192.168.1.30")));
    }

    #[test]
    fn networks() {
        assert!(same_network(ip("203.0.113.7"), ip("203.0.113.7")));
        assert!(same_network(ip("::ffff:203.0.113.7"), ip("203.0.113.7")));
        assert!(!same_network(ip("203.0.113.7"), ip("203.0.113.8")));
        assert!(same_network(ip("2001:db8:1:2::5"), ip("2001:db8:1:2:aaaa::1")));
        assert!(!same_network(ip("2001:db8:1:3::5"), ip("2001:db8:1:2::1")));
        assert!(!same_network(ip("2001:db8:1:2::5"), ip("203.0.113.7")));
    }

    #[test]
    fn next_paths() {
        assert_eq!(safe_next("/t/abc"), "/t/abc");
        assert_eq!(safe_next("//evil.example"), "/");
        assert_eq!(safe_next("/\\evil"), "/");
        assert_eq!(safe_next("https://evil.example"), "/");
    }
}
