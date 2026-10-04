//! 反向代理：终端（ttyd）与本机网页（隧道）。HTTP 流式转发，WebSocket 升级后双向直通。

use std::time::Duration;

use axum::body::Body;
use axum::extract::Request;
use axum::http::{header, HeaderMap, HeaderName, HeaderValue, StatusCode, Version};
use axum::response::{IntoResponse, Response};
use http_body_util::BodyExt;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::client::legacy::Client;
use hyper_util::rt::{TokioExecutor, TokioIo};
use super::remote::Remote;

use super::{assets, App};
use crate::instances::Instances;

pub type HttpClient = Client<HttpConnector, Body>;

pub fn client() -> HttpClient {
    let mut c = HttpConnector::new();
    c.set_connect_timeout(Some(Duration::from_secs(5)));
    c.set_nodelay(true);
    Client::builder(TokioExecutor::new()).pool_idle_timeout(Duration::from_secs(30)).build(c)
}

const HOP_BY_HOP: &[&str] = &[
    "connection",
    "keep-alive",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
    "proxy-connection",
    "proxy-authenticate",
    "proxy-authorization",
];

pub struct Opts {
    /// 上游 Basic 认证（ttyd 的随机口令）
    pub auth: Option<String>,
    /// 改写 Host（开发服务器通常只接受 localhost）
    pub host: Option<String>,
    /// 在 ttyd 首页注入终端字体与辅助脚本
    pub inject_fonts: bool,
    /// Location 里的本机地址改回浏览器看到的地址：(本机前缀列表, 替换为)
    pub rewrite_location: Option<(Vec<String>, String)>,
    /// 这是终端连接：连着的时候不自动更新（见 updater::TerminalGuard）
    pub terminal: bool,
}

pub fn is_websocket(h: &HeaderMap) -> bool {
    h.get(header::UPGRADE).and_then(|v| v.to_str().ok()).is_some_and(|v| v.eq_ignore_ascii_case("websocket"))
}

fn strip_hop_by_hop(h: &mut HeaderMap) {
    let listed: Vec<HeaderName> = h
        .get_all(header::CONNECTION)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .filter_map(|n| HeaderName::from_bytes(n.trim().as_bytes()).ok())
        .collect();
    for n in listed {
        h.remove(n);
    }
    for n in HOP_BY_HOP {
        h.remove(*n);
    }
}

pub async fn forward(client: &HttpClient, mut req: Request, port: u16, opts: Opts) -> Response {
    let ws = is_websocket(req.headers());
    let client_upgrade = ws.then(|| hyper::upgrade::on(&mut req));
    let (mut parts, body) = req.into_parts();
    let pq = parts.uri.path_and_query().map(|p| p.as_str().to_string()).unwrap_or_else(|| "/".into());
    parts.uri = match format!("http://127.0.0.1:{port}{pq}").parse() {
        Ok(u) => u,
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };
    parts.version = Version::HTTP_11;
    let h = &mut parts.headers;
    strip_hop_by_hop(h);
    // 访问码 Cookie 只属于看看自己，不交给 ttyd 或开发网页。
    strip_cookie(h, super::access::COOKIE);
    if ws {
        h.insert(header::CONNECTION, HeaderValue::from_static("upgrade"));
        h.insert(header::UPGRADE, HeaderValue::from_static("websocket"));
    }
    if let Some(a) = &opts.auth {
        if let Ok(v) = HeaderValue::from_str(a) {
            h.insert(header::AUTHORIZATION, v);
        }
    }
    if let Some(host) = &opts.host {
        if let Ok(v) = HeaderValue::from_str(host) {
            h.insert(header::HOST, v);
        }
    }
    if opts.inject_fonts {
        // 要改写页面内容，请 ttyd 返回未压缩的首页。
        h.remove(header::ACCEPT_ENCODING);
    }
    let body = if ws { Body::empty() } else { body };
    let fut = client.request(Request::from_parts(parts, body));
    let mut resp = match tokio::time::timeout(Duration::from_secs(60), fut).await {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => {
            tracing::debug!(error = %e, port, "上游连接失败");
            return assets::message_page(StatusCode::BAD_GATEWAY, "upstream_down");
        }
        Err(_) => return assets::message_page(StatusCode::GATEWAY_TIMEOUT, "upstream_down"),
    };

    if ws && resp.status() == StatusCode::SWITCHING_PROTOCOLS {
        let server_upgrade = hyper::upgrade::on(&mut resp);
        let client_upgrade = client_upgrade.expect("websocket");
        let guard = opts.terminal.then(crate::updater::TerminalGuard::new);
        tokio::spawn(async move {
            let _guard = guard;
            match tokio::try_join!(client_upgrade, server_upgrade) {
                Ok((c, s)) => {
                    let _ = tokio::io::copy_bidirectional(&mut TokioIo::new(c), &mut TokioIo::new(s)).await;
                }
                Err(e) => tracing::debug!(error = %e, "WebSocket 升级失败"),
            }
        });
        let (parts, _) = resp.into_parts();
        return Response::from_parts(parts, Body::empty());
    }

    let (mut parts, body) = resp.into_parts();
    strip_hop_by_hop(&mut parts.headers);
    if let Some((from, to)) = &opts.rewrite_location {
        rewrite_location(&mut parts.headers, from, to);
    }
    let is_html = parts
        .headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("text/html"));
    if opts.inject_fonts && parts.status == StatusCode::OK && is_html && parts.headers.get(header::CONTENT_ENCODING).is_none() {
        let bytes = match tokio::time::timeout(Duration::from_secs(30), body.collect()).await {
            Ok(Ok(b)) => b.to_bytes(),
            _ => return assets::message_page(StatusCode::BAD_GATEWAY, "upstream_down"),
        };
        let html = inject(&String::from_utf8_lossy(&bytes));
        parts.headers.remove(header::CONTENT_LENGTH);
        parts.headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        return Response::from_parts(parts, Body::from(html));
    }
    Response::from_parts(parts, Body::new(body.map_err(axum::Error::new)))
}

