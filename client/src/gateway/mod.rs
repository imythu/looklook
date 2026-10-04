//! 本机网关：同一套路由挂在两个入口上。
//!
//! - **直接入口**（默认 `0.0.0.0:1234`）：浏览器直接连到这台机器打开管理台。按来源 IP 放行：
//!   本机总是可以；局域网、白名单要用户在设置里允许，并可要求访问码；Host 只接受本机、IP 与局域网名字
//!   （防 DNS 重绑定）。见 `access.rs`。
//! - **远程入口**（随机端口，rathole 的 `local_addr`）：每个请求必须带平台签名的会话凭证和引用它的
//!   网关令牌 v3（服务端 docs/04 §4.4，见 `remote.rs`）。`user_host` 进管理台/终端路由，`tunnel` 转到凭证里的本机端口。
//!
//! 路由：`/api/*` 本机接口；`/mcp` AI 助手用的 MCP 接口（只限本机，见 `mcp.rs`）；`/i/{id}/…` 终端（反向代理到 ttyd）；`/fonts/*` 终端字体；其余为管理台页面。

pub mod access;
mod api;
mod assets;
pub mod mcp;
mod proxy;
mod remote;

use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, RwLock};

use axum::extract::{ConnectInfo, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{any, get};
use axum::Router;
use looklook_protocol::edge_auth;
use looklook_protocol::gateway_token::{self, Kind};

use crate::account::Account;
use crate::error::LocalError;
use crate::instances::Instances;
use crate::paths::Paths;
use crate::store::Store;

pub use proxy::{client as proxy_client, HttpClient};

/// 请求从哪个入口进来。
#[derive(Debug, Clone)]
pub enum Access {
    Local,
    Remote { host: String, proto: String },
}

impl Access {
    pub fn is_local(&self) -> bool {
        matches!(self, Access::Local)
    }
}

pub struct Inner {
    pub account: Arc<Account>,
    pub instances: Arc<Instances>,
    pub updater: Arc<crate::updater::Updater>,
    pub metrics: Arc<crate::metrics::Metrics>,
    /// 终端文件传输：进行中的分块上传（docs/FILE_TRANSFER.md §4）
    pub transfers: Arc<crate::transfer::Transfers>,
    pub store: Arc<Store>,
    pub paths: Paths,
    pub ui_addr: SocketAddr,
    pub relay_addr: SocketAddr,
    pub http: HttpClient,
    /// 直接访问控制（局域网、白名单、访问码、打开地址），改设置时同步更新
    pub access: RwLock<access::AccessConfig>,
    pub limiter: access::Limiter,
    /// 这台机器的局域网 IP（启动时取一次，只用于显示）
    pub lan_ips: Vec<IpAddr>,
    /// 远程入口的凭证缓存与 jti 去重
    pub remote: remote::Verifier,
}

impl Inner {
    pub fn access(&self) -> access::AccessConfig {
        self.access.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// 把浏览器打开地址写到 `run/open`（`looklook open`、托盘菜单使用），返回不带结尾 `/` 的地址。
    pub fn write_open_file(&self) -> std::io::Result<String> {
        let base = match self.access().open_host() {
            Some(h) => format!("http://{}:{}", h.url_host(), self.ui_addr.port()),
            None => format!("http://{}", self.ui_addr),
        };
        std::fs::write(self.paths.home.join("run").join("open"), &base)?;
        Ok(base)
    }

    /// 管理台的访问情况，界面据此显示端口、局域网地址与安全提醒。
    pub fn console_info(&self) -> serde_json::Value {
        let c = self.access();
        serde_json::json!({
            "port": self.ui_addr.port(),
            "lan_ips": self.lan_ips.iter().map(|ip| ip.to_string()).collect::<Vec<_>>(),
            "allow_lan": c.allow_lan,
            "allowed_ips": c.allowed_ips.len(),
            "code_enabled": c.code_enabled,
        })
    }
}

pub type App = Arc<Inner>;

// The platform reserves and strips X-LL-* before proxying browser requests.
pub const CLIENT_HEADER: &str = "x-looklook-client";

pub fn router(app: App) -> Router {
    Router::new()
        .nest("/api", api::routes(app.clone()))
        .route("/i/{id}", get(|axum::extract::Path(id): axum::extract::Path<String>| async move { Redirect::permanent(&format!("/i/{id}/")) }))
        .route("/i/{id}/", any(terminal))
        .route("/i/{id}/{*rest}", any(terminal))
        .route(mcp::PATH, axum::routing::post(mcp::handle).get(mcp::not_allowed).delete(mcp::not_allowed))
        .route("/fonts/{*path}", get(assets::font))
        .route("/static/term.js", get(assets::term_js))
        .fallback(assets::ui)
        .layer(middleware::from_fn(security_headers))
        .with_state(app)
}

/// 管理台、终端页只允许同源嵌入；地址里的一次性口令不随 Referer 泄露。
async fn security_headers(req: Request, next: Next) -> Response {
    let mut r = next.run(req).await;
    let h = r.headers_mut();
    h.entry("x-frame-options").or_insert(HeaderValue::from_static("SAMEORIGIN"));
    h.entry("x-content-type-options").or_insert(HeaderValue::from_static("nosniff"));
    h.entry("referrer-policy").or_insert(HeaderValue::from_static("no-referrer"));
    r
}

/// 直接入口：按来源 IP 放行，检查 Host，需要时检查访问码。
pub async fn local_guard(State(app): State<App>, mut req: Request, next: Next) -> Response {
    // 没有连接信息只会出现在测试里，按本机处理。
    let conn = req.extensions().get::<ConnectInfo<Conn>>().map(|c| c.0);
    let peer_ip = conn.map(|c| c.remote.ip().to_canonical()).unwrap_or(IpAddr::from([127, 0, 0, 1]));
    let same_host = conn.and_then(|c| c.local).is_some_and(|l| l.ip().to_canonical() == peer_ip);
    let cfg = app.access();
    let peer = access::classify(peer_ip, &cfg, same_host);
    if peer == access::Peer::Denied {
        return assets::denied_page(&peer_ip.to_string());
    }
    if !host_name(req.headers()).is_some_and(|n| access::host_allowed(n, cfg.open_host().as_ref())) {
        return (StatusCode::FORBIDDEN, "forbidden host").into_response();
    }
    if peer.needs_code(&cfg) && !cookie(req.headers(), access::COOKIE).is_some_and(|v| ct_eq(v, &cfg.cookie_value())) {
        if req.uri().path() == access::FORM_PATH && req.method() == Method::POST {
            return submit_code(&app, &cfg, peer_ip, req).await;
        }
        if req.uri().path().starts_with("/api/") {
            return LocalError::new("ACCESS_CODE_REQUIRED").into_response();
        }
        let next_path = req.uri().path_and_query().map(|p| p.as_str()).unwrap_or("/");
        return assets::code_page(next_path, None);
    }
    req.extensions_mut().insert(Access::Local);
    req.extensions_mut().insert(peer);
    next.run(req).await
}

/// 访问码表单：对了就发 Cookie 并回到原来的页面。
async fn submit_code(app: &App, cfg: &access::AccessConfig, ip: IpAddr, req: Request) -> Response {
    if app.limiter.blocked(ip) {
        return assets::code_page("/", Some("too_many"));
    }
    use axum::extract::FromRequest;
    let form = axum::Form::<std::collections::HashMap<String, String>>::from_request(req, &()).await.map(|f| f.0).unwrap_or_default();
    let field = |k: &str| form.get(k).map(String::as_str).unwrap_or("");
    // 只回到本站的相对路径
    let next = Some(field("next")).filter(|n| n.starts_with('/') && !n.starts_with("//") && !n.starts_with("/\\")).unwrap_or("/");
    if !cfg.code_matches(field("code")) {
        app.limiter.fail(ip);
        tracing::warn!(%ip, "访问码错误");
        return assets::code_page(next, Some(if app.limiter.blocked(ip) { "too_many" } else { "wrong" }));
    }
    app.limiter.clear(ip);
    let mut r = Redirect::to(next).into_response();
    let c = format!("{}={}; HttpOnly; SameSite=Strict; Path=/; Max-Age=2592000", access::COOKIE, cfg.cookie_value());
    r.headers_mut().insert(header::SET_COOKIE, HeaderValue::from_str(&c).expect("cookie"));
    r
}

/// 远程入口：验证会话凭证与网关令牌；隧道请求直接转发。
async fn relay_entry(app: App, inner: Router, mut req: Request) -> Response {
    let header = |h: &HeaderMap, name: &str| h.get(name).and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
    let (cap, token) = (header(req.headers(), edge_auth::CAP_HEADER), header(req.headers(), gateway_token::HEADER));
    let remote = match app.remote.verify(&app.account, &cap, &token) {
        Ok(r) => r,
        Err(why) => {
            tracing::debug!(why, "远程请求未通过校验");
            return (StatusCode::UNAUTHORIZED, "unauthorized").into_response();
        }
    };
    req.headers_mut().remove(gateway_token::HEADER);
    req.headers_mut().remove(edge_auth::CAP_HEADER);
    let proto = req
        .headers()
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .filter(|p| *p == "http" || *p == "https")
        .unwrap_or("https")
        .to_string();
    match remote.kind {
        Kind::Tunnel => proxy::tunnel(&app, req, &remote, &proto).await,
        Kind::UserHost => {
            req.extensions_mut().insert(Access::Remote { host: remote.host, proto });
            use tower::ServiceExt;
            inner.oneshot(req).await.into_response()
        }
    }
}

/// 终端：`/i/{id}/…` → 对应 ttyd。
async fn terminal(State(app): State<App>, req: Request) -> Response {
    let access = req.extensions().get::<Access>().cloned().unwrap_or(Access::Local);
    let id = req.uri().path().trim_start_matches("/i/").split('/').next().unwrap_or("").to_string();
    let gate = app.account.gate();
    if !gate.allowed() {
        return assets::message_page(StatusCode::FORBIDDEN, "locked");
    }
    let Some((port, auth)) = app.instances.upstream(&id) else {
        return assets::message_page(StatusCode::SERVICE_UNAVAILABLE, "not_running");
    };
    if proxy::is_websocket(req.headers()) && !origin_matches(req.headers(), &access) {
        return (StatusCode::FORBIDDEN, "origin").into_response();
    }
    let index = req.method() == Method::GET && req.uri().path() == format!("/i/{id}/");
    let opts = proxy::Opts { auth: Some(auth), host: None, inject_fonts: index, rewrite_location: None, terminal: true };
    proxy::forward(&app.http, req, port, opts).await
}

/// 写请求与 WebSocket 的来源检查：`Origin` 必须是当前站点本身。
pub fn origin_matches(h: &HeaderMap, access: &Access) -> bool {
    let Some(origin) = h.get(header::ORIGIN).and_then(|v| v.to_str().ok()) else {
        // 非浏览器请求没有 Origin；本机入口已有访问口令，远程入口已有网关令牌。
        return true;
    };
    match access {
        Access::Local => {
            let host = h.get(header::HOST).and_then(|v| v.to_str().ok()).unwrap_or("");
            origin.eq_ignore_ascii_case(&format!("http://{host}"))
        }
        // 平台转发的主机名不带端口（开发环境的非标准端口会出现在 Origin 里），只比较协议与主机名。
        Access::Remote { host, proto } => origin
            .split_once("://")
            .is_some_and(|(scheme, rest)| scheme.eq_ignore_ascii_case(proto) && strip_port(rest).eq_ignore_ascii_case(strip_port(host))),
    }
}

fn strip_port(hostport: &str) -> &str {
    match hostport.rsplit_once(':') {
        Some((h, p)) if !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()) => h,
        _ => hostport,
    }
}

/// 请求头 Host 里的主机名（去掉端口与 IPv6 方括号）。
fn host_name(h: &HeaderMap) -> Option<&str> {
    let host = h.get(header::HOST).and_then(|v| v.to_str().ok())?;
    Some(if let Some(rest) = host.strip_prefix('[') {
        rest.split(']').next().unwrap_or("")
    } else {
        host.rsplit_once(':').map(|(n, _)| n).unwrap_or(host)
    })
}



pub fn cookie<'a>(h: &'a HeaderMap, name: &str) -> Option<&'a str> {
    h.get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .find_map(|kv| kv.trim().strip_prefix(name).and_then(|r| r.strip_prefix('=')))
}

