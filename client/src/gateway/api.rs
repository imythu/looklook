//! 本机接口 `/api/*`（管理台使用）。写请求要求 `X-Looklook-Client: 1` 且 `Origin` 为本站，
//! 跨站页面既带不上自定义头（需要预检，本服务不响应 CORS），也伪造不了来源。

use axum::extract::{DefaultBodyLimit, Path, Query, Request, State};
use axum::http::{Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch, post, put};
use axum::{Extension, Json, Router};
use looklook_protocol::dto::{CreateTunnel, UpdateTunnel};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::access::{self, Peer};
use super::{origin_matches, Access, App, CLIENT_HEADER};
use crate::account::VERSION;
use crate::error::LocalError;
use crate::instances::{log_tail, Backend, Input, Instances};
use crate::settings::Settings;
use crate::tools;
use crate::transfer;

type R<T> = Result<T, LocalError>;

pub fn routes(app: App) -> Router<App> {
    Router::new()
        .route("/status", get(status))
        .route("/auth/login", post(login))
        .route("/auth/login/cancel", post(login_cancel))
        .route("/auth/status", get(login_status))
        .route("/auth/logout", post(logout))
        .route("/auth/reconnect", post(reconnect))
        .route("/server", put(set_server))
        .route("/account", get(account))
        .route("/instances", get(list_instances).post(create_instance))
        .route("/instances/{id}", get(get_instance).patch(update_instance).delete(delete_instance))
        .route("/instances/{id}/start", post(start_instance))
        .route("/instances/{id}/stop", post(stop_instance))
        .route("/instances/{id}/restart", post(restart_instance))
        .route("/instances/{id}/logs", get(instance_logs))
        .route("/instances/{id}/files", get(instance_files))
        .route("/instances/{id}/uploads", post(create_upload))
        .route("/instances/{id}/download", get(download))
        .route("/instances/{id}/transfer/finish", post(end_transfer))
        .route("/instances/{id}/transfer/cancel", post(end_transfer))
        // 只有上传块放宽请求体上限（一块 8 MiB），其他接口仍是默认的 2 MB
        .route("/uploads/{uid}", put(upload_chunk).layer(DefaultBodyLimit::max(transfer::BODY_LIMIT)).delete(cancel_upload))
        .route("/uploads/{uid}/finish", post(finish_upload))
        .route("/instances/{id}/page", post(instance_page))
        .route("/fs/list", get(fs_list))
        .route("/relays", get(list_relays))
        .route("/relays/preference", put(set_relay))
        .route("/tunnels", get(list_tunnels).post(create_tunnel))
        .route("/tunnels/{id}", patch(update_tunnel).delete(delete_tunnel))
        .route("/settings", get(get_settings).put(put_settings))
        .route("/device/name", put(put_device_name))
        .route("/direct", post(direct))
        .route("/access", get(get_access).put(put_access))
        .route("/mcp", get(get_mcp).put(put_mcp))
        .route("/mcp/install", post(mcp_install))
        .route("/mcp/uninstall", post(mcp_uninstall))
        .route("/shells", get(get_shells))
        .route("/tools", get(get_tools))
        .route("/tools/install", post(install_tool))
        .route("/diag", get(diag_list))
        .route("/diag/clear", post(diag_clear))
        .route("/diag/ui-error", post(diag_ui_error))
        .route("/diag/bundle", get(diag_bundle))
        .route("/diag/bundle.txt", get(diag_bundle_text))
        .route("/diag/report", post(diag_report))
        .route("/promotions", get(promotions))
        .route("/metrics", get(metrics))
        .route("/update", get(update))
        .route("/update/install", post(update_install))
        .route("/update/dismiss", post(update_dismiss))
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
    if matches!(group, "instances" | "uploads" | "fs" | "settings" | "tools" | "tunnels" | "metrics") && app.account.session().is_none() {
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

/// 本机直连的口令（只在远程打开时，见 gateway/direct.rs）。写请求已由 `guard` 确认 `Origin` 就是这个子站。
async fn direct(State(app): State<App>, Extension(access): Extension<Access>, headers: axum::http::HeaderMap) -> R<Json<Value>> {
    if access.is_local() {
        return Err(LocalError::not_found());
    }
    let origin = headers.get(axum::http::header::ORIGIN).and_then(|v| v.to_str().ok()).unwrap_or("");
    // 管理台只绑在某个局域网地址上时，127.0.0.1 连不到它。
    let ip = app.ui_addr.ip();
    if !super::direct::valid_origin(origin) || !(ip.is_unspecified() || ip.is_loopback()) {
        return Ok(Json(json!({ "available": false })));
    }
    Ok(Json(json!({ "available": true, "port": app.ui_addr.port(), "nonce": app.direct.issue(origin) })))
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
        "console": app.console_info(),
        "update": app.updater.view(),
        "metrics": app.metrics.brief(),
        "relay_check": app.relay_watch.view(),
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

// ---------------- 终端 ----------------

async fn list_instances(State(app): State<App>) -> R<Json<Value>> {
    Ok(Json(json!({ "items": app.instances.list().await? })))
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
    let v = if req.input.launch.as_deref() == Some("dsh") { create_dsh(&app, req.input).await? } else { app.instances.create(req.input).await? };
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

/// 打开 DSH：确保它在运行、映射还在（被删了就重建，被关了就打开），返回映射地址。
async fn instance_page(State(app): State<App>, Path(id): Path<String>) -> R<Json<Value>> {
    require_allowed(&app)?;
    require_persistent(&app)?;
    let v = app.instances.get(&id).await?;
    let port = v.web_port.ok_or_else(LocalError::not_found)?;
    // 已经在运行时什么也不做；任务退出了（例如 dsh 崩了）会重新运行
    if !v.running || v.task_alive != Some(true) {
        app.instances.start(&id).await?;
    }
    let saved: Option<DshPage> = app.store.get(&page_key(&id))?;
    let existing = match saved {
        Some(p) => app.account.tunnels().await?.items.into_iter().find(|t| t.id == p.tunnel_id),
        None => None,
    };
    let tunnel = match existing {
        Some(t) if t.status == "enabled" && t.target_port == i32::from(port) => t,
        Some(t) => {
            let req = UpdateTunnel { name: None, target_port: Some(port.into()), status: Some("enabled".into()) };
            app.account.update_tunnel(&t.id, &req).await?
        }
        None => {
            let t = app.account.create_tunnel(&CreateTunnel { name: "DSH".into(), target_port: port.into() }).await?;
            app.store.set(&page_key(&id), &DshPage { tunnel_id: t.id.clone() })?;
            t
        }
    };
    // 刚启动时等它开始监听（第一次运行要初始化配置，会慢一些），免得打开就是“连不上”
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(20);
    let ready = loop {
        if tokio::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port)).await.is_ok() {
            break true;
        }
        if tokio::time::Instant::now() >= deadline {
            break false;
        }
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    };
    Ok(Json(json!({ "url": tunnel.url, "ready": ready })))
}

async fn get_instance(State(app): State<App>, Path(id): Path<String>) -> R<impl IntoResponse> {
    Ok(Json(app.instances.get(&id).await?))
}

async fn update_instance(State(app): State<App>, Path(id): Path<String>, Json(input): Json<Input>) -> R<impl IntoResponse> {
    Ok(Json(app.instances.update(&id, input).await?))
}

async fn delete_instance(State(app): State<App>, Path(id): Path<String>) -> R<StatusCode> {
    let page: Option<DshPage> = app.store.get(&page_key(&id))?;
    app.instances.delete(&id).await?;
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

// ---------------- 文件夹浏览（新建终端时选工作目录） ----------------

#[derive(Deserialize)]
struct FsQ {
    #[serde(default)]
    path: String,
    #[serde(default, deserialize_with = "de_flag")]
    hidden: bool,
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
                    let is_dir = match std::fs::metadata(e.path()) {
                        Ok(m) => m.is_dir(),
                        Err(_) => e.file_type().map(|t| t.is_dir()).unwrap_or(false),
                    };
                    (is_dir && (q.hidden || !is_hidden(&e, &name))).then_some(FsEntry { name, is_dir })
                })
                .collect();
            entries.sort_by_key(|e| e.name.to_lowercase());
            entries.truncate(2000);
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

async fn get_settings(State(app): State<App>) -> Json<Settings> {
    Json(Settings::load(&app.store))
}

async fn put_settings(State(app): State<App>, Json(s): Json<Settings>) -> R<Json<Settings>> {
    let s = s.normalized();
    crate::shells::validate(&s.default_shell)?;
    match std::fs::metadata(&s.default_workdir) {
        Ok(m) if m.is_dir() => {}
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            return Err(LocalError::new("WORKDIR_DENIED").with("path", s.default_workdir));
        }
        _ => return Err(LocalError::new("WORKDIR_NOT_FOUND").with("path", s.default_workdir)),
    }
    app.account.save_settings(&s).await?;
    Ok(Json(s))
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
    let net = |s: &str| access::parse_net(s).map(access::format_net).map_err(|rule| LocalError::invalid("allowed_ips", rule).with("value", s));
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
    *app.access.write().unwrap_or_else(|e| e.into_inner()) = c;
    let _ = app.write_open_file();
    Ok(Json(access_view(&app, peer.map(|p| p.0))))
}

// ---------------- AI 助手接入（MCP） ----------------

async fn mcp_view(app: &App) -> Value {
    let c = super::mcp::McpConfig::load(&app.store);
    let url = super::mcp::endpoint(app);
    let agents = crate::agents::status(app.instances.shell(), &url, &c.token).await;
    json!({ "enabled": c.enabled, "token": c.token, "url": url, "agents": agents })
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
    Json(json!({ "tools": tools::detect(app.instances.shell()).await }))
}

#[derive(Deserialize)]
struct InstallReq {
    tool: String,
    #[serde(default)]
    mirror: String,
    /// 终端名称（界面按语言生成）
    #[serde(default)]
    name: Option<String>,
}

async fn install_tool(State(app): State<App>, Json(req): Json<InstallReq>) -> R<impl IntoResponse> {
    require_allowed(&app)?;
    let cmd = tools::install_command(&req.tool, &req.mirror).ok_or_else(|| LocalError::invalid("tool", "invalid"))?;
    let v = app
        .instances
        .create(Input { name: req.name, launch: Some("custom".into()), command: Some(cmd), ..Default::default() })
        .await?;
    Ok((StatusCode::CREATED, Json(app.instances.start(&v.row.id).await?)))
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

async fn update_dismiss(State(app): State<App>, Json(req): Json<DismissReq>) -> R<Json<Value>> {
    app.updater.dismiss(req.version.trim())?;
    Ok(Json(app.updater.view()))
}

#[cfg(test)]
mod tests {
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

    fn q(path: &std::path::Path, hidden: bool) -> FsQ {
        FsQ { path: path.display().to_string(), hidden }
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
}
