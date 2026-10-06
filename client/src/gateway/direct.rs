//! 远程打开时的“本机直连”：浏览器就开在这台电脑上时，终端不绕中转，直接连本机。
//!
//! 1. 管理台经中转 `POST /api/direct`：记下页面来源（会话凭证与 `Origin` 检查已确认是用户自己的子站），发一个一次性口令；
//! 2. 管理台在后台请求 `http://127.0.0.1:{端口}/direct/ping?n=口令`：口令对得上、来源一致、来自本机时才回 204 并带 CORS 头。
//!    别的网页拿不到口令，探测不出本机装了看看；用户另一台电脑上的看看没有这个口令，也冒充不了这台；
//! 3. 通过后终端 iframe 直接打开 `http://127.0.0.1:{端口}/i/{id}/`（回环地址不算混合内容）；
//!    本机访问的终端页对登记过的来源放开 `frame-ancestors`（见 `frame_ancestors`）。
//!
//! 局域网地址不在这里：https 页面里请求 `http://192.168.x.x` 会被浏览器当作混合内容拦下，探测和嵌入都做不到；
//! 局域网改为整页切换，见 `lan.rs` 与 docs/LAN_SWITCH.md。

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::extract::{Query, Request, State};
use axum::http::{header, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};

use super::{access, Access, App};

pub const PING_PATH: &str = "/direct/ping";
const NONCE_TTL: Duration = Duration::from_secs(600);
/// 登记的来源保留一天（与子站会话凭证一样长），期间打开的终端都能嵌入。
const EMBED_TTL: Duration = Duration::from_secs(86_400);
const MAX_NONCES: usize = 64;
const MAX_EMBEDDERS: usize = 8;

#[derive(Default)]
pub struct Direct {
    nonces: Mutex<HashMap<String, (String, Instant)>>,
    embedders: Mutex<HashMap<String, Instant>>,
}

/// 只接受 `scheme://host[:port]` 形式的来源，放进 CSP 也不会多出指令。
pub fn valid_origin(o: &str) -> bool {
    let Some((scheme, rest)) = o.split_once("://") else { return false };
    matches!(scheme, "http" | "https")
        && !rest.is_empty()
        && rest.len() <= 255
        && rest.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b':' | b'[' | b']'))
}

impl Direct {
    /// 给这个来源发一个口令（来源已经过校验），同时登记它可以嵌入本机终端。
    pub fn issue(&self, origin: &str) -> String {
        let now = Instant::now();
        let nonce = crate::util::random_secret();
        let mut n = lock(&self.nonces);
        n.retain(|_, (_, at)| now.duration_since(*at) < NONCE_TTL);
        if n.len() >= MAX_NONCES {
            n.clear();
        }
        n.insert(nonce.clone(), (origin.to_string(), now));
        drop(n);
        let mut e = lock(&self.embedders);
        e.retain(|_, at| now.duration_since(*at) < EMBED_TTL);
        e.insert(origin.to_string(), now);
        if e.len() > MAX_EMBEDDERS {
            if let Some(oldest) = e.iter().min_by_key(|(_, at)| **at).map(|(k, _)| k.clone()) {
                e.remove(&oldest);
            }
        }
        nonce
    }

    /// 口令有效且属于这个来源（有效期内可以反复用：前端会测几次取最快的）。
    pub fn check(&self, nonce: &str, origin: &str) -> bool {
        lock(&self.nonces).get(nonce).is_some_and(|(o, at)| at.elapsed() < NONCE_TTL && super::ct_eq(o, origin))
    }

    /// 本机终端页的 `frame-ancestors`；没有登记过来源时为 None（保持只允许同源嵌入）。
    pub fn frame_ancestors(&self) -> Option<String> {
        let e = lock(&self.embedders);
        let mut list: Vec<&str> = e.iter().filter(|(_, at)| at.elapsed() < EMBED_TTL).map(|(k, _)| k.as_str()).collect();
        if list.is_empty() {
            return None;
        }
        list.sort_unstable();
        Some(format!("frame-ancestors 'self' {}", list.join(" ")))
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

#[derive(serde::Deserialize)]
pub struct PingQuery {
    #[serde(default)]
    n: String,
}

/// `GET|OPTIONS /direct/ping?n=`：只在直接入口、来自本机时应答；其他情况一律 404，不带 CORS 头。
pub async fn ping(State(app): State<App>, Query(q): Query<PingQuery>, req: Request) -> Response {
    let local = req.extensions().get::<Access>().is_some_and(Access::is_local) && req.extensions().get::<access::Peer>() == Some(&access::Peer::Local);
    let origin = req.headers().get(header::ORIGIN).and_then(|v| v.to_str().ok()).unwrap_or("");
    if !local || !valid_origin(origin) || !app.direct.check(&q.n, origin) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let mut r = StatusCode::NO_CONTENT.into_response();
    let h = r.headers_mut();
    h.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, HeaderValue::from_str(origin).expect("origin"));
    h.insert(header::VARY, HeaderValue::from_static("Origin"));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    if req.method() == Method::OPTIONS {
        h.insert(header::ACCESS_CONTROL_ALLOW_METHODS, HeaderValue::from_static("GET"));
        // 旧版 Chrome 的“私有网络访问”预检
        h.insert("access-control-allow-private-network", HeaderValue::from_static("true"));
        h.insert(header::ACCESS_CONTROL_MAX_AGE, HeaderValue::from_static("600"));
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nonce_bound_to_origin() {
        let d = Direct::default();
        assert!(d.frame_ancestors().is_none());
        let n = d.issue("https://alice.r2.looklook.dev");
        assert!(d.check(&n, "https://alice.r2.looklook.dev"));
        assert!(d.check(&n, "https://alice.r2.looklook.dev"));
        assert!(!d.check(&n, "https://evil.example"));
        assert!(!d.check("guess", "https://alice.r2.looklook.dev"));
        assert_eq!(d.frame_ancestors().as_deref(), Some("frame-ancestors 'self' https://alice.r2.looklook.dev"));
    }

    #[test]
    fn embedders_capped() {
        let d = Direct::default();
        for i in 0..20 {
            d.issue(&format!("https://u{i}.r2.looklook.dev"));
        }
        assert_eq!(lock(&d.embedders).len(), MAX_EMBEDDERS);
    }

    #[test]
    fn origins() {
        assert!(valid_origin("https://alice.r2.looklook.dev"));
        assert!(valid_origin("http://carol.4.looklook.example:31235"));
        assert!(!valid_origin("https://a.dev/x"));
        assert!(!valid_origin("https://a.dev; script-src *"));
        assert!(!valid_origin("javascript://x"));
        assert!(!valid_origin("null"));
    }
}
