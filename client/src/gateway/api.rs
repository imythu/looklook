//! 本机接口 `/api/*`（管理台使用）。写请求要求 `X-Looklook-Client: 1` 且 `Origin` 为本站，
//! 跨站页面既带不上自定义头（需要预检，本服务不响应 CORS），也伪造不了来源。

use axum::extract::{DefaultBodyLimit, Path, Query, Request, State};
use axum::http::{Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, patch, post, put};
use axum::{Extension, Json, Router};
use looklook_protocol::dto::{CreatePageShare, CreateTunnel, Tunnel, UpdateTunnel};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::access::{self, Peer};
use super::{origin_matches, Access, App, CLIENT_HEADER};
use crate::account::VERSION;
use crate::error::LocalError;
use crate::instances::{log_tail, Backend, Input, Instances};
use crate::settings::Settings;
use crate::ssh_transfer as xfer;
use crate::tools;
use crate::transfer;

type R<T> = Result<T, LocalError>;

pub fn routes(app: App) -> Router<App> {
    Router::new()
        .route("/status", get(status))
        // 远程打开时顶栏测往返时间（浏览器 → 中转 → 这台电脑）
        .route("/ping", get(|| async { StatusCode::NO_CONTENT }))
        .route("/auth/login", post(login))
        .route("/auth/login/cancel", post(login_cancel))
        .route("/auth/status", get(login_status))
        .route("/auth/logout", post(logout))
        .route("/auth/reconnect", post(reconnect))
        .route("/server", put(set_server))
        .route("/account", get(account))
        .route("/rtc/devices", get(rtc_devices))
        .route("/rtc/local-only", put(put_local_only))
        .route("/rtc/paths", get(rtc_paths))
        .route("/rtc/net", get(crate::rtc::net::get_net).put(crate::rtc::net::put_net))
        .route("/rtc/net/detect", post(crate::rtc::net::post_detect))
        .route("/platform/{*rest}", get(platform_web).post(platform_web).put(platform_web).patch(platform_web).delete(platform_web))
        .route("/instances", get(list_instances).post(create_instance))
        .route("/instances/{id}", get(get_instance).patch(update_instance).delete(delete_instance))
        .route("/instances/{id}/start", post(start_instance))
        .route("/instances/{id}/stop", post(stop_instance))
        .route("/instances/{id}/restart", post(restart_instance))
        .route("/instances/{id}/logs", get(instance_logs))
        .route("/instances/{id}/text", get(instance_text))
        .route("/instances/{id}/files", get(instance_files))
        .route("/instances/{id}/uploads", post(create_upload))
        .route("/instances/{id}/download", get(download))
        .route("/instances/{id}/fs", get(instance_fs))
        .route("/instances/{id}/preview", get(preview))
        .route("/instances/{id}/transfer/finish", post(end_transfer))
        .route("/instances/{id}/transfer/cancel", post(end_transfer))
        // 只有上传块放宽请求体上限（一块 8 MiB），其他接口仍是默认的 2 MB
        .route("/uploads/{uid}", put(upload_chunk).layer(DefaultBodyLimit::max(transfer::BODY_LIMIT)).delete(cancel_upload))
        .route("/uploads/{uid}/finish", post(finish_upload))
        .route("/attachments", get(list_attachments).delete(clear_attachments))
        .route("/instances/{id}/attachments/{name}", get(get_attachment).delete(delete_attachment))
        .route("/instances/{id}/git", get(git_status))
        .route("/instances/{id}/git/diff", get(git_diff))
        .route("/instances/{id}/git/show", get(git_show))
        .route("/instances/{id}/page", post(instance_page))
        .route("/instances/{id}/chat", get(super::chat::snapshot))
        .route("/instances/{id}/chat/events", get(super::chat::events))
        .route("/instances/{id}/chat/history", get(super::chat::history))
        .route("/instances/{id}/chat/item", get(super::chat::item))
        .route("/instances/{id}/chat/image", get(super::chat::image))
        .route("/instances/{id}/chat/agent", get(super::chat::agent))
        .route("/instances/{id}/chat/commands", get(super::chat::commands))
        .route("/instances/{id}/chat/prompts", get(super::chat::prompts))
        .route("/instances/{id}/chat/input", post(super::chat::input))
        .route("/instances/{id}/chat/interrupt", post(super::chat::interrupt))
        .route("/instances/{id}/chat/permission", post(super::chat::permission))
        .route("/instances/{id}/chat/plan", post(super::chat::plan))
        .route("/instances/{id}/chat/relaunch", post(super::chat::relaunch))
        .route("/instances/{id}/chat/switch-profile", post(super::chat::switch_profile))
        .route("/instances/{id}/chat/codex-terminal", post(super::chat::codex_terminal))
        .route("/instances/{id}/chat/adopt", post(super::chat::adopt))
        .route("/instances/{id}/chat/setting", post(super::chat::setting))
        .route("/instances/{id}/chat/tasks", get(super::chat::tasks))
        .route("/instances/{id}/chat/models", get(super::chat::models))
        .route("/instances/{id}/chat/usage", get(super::chat::usage))
        .route("/instances/{id}/chat/files", get(super::chat::files))
        .route("/instances/{id}/chat/file", get(super::chat::file))
        .route("/instances/{id}/chat/artifacts", get(super::chat::artifacts))
        .route("/instances/{id}/chat/artifact", get(super::chat::artifact))
        .route("/instances/{id}/chat/changes", get(super::chat::changes))
        .route("/instances/{id}/chat/config", get(super::chat::config))
        .route("/instances/{id}/chat/task/output", get(super::chat::task_output))
        .route("/instances/{id}/chat/task/stop", post(super::chat::task_stop))
        .route("/instances/{id}/chat/dialog", post(super::chat::dialog))
        .route("/ssh/hosts", get(ssh_list).post(ssh_create))
        .route("/ssh/keys", get(ssh_keys))
        .route("/ssh/keys/info", post(ssh_key_info))
        .route("/ssh/keys/system", get(ssh_system_keys))
        .route("/ssh/hosts/{id}", put(ssh_update).delete(ssh_delete))
        .route("/ssh/hosts/{id}/open", post(ssh_open))
        .route("/ssh/hosts/{id}/test", post(ssh_test))
        .route("/ssh/hosts/{id}/ai", put(ssh_allow_ai))
        .route("/ssh/test", post(ssh_test_draft))
        // 从另一台电脑导入（docs/SSH_TRANSFER.md）：旧电脑这边
        .route("/ssh/transfer/list", post(xfer_list))
        .route("/ssh/transfer/start", post(xfer_start))
        .route("/ssh/transfer/reveal", post(xfer_reveal))
        .route("/ssh/transfer/fetch", post(xfer_fetch))
        .route("/ssh/transfer/cancel", post(xfer_cancel))
        .route("/ssh/transfer/pending", get(xfer_pending))
        .route("/ssh/transfer/settings", get(xfer_settings).put(put_xfer_settings))
        .route("/ssh/transfer/log", get(xfer_log))
        .route("/ssh/transfer/{id}/approve", post(xfer_approve))
        .route("/ssh/transfer/{id}/reject", post(xfer_reject))
        .route("/ssh/transfer/{id}/email-code", post(xfer_email_code))
        // 新电脑这边
        .route("/ssh/import/sources", get(import_sources))
        .route("/ssh/import/hosts", post(import_hosts))
        .route("/ssh/import/start", post(import_start))
        .route("/ssh/import/{id}", get(import_view))
        .route("/ssh/import/{id}/cancel", post(import_cancel))
        .route("/fs/list", get(fs_list))
        .route("/fs/download", get(fs_download))
        .route("/fs/preview", get(fs_preview))
        .route("/fs/mkdir", post(fs_mkdir))
        .route("/fs/rename", post(fs_rename))
        .route("/fs/delete", post(fs_delete))
        .route("/fs/uploads", post(fs_upload))
        .route("/relay/reconnect", post(relay_reconnect))
        .route("/relays", get(list_relays))
        .route("/relays/preference", put(set_relay))
        .route("/tunnels", get(list_tunnels).post(create_tunnel))
        .route("/tunnels/{id}", patch(update_tunnel).delete(delete_tunnel))
        .route("/tunnels/{id}/shares", get(list_page_shares).post(create_page_share))
        .route("/tunnels/{id}/shares/{share}", delete(delete_page_share))
        .route("/settings", get(get_settings).put(put_settings))
        .route("/sync", get(get_sync).put(put_sync))
        .route("/sync/exchange", post(sync_exchange))
        .route("/projects", get(get_projects).put(put_projects))
        .route("/git/remotes", post(git_remotes))
        .route("/history", get(list_history))
        .route("/history/marks", put(put_history_mark))
        .route("/history/folders", get(history_folders))
        .route("/docker/containers", get(docker_containers))
        .route("/docker/containers/{name}", get(docker_probe))
        .route("/docker/containers/{name}/fs", get(docker_fs))
        .route("/history/{agent}/{id}", get(history_preview))
        .route("/device/name", put(put_device_name))
        .route("/access", get(get_access).put(put_access))
        .route("/mcp", get(get_mcp).put(put_mcp))
        .route("/mcp/install", post(mcp_install))
        .route("/mcp/uninstall", post(mcp_uninstall))
        .route("/shells", get(get_shells))
        .route("/tools", get(get_tools))
        .route("/tools/install", post(install_tool))
        .route("/tools/install/{tool}", get(install_job))
        .route("/diag", get(diag_list))
        .route("/diag/clear", post(diag_clear))
        .route("/diag/ui-error", post(diag_ui_error))
        .route("/diag/bundle", get(diag_bundle))
        .route("/diag/bundle.txt", get(diag_bundle_text))
        .route("/diag/report", post(diag_report))
        .route("/promotions", get(promotions))
        .route("/model-providers", get(get_model_providers))
        .route("/model-providers/{agent}", put(put_model_provider).delete(delete_model_provider))
        .route("/claude-profiles", get(get_claude_profiles).post(add_claude_profile))
        .route("/claude-profiles/default", put(set_default_claude_profile))
        .route("/claude-profiles/{id}", put(update_claude_profile).delete(delete_claude_profile))
        .route("/claude-profiles/{id}/status", get(claude_profile_status))
        .route("/claude-profiles/{id}/usage", post(claude_profile_usage))
        .route("/claude-profiles/{id}/login", get(claude_login_state).post(login_claude_profile).delete(cancel_claude_login))
        .route("/claude-profiles/{id}/login/code", post(claude_login_code))
        .route("/metrics", get(metrics))
        .route("/update", get(update))
        .route("/update/install", post(update_install))
        .route("/update/dismiss", post(update_dismiss))
        .route("/restart", post(restart))
        .route("/update/seen", post(update_seen))
        .fallback(|| async { LocalError::not_found() })
        .layer(middleware::from_fn_with_state(app.clone(), guard))
        .with_state(app)
}

async fn guard(State(app): State<App>, req: Request, next: Next) -> Response {
    let access = req.extensions().get::<Access>().cloned().unwrap_or(Access::Local);
    if !matches!(*req.method(), Method::GET | Method::HEAD) {
        let marked = req.headers().get(CLIENT_HEADER).and_then(|v| v.to_str().ok()) == Some("1")
            // Keep existing local CLI callers working during an upgrade.
            || (access.is_local() && req.headers().get("x-ll-client").and_then(|v| v.to_str().ok()) == Some("1"));
        if !marked || !origin_matches(req.headers(), &access) {
            return LocalError::new("CSRF_FAILED").into_response();
        }
    }
    // The UI login screen alone cannot protect these APIs. Keep bootstrap,
    // status and local access configuration available before account login.
    let group = req.uri().path().trim_start_matches('/').split('/').next().unwrap_or("");
    if matches!(group, "instances" | "uploads" | "attachments" | "ssh" | "fs" | "settings" | "sync" | "tools" | "tunnels" | "metrics" | "model-providers" | "claude-profiles" | "platform") && app.account.session().is_none() {
        return LocalError::new("NOT_LOGGED_IN").into_response();
    }
    let mut r = next.run(req).await;
    r.headers_mut().insert(axum::http::header::CACHE_CONTROL, axum::http::HeaderValue::from_static("no-store"));
    r
}

pub(super) fn require_allowed(app: &App) -> R<()> {
    let gate = app.account.gate();
    if gate.allowed() {
        Ok(())
    } else {
        Err(LocalError::locked(gate.reason()))
    }
}

fn require_local(access: &Access) -> R<()> {
    if access.is_local() {
        Ok(())
    } else {
        Err(LocalError::new("REMOTE_FORBIDDEN"))
    }
}

// ---------------- 状态与账户 ----------------

async fn status(State(app): State<App>, Extension(access): Extension<Access>) -> Json<Value> {
    Json(json!({
        "version": VERSION,
        "target": crate::paths::target(),
        "access": if access.is_local() { "local" } else { "remote" },
        "account": app.account.status(),
        "relay": crate::relay::status(),
        "capabilities": app.instances.capabilities(),
        "settings": Settings::load(&app.store),
        // 设置在电脑之间同步有没有打开（界面在同步项旁边标一下）
        "sync_enabled": crate::settings_sync::enabled(&app.store),
        "console": app.console_info(),
        "update": app.updater.view(),
        "metrics": app.metrics.brief(),
        "relay_check": app.relay_watch.view(),
        // `looklook restart` 靠它判断已经换成新进程
        "pid": access.is_local().then(std::process::id),
    }))
}

/// 发起浏览器授权登录：返回授权码与授权页地址，并在桌面环境自动打开浏览器；进度用 `GET /auth/status` 查询。
async fn login(State(app): State<App>, Extension(access): Extension<Access>) -> R<Json<Value>> {
    require_local(&access)?;
    let start = app.account.start_login().await?;
    if crate::util::has_desktop() {
        let url = start.verification_url.clone();
        // xdg-open 等可能阻塞到浏览器返回，放到阻塞线程里。
        tokio::task::spawn_blocking(move || crate::util::open_url(&url));
    }
    Ok(Json(json!(start)))
}

