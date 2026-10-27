//! DSH 的网页界面在会话标签里打开：`/w/{id}/…` → `dsh web` 监听的本机端口（和管理台同源，iframe 里能用）。
//!
//! DSH 按官方文档支持“去掉前缀的反向代理”（首页带 `<base href="./">`，资源、接口、WebSocket 都按相对地址走）。
//! 它的安全检查：`Host` 必须是本机地址，带 `Origin` 时必须和 `Host` 一致，`sec-fetch-site: cross-site` 拒绝；
//! 登录靠启动口令换来的 Cookie（`HttpOnly`、`SameSite=Strict`，绑定 Host）。
//!
//! 这里由网关自己用口令换 Cookie、转发时带上，浏览器拿不到口令也拿不到 DSH 的 Cookie：
//! 能打开管理台的人才能走到这里（本机入口的访问码、远程入口的网关令牌），远程经 WebRTC 时
//! Service Worker 做出来的响应设不了 Cookie，也照样能用。来源检查在这里做完，再把 `Host`、`Origin` 改成本机地址交给 DSH。

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use axum::extract::{Request, State};
use axum::http::{header, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};

use super::{api, assets, origin_matches, proxy, Access, App};

/// 每个 DSH 换到的 Cookie（`名字=值`）。dsh 重启后口令和签名都可能变，转发回 401 时丢掉重换。
fn cookies() -> &'static Mutex<HashMap<String, String>> {
    static C: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    C.get_or_init(Default::default)
}

fn cached(id: &str) -> Option<String> {
    cookies().lock().unwrap_or_else(|e| e.into_inner()).get(id).cloned()
}

fn forget(id: &str) {
    cookies().lock().unwrap_or_else(|e| e.into_inner()).remove(id);
}

/// 确保 DSH 在运行，拿到能用的 Cookie（等 dsh 启动完，最多 60 秒）。
pub(super) async fn ensure(app: &App, id: &str, port: u16) -> Result<Option<String>, crate::error::LocalError> {
    let v = app.instances.get(id).await?;
    // 任务退出了（例如 dsh 崩了）会重新运行
    if !v.running || v.task_alive != Some(true) {
        forget(id);
        app.instances.start(id).await?;
    }
    if let Some(c) = cached(id) {
        return Ok(Some(c));
    }
    let login = api::dsh_login(app, id, port).await;
    if let Some(l) = &login {
        cookies().lock().unwrap_or_else(|e| e.into_inner()).insert(id.to_string(), l.cookie.clone());
    }
    Ok(login.map(|l| l.cookie))
}

/// `/w/{id}/…`
pub async fn web(State(app): State<App>, mut req: Request) -> Response {
    let access = req.extensions().get::<Access>().cloned().unwrap_or(Access::Local);
    let path = req.uri().path().to_string();
    let rest = path.strip_prefix("/w/").unwrap_or("");
    let (id, sub) = rest.split_once('/').map_or((rest, ""), |(id, sub)| (id, sub));
    let id = id.to_string();
    if !app.account.gate().allowed() {
        return assets::message_page(StatusCode::FORBIDDEN, "locked");
    }
    // 写请求、WebSocket 都会带 Origin：必须是管理台自己（DSH 那边只会看到改过的本机地址）
    if req.headers().contains_key(header::ORIGIN) && !origin_matches(req.headers(), &access) {
        return (StatusCode::FORBIDDEN, "origin").into_response();
    }
    let port = match app.instances.get(&id).await {
        Ok(v) => match v.web_port {
            Some(p) => p,
            None => return assets::message_page(StatusCode::NOT_FOUND, "not_running"),
        },
        Err(_) => return assets::message_page(StatusCode::NOT_FOUND, "not_running"),
    };
    let index = sub.is_empty() && req.method() == Method::GET;
    // 首页才去启动、换 Cookie；别的请求用已有的（没有就让 DSH 回 401，页面会重连）
    let cookie = if index {
        match ensure(&app, &id, port).await {
            Ok(Some(c)) => Some(c),
            Ok(None) => return assets::message_page(StatusCode::SERVICE_UNAVAILABLE, "upstream_down"),
            Err(e) => return e.into_response(),
        }
    } else {
        cached(&id)
    };
    let local = format!("127.0.0.1:{port}");
    let query = req.uri().query().map(|q| format!("?{q}")).unwrap_or_default();
    let Ok(uri) = format!("/{sub}{query}").parse() else { return StatusCode::BAD_REQUEST.into_response() };
    *req.uri_mut() = uri;
    let h = req.headers_mut();
    // 浏览器带来的 Cookie 是管理台的，不交给 DSH；只带网关换到的那个
    h.remove(header::COOKIE);
    if let Some(c) = cookie.as_deref().and_then(|c| HeaderValue::from_str(c).ok()) {
        h.insert(header::COOKIE, c);
    }
    if h.contains_key(header::ORIGIN) {
        if let Ok(v) = HeaderValue::from_str(&format!("http://{local}")) {
            h.insert(header::ORIGIN, v);
        }
    }
    h.remove("sec-fetch-site");
    h.remove(header::REFERER);
    let base = format!("/w/{id}");
    let opts = proxy::Opts {
        auth: None,
        host: Some(local.clone()),
        inject: index.then_some(proxy::WEB_TAGS),
        rewrite_location: Some((vec![format!("http://{local}"), format!("http://localhost:{port}")], base)),
        terminal: false,
    };
    let mut r = proxy::forward(&app.http, req, port, opts).await;
    if r.status() == StatusCode::UNAUTHORIZED {
        // dsh 在终端里被重启过：Cookie 作废了。首页就重新载入一次，下次重新换
        forget(&id);
        if index {
            return axum::response::Redirect::to(&format!("/w/{id}/{query}")).into_response();
        }
    }
    // DSH 不会再发 Cookie（口令只在首页交换时用）；万一发了也不交给浏览器
    r.headers_mut().remove(header::SET_COOKIE);
    r
}
