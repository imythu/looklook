//! Claude Code 对话界面的接口（docs/CLAUDE_UI.md §4.7）。
//!
//! - `POST /hook`：`looklook hook` 转来的 hook 事件。只接受这台机器自己发来的、带本次运行令牌的请求。
//! - `/api/instances/{id}/chat…`：管理台用。和终端一样要登录、账户可用；写请求经过 `/api` 的来源检查。
//!   事件用 SSE 推送（`chat/events`）：先发一份快照，之后是状态、新增或更新的条目、要求重新加载。

use std::convert::Infallible;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use futures_util::Stream;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::broadcast::error::RecvError;

use super::access::Peer;
use super::api::require_allowed;
use super::{Access, App};
use crate::chat::{hooks, Event, PermissionAnswer};
use crate::error::LocalError;

type R<T> = Result<T, LocalError>;

pub async fn hook(State(app): State<App>, access: Option<Extension<Access>>, peer: Option<Extension<Peer>>, headers: HeaderMap, body: Bytes) -> Response {
    let local = access.is_none_or(|a| a.0.is_local()) && peer.is_none_or(|p| p.0 == Peer::Local);
    if !local {
        return StatusCode::NOT_FOUND.into_response();
    }
    let header = |k: &str| headers.get(k).and_then(|v| v.to_str().ok()).unwrap_or("");
    // 浏览器发不出这个请求：带 Origin 的一律拒绝
    if headers.contains_key(axum::http::header::ORIGIN) || !app.chat.token_ok(header(hooks::TOKEN_HEADER)) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let id = header(hooks::INSTANCE_HEADER).to_string();
    let Ok(ev) = serde_json::from_slice::<Value>(&body) else { return (StatusCode::OK, "").into_response() };
    let out = app.chat.hook(&id, ev).await;
    (StatusCode::OK, out).into_response()
}

pub async fn snapshot(State(app): State<App>, Path(id): Path<String>) -> R<Json<Value>> {
    Ok(Json(app.chat.snapshot(&id).await?))
}

/// SSE：`snapshot`（第一条，以及落后太多时）、`status`、`items`、`reset`（界面重新取快照）。
pub async fn events(State(app): State<App>, Path(id): Path<String>) -> R<Sse<impl Stream<Item = Result<SseEvent, Infallible>>>> {
    require_allowed(&app)?;
    let (rx, viewer) = app.chat.subscribe(&id)?;
    let first = app.chat.snapshot(&id).await?;
    let stream = futures_util::stream::unfold((app, id, rx, viewer, Some(first)), |(app, id, mut rx, viewer, first)| async move {
        if let Some(snap) = first {
            let ev = SseEvent::default().event("snapshot").data(snap.to_string());
            return Some((Ok(ev), (app, id, rx, viewer, None)));
        }
        let ev = match rx.recv().await {
            Ok(Event::Status(s)) => SseEvent::default().event("status").data(s.to_string()),
            Ok(Event::Items { start, items }) => SseEvent::default().event("items").data(json!({ "start": start, "items": items }).to_string()),
            Ok(Event::Reset) => SseEvent::default().event("reset").data("{}"),
            // 落后太多（界面卡住了）：重新发一份快照
            Err(RecvError::Lagged(_)) => match app.chat.snapshot(&id).await {
                Ok(s) => SseEvent::default().event("snapshot").data(s.to_string()),
                Err(_) => return None,
            },
            Err(RecvError::Closed) => return None,
        };
        Some((Ok(ev), (app, id, rx, viewer, None)))
    });
    Ok(Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15))))
}

#[derive(Deserialize)]
pub struct HistoryQuery {
    before: usize,
    #[serde(default = "default_limit")]
    limit: usize,
}

fn default_limit() -> usize {
    crate::chat::SNAPSHOT_ITEMS
}

pub async fn history(State(app): State<App>, Path(id): Path<String>, Query(q): Query<HistoryQuery>) -> R<Json<Value>> {
    Ok(Json(app.chat.history(&id, q.before, q.limit).await?))
}

#[derive(Deserialize)]
pub struct ItemQuery {
    key: String,
}

pub async fn item(State(app): State<App>, Path(id): Path<String>, Query(q): Query<ItemQuery>) -> R<Json<Value>> {
    Ok(Json(app.chat.item(&id, &q.key).await?))
}

#[derive(Deserialize)]
pub struct AgentQuery {
    #[serde(default)]
    agent: Option<String>,
    #[serde(default)]
    tool: Option<String>,
}

pub async fn agent(State(app): State<App>, Path(id): Path<String>, Query(q): Query<AgentQuery>) -> R<Json<Value>> {
    Ok(Json(app.chat.agent(&id, q.agent.as_deref(), q.tool.as_deref()).await?))
}