async fn login_status(State(app): State<App>) -> Json<Value> {
    Json(app.account.login_state())
}

async fn login_cancel(State(app): State<App>, Extension(access): Extension<Access>) -> R<Json<Value>> {
    require_local(&access)?;
    app.account.cancel_login();
    Ok(Json(app.account.login_state()))
}

#[derive(Deserialize, Default)]
struct LogoutReq {
    #[serde(default)]
    force: bool,
}

async fn logout(State(app): State<App>, body: Option<Json<LogoutReq>>) -> R<Json<Value>> {
    let force = body.map(|b| b.0.force).unwrap_or(false);
    app.account.logout(force).await?;
    Ok(Json(app.account.status()))
}

/// 管理台右上角的“立即重连”：重建远程访问通道（不影响本机、局域网打开）。
async fn relay_reconnect() -> StatusCode {
    crate::relay::request_restart();
    StatusCode::ACCEPTED
}

async fn reconnect(State(app): State<App>) -> Json<Value> {
    app.account.wake();
    Json(json!({}))
}

#[derive(Deserialize)]
struct ServerReq {
    url: String,
}

async fn set_server(State(app): State<App>, Extension(access): Extension<Access>, Json(req): Json<ServerReq>) -> R<Json<Value>> {
    require_local(&access)?;
    app.account.set_server(&req.url).map_err(|e| LocalError::new("SERVER_INVALID").with("detail", e.to_string()))?;
    Ok(Json(app.account.status()))
}

async fn account(State(app): State<App>) -> R<Json<Value>> {
    if app.account.session().is_none() {
        return Err(LocalError::new("NOT_LOGGED_IN"));
    }
    // 连不上平台时返回本机缓存的信息。
    let fresh = app.account.account().await.ok();
    let s = app.account.session().ok_or_else(|| LocalError::new("NOT_LOGGED_IN"))?;
    Ok(Json(json!({
        "user": s.user,
        "user_host_url": s.user_host_url,
        "device_id": s.device_id,
        "fresh": fresh.is_some(),
    })))
}

/// 同一账户的电脑（本机 App 的电脑切换与会话列表用，`rtc::outbound`）：只有本机打开的管理台才连得到其他电脑，
/// 其他方式打开时返回空列表，界面就按单台电脑显示。
/// “只看本机”时也返回空列表（`local_only: true`），不向平台取，也就不会连其他电脑。
async fn rtc_devices(State(app): State<App>, req: Request) -> R<Json<Value>> {
    if req.extensions().get::<Peer>() != Some(&Peer::Local) || app.account.session().is_none() {
        return Ok(Json(json!({ "items": [], "default": null, "max": 0 })));
    }
    if local_only(&app) {
        return Ok(Json(json!({ "items": [], "default": null, "max": 0, "local_only": true })));
    }
    let mut d = app.outbound.devices(&app.account, false).await?;
    // ICE 凭证只给本机用，不交给页面
    d.ice_servers.clear();
    let mut v = serde_json::to_value(d).unwrap_or_default();
    if let Some(o) = v.as_object_mut() {
        o.insert("local_only".into(), false.into());
    }
    Ok(Json(v))
}

/// 本机 App 连着的其他电脑走的路与往返时间（电脑菜单里的色点，`rtc::outbound`）。
async fn rtc_paths(State(app): State<App>, peer: Option<Extension<Peer>>) -> Json<Value> {
    if peer.map(|p| p.0) != Some(Peer::Local) {
        return Json(json!({ "items": {} }));
    }
    Json(json!({ "items": app.outbound.paths().await }))
}

const LOCAL_ONLY_KEY: &str = "local_only";

/// 本机 App 的“只看本机”：不显示同一账户的其他电脑，所有请求都留在本机（不经平台、不建 WebRTC）。
pub fn local_only(app: &App) -> bool {
    app.store.get::<bool>(LOCAL_ONLY_KEY).ok().flatten().unwrap_or(false)
}

#[derive(Deserialize)]
struct LocalOnlyReq {
    on: bool,
}

/// 只有这台电脑自己打开的管理台能改（局域网、远程打开时看不到其他电脑的列表，也就没有这个开关）。
async fn put_local_only(State(app): State<App>, peer: Option<Extension<Peer>>, Json(req): Json<LocalOnlyReq>) -> R<Json<Value>> {
    if peer.map(|p| p.0) != Some(Peer::Local) {
        return Err(LocalError::new("REMOTE_FORBIDDEN"));
    }
    app.store.set(LOCAL_ONLY_KEY, &req.on)?;
    Ok(Json(json!({ "local_only": req.on })))
}

/// 账户页面（`/api/platform/*`）：本机或局域网打开管理台时，由这台电脑签名转发到平台（服务端 `/api/client/v1/web/*`）。
/// 远程打开时管理台直接请求用户站的 `/_ll/web/*`，不经过这里。
async fn platform_web(State(app): State<App>, Path(rest): Path<String>, req: Request) -> Response {
    const LIMIT: usize = 64 * 1024;
    let method = req.method().as_str().to_string();
    let query = req.uri().query().map(|q| format!("?{q}")).unwrap_or_default();
    // 路径只允许平台接口用到的字符，免得拼出别的路径
    if !rest.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_/".contains(&b)) || rest.contains("//") {
        return LocalError::not_found().into_response();
    }
    let body = match axum::body::to_bytes(req.into_body(), LIMIT).await {
        Ok(b) if b.is_empty() => None,
        Ok(b) => Some(b.to_vec()),
        Err(_) => return LocalError::new("VALIDATION_FAILED").into_response(),
    };
    match app.account.web(&method, &format!("{rest}{query}"), body).await {
        Ok((204, _)) => StatusCode::NO_CONTENT.into_response(),
        Ok((status, bytes)) => (
            StatusCode::from_u16(status).unwrap_or(StatusCode::OK),
            [(axum::http::header::CONTENT_TYPE, "application/json")],
            bytes,
        )
            .into_response(),
        Err(e) => LocalError::from(e).into_response(),
    }
}

// ---------------- 终端 ----------------

async fn list_instances(State(app): State<App>) -> R<Json<Value>> {
    let items: Vec<Value> = app.instances.list().await?.into_iter().map(|v| with_chat(&app, v)).collect();
    Ok(Json(json!({ "items": items })))
}

/// 带对话界面的终端加上对话状态（会话卡片上的“等待确认”角标）
fn with_chat(app: &App, v: crate::instances::View) -> Value {
    let chat = v.chat_ui.then(|| app.chat.brief(&v.row.id)).flatten();
    let mut out = serde_json::to_value(v).unwrap_or_default();
    if let (Some(c), Some(o)) = (chat, out.as_object_mut()) {
        o.insert("chat".into(), c);
    }
    out
}

#[derive(Deserialize)]
struct CreateReq {
    #[serde(flatten)]
    input: Input,
    #[serde(default)]
    start: bool,
}

async fn create_instance(State(app): State<App>, Json(req): Json<CreateReq>) -> R<impl IntoResponse> {
    require_allowed(&app)?;
    let (prompt, resume) = (req.input.prompt.clone().map(|p| p.trim().to_string()).filter(|p| !p.is_empty()), req.input.resume.clone());
    let v = if req.input.launch.as_deref() == Some("dsh") { create_dsh(&app, req.input).await? } else { app.instances.create(req.input).await? };
    // 对话模式的 Codex：第一句话、接着以前的对话都交给 app-server（终端里只是 shell）
    if app.instances.codex_app_mode(&v.row) && (prompt.is_some() || resume.is_some()) {
        let (chat, id) = (app.chat.clone(), v.row.id.clone());
        tokio::spawn(async move { chat.codex_first(&id, prompt, resume).await });
    }
    let v = if req.start { app.instances.start(&v.row.id).await? } else { v };
    Ok((StatusCode::CREATED, Json(v)))
}

// ---------------- DSH（网页界面） ----------------
//
// DSH 不是终端界面：`dsh web` 在 tmux 里常驻，只监听 127.0.0.1 的一个端口，再给这个端口建一个本机网页映射。
// 打开 DSH 就是打开这个映射地址——在本机、局域网、外面都是同一个地址。一台电脑只有一个 DSH，
// 一个进程管理所有项目文件夹（在网页里添加）。

/// 新建 DSH 时建的那条本机网页映射：`instance_page.{实例ID}`
#[derive(Serialize, Deserialize)]
struct DshPage {
    tunnel_id: String,
}

fn page_key(id: &str) -> String {
    format!("instance_page.{id}")
}

fn require_persistent(app: &App) -> R<()> {
    // 直接运行模式下命令只在有人打开终端时运行，网页界面没法常驻
    match app.instances.backend {
        Backend::Direct => Err(LocalError::new("DSH_NEEDS_PERSISTENT")),
        Backend::Tmux(_) => Ok(()),
    }
}

async fn create_dsh(app: &App, mut input: Input) -> R<crate::instances::View> {
    require_persistent(app)?;
    if app.instances.dsh()?.is_some() {
        return Err(LocalError::new("DSH_EXISTS"));
    }
    let port = Instances::pick_web_port()?;
    check_tunnel_port(app, Some(port.into()))?;
    let tunnel = app.account.create_tunnel(&CreateTunnel { name: "DSH".into(), target_port: port.into() }).await?;
    input.web_port = Some(port);
    let v = match app.instances.create(input).await {
        Ok(v) => v,
        Err(e) => {
            let _ = app.account.delete_tunnel(&tunnel.id).await;
            return Err(e);
        }
    };
    app.store.set(&page_key(&v.row.id), &DshPage { tunnel_id: tunnel.id })?;
    Ok(v)
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct PageQ {
    /// 桌面应用：由看看用系统浏览器打开（带着口令），不在应用窗口里打开
    open: bool,
}

/// 打开 DSH：确保它在运行、映射还在（被删了就重建，被关了就打开），返回带口令的映射地址。
/// `?open=1` 且是本机桌面时直接用系统浏览器打开，返回 `opened`。
async fn instance_page(State(app): State<App>, Extension(access): Extension<Access>, Path(id): Path<String>, Query(q): Query<PageQ>) -> R<Json<Value>> {
    require_allowed(&app)?;
    require_persistent(&app)?;
    let v = app.instances.get(&id).await?;
    let port = v.web_port.ok_or_else(LocalError::not_found)?;
    // 已经在运行时什么也不做；任务退出了（例如 dsh 崩了）会重新运行
    if !v.running || v.task_alive != Some(true) {
        app.instances.start(&id).await?;
    }
    // 映射（要问平台）和口令（等 dsh 启动完）互不依赖，一起等
    let (tunnel, token) = tokio::join!(dsh_tunnel(&app, &id, port), dsh_token(&app, &id, port));
    let tunnel = tunnel?;
    let url = match &token {
        Some(t) => with_query(&tunnel.url, "token", t),
        None => tunnel.url.clone(),
    };
    // 没拿到口令时不打开：打开了也只是 DSH 的“需要认证”页
    let opened = q.open && token.is_some() && access.is_local() && crate::util::has_desktop() && {
        let u = url.clone();
        tokio::task::spawn_blocking(move || crate::util::open_url(&u)).await.unwrap_or(false)
    };
    Ok(Json(json!({ "url": url, "ready": token.is_some(), "opened": opened })))
}

/// DSH 的本机网页映射：被删了就重建，被关了或端口不对就改回来。
async fn dsh_tunnel(app: &App, id: &str, port: u16) -> R<Tunnel> {
    let saved: Option<DshPage> = app.store.get(&page_key(id))?;
    let existing = match saved {
        Some(p) => app.account.tunnels().await?.items.into_iter().find(|t| t.id == p.tunnel_id),
        None => None,
    };
    Ok(match existing {
        Some(t) if t.status == "enabled" && t.target_port == i32::from(port) => t,
        Some(t) => {
            let req = UpdateTunnel { name: None, target_port: Some(port.into()), status: Some("enabled".into()) };
            app.account.update_tunnel(&t.id, &req).await?
        }
        None => {
            let t = app.account.create_tunnel(&CreateTunnel { name: "DSH".into(), target_port: port.into() }).await?;
            app.store.set(&page_key(id), &DshPage { tunnel_id: t.id.clone() })?;
            t
        }
    })
}

/// 这次运行的 `dsh web` 打印的启动口令（`dsh web: http://127.0.0.1:P/?token=…`）。浏览器带着它打开一次，
/// DSH 就发一个 30 天的 Cookie（按 Host 区分；本机网页映射把 Host 改成 localhost:P，所以本机、远程是同一个）。
/// 口令每次启动都换，而且 dsh 要等插件全部加载完才打印（第一次运行要初始化配置，会慢很多）。
/// 输出里可能还留着上次运行的那一行，所以拿到口令后先向 dsh 验证一次，通过了才用；最多等 60 秒。
async fn dsh_token(app: &App, id: &str, port: u16) -> Option<String> {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        if let Some(t) = app.instances.full_output(id).await.as_deref().and_then(parse_dsh_token) {
            if dsh_token_works(port, &t).await {
                return Some(t);
            }
        }
        if tokio::time::Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    }
}

/// 口令换 Cookie：有效时 dsh 返回 303，无效 401，还没监听时连不上。
async fn dsh_token_works(port: u16, token: &str) -> bool {
    let Ok(client) = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).timeout(std::time::Duration::from_secs(3)).no_proxy().build() else {
        return false;
    };
    let url = format!("http://127.0.0.1:{port}/?token={token}");
    matches!(client.get(url).send().await, Ok(r) if r.status() == StatusCode::SEE_OTHER)
}