/// 在 ttyd 首页的 `</head>` 前加入字体样式与辅助脚本。
pub fn inject(html: &str) -> String {
    const TAGS: &str = concat!(
        r#"<link rel="preload" href="/fonts/JetBrainsMono-Regular.woff2" as="font" type="font/woff2" crossorigin>"#,
        r#"<link rel="stylesheet" href="/fonts/fonts.css">"#,
        r#"<script src="/static/term.js"></script>"#
    );
    match html.find("</head>") {
        Some(i) => format!("{}{TAGS}{}", &html[..i], &html[i..]),
        None => format!("{TAGS}{html}"),
    }
}

fn strip_cookie(h: &mut HeaderMap, name: &str) {
    let kept: Vec<String> = h
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .map(str::trim)
        .filter(|kv| !kv.is_empty() && kv.split('=').next() != Some(name))
        .map(str::to_owned)
        .collect();
    h.remove(header::COOKIE);
    if !kept.is_empty() {
        if let Ok(v) = HeaderValue::from_str(&kept.join("; ")) {
            h.insert(header::COOKIE, v);
        }
    }
}

fn rewrite_location(h: &mut HeaderMap, from: &[String], to: &str) {
    let Some(loc) = h.get(header::LOCATION).and_then(|v| v.to_str().ok()).map(str::to_owned) else { return };
    for f in from {
        if let Some(rest) = loc.strip_prefix(f.as_str()) {
            if let Ok(v) = HeaderValue::from_str(&format!("{to}{rest}")) {
                h.insert(header::LOCATION, v);
            }
            return;
        }
    }
}

/// 本机网页（隧道）：平台签名的会话凭证给出目标端口，只转到 127.0.0.1。
pub async fn tunnel(app: &App, mut req: Request, p: &Remote, proto: &str) -> Response {
    if !app.account.gate().allowed() {
        return assets::message_page(StatusCode::FORBIDDEN, "locked");
    }
    let Some(port) = p.port.filter(|port| *port != 0) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    // 不能借本机网页访问看看自己的入口和终端服务。
    if port == app.ui_addr.port() || port == app.relay_addr.port() || Instances::is_reserved_port(port) {
        return assets::message_page(StatusCode::FORBIDDEN, "port_reserved");
    }
    let local = format!("localhost:{port}");
    // 开发服务器常只接受 localhost 来源；平台已经校验过浏览器的 Origin。
    if req.headers().contains_key(header::ORIGIN) {
        if let Ok(v) = HeaderValue::from_str(&format!("http://{local}")) {
            req.headers_mut().insert(header::ORIGIN, v);
        }
    }
    let public = format!("{proto}://{}", p.host);
    let opts = Opts {
        auth: None,
        host: Some(local.clone()),
        inject_fonts: false,
        rewrite_location: Some((vec![format!("http://{local}"), format!("http://127.0.0.1:{port}")], public)),
        terminal: false,
    };
    let r = forward(&app.http, req, port, opts).await;
    if r.status() == StatusCode::BAD_GATEWAY || r.status() == StatusCode::GATEWAY_TIMEOUT {
        return assets::message_page_with(r.status(), "page_down", &[("port", &port.to_string())]);
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn injects_before_head_end() {
        let out = inject("<html><head><title>x</title></head><body></body></html>");
        assert!(out.contains(r#"<script src="/static/term.js"></script></head>"#));
        assert!(out.find("fonts.css").unwrap() < out.find("</head>").unwrap());
    }

    #[test]
    fn location_rewrite_and_cookie_strip() {
        let mut h = HeaderMap::new();
        h.insert(header::LOCATION, HeaderValue::from_static("http://localhost:5173/login?x=1"));
        rewrite_location(&mut h, &["http://localhost:5173".into()], "https://alice--k7m2q9xa.looklook.example");
        assert_eq!(h[header::LOCATION], "https://alice--k7m2q9xa.looklook.example/login?x=1");

        let mut h = HeaderMap::new();
        h.insert(header::COOKIE, HeaderValue::from_static("a=1; ll_local=secret; b=2"));
        strip_cookie(&mut h, "ll_local");
        assert_eq!(h[header::COOKIE], "a=1; b=2");
    }
}