pub async fn commands(State(app): State<App>, Path(id): Path<String>) -> R<Json<Value>> {
    Ok(Json(app.chat.commands(&id)?))
}

#[derive(Deserialize)]
pub struct InputReq {
    #[serde(default)]
    text: String,
    /// 附件目录里的图片（先经“发给 AI”上传）
    #[serde(default)]
    images: Vec<String>,
}

pub async fn input(State(app): State<App>, Path(id): Path<String>, Json(req): Json<InputReq>) -> R<Json<Value>> {
    require_allowed(&app)?;
    if req.text.len() > 200_000 || req.images.len() > 10 {
        return Err(LocalError::invalid("text", "too_long"));
    }
    // 图片只能来自这个终端的附件目录
    let dir = app.paths.attach_dir(&id);
    if req.images.iter().any(|p| !std::path::Path::new(p).starts_with(&dir) || p.contains("..")) {
        return Err(LocalError::invalid("images", "invalid"));
    }
    Ok(Json(app.chat.input(&id, &req.text, &req.images).await?))
}

pub async fn interrupt(State(app): State<App>, Path(id): Path<String>) -> R<StatusCode> {
    require_allowed(&app)?;
    app.chat.interrupt(&id).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn permission(State(app): State<App>, Path(id): Path<String>, Json(req): Json<PermissionAnswer>) -> R<StatusCode> {
    require_allowed(&app)?;
    app.chat.answer(&id, &req)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct PlanReq {
    choice: u8,
    /// choice 为 3 时：要 Claude 怎么改
    feedback: Option<String>,
}

pub async fn plan(State(app): State<App>, Path(id): Path<String>, Json(req): Json<PlanReq>) -> R<StatusCode> {
    require_allowed(&app)?;
    app.chat.plan(&id, req.choice, req.feedback.as_deref()).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn relaunch(State(app): State<App>, Path(id): Path<String>) -> R<StatusCode> {
    require_allowed(&app)?;
    app.chat.relaunch(&id).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct SettingReq {
    kind: String,
    value: String,
}

/// 快捷设置：模型、思考强度、权限模式
pub async fn setting(State(app): State<App>, Path(id): Path<String>, Json(req): Json<SettingReq>) -> R<StatusCode> {
    require_allowed(&app)?;
    app.chat.setting(&id, &req.kind, &req.value).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// 模型菜单
pub async fn models(State(app): State<App>, Path(id): Path<String>) -> R<Json<Value>> {
    Ok(Json(app.chat.models(&id).await?))
}

/// 上下文、token、订阅用量
pub async fn usage(State(app): State<App>, Path(id): Path<String>) -> R<Json<Value>> {
    Ok(Json(app.chat.usage(&id).await?))
}

/// 权限规则和 MCP 服务器
pub async fn config(State(app): State<App>, Path(id): Path<String>) -> R<Json<Value>> {
    Ok(Json(app.chat.config(&id)?))
}

/// 这个会话改过的文件
pub async fn changes(State(app): State<App>, Path(id): Path<String>) -> R<Json<Value>> {
    Ok(Json(app.chat.changes(&id).await?))
}

#[derive(Deserialize)]
pub struct DialogReq {
    /// 选第几项；没有时是取消（Esc）
    index: Option<usize>,
    #[serde(default)]
    title: String,
}

/// TUI 上的选择框：选一项或取消
pub async fn dialog(State(app): State<App>, Path(id): Path<String>, Json(req): Json<DialogReq>) -> R<StatusCode> {
    require_allowed(&app)?;
    match req.index {
        Some(i) => app.chat.dialog_choose(&id, &req.title, i).await?,
        None => app.chat.dialog_cancel(&id).await?,
    }
    Ok(StatusCode::NO_CONTENT)
}

/// 子代理和后台命令
pub async fn tasks(State(app): State<App>, Path(id): Path<String>) -> R<Json<Value>> {
    Ok(Json(app.chat.tasks(&id).await?))
}

#[derive(Deserialize)]
pub struct TaskQuery {
    tool: String,
}

pub async fn task_output(State(app): State<App>, Path(id): Path<String>, Query(q): Query<TaskQuery>) -> R<Json<Value>> {
    Ok(Json(app.chat.task_output(&id, &q.tool).await?))
}

pub async fn task_stop(State(app): State<App>, Path(id): Path<String>, Json(q): Json<TaskQuery>) -> R<StatusCode> {
    require_allowed(&app)?;
    app.chat.stop_task(&id, &q.tool).await?;
    Ok(StatusCode::NO_CONTENT)
}