pub(crate) fn ct_eq(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// 连接两端的地址：判断“从这台机器自己连过来”（两端同一个 IP）要用到本端地址。
#[derive(Debug, Clone, Copy)]
pub struct Conn {
    pub remote: SocketAddr,
    pub local: Option<SocketAddr>,
}

impl axum::extract::connect_info::Connected<axum::serve::IncomingStream<'_, tokio::net::TcpListener>> for Conn {
    fn connect_info(s: axum::serve::IncomingStream<'_, tokio::net::TcpListener>) -> Self {
        Conn { remote: *s.remote_addr(), local: s.io().local_addr().ok() }
    }
}

pub async fn serve(listener: tokio::net::TcpListener, app: axum::Router, shutdown: tokio_util::sync::CancellationToken) {
    // 带上连接的来源地址，直接入口按来源 IP 放行。
    let r = axum::serve(listener, app.into_make_service_with_connect_info::<Conn>()).with_graceful_shutdown(async move { shutdown.cancelled().await }).await;
    if let Err(e) = r {
        tracing::error!(error = %e, "网关退出");
    }
}

pub fn local_app(app: App) -> Router {
    router(app.clone()).layer(middleware::from_fn_with_state(app, local_guard))
}

pub fn relay_app(app: App) -> Router {
    let inner = router(app.clone());
    Router::new().fallback(move |req: Request| {
        let (app, inner) = (app.clone(), inner.clone());
        async move { relay_entry(app, inner, req).await }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut m = HeaderMap::new();
        for (k, v) in pairs {
            m.append(axum::http::HeaderName::from_bytes(k.as_bytes()).unwrap(), HeaderValue::from_str(v).unwrap());
        }
        m
    }

    #[test]
    fn host_names() {
        assert_eq!(host_name(&h(&[("host", "127.0.0.1:1234")])), Some("127.0.0.1"));
        assert_eq!(host_name(&h(&[("host", "[::1]:1234")])), Some("::1"));
        assert_eq!(host_name(&h(&[("host", "nas.local")])), Some("nas.local"));
        assert_eq!(host_name(&h(&[])), None);
    }

    #[test]
    fn origins() {
        let local = Access::Local;
        assert!(origin_matches(&h(&[("host", "127.0.0.1:1234"), ("origin", "http://127.0.0.1:1234")]), &local));
        assert!(!origin_matches(&h(&[("host", "127.0.0.1:1234"), ("origin", "https://evil.example")]), &local));
        assert!(origin_matches(&h(&[("host", "127.0.0.1:1234")]), &local));
        let remote = Access::Remote { host: "alice.looklook.example".into(), proto: "https".into() };
        assert!(origin_matches(&h(&[("origin", "https://alice.looklook.example")]), &remote));
        assert!(!origin_matches(&h(&[("origin", "https://alice--k7m2q9xa.looklook.example")]), &remote));
        assert!(!origin_matches(&h(&[("origin", "http://alice.looklook.example")]), &remote));
        assert!(!origin_matches(&h(&[("origin", "https://alice.looklook.example.evil.example")]), &remote));
        let dev = Access::Remote { host: "carol.4.looklook.example".into(), proto: "http".into() };
        assert!(origin_matches(&h(&[("origin", "http://carol.4.looklook.example:31235")]), &dev));
    }

    #[test]
    fn cookies() {
        let m = h(&[("cookie", "a=1; ll_local=xyz; b=2")]);
        assert_eq!(cookie(&m, "ll_local"), Some("xyz"));
        assert_eq!(cookie(&m, "ll_loc"), None);
        assert!(ct_eq("abc", "abc"));
        assert!(!ct_eq("abc", "abd"));
    }
}