/// 输出里最后一个 `dsh web:` 行的口令。
fn parse_dsh_token(text: &str) -> Option<String> {
    text.lines().rev().filter(|l| l.contains("dsh web:")).find_map(|l| {
        let t: String = l.split("token=").nth(1)?.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').collect();
        (t.len() >= 16).then_some(t)
    })
}

fn with_query(url: &str, key: &str, value: &str) -> String {
    let (base, frag) = url.split_once('#').map_or((url, None), |(b, f)| (b, Some(f)));
    let sep = if base.contains('?') { '&' } else { '?' };
    let base = if base.contains('?') || base.ends_with('/') || base.matches('/').count() > 2 { base.to_string() } else { format!("{base}/") };
    let mut out = format!("{base}{sep}{key}={value}");
    if let Some(f) = frag {
        out.push('#');
        out.push_str(f);
    }
    out
}

async fn get_instance(State(app): State<App>, Path(id): Path<String>) -> R<impl IntoResponse> {
    Ok(Json(with_chat(&app, app.instances.get(&id).await?)))
}

async fn update_instance(State(app): State<App>, Path(id): Path<String>, Json(input): Json<Input>) -> R<impl IntoResponse> {
    Ok(Json(app.instances.update(&id, input).await?))
}

async fn delete_instance(State(app): State<App>, Path(id): Path<String>) -> R<StatusCode> {
    let page: Option<DshPage> = app.store.get(&page_key(&id))?;
    app.instances.delete(&id).await?;
    app.chat.forget(&id);
    if let Some(p) = page {
        // 映射跟着删；平台上已经没有（用户自己删了）或暂时连不上都不影响删除终端
        if let Err(e) = app.account.delete_tunnel(&p.tunnel_id).await {
            tracing::warn!(error = %e, "删除 DSH 的网页映射失败");
        }
        let _ = app.store.remove(&page_key(&id));
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn start_instance(State(app): State<App>, Path(id): Path<String>) -> R<impl IntoResponse> {
    require_allowed(&app)?;
    Ok(Json(app.instances.start(&id).await?))
}

async fn stop_instance(State(app): State<App>, Path(id): Path<String>) -> R<impl IntoResponse> {
    app.instances.stop(&id).await?;
    Ok(Json(app.instances.get(&id).await?))
}

async fn restart_instance(State(app): State<App>, Path(id): Path<String>) -> R<impl IntoResponse> {
    require_allowed(&app)?;
    Ok(Json(app.instances.restart(&id).await?))
}

async fn instance_logs(State(app): State<App>, Path(id): Path<String>) -> R<Json<Value>> {
    let v = app.instances.get(&id).await?;
    let output = app.instances.recent_output(&id).await;
    Ok(Json(json!({
        // 终端里最近的真实输出（仅 tmux 后端）；为 null 表示没有可读的会话
        "output": output,
        // 终端服务（ttyd）的启动与运行记录
        "text": log_tail(&app.paths.instance_log(&id)),
        "running": v.running,
        "task_alive": v.task_alive,
        "started_at": v.started_at,
        "last_activity_at": v.last_activity_at,
        "error": v.error,
    })))
}

/// 终端里的文字（“选择文字”看法：手机长按、鼠标拖选都能用浏览器自己的选择）。null：不是 tmux 会话，由页面读终端自己的缓冲区。
async fn instance_text(State(app): State<App>, Path(id): Path<String>) -> R<Json<Value>> {
    app.instances.get(&id).await?;
    Ok(Json(json!({ "text": app.instances.text(&id).await })))
}

// ---------------- 终端文件传输（docs/FILE_TRANSFER.md §4） ----------------

/// 终端当前目录、附件目录，以及面板里有没有本机的 rz / sz。
async fn instance_files(State(app): State<App>, Path(id): Path<String>) -> R<Json<transfer::FilesInfo>> {
    Ok(Json(transfer::files_info(&app.instances, &app.paths, &id).await?))
}

#[derive(Deserialize)]
struct UploadReq {
    /// 原文件名；剪贴板截图可以为空
    #[serde(default)]
    name: String,
    size: u64,
    /// `cwd` | `attach` | `rz`
    dest: String,
}

async fn create_upload(State(app): State<App>, Path(id): Path<String>, Json(req): Json<UploadReq>) -> R<impl IntoResponse> {
    require_allowed(&app)?;
    let dest = transfer::Dest::parse(&req.dest).ok_or_else(|| LocalError::invalid("dest", "enum"))?;
    let c = app.transfers.create(&app.instances, &id, &req.name, req.size, dest).await?;
    Ok((StatusCode::CREATED, Json(c)))
}

#[derive(Deserialize)]
struct OffsetQ {
    offset: u64,
}

/// 一块原始字节；`offset` 必须等于已收字节数，否则 409 `UPLOAD_OFFSET {received}`，前端从 `received` 续传。
async fn upload_chunk(State(app): State<App>, Path(uid): Path<String>, Query(q): Query<OffsetQ>, body: axum::body::Bytes) -> R<Json<Value>> {
    let received = app.transfers.write(&uid, q.offset, body).await?;
    Ok(Json(json!({ "received": received })))
}

async fn finish_upload(State(app): State<App>, Path(uid): Path<String>) -> R<Json<Value>> {
    Ok(Json(json!({ "path": app.transfers.finish(&uid).await? })))
}

async fn cancel_upload(State(app): State<App>, Path(uid): Path<String>) -> StatusCode {
    app.transfers.cancel(&uid).await;
    StatusCode::NO_CONTENT
}

#[derive(Deserialize)]
struct PathQ {
    path: String,
}

/// 能用管理台的人本来就有完整的 shell，不另加路径限制（§4.3）。
async fn download(State(app): State<App>, Path(id): Path<String>, Query(q): Query<PathQ>) -> R<Response> {
    app.instances.get(&id).await?;
    transfer::download(&q.path).await
}

/// 结束本机的 rz / sz（传完、或用户关掉面板）。
async fn end_transfer(State(app): State<App>, Path(id): Path<String>) -> R<StatusCode> {
    transfer::end_transfer(&app.instances, &id).await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------- 附件（粘贴的截图、“发给 AI”的文件，docs/FILE_TRANSFER.md §4.4） ----------------

#[derive(Deserialize)]
struct AttachQ {
    #[serde(default)]
    instance: Option<String>,
}

/// 附件列表（新的在前）、附件根目录和保留天数；`?instance=` 只列一个终端的。
async fn list_attachments(State(app): State<App>, Query(q): Query<AttachQ>) -> R<Json<Value>> {
    let paths = app.paths.clone();
    let inst = q.instance.filter(|s| !s.is_empty());
    let items = tokio::task::spawn_blocking(move || transfer::list_attachments(&paths, inst.as_deref())).await.unwrap_or_default();
    Ok(Json(json!({
        "dir": app.paths.uploads().display().to_string(),
        "days": Settings::load(&app.store).normalized().attach_days,
        "items": items,
    })))
}

async fn clear_attachments(State(app): State<App>, Query(q): Query<AttachQ>) -> R<Json<Value>> {
    let paths = app.paths.clone();
    let inst = q.instance.filter(|s| !s.is_empty());
    let removed = tokio::task::spawn_blocking(move || transfer::clear_attachments(&paths, inst.as_deref())).await.unwrap_or(0);
    Ok(Json(json!({ "removed": removed })))
}

async fn get_attachment(State(app): State<App>, Path((inst, name)): Path<(String, String)>) -> R<Response> {
    transfer::serve_attachment(&app.paths, &inst, &name).await
}

async fn delete_attachment(State(app): State<App>, Path((inst, name)): Path<(String, String)>) -> R<StatusCode> {
    transfer::delete_attachment(&app.paths, &inst, &name)?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------- 文件夹浏览（新建终端时选工作目录） ----------------

#[derive(Deserialize)]
struct FsQ {
    #[serde(default)]
    path: String,
    #[serde(default, deserialize_with = "de_flag")]
    hidden: bool,
    /// 同时列出文件（终端页的“文件”标签）；默认只列子目录（选工作目录）
    #[serde(default, deserialize_with = "de_flag")]
    files: bool,
}

/// 查询串布尔值：接受 `1/0`、`true/false`、`yes/no`、`on/off`，空值视为 false
fn de_flag<'de, D: serde::Deserializer<'de>>(d: D) -> Result<bool, D::Error> {
    let s = String::deserialize(d)?;
    match s.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "" | "0" | "false" | "no" | "off" => Ok(false),
        other => Err(serde::de::Error::custom(format!("invalid boolean: {other}"))),
    }
}

#[derive(serde::Serialize)]
struct FsEntry {
    name: String,
    is_dir: bool,
    /// 以下只在 `files=1` 时给出
    #[serde(skip_serializing_if = "Option::is_none")]
    size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mtime_ms: Option<i64>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    link: bool,
}

#[derive(serde::Serialize)]
struct FsList {
    path: String,
    parent: Option<String>,
    entries: Vec<FsEntry>,
    /// 请求的路径不存在时，返回的是最近的已存在上级目录
    exists: bool,
    /// 没有权限读取这个文件夹（或其上级不让进入）：`entries` 为空，界面按 `os` 给出对应的授权说明
    denied: bool,
    /// 本机系统（`windows` / `macos` / `linux` …），界面据此提示怎么授权
    os: &'static str,
    /// macOS 上被拒时附带本程序路径，方便用户在“完全磁盘访问权限”里手动添加
    #[serde(skip_serializing_if = "Option::is_none")]
    app_path: Option<String>,
    /// 顶层位置：Windows 是各个盘符（`C:\`、`D:\` …），其他系统只有 `/`。界面在 Windows 上据此显示“此电脑”
    roots: Vec<String>,
    /// 用户主目录（“主目录”快捷按钮）
    home: Option<String>,
}

/// `GET /api/fs/list?path=<dir>&hidden=0|1`：只列出子目录。路径为空取用户主目录；
/// 路径不存在时退到最近的已存在上级目录（`exists=false`），方便边输入边浏览。
/// 没有权限时不报错，而是返回 `denied=true` 的空列表，面包屑和“上一级”照常可用。
async fn fs_list(Query(q): Query<FsQ>) -> R<Json<FsList>> {
    // 读目录是阻塞调用；macOS 上首次访问受保护文件夹还会等用户在系统弹窗里点选，不能占着异步线程。
    tokio::task::spawn_blocking(move || list_dirs(&q))
        .await
        .map_err(|e| LocalError::from(anyhow::anyhow!(e)))?
        .map(Json)
}

fn list_dirs(q: &FsQ) -> Result<FsList, LocalError> {
    use std::io::ErrorKind;
    let raw = q.path.trim();
    let requested = if raw.is_empty() || raw == "~" {
        dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from("/"))
    } else if let Some(rest) = raw.strip_prefix("~/").or_else(|| raw.strip_prefix("~\\")) {
        dirs::home_dir().unwrap_or_default().join(rest)
    } else if raw.len() == 2 && raw.ends_with(':') && cfg!(windows) {
        // 只输入盘符（`D:`）是“D 盘当前目录”的意思，这里按盘的根目录理解
        std::path::PathBuf::from(format!("{raw}\\"))
    } else {
        std::path::PathBuf::from(raw)
    };
    if !requested.is_absolute() {
        return Err(LocalError::new("WORKDIR_NOT_FOUND").with("path", raw));
    }
    // 往上找最近的已存在目录。上级不让进入（stat 报权限错误）时也只能往上退，但要记下来：
    // 这时不能说“路径不存在、会自动创建”，而是没有权限。
    let mut dir = requested.clone();
    let mut denied = false;
    loop {
        match std::fs::metadata(&dir) {
            Ok(m) if m.is_dir() => break,
            Err(e) if e.kind() == ErrorKind::PermissionDenied => denied = true,
            _ => {}
        }
        if !dir.pop() {
            return Err(LocalError::new("WORKDIR_NOT_FOUND").with("path", raw));
        }
    }
    let exists = dir == requested;
    let mut entries = Vec::new();
    match std::fs::read_dir(&dir) {
        Ok(rd) => {
            entries = rd
                .filter_map(|e| e.ok())
                .filter_map(|e| {
                    let name = e.file_name().to_string_lossy().into_owned();
                    // 跟随符号链接判断是否目录；目标读不到属性（受保护位置）时按目录项本身的类型算
                    let meta = std::fs::metadata(e.path()).ok();
                    let is_dir = match &meta {
                        Some(m) => m.is_dir(),
                        None => e.file_type().map(|t| t.is_dir()).unwrap_or(false),
                    };
                    if !(is_dir || q.files) || !(q.hidden || !is_hidden(&e, &name)) {
                        return None;
                    }
                    let (size, mtime_ms, link) = if q.files {
                        let mtime = meta.as_ref().and_then(|m| m.modified().ok()).and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok());
                        let link = e.file_type().is_ok_and(|t| t.is_symlink());
                        (meta.as_ref().filter(|m| m.is_file()).map(|m| m.len()), mtime.map(|d| d.as_millis() as i64), link)
                    } else {
                        (None, None, false)
                    };
                    Some(FsEntry { name, is_dir, size, mtime_ms, link })
                })
                .collect();
            // 文件夹在前（选工作目录时本来就只有文件夹）
            entries.sort_by_key(|e| (!e.is_dir, e.name.to_lowercase()));
            entries.truncate(if q.files { 5000 } else { 2000 });
        }
        // Linux/macOS 的 EACCES、EPERM（含 macOS 隐私保护拒绝）和 Windows 的“拒绝访问”都归到这里
        Err(e) if e.kind() == ErrorKind::PermissionDenied => denied = true,
        Err(e) => {
            tracing::debug!(path = %dir.display(), error = %e, "读取文件夹失败");
            return Err(LocalError::new("WORKDIR_NOT_FOUND").with("path", dir.display().to_string()));
        }
    }
    let app_path = (denied && cfg!(target_os = "macos"))
        .then(|| std::env::current_exe().ok().map(|p| p.display().to_string()))
        .flatten();
    Ok(FsList {
        path: dir.display().to_string(),
        parent: dir.parent().map(|p| p.display().to_string()),
        entries,
        exists,
        denied,
        os: std::env::consts::OS,
        app_path,
        roots: roots(),
        home: dirs::home_dir().map(|p| p.display().to_string()),
    })
}

/// `GET /api/instances/{id}/fs?path=&hidden=`：终端页“文件”标签，列出文件夹和文件；路径为空时从终端的当前目录开始。
/// 挂在终端下面是为了多设备时按终端路由到它所在的电脑；能用管理台的人本来就有完整的 shell，不另加路径限制。
async fn instance_fs(State(app): State<App>, Path(id): Path<String>, Query(mut q): Query<FsQ>) -> R<Json<FsList>> {
    if q.path.trim().is_empty() {
        q.path = transfer::files_info(&app.instances, &app.paths, &id).await?.cwd;
    } else {
        app.instances.get(&id).await?;
    }
    q.files = true;
    fs_list(Query(q)).await
}

/// `GET /api/instances/{id}/preview?path=`：预览文件的前 [`transfer::PREVIEW_MAX`] 字节。
/// 只按扩展名给出几种位图类型，其余一律 `text/plain`，并加 `sandbox` CSP：用户目录里的 HTML/SVG 不能在管理台的源上执行。
async fn preview(State(app): State<App>, Path(id): Path<String>, Query(q): Query<PathQ>) -> R<Response> {
    app.instances.get(&id).await?;
    transfer::preview(&q.path).await
}

// ---------------- 终端页的 “Git” 标签（git.rs） ----------------

#[derive(Deserialize)]
struct GitQ {
    /// 仓库里的任意目录；空 = 终端当前目录
    #[serde(default)]
    path: String,
    #[serde(default)]
    file: String,
    #[serde(default, deserialize_with = "de_flag")]
    untracked: bool,
    #[serde(default)]
    commit: String,
}

async fn git_dir(app: &App, id: &str, path: &str) -> R<std::path::PathBuf> {
    if path.trim().is_empty() {
        Ok(transfer::files_info(&app.instances, &app.paths, id).await?.cwd.into())
    } else {
        app.instances.get(id).await?;
        Ok(path.trim().into())
    }
}

async fn git_status(State(app): State<App>, Path(id): Path<String>, Query(q): Query<GitQ>) -> R<Json<crate::git::Status>> {
    let dir = git_dir(&app, &id, &q.path).await?;
    Ok(Json(blocking(move || crate::git::status(&dir)).await?))
}

async fn git_diff(State(app): State<App>, Path(id): Path<String>, Query(q): Query<GitQ>) -> R<Json<crate::git::Diff>> {
    let dir = git_dir(&app, &id, &q.path).await?;
    Ok(Json(blocking(move || crate::git::diff_file(&dir, &q.file, q.untracked)).await?))
}

async fn git_show(State(app): State<App>, Path(id): Path<String>, Query(q): Query<GitQ>) -> R<Json<crate::git::Diff>> {
    let dir = git_dir(&app, &id, &q.path).await?;
    Ok(Json(blocking(move || crate::git::show(&dir, &q.commit)).await?))
}

// ---------------- SSH 主机（ssh.rs） ----------------

async fn ssh_list(State(app): State<App>) -> Json<Value> {
    let mcp = super::mcp::McpConfig::load(&app.store);
    // AI 助手能不能真用上“允许 AI 使用”的主机：MCP 接口要打开，SSH 工具也没被关掉
    Json(json!({ "items": crate::ssh::views(&app.store), "ssh": crate::shells::which("ssh").is_some(), "ai": { "mcp": mcp.enabled, "ssh": !mcp.ssh_disabled } }))
}

async fn ssh_create(State(app): State<App>, Json(req): Json<crate::ssh::HostInput>) -> R<impl IntoResponse> {
    let h = crate::ssh::save(&app.store, &app.paths.home, None, req)?;
    Ok((StatusCode::CREATED, Json(h)))
}

async fn ssh_update(State(app): State<App>, Path(id): Path<String>, Json(req): Json<crate::ssh::HostInput>) -> R<Json<crate::ssh::Saved>> {
    Ok(Json(crate::ssh::save(&app.store, &app.paths.home, Some(&id), req)?))
}

// ---------------- SSH 主机：从另一台电脑导入（ssh_transfer.rs，docs/SSH_TRANSFER.md） ----------------

/// 旧电脑这边给新电脑调的接口：只接受经 WebRTC 的请求，中转 HTTP 回退（中转看得到内容）一律拒绝。
fn rtc_only(access: &Access) -> R<()> {
    match access {
        Access::Rtc { .. } => Ok(()),
        _ => Err(LocalError::new("SSH_TRANSFER_NEED_RTC")),
    }
}

/// 确认 / 拒绝导出：本机界面总可以；经 WebRTC 的远程会话要这台电脑打开了“允许远程确认”。返回是不是远程。
fn approver(app: &App, access: &Access) -> R<bool> {
    match access {
        Access::Local => Ok(false),
        Access::Rtc { .. } if xfer::Config::load(&app.store).allow_remote_approve => Ok(true),
        Access::Rtc { .. } => Err(LocalError::new("SSH_TRANSFER_LOCAL_APPROVE")),
        Access::Remote { .. } => Err(LocalError::new("SSH_TRANSFER_NEED_RTC")),
    }
}

/// 新电脑这边要连另一台电脑：“只看本机”时不连。
fn can_dial(app: &App) -> R<()> {
    require_allowed(app)?;
    if local_only(app) {
        return Err(LocalError::new("SSH_TRANSFER_LOCAL_ONLY"));
    }
    Ok(())
}

async fn xfer_list(State(app): State<App>, Extension(access): Extension<Access>) -> R<Json<Value>> {
    rtc_only(&access)?;
    require_allowed(&app)?;
    Ok(Json(xfer::list(&app).await?))
}

async fn xfer_start(State(app): State<App>, Extension(access): Extension<Access>, Json(req): Json<xfer::StartReq>) -> R<Json<xfer::StartResp>> {
    rtc_only(&access)?;
    require_allowed(&app)?;
    Ok(Json(xfer::start(&app, req).await?))
}

async fn xfer_reveal(State(app): State<App>, Extension(access): Extension<Access>, Json(req): Json<xfer::RevealReq>) -> R<StatusCode> {
    rtc_only(&access)?;
    xfer::reveal(&app, req)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn xfer_fetch(State(app): State<App>, Extension(access): Extension<Access>, Json(req): Json<xfer::IdReq>) -> R<Json<xfer::FetchResp>> {
    rtc_only(&access)?;
    Ok(Json(xfer::fetch(&app, req)))
}

async fn xfer_cancel(State(app): State<App>, Extension(access): Extension<Access>, Json(req): Json<xfer::IdReq>) -> R<StatusCode> {
    rtc_only(&access)?;
    xfer::cancel_export(&app, req).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn xfer_pending(State(app): State<App>, Extension(access): Extension<Access>) -> R<Json<Value>> {
    let remote = approver(&app, &access)?;
    let mut v = xfer::pending(&app);
    v["remote"] = remote.into();
    Ok(Json(v))
}

#[derive(Deserialize)]
struct ApproveReq {
    code: String,
    #[serde(default)]
    email_code: Option<String>,
}

async fn xfer_approve(State(app): State<App>, Extension(access): Extension<Access>, Path(id): Path<String>, Json(req): Json<ApproveReq>) -> R<Json<Value>> {
    let remote = approver(&app, &access)?;
    Ok(Json(xfer::approve(&app, &id, &req.code, req.email_code.as_deref(), remote).await?))
}

async fn xfer_reject(State(app): State<App>, Extension(access): Extension<Access>, Path(id): Path<String>) -> R<StatusCode> {
    approver(&app, &access)?;
    xfer::reject(&app, &id)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn xfer_email_code(State(app): State<App>, Extension(access): Extension<Access>, Path(id): Path<String>) -> R<StatusCode> {
    if !approver(&app, &access)? {
        // 本机确认不需要邮箱验证码
        return Err(LocalError::new("SSH_TRANSFER_STATE"));
    }
    xfer::email_code(&app, &id).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn xfer_settings(State(app): State<App>) -> Json<xfer::Config> {
    Json(xfer::Config::load(&app.store))
}

/// “允许远程确认 SSH 导出”只能在这台电脑的本机界面打开，远程会话打不开它（§4.3）。
async fn put_xfer_settings(State(app): State<App>, Extension(access): Extension<Access>, Json(req): Json<xfer::Config>) -> R<Json<xfer::Config>> {
    require_local(&access)?;
    req.save(&app.store)?;
    Ok(Json(req))
}

async fn xfer_log(State(app): State<App>) -> Json<Value> {
    Json(json!({ "items": xfer::log(&app.store) }))
}

async fn import_sources(State(app): State<App>) -> R<Json<Value>> {
    can_dial(&app)?;
    Ok(Json(xfer::sources(&app).await?))
}

#[derive(Deserialize)]
struct ImportFrom {
    from: String,
}

async fn import_hosts(State(app): State<App>, Json(req): Json<ImportFrom>) -> R<Json<Value>> {
    can_dial(&app)?;
    Ok(Json(xfer::remote_hosts(&app, &req.from).await?))
}

async fn import_start(State(app): State<App>, Json(req): Json<xfer::ImportReq>) -> R<Json<Value>> {
    can_dial(&app)?;
    Ok(Json(xfer::begin(&app, req).await?))
}

async fn import_view(State(app): State<App>, Path(id): Path<String>) -> R<Json<Value>> {
    xfer::view(&app, &id).map(Json).ok_or_else(LocalError::not_found)
}

async fn import_cancel(State(app): State<App>, Path(id): Path<String>) -> R<StatusCode> {
    xfer::cancel_import(&app, &id)?;
    Ok(StatusCode::NO_CONTENT)
}

/// 这台电脑 `~/.ssh` 里的私钥文件（路径），添加主机时快捷选择。
async fn ssh_keys() -> Json<Value> {
    Json(json!({ "items": tokio::task::spawn_blocking(crate::ssh::local_keys).await.unwrap_or_default() }))
}

#[derive(serde::Deserialize)]
struct KeyInfoReq {
    /// 这台电脑上的私钥文件
    #[serde(default)]
    path: Option<String>,
    /// 浏览器里选的 / 粘贴的私钥内容
    #[serde(default)]
    text: Option<String>,
    /// 已保存主机上传的私钥
    #[serde(default)]
    host: Option<String>,
}

/// `POST /api/ssh/keys/info`：私钥的脱敏概况（算法、指纹、首尾几个字符），让用户确认选对了钥匙。
async fn ssh_key_info(State(app): State<App>, Json(req): Json<KeyInfoReq>) -> R<Json<crate::ssh::KeyInfo>> {
    let info = tokio::task::spawn_blocking(move || match (req.path, req.text, req.host) {
        (_, Some(text), _) => crate::ssh::key_info(&text),
        (_, _, Some(id)) => crate::ssh::stored_key_info(&app.store, &app.paths.home, &id),
        (Some(path), _, _) => crate::ssh::key_info_at(&path),
        _ => Err(LocalError::invalid("key_path", "required")),
    })
    .await
    .map_err(|e| LocalError::from(anyhow::anyhow!(e)))??;
    Ok(Json(info))
}

/// `GET /api/ssh/keys/system`：“系统密钥”登录时 ssh 会用的钥匙（ssh-agent 里的和 `~/.ssh` 的默认私钥），脱敏显示。
async fn ssh_system_keys() -> Json<crate::ssh::SystemKeys> {
    Json(tokio::task::spawn_blocking(crate::ssh::system_keys).await.unwrap_or(crate::ssh::SystemKeys { agent: None, files: Vec::new() }))
}

async fn ssh_delete(State(app): State<App>, Path(id): Path<String>) -> R<StatusCode> {
    crate::ssh::delete(&app.store, &id)?;
    Ok(StatusCode::NO_CONTENT)
}

/// 在 shell 里运行 `looklook ssh <id>` 的那一行：程序路径按 shell 的写法加引号（Windows 路径常带空格）。
fn ssh_launch_line(exe: &str, id: &str, family: crate::shells::Family) -> String {
    use crate::shells::Family;
    let plain = !exe.chars().any(|c| c.is_whitespace() || "'\"$`&()|;<>".contains(c));
    match family {
        _ if plain => format!("{exe} ssh {id}"),
        Family::Pwsh => format!("& '{}' ssh {id}", exe.replace('\'', "''")),
        Family::Cmd => format!("\"{exe}\" ssh {id}"),
        _ => format!("'{}' ssh {id}", exe.replace('\'', "'\\''")),
    }
}

/// 新建一个 SSH 终端并启动：终端里的 shell 运行 `looklook ssh <id>`，ssh 退出后回到 shell（按 ↑ 回车即可重连）。
async fn ssh_open(State(app): State<App>, Path(id): Path<String>) -> R<impl IntoResponse> {
    require_allowed(&app)?;
    let h = crate::ssh::list(&app.store).into_iter().find(|h| h.id == id).ok_or_else(LocalError::not_found)?;
    // 稳定入口（Linux 的 bin/looklook）：命令留在终端里、shell 历史里，版本目录被清理后按 ↑ 回车也要能重连
    let exe = crate::versions::stable_exe(&app.paths.home).ok_or_else(|| LocalError::new("SSH_FAILED").with("detail", "exe"))?.display().to_string();
    let family = crate::shells::resolve("", &Settings::load(&app.store).default_shell)?.family;
    let input = Input {
        name: Some(format!("SSH · {}", h.name)),
        launch: Some("custom".into()),
        command: Some(ssh_launch_line(&exe, &h.id, family)),
        workdir: dirs::home_dir().map(|p| p.display().to_string()),
        ..Default::default()
    };
    let v = app.instances.create(input).await?;
    let v = app.instances.start(&v.row.id).await?;
    Ok((StatusCode::CREATED, Json(v)))
}

/// 测试连接：在主机上运行一条无害的命令。结果（含卡在哪一步、失败原因和完整日志）放在应答里。
async fn ssh_test(State(app): State<App>, Path(id): Path<String>) -> R<Json<crate::ssh::TestReport>> {
    let h = crate::ssh::list(&app.store).into_iter().find(|h| h.id == id).ok_or_else(LocalError::not_found)?;
    Ok(Json(crate::ssh::test(app.store.clone(), app.paths.home.clone(), h).await?))
}

#[derive(Deserialize)]
struct SshDraftReq {
    /// 修改已有主机时它的 id（留空的密码、已上传的私钥沿用它的）
    #[serde(default)]
    id: Option<String>,
    host: crate::ssh::HostInput,
}

/// 添加 / 修改主机时，保存之前先测试表单里填的。
async fn ssh_test_draft(State(app): State<App>, Json(req): Json<SshDraftReq>) -> R<Json<crate::ssh::TestReport>> {
    let id = req.id.filter(|s| !s.is_empty());
    Ok(Json(crate::ssh::test_draft(&app.store, &app.paths.home, id, req.host).await?))
}

#[derive(Deserialize)]
struct AllowAiReq {
    allow: bool,
}

async fn ssh_allow_ai(State(app): State<App>, Path(id): Path<String>, Json(req): Json<AllowAiReq>) -> R<Json<crate::ssh::HostView>> {
    Ok(Json(crate::ssh::set_allow_ai(&app.store, &id, req.allow)?))
}

// ---------------- 管理台“文件”页（不属于某个终端；多台电脑时按 X-LL-Device 发给选中的那台） ----------------

async fn fs_download(Query(q): Query<PathQ>) -> R<Response> {
    transfer::download(&q.path).await
}

/// 同 [`preview`]：只给几种位图类型，其余是加了 `sandbox` CSP 的纯文本。
async fn fs_preview(Query(q): Query<PathQ>) -> R<Response> {
    transfer::preview(&q.path).await
}

#[derive(Deserialize)]
struct FsNameReq {
    /// mkdir：所在文件夹；rename / delete：要改的文件或文件夹
    path: String,
    #[serde(default)]
    name: String,
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> R<T> + Send + 'static) -> R<T> {
    tokio::task::spawn_blocking(f).await.map_err(|e| LocalError::from(anyhow::anyhow!(e)))?
}

async fn fs_mkdir(Json(req): Json<FsNameReq>) -> R<Json<Value>> {
    let path = blocking(move || crate::fsops::mkdir(&req.path, &req.name)).await?;
    Ok(Json(json!({ "path": path })))
}

async fn fs_rename(Json(req): Json<FsNameReq>) -> R<Json<Value>> {
    let path = blocking(move || crate::fsops::rename(&req.path, &req.name)).await?;
    Ok(Json(json!({ "path": path })))
}

async fn fs_delete(Json(req): Json<FsNameReq>) -> R<StatusCode> {
    tracing::info!(path = %req.path, "管理台删除文件");
    blocking(move || crate::fsops::remove(&req.path)).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct FsUploadReq {
    dir: String,
    #[serde(default)]
    name: String,
    size: u64,
}

/// 上传到指定文件夹；之后的分块、完成、取消与终端上传相同（`/api/uploads/{uid}…`）。
async fn fs_upload(State(app): State<App>, Json(req): Json<FsUploadReq>) -> R<impl IntoResponse> {
    require_allowed(&app)?;
    let c = app.transfers.create_in_dir(&req.dir, &req.name, req.size)?;
    Ok((StatusCode::CREATED, Json(c)))
}

/// 顶层位置。Windows 用 GetLogicalDrives 的位图列出盘符：不访问磁盘，没插卡的读卡器、断开的网络盘也不会卡住。
fn roots() -> Vec<String> {
    #[cfg(windows)]
    {
        #[link(name = "kernel32")]
        extern "system" {
            fn GetLogicalDrives() -> u32;
        }
        let mask = unsafe { GetLogicalDrives() };
        drive_roots(mask)
    }
    #[cfg(not(windows))]
    {
        vec!["/".into()]
    }
}

/// 盘符位图（bit 0 = A:）→ `["C:\\", "D:\\"]`
#[cfg_attr(not(windows), allow(dead_code))]
fn drive_roots(mask: u32) -> Vec<String> {
    (0..26u8).filter(|i| mask & (1 << i) != 0).map(|i| format!("{}:\\", (b'A' + i) as char)).collect()
}

/// 隐藏文件夹：各系统都认 `.` 开头；Windows 另看“隐藏/系统”属性（如用户目录里的
/// `Application Data`、`My Documents` 等兼容性链接，点进去必然拒绝访问）；macOS 另看 `chflags hidden`（如 `~/Library`）。
fn is_hidden(e: &std::fs::DirEntry, name: &str) -> bool {
    if name.starts_with('.') {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const HIDDEN: u32 = 0x2;
        const SYSTEM: u32 = 0x4;
        // DirEntry::metadata 不跟随链接，取的是链接本身的属性
        if e.metadata().is_ok_and(|m| m.file_attributes() & (HIDDEN | SYSTEM) != 0) {
            return true;
        }
    }
    #[cfg(target_os = "macos")]
    {
        use std::os::macos::fs::MetadataExt;
        const UF_HIDDEN: u32 = 0x8000;
        if e.metadata().is_ok_and(|m| m.st_flags() & UF_HIDDEN != 0) {
            return true;
        }
    }
    let _ = e;
    false
}

// ---------------- 本机网页（隧道，登记在平台） ----------------

// ---------------- 线路（中转） ----------------

/// 可选的线路，以及从这台电脑到每条线路的延迟（并发测，最多几秒）。结果也交给后台测速，更新提示。
async fn list_relays(State(app): State<App>) -> R<Json<Value>> {
    let c = crate::relay_watch::measure(&app.account).await?;
    app.relay_watch.record(c.clone());
    Ok(Json(json!({ "items": c.items, "preferred": c.preferred, "current": c.current, "auto": c.auto, "best": c.best })))
}

#[derive(Deserialize)]
struct RelayPreference {
    relay: Option<String>,
}

async fn set_relay(State(app): State<App>, Json(req): Json<RelayPreference>) -> R<Json<Value>> {
    let r = app.account.set_relay(req.relay, false).await?;
    app.relay_watch.changed();
    Ok(Json(json!({ "preferred": r.preferred, "current": r.current })))
}

async fn list_tunnels(State(app): State<App>) -> R<Json<Value>> {
    let list = app.account.tunnels().await?;
    Ok(Json(serde_json::to_value(list).unwrap_or_default()))
}

async fn create_tunnel(State(app): State<App>, Json(req): Json<CreateTunnel>) -> R<impl IntoResponse> {
    require_allowed(&app)?;
    check_tunnel_port(&app, Some(req.target_port))?;
    Ok((StatusCode::CREATED, Json(app.account.create_tunnel(&req).await?)))
}

async fn update_tunnel(State(app): State<App>, Path(id): Path<String>, Json(req): Json<UpdateTunnel>) -> R<impl IntoResponse> {
    check_tunnel_port(&app, req.target_port)?;
    Ok(Json(app.account.update_tunnel(&id, &req).await?))
}

async fn delete_tunnel(State(app): State<App>, Path(id): Path<String>) -> R<StatusCode> {
    app.account.delete_tunnel(&id).await?;
    Ok(StatusCode::NO_CONTENT)
}

// 本机网页的临时分享：转给平台（平台只允许分享这台设备自己的网页）。
async fn list_page_shares(State(app): State<App>, Path(id): Path<String>) -> R<Json<Value>> {
    let list = app.account.page_shares(&id).await?;
    Ok(Json(serde_json::to_value(list).unwrap_or_default()))
}

async fn create_page_share(State(app): State<App>, Path(id): Path<String>, Json(req): Json<CreatePageShare>) -> R<impl IntoResponse> {
    require_allowed(&app)?;
    Ok((StatusCode::CREATED, Json(app.account.create_page_share(&id, &req).await?)))
}

async fn delete_page_share(State(app): State<App>, Path((id, share)): Path<(String, String)>) -> R<StatusCode> {
    app.account.delete_page_share(&id, &share).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// 平台只校验 1–65535；本机再拒绝看看自己在用的端口。
pub(super) fn check_tunnel_port(app: &App, port: Option<i64>) -> R<()> {
    let Some(p) = port else { return Ok(()) };
    let reserved = u16::try_from(p)
        .map(|p| p == app.ui_addr.port() || p == app.relay_addr.port() || crate::instances::Instances::is_reserved_port(p))
        .unwrap_or(false);
    if reserved {
        return Err(LocalError::new("PORT_RESERVED").with("port", p));
    }
    Ok(())
}

// ---------------- 设置 ----------------

// ---------------- 项目（会话页侧栏） ----------------
//
// 项目就是这台电脑上的一个文件夹，外加在这里新建会话时上次选的类型和“高级”选项。存在这台电脑上，
// 换浏览器、换手机打开都一样。还有会话的文件夹不用记，界面会从会话列表里推出来。

const PROJECTS_KEY: &str = "projects";
const PROJECTS_MAX: usize = 200;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct Project {
    workdir: String,
    /// 上次在这个项目里新建的类型（项目上的 + 用它）
    #[serde(default)]
    launch: String,
    #[serde(default)]
    shell: String,
    /// 在这里新建时选过的全部权限；没选过（只用项目上的 + 建过）为 None，界面按会话或默认设置决定
    #[serde(default, skip_serializing_if = "Option::is_none")]
    full_access: Option<bool>,
    #[serde(default)]
    auto_start: bool,
    /// 上次在这里新建会话的时间（RFC 3339，界面用来排“最近用过的”）
    #[serde(default)]
    used_at: String,
    /// 侧栏里置顶
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pinned: bool,
}

#[derive(Debug, Serialize, Deserialize)]
struct Projects {
    items: Vec<Project>,
}

fn normalize_projects(items: Vec<Project>) -> R<Vec<Project>> {
    let mut out: Vec<Project> = vec![];
    for mut p in items {
        p.workdir = p.workdir.trim().to_string();
        if p.workdir.is_empty() || p.workdir.chars().count() > 1024 {
            return Err(LocalError::invalid("workdir", "invalid"));
        }
        if !matches!(p.launch.as_str(), "" | "shell" | "claude" | "codex" | "opencode" | "dsh" | "custom") {
            p.launch.clear();
        }
        // 同一个文件夹只留第一条（最近用的排前面）
        if !out.iter().any(|q| q.workdir == p.workdir) {
            out.push(p);
        }
    }
    out.truncate(PROJECTS_MAX);
    Ok(out)
}

async fn get_projects(State(app): State<App>) -> R<Json<Projects>> {
    let items = app.store.get::<Vec<Project>>(PROJECTS_KEY)?.unwrap_or_default();
    Ok(Json(Projects { items }))
}

async fn put_projects(State(app): State<App>, Json(req): Json<Projects>) -> R<Json<Projects>> {
    let items = normalize_projects(req.items)?;
    app.store.set(PROJECTS_KEY, &items)?;
    Ok(Json(Projects { items }))
}

#[derive(Deserialize)]
struct RemotesReq {
    dirs: Vec<String>,
}

/// 一批文件夹各自所在仓库的远程地址（规范后的 `host/path`）：多台电脑上的同一个仓库在侧栏里合成一个项目。
/// 只读 `.git/config`；不是仓库、没有远程的不出现在结果里。
async fn git_remotes(Json(req): Json<RemotesReq>) -> R<Json<Value>> {
    if req.dirs.len() > PROJECTS_MAX * 2 {
        return Err(LocalError::invalid("dirs", "too_long"));
    }
    let map = blocking(move || {
        Ok(req
            .dirs
            .into_iter()
            .filter(|d| !d.trim().is_empty() && std::path::Path::new(d).is_absolute())
            .filter_map(|d| crate::git::remote(std::path::Path::new(&d)).map(|r| (d, r)))
            .collect::<std::collections::HashMap<String, String>>())
    })
    .await?;
    Ok(Json(json!({ "remotes": map })))
}

// ---------------- 以前的对话（history.rs） ----------------

/// 以前的对话。正开在某个看看会话里的标上 `instance_id`（界面上直接打开它，不再开第二个）：
/// 按这个会话接着的对话号，Claude Code 对话界面还按 hooks 记下的当前会话号。
async fn list_history(State(app): State<App>, Query(q): Query<crate::history::Query>) -> R<Json<Value>> {
    let mut items = crate::history::list(q).await;
    let open = open_convs(&app, &app.instances.list().await?);
    for c in &mut items {
        c.instance_id = open.get(&c.id).cloned();
    }
    let marks = app.store.get::<HistoryMarks>(HISTORY_MARKS_KEY).ok().flatten().unwrap_or_default();
    Ok(Json(json!({ "items": items, "marks": marks, "persistent": matches!(app.instances.backend, Backend::Tmux(_)) })))
}

/// 正开在看看会话里的对话：对话号 → 会话 ID。按这个会话接着的对话号、Claude Code 对话界面 hooks 记下的当前会话号，
/// 还有从记录里认出来的（[`crate::instances::View::conv_id`]）。
fn open_convs(app: &App, views: &[crate::instances::View]) -> std::collections::HashMap<String, String> {
    let mut open = std::collections::HashMap::new();
    for v in views {
        if !v.running || v.task_alive == Some(false) {
            continue;
        }
        let id = &v.row.id;
        if let Ok(Some(conv)) = app.store.get::<String>(&crate::instances::resume_key(id)) {
            open.insert(conv, id.clone());
        }
        if let Some(b) = crate::chat::binding(&app.store, id) {
            open.insert(b.session_id, id.clone());
        }
        if let Some(c) = &v.conv_id {
            open.insert(c.clone(), id.clone());
        }
    }
    open
}

#[derive(Deserialize)]
struct FoldersQuery {
    /// 每个文件夹带最近几个对话（最多 20）
    per: Option<usize>,
    /// `projects`：只要记着的项目和会话所在的文件夹（侧栏）；不给时是所有有过对话的文件夹（添加项目时挑）
    scope: Option<String>,
}

/// 以前的对话按文件夹分组：各工具的数量和最近几个。隐藏了的不算。
async fn history_folders(State(app): State<App>, Query(q): Query<FoldersQuery>) -> R<Json<Value>> {
    let marks = app.store.get::<HistoryMarks>(HISTORY_MARKS_KEY)?.unwrap_or_default();
    let mut items = crate::history::folders(q.per.unwrap_or(5).min(20), move |c| !marks.get(&format!("{}:{}", c.agent, c.id)).is_some_and(|m| m.hide)).await;
    let views = app.instances.list().await?;
    if q.scope.as_deref() == Some("projects") {
        let stored = app.store.get::<Vec<Project>>(PROJECTS_KEY)?.unwrap_or_default();
        let dirs: std::collections::HashSet<String> = stored.iter().map(|p| p.workdir.as_str()).chain(views.iter().map(|v| v.row.workdir.as_str())).map(crate::history::dir_key).collect();
        items.retain(|f| dirs.contains(&crate::history::dir_key(&f.workdir)));
    }
    let open = open_convs(&app, &views);
    for c in items.iter_mut().flat_map(|f| f.items.iter_mut()) {
        c.instance_id = open.get(&c.id).cloned();
    }
    Ok(Json(json!({ "items": items, "persistent": matches!(app.instances.backend, Backend::Tmux(_)) })))
}

/// 以前的对话上的标记（收藏、隐藏），存在这台电脑上：`工具:会话号` → 标记。
const HISTORY_MARKS_KEY: &str = "history_marks";
type HistoryMarks = std::collections::BTreeMap<String, HistoryMark>;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct HistoryMark {
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    star: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    hide: bool,
}

#[derive(Deserialize)]
struct HistoryMarkReq {
    agent: String,
    id: String,
    star: Option<bool>,
    hide: Option<bool>,
}

async fn put_history_mark(State(app): State<App>, Json(req): Json<HistoryMarkReq>) -> R<Json<Value>> {
    if !crate::history::AGENTS.contains(&req.agent.as_str()) || !crate::history::valid_id(&req.id) {
        return Err(LocalError::invalid("id", "invalid"));
    }
    let mut marks = app.store.get::<HistoryMarks>(HISTORY_MARKS_KEY)?.unwrap_or_default();
    let key = format!("{}:{}", req.agent, req.id);
    let mut m = marks.remove(&key).unwrap_or_default();
    if let Some(v) = req.star {
        m.star = v;
    }
    if let Some(v) = req.hide {
        m.hide = v;
    }
    if m.star || m.hide {
        marks.insert(key, m);
    }
    // 只留最近的一些（不会无限变大）
    while marks.len() > 5000 {
        let first = marks.keys().next().cloned();
        if let Some(k) = first {
            marks.remove(&k);
        }
    }
    app.store.set(HISTORY_MARKS_KEY, &marks)?;
    Ok(Json(json!({ "marks": marks })))
}

// ---------------- Docker 容器（docker.rs） ----------------

/// 运行中的容器。没装 docker、docker 没在运行时返回空列表和原因（界面上提示）。
async fn docker_containers() -> Json<Value> {
    match crate::docker::list().await {
        Ok(items) => Json(json!({ "items": items })),
        Err(e) => Json(json!({ "items": [], "error": e.code })),
    }
}

/// `GET /api/docker/containers/{name}/fs?path=&hidden=`：容器里的子文件夹，字段同 `/fs/list`。
async fn docker_fs(Path(name): Path<String>, Query(q): Query<FsQ>) -> R<Json<crate::docker::DirList>> {
    Ok(Json(crate::docker::list_dirs(&name, &q.path, q.hidden).await?))
}

/// 容器里的默认用户、目录和装了的 AI 工具（开始页选了容器后显示）。
async fn docker_probe(Path(name): Path<String>) -> R<Json<crate::docker::Probe>> {
    Ok(Json(crate::docker::probe(&name).await?))
}

#[derive(Deserialize)]
struct PreviewQuery {
    #[serde(default)]
    full: bool,
}

async fn history_preview(Path((agent, id)): Path<(String, String)>, Query(q): Query<PreviewQuery>) -> R<Json<Value>> {
    let msgs = crate::history::preview(agent, id, q.full).await.ok_or_else(LocalError::not_found)?;
    Ok(Json(json!({ "items": msgs })))
}

async fn get_settings(State(app): State<App>) -> Json<Settings> {
    Json(Settings::load(&app.store))
}

/// 只需带要改的字段，其余沿用现在的值（界面各处只发自己改的那几项，不会把别的电脑刚同步来的值用旧值盖回去）。
async fn put_settings(State(app): State<App>, Json(patch): Json<serde_json::Map<String, Value>>) -> R<Json<Settings>> {
    let mut cur = serde_json::to_value(Settings::load(&app.store)).map_err(anyhow::Error::from)?;
    for (k, v) in patch {
        if cur.get(&k).is_some() {
            cur[&k] = v;
        }
    }
    let s: Settings = serde_json::from_value(cur).map_err(|_| LocalError::invalid("settings", "format"))?;
    let s = s.normalized();
    crate::shells::validate(&s.default_shell)?;
    match std::fs::metadata(&s.default_workdir) {
        Ok(m) if m.is_dir() => {}
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            return Err(LocalError::new("WORKDIR_DENIED").with("path", s.default_workdir));
        }
        _ => return Err(LocalError::new("WORKDIR_NOT_FOUND").with("path", s.default_workdir)),
    }
    if app.account.save_settings(&s).await? {
        app.sync.poke();
    }
    Ok(Json(s))
}

// ---------------- 设置在电脑之间同步（settings_sync.rs，docs/SETTINGS_SYNC.md） ----------------

async fn get_sync(State(app): State<App>) -> Json<Value> {
    Json(app.sync.view(&app).await)
}

#[derive(Deserialize)]
struct SyncReq {
    enabled: bool,
}

async fn put_sync(State(app): State<App>, Json(req): Json<SyncReq>) -> R<Json<Value>> {
    crate::settings_sync::set_enabled(&app.store, req.enabled)?;
    if req.enabled {
        app.sync.poke();
    }
    Ok(Json(app.sync.view(&app).await))
}

/// 另一台电脑来对账：只接受经 WebRTC 的请求，请求还要带那台电脑的设备签名（浏览器签不出来）。
async fn sync_exchange(State(app): State<App>, Extension(access): Extension<Access>, Json(req): Json<crate::settings_sync::ExchangeReq>) -> R<Json<crate::settings_sync::ExchangeResp>> {
    if !matches!(access, Access::Rtc { .. }) {
        return Err(LocalError::new("SYNC_NEED_RTC"));
    }
    require_allowed(&app)?;
    Ok(Json(crate::settings_sync::exchange(&app, req).await?))
}

#[derive(Deserialize)]
struct DeviceName {
    name: String,
}

/// 只改电脑名称（远程打开时在电脑列表里改名用；为空 = 用主机名）。
async fn put_device_name(State(app): State<App>, Json(req): Json<DeviceName>) -> R<Json<Value>> {
    let s = Settings { device_name: req.name, ..Settings::load(&app.store) }.normalized();
    app.account.save_settings(&s).await?;
    Ok(Json(json!({ "name": s.device_name(), "custom": !s.device_name.is_empty() })))
}

// ---------------- 访问控制 ----------------

fn access_view(app: &App, peer: Option<Peer>) -> Value {
    let c = app.access();
    json!({
        "allow_lan": c.allow_lan,
        "allowed_ips": c.allowed_ips,
        // 白名单域名当前解析到的 IP（解析不出来是空列表）
        "resolved": app.resolved.view(&c),
        "code_enabled": c.code_enabled,
        "code": c.code,
        "open_host": c.open_host,
        "port": app.ui_addr.port(),
        "lan_ips": app.lan_ips.iter().map(|ip| ip.to_string()).collect::<Vec<_>>(),
        "peer": peer,
        // Windows 防火墙放行说明里要用到程序的完整路径
        "exe": std::env::current_exe().ok().map(|p| p.display().to_string()),
    })
}

/// 访问控制只能直接访问（本机、局域网、白名单）时查看和修改；通过专属地址远程访问时不行。
async fn get_access(State(app): State<App>, Extension(access): Extension<Access>, peer: Option<Extension<Peer>>) -> R<Json<Value>> {
    require_local(&access)?;
    Ok(Json(access_view(&app, peer.map(|p| p.0))))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct AccessReq {
    allow_lan: Option<bool>,
    /// 整个白名单（界面用）；命令行用 add_ip / remove_ip
    allowed_ips: Option<Vec<String>>,
    add_ip: Option<String>,
    remove_ip: Option<String>,
    code_enabled: Option<bool>,
    /// 换一个随机的新访问码
    new_code: bool,
    /// 自己设置访问码
    code: Option<String>,
    open_host: Option<String>,
}

async fn put_access(State(app): State<App>, Extension(access): Extension<Access>, peer: Option<Extension<Peer>>, Json(req): Json<AccessReq>) -> R<Json<Value>> {
    require_local(&access)?;
    let mut c = app.access();
    let net = |s: &str| access::parse_entry(s).map(|e| access::format_entry(&e)).map_err(|rule| LocalError::invalid("allowed_ips", rule).with("value", s));
    if let Some(v) = req.allow_lan {
        c.allow_lan = v;
    }
    if let Some(list) = req.allowed_ips {
        c.allowed_ips = list.iter().map(|s| net(s)).collect::<Result<_, _>>()?;
    }
    if let Some(ip) = req.add_ip {
        c.allowed_ips.push(net(&ip)?);
    }
    if let Some(ip) = req.remove_ip {
        let target = net(&ip)?;
        c.allowed_ips.retain(|x| *x != target);
    }
    let mut seen = std::collections::HashSet::new();
    c.allowed_ips.retain(|x| seen.insert(x.clone()));
    if c.allowed_ips.len() > access::MAX_ALLOWED {
        return Err(LocalError::invalid("allowed_ips", "ip_too_many"));
    }
    if let Some(v) = req.code_enabled {
        c.code_enabled = v;
    }
    if req.new_code {
        c.code = access::generate_code();
    }
    if let Some(code) = req.code {
        access::check_custom_code(&code).map_err(|rule| LocalError::invalid("code", rule))?;
        c.code = code.trim().to_string();
    }
    if let Some(h) = req.open_host {
        let parsed = crate::settings::parse_open_host(&h).map_err(|rule| LocalError::invalid("open_host", rule))?;
        c.open_host = if h.trim().is_empty() { String::new() } else { parsed.to_string() };
    }
    c.save(&app.store)?;
    let domains_changed = !c.domains().eq(app.access().domains());
    *app.access.write().unwrap_or_else(|e| e.into_inner()) = c;
    let _ = app.write_open_file();
    if domains_changed {
        // 马上解析新加的域名，界面能直接看到它解析到哪个 IP
        app.resolved.refresh(&app.access()).await;
    }
    Ok(Json(access_view(&app, peer.map(|p| p.0))))
}

// ---------------- AI 助手接入（MCP） ----------------

async fn mcp_view(app: &App) -> Value {
    let c = super::mcp::McpConfig::load(&app.store);
    let url = super::mcp::endpoint(app);
    let agents = crate::agents::status(app.instances.shell(), &url, &c.token).await;
    json!({ "enabled": c.enabled, "ssh": !c.ssh_disabled, "token": c.token, "url": url, "agents": agents, "tools": super::mcp::tool_names() })
}

/// 和访问控制一样，只能直接在这台机器（或允许的局域网）上查看和修改：令牌只在本机有用，装配置也是改本机文件。
async fn get_mcp(State(app): State<App>, Extension(access): Extension<Access>) -> R<Json<Value>> {
    require_local(&access)?;
    Ok(Json(mcp_view(&app).await))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct McpReq {
    enabled: Option<bool>,
    /// 让 AI 助手使用 SSH 主机（`list_ssh_hosts`、`ssh_run`）
    ssh: Option<bool>,
    /// 换一个新令牌；已经装好的助手配置一并更新
    rotate: bool,
}

async fn put_mcp(State(app): State<App>, Extension(access): Extension<Access>, Json(req): Json<McpReq>) -> R<Json<Value>> {
    require_local(&access)?;
    let mut c = super::mcp::McpConfig::load(&app.store);
    let old = c.token.clone();
    if let Some(v) = req.enabled {
        c.enabled = v;
    }
    if let Some(v) = req.ssh {
        c.ssh_disabled = !v;
    }
    if req.rotate || (c.enabled && c.token.is_empty()) {
        c.token = super::mcp::generate_token();
    }
    c.save(&app.store)?;
    if c.token != old && !old.is_empty() {
        // 原来装好的（地址对、令牌是旧的）跟着换；装过但本来就是旧配置的不动，界面会提示重装
        let url = super::mcp::endpoint(&app);
        for st in crate::agents::status(app.instances.shell(), &url, &old).await {
            if st.mcp == "ok" {
                let agent = crate::agents::Agent::parse(st.agent).expect("agent id");
                if let Err(e) = crate::agents::refresh(agent, app.instances.shell(), &url, &c.token).await {
                    tracing::warn!(agent = st.agent, code = %e.code, "换令牌后更新 AI 助手配置失败");
                }
            }
        }
    }
    Ok(Json(mcp_view(&app).await))
}

#[derive(Deserialize)]
struct AgentReq {
    agent: String,
    /// `auto` | `ask` | `none`
    #[serde(default)]
    skill: String,
}

async fn mcp_install(State(app): State<App>, Extension(access): Extension<Access>, Json(req): Json<AgentReq>) -> R<Json<Value>> {
    require_local(&access)?;
    let agent = crate::agents::Agent::parse(&req.agent).ok_or_else(|| LocalError::invalid("agent", "enum"))?;
    let skill = crate::agents::Skill::parse(&req.skill).ok_or_else(|| LocalError::invalid("skill", "enum"))?;
    let c = super::mcp::McpConfig::load(&app.store);
    if !c.enabled || c.token.is_empty() {
        return Err(LocalError::new("MCP_DISABLED"));
    }
    crate::agents::install(agent, app.instances.shell(), &super::mcp::endpoint(&app), &c.token, skill).await?;
    Ok(Json(mcp_view(&app).await))
}

async fn mcp_uninstall(State(app): State<App>, Extension(access): Extension<Access>, Json(req): Json<AgentReq>) -> R<Json<Value>> {
    require_local(&access)?;
    let agent = crate::agents::Agent::parse(&req.agent).ok_or_else(|| LocalError::invalid("agent", "enum"))?;
    crate::agents::uninstall(agent, app.instances.shell()).await?;
    Ok(Json(mcp_view(&app).await))
}

// ---------------- shell ----------------

/// 本机可用的 shell 与“自动”会选中的那个（新建终端、设置里选 shell 用）。
async fn get_shells(State(app): State<App>) -> Json<Value> {
    let default = Settings::load(&app.store).default_shell;
    let (items, auto) = tokio::task::spawn_blocking(|| (crate::shells::available(), crate::shells::auto())).await.unwrap_or_default();
    Json(json!({ "items": items, "auto": auto, "default": default, "os": std::env::consts::OS }))
}

// ---------------- AI 工具 ----------------

async fn get_tools(State(app): State<App>) -> Json<Value> {
    let mut d = tools::detect(app.instances.shell()).await;
    // 不在 PATH 里的 Claude Code（官方安装脚本的位置、Claude 桌面版自带的）
    if d.claude.is_none() {
        d.claude = crate::agents::claude_fallback();
    }
    Json(json!({ "tools": d }))
}

#[derive(Deserialize)]
struct InstallReq {
    tool: String,
    #[serde(default)]
    mirror: String,
}

/// 在后台装（不建终端）；界面用 `GET /tools/install/{tool}` 看进度。
async fn install_tool(State(app): State<App>, Json(req): Json<InstallReq>) -> R<impl IntoResponse> {
    require_allowed(&app)?;
    let job = tools::start_install(app.instances.shell(), &req.tool, &req.mirror).ok_or_else(|| LocalError::invalid("tool", "invalid"))?;
    Ok((StatusCode::ACCEPTED, Json(job)))
}

async fn install_job(Path(tool): Path<String>) -> R<Json<tools::Job>> {
    tools::job(&tool).map(Json).ok_or_else(LocalError::not_found)
}

// ---------------- 错误记录与问题报告 ----------------

async fn diag_list(State(app): State<App>) -> Json<Value> {
    Json(json!({ "errors": crate::diag::recent(), "can_report": app.account.session().is_some() }))
}

async fn diag_clear() -> Json<Value> {
    crate::diag::clear();
    Json(json!({ "errors": [] }))
}

#[derive(Deserialize)]
struct UiErrorReq {
    message: String,
    #[serde(default)]
    detail: String,
}

/// 管理台页面上的脚本错误，记进错误记录。
async fn diag_ui_error(Json(req): Json<UiErrorReq>) -> StatusCode {
    crate::diag::record_ui(&req.message, &req.detail);
    StatusCode::NO_CONTENT
}

async fn make_bundle(app: &App) -> crate::diag::Bundle {
    let instances = app.instances.list().await.unwrap_or_default();
    let meta = crate::diag::meta(app, &instances);
    let secrets = vec![app.access().code, super::mcp::McpConfig::load(&app.store).token];
    let logs = app.paths.home.join("logs");
    tokio::task::spawn_blocking(move || crate::diag::bundle(meta, &logs, &secrets)).await.unwrap_or_else(|_| crate::diag::Bundle { meta: json!({}), errors: String::new(), logs: String::new() })
}

/// 报告会附带的内容（发送前给用户预览）。
async fn diag_bundle(State(app): State<App>) -> Json<crate::diag::Bundle> {
    Json(make_bundle(&app).await)
}

/// 下载成文本文件（没登录或连不上服务器时，用户自己发给我们）。
async fn diag_bundle_text(State(app): State<App>) -> impl IntoResponse {
    let text = make_bundle(&app).await.text("");
    let name = format!("looklook-report-{}.txt", crate::util::now_rfc3339().replace([':', '.'], "-"));
    (
        [
            (axum::http::header::CONTENT_TYPE, "text/plain; charset=utf-8".to_string()),
            (axum::http::header::CONTENT_DISPOSITION, format!("attachment; filename=\"{name}\"")),
        ],
        text,
    )
}

#[derive(Deserialize)]
struct ReportReq {
    description: String,
    #[serde(default)]
    contact: String,
    /// 附带运行信息、错误记录和日志
    #[serde(default)]
    include: bool,
}

async fn diag_report(State(app): State<App>, Json(req): Json<ReportReq>) -> R<Json<Value>> {
    let description = req.description.trim();
    if description.is_empty() {
        return Err(LocalError::invalid("description", "required"));
    }
    let b = if req.include { make_bundle(&app).await } else { crate::diag::Bundle { meta: json!({ "version": VERSION, "target": crate::paths::target() }), errors: String::new(), logs: String::new() } };
    let body = json!({
        "description": description,
        "contact": req.contact.trim(),
        "client_version": VERSION,
        "target": crate::paths::target(),
        "meta": b.meta,
        "errors": b.errors,
        "logs": b.logs,
    });
    Ok(Json(app.account.report(&body).await?))
}

// ---------------- 平台公共信息 ----------------

#[derive(Deserialize)]
struct LocaleQ {
    #[serde(default)]
    locale: Option<String>,
}

async fn promotions(State(app): State<App>, Query(q): Query<LocaleQ>) -> Json<Value> {
    let locale = q.locale.filter(|l| l == "en-US").unwrap_or_else(|| "zh-CN".into());
    let r: Result<Value, _> = app.account.platform().public(&format!("/promotions?slot=client_home&locale={locale}")).await;
    Json(r.unwrap_or_else(|_| json!({ "items": [] })))
}

// ---------------- 模型服务商（第三方中转站，见 model.rs） ----------------

/// 平台推荐的服务商（含推广）；连不上平台时为空列表并带上错误码，已保存的选择照常显示。
async fn platform_providers(app: &App, locale: &str) -> Result<Vec<Value>, LocalError> {
    let r: Value = app.account.platform().public(&format!("/model-providers?locale={locale}")).await?;
    Ok(r.get("items").and_then(Value::as_array).cloned().unwrap_or_default())
}

#[derive(Deserialize)]
struct ModelQ {
    #[serde(default)]
    locale: Option<String>,
    /// 只要本机保存的选择，不取平台列表（新建终端表单里显示用）
    #[serde(default)]
    local: bool,
}

async fn get_model_providers(State(app): State<App>, Query(q): Query<ModelQ>) -> Json<Value> {
    let locale = q.locale.filter(|l| l == "en-US").unwrap_or_else(|| "zh-CN".into());
    let (providers, error) = match q.local {
        true => (vec![], None),
        false => match platform_providers(&app, &locale).await {
            Ok(p) => (p, None),
            Err(e) => (vec![], Some(e.code)),
        },
    };
    Json(model_view(&app, providers, error))
}

fn model_view(app: &App, providers: Vec<Value>, error: Option<String>) -> Value {
    use crate::model::{self, ModelAgent};
    // Claude Code 的第三方接口改在“Claude Code 账号”里管（claude_profiles.rs），这里只剩 Codex
    let agents: Vec<Value> = ModelAgent::ALL
        .iter()
        .filter(|a| **a != ModelAgent::Claude)
        .map(|a| {
            json!({
                "agent": a.id(),
                "client": a.platform_client(),
                "choice": model::choice(&app.store, *a),
                "key_set": model::key_set(&app.paths.home, *a),
            })
        })
        .collect();
    json!({
        "accepted_at": model::disclaimer_accepted(&app.store),
        "agents": agents,
        "providers": providers,
        "error": error,
    })
}

#[derive(Deserialize)]
struct ModelReq {
    /// 平台列表里的服务商；不填表示自填接口地址（`name` + `base_url`）
    #[serde(default)]
    provider_id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    base_url: Option<String>,
    /// 不填沿用已保存的 Key
    #[serde(default)]
    api_key: Option<String>,
    /// 同意免责声明（第一次保存时必须）
    #[serde(default)]
    accept: bool,
}

fn model_agent(s: &str) -> R<crate::model::ModelAgent> {
    crate::model::ModelAgent::parse(s).filter(|a| *a != crate::model::ModelAgent::Claude).ok_or_else(LocalError::not_found)
}

/// 平台推荐的服务商给某个客户端的接口地址（以平台下发的为准，不信任请求里的）：返回 (名字, 地址)
async fn platform_endpoint(app: &App, id: &str, client: &str) -> R<(String, String)> {
    let list = platform_providers(app, "zh-CN").await?;
    let p = list.iter().find(|p| p["id"].as_str() == Some(id)).ok_or_else(|| LocalError::new("MODEL_PROVIDER_GONE"))?;
    let base_url = p["clients"]
        .as_array()
        .and_then(|cs| cs.iter().find(|c| c["client"].as_str() == Some(client)))
        .and_then(|c| c["base_url"].as_str())
        .ok_or_else(|| LocalError::new("MODEL_PROVIDER_GONE"))?;
    Ok((p["name"].as_str().unwrap_or_default().into(), base_url.into()))
}

async fn put_model_provider(State(app): State<App>, Path(agent): Path<String>, Json(req): Json<ModelReq>) -> R<Json<Value>> {
    use crate::model::{self, Choice};
    let agent = model_agent(&agent)?;
    if req.accept {
        model::accept_disclaimer(&app.store)?;
    } else if model::disclaimer_accepted(&app.store).is_none() {
        return Err(LocalError::new("MODEL_DISCLAIMER_REQUIRED"));
    }
    let client = agent.platform_client();
    let choice = match req.provider_id.as_deref().filter(|s| !s.is_empty()) {
        // 接口地址以平台下发的为准，不信任请求里的
        Some(id) => {
            let (name, base_url) = platform_endpoint(&app, id, client).await?;
            Choice { provider_id: Some(id.into()), name, base_url, selected_at: crate::util::now_rfc3339() }
        }
        None => {
            let name: String = req.name.as_deref().unwrap_or("").trim().chars().filter(|c| !c.is_control()).take(40).collect();
            if name.is_empty() {
                return Err(LocalError::invalid("name", "required"));
            }
            let base_url = req.base_url.as_deref().unwrap_or("").trim().trim_end_matches('/').to_string();
            Choice { provider_id: None, name, base_url, selected_at: crate::util::now_rfc3339() }
        }
    };
    model::save(&app.store, &app.paths.home, agent, &choice, req.api_key.as_deref())?;
    // 统计选用人数；失败不影响使用
    if let Some(id) = choice.provider_id.clone() {
        let account = app.account.clone();
        tokio::spawn(async move {
            if let Err(e) = account.select_model_provider(&id, client).await {
                tracing::debug!(error = %e, "模型服务商选用上报失败");
            }
        });
    }
    Ok(Json(model_view(&app, vec![], None)))
}

async fn delete_model_provider(State(app): State<App>, Path(agent): Path<String>) -> R<Json<Value>> {
    crate::model::clear(&app.store, &app.paths.home, model_agent(&agent)?)?;
    Ok(Json(model_view(&app, vec![], None)))
}

// ---------------- Claude Code 账号切换（见 claude_profiles.rs） ----------------

fn claude_profiles_view(app: &App) -> Value {
    use crate::claude_profiles as cp;
    let profiles: Vec<Value> = cp::list(&app.store)
        .into_iter()
        .map(|p| {
            let mut v = json!(p);
            if p.kind == cp::Kind::Api {
                v["key_set"] = json!(cp::key_set(&app.paths.home, &p.id));
            }
            v
        })
        .collect();
    json!({
        "default": cp::default_id(&app.store),
        "profiles": profiles,
        "accepted_at": crate::model::disclaimer_accepted(&app.store),
    })
}

async fn get_claude_profiles(State(app): State<App>) -> Json<Value> {
    Json(claude_profiles_view(&app))
}

/// 登录状态（`claude auth status`，要一两秒）：界面每个账号单独问
async fn claude_profile_status(State(app): State<App>, Path(id): Path<String>) -> R<Json<Value>> {
    let p = crate::claude_profiles::get(&app.store, &id).ok_or_else(LocalError::not_found)?;
    Ok(Json(crate::claude_profiles::status(app.instances.shell(), &app.paths.home, &p).await))
}

/// 查官方账号的订阅用量（`claude -p /usage`，要几秒）；顺便给出登录状态，登录失效时界面提示重新登录
async fn claude_profile_usage(State(app): State<App>, Path(id): Path<String>) -> R<Json<Value>> {
    let p = crate::claude_profiles::get(&app.store, &id).ok_or_else(LocalError::not_found)?;
    if p.kind != crate::claude_profiles::Kind::Official {
        return Err(LocalError::not_found());
    }
    Ok(Json(crate::claude_profiles::usage(app.instances.shell(), &app.paths.home, &p).await))
}

#[derive(Deserialize)]
struct ClaudeProfileReq {
    #[serde(flatten)]
    input: crate::claude_profiles::Input,
    /// 同意免责声明（第一次添加第三方接口时必须）
    #[serde(default)]
    accept: bool,
}

/// 第三方接口：免责声明；选的是平台推荐的服务商时，地址和名字以平台下发的为准
async fn claude_api_input(app: &App, req: &mut ClaudeProfileReq) -> R<()> {
    if req.accept {
        crate::model::accept_disclaimer(&app.store)?;
    } else if crate::model::disclaimer_accepted(&app.store).is_none() {
        return Err(LocalError::new("MODEL_DISCLAIMER_REQUIRED"));
    }
    if let Some(id) = req.input.provider_id.clone().filter(|s| !s.is_empty()) {
        let (name, base_url) = platform_endpoint(app, &id, crate::model::ModelAgent::Claude.platform_client()).await?;
        req.input.base_url = Some(base_url);
        if req.input.name.as_deref().is_none_or(|n| n.trim().is_empty()) {
            req.input.name = Some(name);
        }
        // 统计选用人数；失败不影响使用
        let account = app.account.clone();
        tokio::spawn(async move {
            if let Err(e) = account.select_model_provider(&id, "claude_code").await {
                tracing::debug!(error = %e, "模型服务商选用上报失败");
            }
        });
    }
    Ok(())
}

async fn add_claude_profile(State(app): State<App>, Json(mut req): Json<ClaudeProfileReq>) -> R<Json<Value>> {
    if req.input.kind == Some(crate::claude_profiles::Kind::Api) {
        claude_api_input(&app, &mut req).await?;
    }
    let p = crate::claude_profiles::add(&app.store, &app.paths.home, req.input)?;
    let mut v = claude_profiles_view(&app);
    v["added"] = json!(p.id);
    Ok(Json(v))
}

async fn update_claude_profile(State(app): State<App>, Path(id): Path<String>, Json(mut req): Json<ClaudeProfileReq>) -> R<Json<Value>> {
    let p = crate::claude_profiles::get(&app.store, &id).ok_or_else(LocalError::not_found)?;
    if p.kind == crate::claude_profiles::Kind::Api && (req.input.base_url.is_some() || req.input.provider_id.is_some()) {
        claude_api_input(&app, &mut req).await?;
    }
    crate::claude_profiles::update(&app.store, &app.paths.home, &id, req.input)?;
    Ok(Json(claude_profiles_view(&app)))
}

/// 删掉。另外登录的官方账号先让 Claude Code 自己退出登录（吊销令牌、删 macOS 钥匙串条目），失败也接着删目录。
async fn delete_claude_profile(State(app): State<App>, Path(id): Path<String>) -> R<Json<Value>> {
    use crate::claude_profiles as cp;
    let p = cp::get(&app.store, &id).ok_or_else(LocalError::not_found)?;
    if p.id == cp::OFFICIAL {
        return Err(LocalError::invalid("id", "builtin"));
    }
    if p.kind == cp::Kind::Official {
        if let Some(cli) = crate::agents::claude_cli(app.instances.shell()).await {
            let dir = cp::account_dir(&app.paths.home, &id);
            let r = crate::agents::cli_output(app.instances.shell(), &cli, &["auth", "logout"], &[("CLAUDE_CONFIG_DIR", Some(dir.as_os_str()))], std::time::Duration::from_secs(30)).await;
            if let Err(e) = r {
                tracing::warn!(error = %e, profile = %id, "claude auth logout 失败");
            }
        }
    }
    cp::remove(&app.store, &app.paths.home, &id)?;
    Ok(Json(claude_profiles_view(&app)))
}

#[derive(Deserialize)]
struct DefaultReq {
    id: String,
}

async fn set_default_claude_profile(State(app): State<App>, Json(req): Json<DefaultReq>) -> R<Json<Value>> {
    crate::claude_profiles::set_default(&app.store, &req.id)?;
    Ok(Json(claude_profiles_view(&app)))
}

/// 登录官方账号：在后台运行 Claude Code 自己的 `claude auth login`（见 claude_profiles.rs 的“登录”），
/// 返回授权页地址；界面上用户去登录，再把授权码贴回来。
async fn login_claude_profile(State(app): State<App>, Path(id): Path<String>) -> R<Json<crate::claude_profiles::LoginView>> {
    let p = crate::claude_profiles::get(&app.store, &id).ok_or_else(LocalError::not_found)?;
    Ok(Json(crate::claude_profiles::login_start(app.instances.shell(), &app.paths.home, app.store.clone(), &p).await?))
}

async fn claude_login_state(Path(id): Path<String>) -> R<Json<crate::claude_profiles::LoginView>> {
    crate::claude_profiles::login_state(&id).map(Json).ok_or_else(LocalError::not_found)
}

#[derive(Deserialize)]
struct LoginCodeReq {
    code: String,
}

async fn claude_login_code(Path(id): Path<String>, Json(req): Json<LoginCodeReq>) -> R<Json<crate::claude_profiles::LoginView>> {
    Ok(Json(crate::claude_profiles::login_code(&id, &req.code).await?))
}

async fn cancel_claude_login(Path(id): Path<String>) -> StatusCode {
    crate::claude_profiles::login_cancel(&id);
    StatusCode::NO_CONTENT
}

/// 立即检查更新（“检查更新”按钮）。`locale` 决定更新说明的语言。
#[derive(Deserialize)]
struct RangeQ {
    #[serde(default)]
    range: String,
}

/// 系统页：CPU、内存、网络、磁盘与进程；`range` 为 `5m`（默认）、`1h`、`24h`。
async fn metrics(State(app): State<App>, Query(q): Query<RangeQ>) -> Json<Value> {
    Json(app.metrics.view(&q.range))
}

async fn update(State(app): State<App>, Query(q): Query<LocaleQ>) -> R<Json<Value>> {
    if let Some(l) = q.locale.as_deref() {
        app.updater.set_locale(l);
    }
    app.updater.check().await?;
    Ok(Json(app.updater.view()))
}

/// 下载并安装最新版本，完成后自动重启（进度见 `/api/status` 的 `update.job`）。
async fn update_install(State(app): State<App>) -> R<Json<Value>> {
    app.updater.start_install()?;
    Ok(Json(app.updater.view()))
}

#[derive(Deserialize)]
struct DismissReq {
    version: String,
}

/// 这个版本不再提醒（有更新的版本时再提醒）。
/// 看过“已更新到 x.y.z”的提示。
async fn update_seen(State(app): State<App>) -> R<Json<Value>> {
    app.updater.seen()?;
    Ok(Json(app.updater.view()))
}

/// 重启客户端（命令行 `looklook restart`）。
async fn restart(State(app): State<App>, Extension(access): Extension<Access>) -> R<Json<Value>> {
    require_local(&access)?;
    app.updater.restart()?;
    Ok(Json(json!({ "pid": std::process::id() })))
}

async fn update_dismiss(State(app): State<App>, Json(req): Json<DismissReq>) -> R<Json<Value>> {
    app.updater.dismiss(req.version.trim())?;
    Ok(Json(app.updater.view()))
}

#[cfg(test)]
mod tests {

    #[test]
    fn ssh_launch_line_quotes_paths_for_each_shell() {
        use crate::shells::Family;
        assert_eq!(super::ssh_launch_line("/usr/local/bin/looklook", "ab12", Family::Posix), "/usr/local/bin/looklook ssh ab12");
        assert_eq!(super::ssh_launch_line("/Users/o/My Apps/looklook", "ab12", Family::Posix), "'/Users/o/My Apps/looklook' ssh ab12");
        assert_eq!(super::ssh_launch_line("C:\\Program Files\\Looklook\\looklook.exe", "ab12", Family::Pwsh), "& 'C:\\Program Files\\Looklook\\looklook.exe' ssh ab12");
        assert_eq!(super::ssh_launch_line("C:\\Program Files\\Looklook\\looklook.exe", "ab12", Family::Cmd), "\"C:\\Program Files\\Looklook\\looklook.exe\" ssh ab12");
    }

    use super::*;
    use axum::http::Uri;

    fn hidden(qs: &str) -> Option<bool> {
        let uri: Uri = format!("/api/fs/list?{qs}").parse().unwrap();
        Query::<FsQ>::try_from_uri(&uri).ok().map(|q| q.0.hidden)
    }

    #[test]
    fn fs_hidden_flag_accepts_numeric_and_words() {
        assert_eq!(hidden("path=C%3A%5CUsers%5Cimyth&hidden=0"), Some(false));
        assert_eq!(hidden("hidden=1"), Some(true));
        assert_eq!(hidden("hidden=true"), Some(true));
        assert_eq!(hidden("hidden=false"), Some(false));
        assert_eq!(hidden("path=x"), Some(false));
        assert_eq!(hidden("hidden=maybe"), None);
    }

    #[test]
    fn dsh_token_and_url() {
        let out = "old\ndsh web: http://127.0.0.1:3080/?token=aaaaaaaaaaaaaaaaaaaa\nrestart\ndsh web: http://127.0.0.1:3080/?token=ozazZIiC1LJAIc8v7EAa-jgOFmru1Kqh_bd8KubxLQ\n[x] other\n";
        assert_eq!(parse_dsh_token(out).as_deref(), Some("ozazZIiC1LJAIc8v7EAa-jgOFmru1Kqh_bd8KubxLQ"));
        assert_eq!(parse_dsh_token("token=abc\n"), None);
        assert_eq!(with_query("https://a--b.r2.example.com", "token", "T"), "https://a--b.r2.example.com/?token=T");
        assert_eq!(with_query("https://a--b.r2.example.com/", "token", "T"), "https://a--b.r2.example.com/?token=T");
        assert_eq!(with_query("http://127.0.0.1:41002/?x=1", "token", "T"), "http://127.0.0.1:41002/?x=1&token=T");
    }

    fn q(path: &std::path::Path, hidden: bool) -> FsQ {
        FsQ { path: path.display().to_string(), hidden, files: false }
    }

    #[test]
    fn fs_list_with_files_puts_folders_first() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("zdir")).unwrap();
        std::fs::write(tmp.path().join("a.txt"), b"hello").unwrap();
        std::fs::write(tmp.path().join(".hidden"), b"x").unwrap();
        let r = list_dirs(&FsQ { files: true, ..q(tmp.path(), false) }).ok().unwrap();
        let names: Vec<_> = r.entries.iter().map(|e| (e.name.as_str(), e.is_dir, e.size)).collect();
        assert_eq!(names, [("zdir", true, None), ("a.txt", false, Some(5))]);
        assert!(r.entries.iter().all(|e| e.mtime_ms.is_some()));
        // 不带 files 时只列文件夹，也不带大小和时间
        let r = list_dirs(&q(tmp.path(), false)).ok().unwrap();
        assert_eq!(r.entries.len(), 1);
        assert!(r.entries[0].mtime_ms.is_none());
    }

    #[test]
    fn drive_roots_from_bitmask() {
        assert_eq!(drive_roots(0b1100), ["C:\\", "D:\\"]);
        assert_eq!(drive_roots(1 | 1 << 25), ["A:\\", "Z:\\"]);
        assert!(drive_roots(0).is_empty());
    }

    #[test]
    fn fs_list_missing_path_falls_back_to_parent() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("a")).unwrap();
        std::fs::create_dir(tmp.path().join(".h")).unwrap();
        let r = list_dirs(&q(&tmp.path().join("new/deeper"), false)).ok().unwrap();
        assert_eq!(r.path, tmp.path().display().to_string());
        assert!(!r.exists && !r.denied);
        assert_eq!(r.entries.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(), ["a"]);
        let r = list_dirs(&q(tmp.path(), true)).ok().unwrap();
        assert_eq!(r.entries.len(), 2);
    }

    /// 没有读权限时返回 `denied` 而不是报错；root 不受文件权限限制，跳过。
    #[cfg(unix)]
    #[test]
    fn fs_list_reports_permission_denied() {
        use std::os::unix::fs::PermissionsExt;
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let locked = tmp.path().join("locked");
        std::fs::create_dir_all(locked.join("inner")).unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        let r = list_dirs(&q(&locked, false)).ok();
        let r2 = list_dirs(&q(&locked.join("inner"), false)).ok();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        let r = r.unwrap();
        assert!(r.denied && r.exists && r.entries.is_empty());
        assert_eq!(r.parent, Some(tmp.path().display().to_string()));
        // 上级不让进入：退到上级并标记 denied，而不是“不存在、将创建”
        let r2 = r2.unwrap();
        assert!(r2.denied);
        assert_eq!(r2.path, locked.display().to_string());
    }

    #[test]
    fn project_full_access_is_optional() {
        // 以前存下的（false 也是写明的）照旧；新建时没选过的不写，读出来是 None
        let old: Project = serde_json::from_value(serde_json::json!({ "workdir": "/a", "full_access": false })).unwrap();
        assert_eq!(old.full_access, Some(false));
        let quick: Project = serde_json::from_value(serde_json::json!({ "workdir": "/a", "launch": "claude" })).unwrap();
        assert_eq!(quick.full_access, None);
        assert!(serde_json::to_value(&quick).unwrap().get("full_access").is_none());
    }
}
