//! AI 助手的飞书工具（MCP，`gateway/mcp.rs`）：用你本人的身份读群聊、单聊和话题里的消息，下载里面的图片和文件，
//! 看群成员、找人，搜和读云文档，以你的身份发消息。
//!
//! - **令牌**（feishu/user.rs）：第一次用、或者过期了，给绑定的人私聊发一张“授权”卡片，点开在飞书里同意；
//!   工具最多等 45 秒，没等到就告诉 AI“等用户授权后再调一次”。只认绑定的那个飞书账号：别人点开同意了也不算；
//! - **发消息**：每条都先发一张卡片问你（发送 / 不发），点了才发；工具最多等 50 秒，没等到就告诉 AI 已经在等你确认，
//!   之后你点了照样发出去（30 分钟内）。卡片的回调可能落到别的电脑上，经 `feishu_agent` 的 `Rpc::Send` 转回来；
//! - 缺权限（应用没开通用户权限）：发补权限的卡片（`register::addons` 带着 `scopes.user`），补好后要重新授权一次。

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use serde::Serialize;
use serde_json::{json, Value};
use tokio::sync::oneshot;

use crate::feishu::card::{self, Act};
use crate::feishu::user::{self, Token};
use crate::feishu::{self as fs};
use crate::gateway::App;
use crate::notify::{self, load_feishu, load_rules, tr, Feishu};

/// 发消息确认卡片上的按钮
pub const SEND: &str = "fu_send";
pub const SKIP: &str = "fu_skip";
/// 工具等授权最多多久
const AUTH_WAIT: Duration = Duration::from_secs(45);
/// 工具等你点“发送”最多多久（Codex 默认 60 秒就算工具超时）
const SEND_WAIT: Duration = Duration::from_secs(50);
/// 发消息的卡片多久不点就作废
const SEND_TTL: Duration = Duration::from_secs(30 * 60);
/// 一条消息最多多少字
const TEXT_MAX: usize = 4000;
/// 下载文件最大多少
const FILE_MAX: usize = 100 << 20;
/// 读消息时每条最多多少字
const MSG_CHARS: usize = 4000;

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

pub fn handles(a: &str) -> bool {
    matches!(a, SEND | SKIP)
}

#[derive(Default)]
pub struct Tools {
    /// 换新令牌排队（`refresh_token` 只能用一次）
    refresh: tokio::sync::Mutex<()>,
    /// 正在进行的授权（一台电脑同时只有一个），给设置页看
    auth: Mutex<Option<Auth>>,
    /// 等你点“发送 / 不发”的消息：nonce → 结果
    pending: Mutex<HashMap<String, oneshot::Sender<bool>>>,
    /// open_id → 名字
    names: Mutex<HashMap<String, String>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Auth {
    pub id: String,
    pub url: String,
    pub expires_at: i64,
    /// pending / done / wrong_user / 飞书的错误（access_denied、expired_token…）
    pub state: String,
}

impl Tools {
    pub fn auth(&self) -> Option<Auth> {
        lock(&self.auth).clone()
    }

    fn set_state(&self, id: &str, state: &str) {
        if let Some(a) = lock(&self.auth).as_mut().filter(|a| a.id == id) {
            a.state = state.into();
        }
    }
}

/// 给设置页看：授权了没有、是谁、还缺哪些权限
pub fn view(app: &App) -> Value {
    let f = load_feishu(&app.store);
    let t = Token::load(&app.store).filter(|t| f.as_ref().is_some_and(|f| valid(t, f)) && t.refresh_exp > crate::util::system_ms());
    json!({
        "authorized": t.is_some(),
        "name": t.as_ref().map(|t| &t.name),
        "at": t.as_ref().map(|t| t.at),
        "missing": t.as_ref().map(Token::missing).unwrap_or_default(),
        "auth": app.notify.tools.auth(),
    })
}

/// 设置页“撤销”：删掉这台电脑上的令牌（飞书那边的授权由用户在飞书里管理）
pub fn revoke(app: &App) {
    Token::clear(&app.store);
    lock(&app.notify.tools.auth).take();
}

fn valid(t: &Token, f: &Feishu) -> bool {
    t.app_id == f.app_id && Some(&t.open_id) == f.open_id.as_ref()
}

fn en(app: &App) -> bool {
    load_rules(&app.store).lang.starts_with("en")
}

fn fail(code: &str, message: impl Into<String>) -> Value {
    json!({ "error": code, "message": message.into() })
}

fn configured(app: &App) -> Result<(Feishu, String), Value> {
    let f = load_feishu(&app.store).ok_or_else(|| fail("FEISHU_NOT_CONFIGURED", "Feishu is not set up in Looklook. Ask the user to set it up in Looklook Settings → Notifications."))?;
    let owner = f.open_id.clone().ok_or_else(|| fail("FEISHU_NOT_BOUND", "No Feishu account is bound in Looklook yet. Ask the user to finish the Feishu setup in Looklook Settings → Notifications."))?;
    Ok((f, owner))
}

/// 拿一个能用的用户令牌：存着的、换新的，都没有就请用户授权（最多等 [`AUTH_WAIT`]）
async fn token(app: &App) -> Result<(Feishu, String), Value> {
    let (f, _) = configured(app)?;
    let me = &app.notify.tools;
    let fresh = |t: &Token| t.access_exp - 300_000 > crate::util::system_ms();
    if let Some(t) = Token::load(&app.store).filter(|t| valid(t, &f)) {
        if fresh(&t) {
            return Ok((f, t.access));
        }
        if !t.refresh.is_empty() && t.refresh_exp - 60_000 > crate::util::system_ms() {
            let _g = me.refresh.lock().await;
            // 排队的时候别的调用可能已经换好了
            if let Some(t) = Token::load(&app.store).filter(|t| valid(t, &f)) {
                if fresh(&t) {
                    return Ok((f, t.access));
                }
                match user::refresh(&app.notify.http(), &f.domain, &f.app_secret, &t).await {
                    Ok(n) => {
                        if let Err(e) = n.save(&app.store) {
                            tracing::warn!(error = %e, "飞书：新的用户令牌没存上");
                        }
                        return Ok((f, n.access));
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "飞书：用户令牌换新失败，要重新授权");
                        Token::clear(&app.store);
                    }
                }
            }
        }
    }
    let a = start(app).await.map_err(|e| fail("FEISHU_AUTH_FAILED", format!("Could not start Feishu authorization: {}", e.code)))?;
    let until = tokio::time::Instant::now() + AUTH_WAIT;
    while tokio::time::Instant::now() < until {
        tokio::time::sleep(Duration::from_secs(1)).await;
        if let Some(t) = Token::load(&app.store).filter(|t| valid(t, &f) && fresh(t)) {
            return Ok((f, t.access));
        }
        match me.auth() {
            Some(x) if x.id == a.id && x.state == "pending" => {}
            Some(x) if x.id == a.id && x.state != "done" => {
                return Err(fail("FEISHU_AUTH_FAILED", format!("The user did not authorize Feishu access ({}). Do not retry unless they ask.", x.state)));
            }
            _ => {}
        }
    }
    Err(fail(
        "FEISHU_AUTH_PENDING",
        format!("The user has been sent an authorization card in Feishu (link: {}). Ask them to open it and approve, then call this tool again.", a.url),
    ))
}

/// 开始授权（已经有一个没过期的就接着用它），给绑定的人私聊发一张授权卡片
pub async fn start(app: &App) -> Result<Auth, crate::error::LocalError> {
    use crate::error::LocalError;
    let me = &app.notify.tools;
    if let Some(a) = me.auth().filter(|a| a.state == "pending" && a.expires_at > crate::util::system_ms() + 20_000) {
        return Ok(a);
    }
    let f = load_feishu(&app.store).ok_or_else(|| LocalError::new("NOTIFY_NOT_CONFIGURED"))?;
    let owner = f.open_id.clone().ok_or_else(|| LocalError::new("NOTIFY_NOT_BOUND"))?;
    let http = app.notify.http();
    let api = app.notify.api(&f);
    let b = match user::begin(&http, &f.domain, &f.app_id, &f.app_secret, user::SCOPES).await {
        Ok(b) => b,
        Err(e) => {
            // 多半是应用还没开通这些用户权限：发补权限的卡片
            tracing::warn!(error = %e, "飞书：用户授权没开始");
            crate::feishu_agent::grant(app, &api, "user", &[]).await;
            return Err(notify::feishu_error(&e));
        }
    };
    let id = notify::nonce();
    let expires_at = crate::util::system_ms() + b.expires_in as i64 * 1000;
    let a = Auth { id: id.clone(), url: b.url.clone(), expires_at, state: "pending".into() };
    *lock(&me.auth) = Some(a.clone());
    let en = en(app);
    let msg = match api.send_card(&owner, &card::user_auth(en, &b.url, b.expires_in.div_ceil(60))).await {
        Ok(m) => Some(m),
        Err(e) => {
            tracing::warn!(error = %e, "飞书：授权卡片没发出去");
            None
        }
    };
    let app = app.clone();
    tokio::spawn(async move { poll(app, f, owner, id, b.device_code, b.interval, expires_at, msg).await });
    Ok(a)
}

#[allow(clippy::too_many_arguments)]
async fn poll(app: App, f: Feishu, owner: String, id: String, code: String, mut interval: u64, expires_at: i64, msg: Option<String>) {
    let me = &app.notify.tools;
    let http = app.notify.http();
    let en = en(&app);
    let (ok, why) = loop {
        tokio::time::sleep(Duration::from_secs(interval)).await;
        if me.auth().is_none_or(|a| a.id != id) {
            return;
        }
        if crate::util::system_ms() > expires_at {
            break (false, "expired_token".to_string());
        }
        match user::poll(&http, &f.domain, &f.app_id, &f.app_secret, &code).await {
            Ok(user::Poll::Pending) => {}
            Ok(user::Poll::SlowDown) => interval += 5,
            Ok(user::Poll::Failed(e)) => break (false, e),
            Ok(user::Poll::Done(t)) => match user::me(&http, &f.domain, &t.access).await {
                Ok((who, name)) if who == owner => {
                    let t = Token { open_id: who, name, ..t };
                    if let Err(e) = t.save(&app.store) {
                        tracing::warn!(error = %e, "飞书：用户令牌没存上");
                        break (false, "store".into());
                    }
                    let missing = t.missing();
                    if !missing.is_empty() {
                        tracing::info!(?missing, "飞书：用户授权少了一些权限");
                        crate::feishu_agent::grant(&app, &app.notify.api(&f), "user", &[]).await;
                    }
                    break (true, String::new());
                }
                Ok(_) => break (false, "wrong_user".into()),
                Err(e) => break (false, format!("user_info: {e}")),
            },
            Err(e) => tracing::debug!(error = %e, "飞书：问授权结果失败，稍后再问"),
        }
    };
    me.set_state(&id, if ok { "done" } else { &why });
    tracing::info!(ok, why, "飞书：用户授权结束");
    if let Some(m) = msg {
        let text = match why.as_str() {
            "" => tr(en, "AI 助手现在可以用你的身份读飞书了。", "AI assistants can now use Feishu as you."),
            "access_denied" => tr(en, "你拒绝了授权。", "You declined."),
            "expired_token" => tr(en, "链接过期了。AI 助手下次用到时会再发一张。", "The link expired. You'll get a new one next time an AI assistant needs it."),
            "wrong_user" => tr(en, "授权的不是绑定的飞书账号，没有用。", "Authorized by a different Feishu account than the bound one; ignored."),
            e => tr(en, &format!("没成功：{e}"), &format!("Failed: {e}")),
        };
        if let Err(e) = app.notify.api(&f).patch_card(&m, &card::user_auth_done(en, ok, &text)).await {
            tracing::debug!(error = %e, "飞书：授权卡片没改上");
        }
    }
}

/// 飞书接口的错误 → 给 AI 看的。缺权限：删掉令牌，发补权限的卡片，下次调用重新授权
async fn api_err(app: &App, e: anyhow::Error) -> Value {
    let code = fs::code_of(&e);
    if code.is_some_and(fs::is_scope_error) {
        Token::clear(&app.store);
        if let Some(f) = load_feishu(&app.store) {
            crate::feishu_agent::grant(app, &app.notify.api(&f), "user", &[]).await;
        }
        return fail("FEISHU_SCOPE_MISSING", format!("Feishu says a permission is missing ({e}). The user has been sent a card in Feishu to grant it; after that the next call asks them to authorize again."));
    }
    if code.is_some_and(|c| matches!(c, 99991668 | 99991677 | 20005)) {
        Token::clear(&app.store);
        return fail("FEISHU_AUTH_EXPIRED", "The Feishu authorization is no longer valid. Call the tool again; the user will be asked to authorize.");
    }
    fail("FEISHU_API", e.to_string())
}

async fn get(app: &App, f: &Feishu, tok: &str, path: &str) -> Result<Value, Value> {
    match user::call(&app.notify.http(), &f.domain, tok, reqwest::Method::GET, path, &Value::Null).await {
        Ok(v) => Ok(v),
        Err(e) => Err(api_err(app, e).await),
    }
}

async fn post(app: &App, f: &Feishu, tok: &str, path: &str, body: &Value) -> Result<Value, Value> {
    match user::call(&app.notify.http(), &f.domain, tok, reqwest::Method::POST, path, body).await {
        Ok(v) => Ok(v),
        Err(e) => Err(api_err(app, e).await),
    }
}

/// 路径加上查询参数（空的不带）
fn with_query(path: &str, q: &[(&str, &str)]) -> String {
    let mut u = reqwest::Url::parse("http://x").expect("固定的地址");
    u.set_path(path);
    {
        let mut p = u.query_pairs_mut();
        for (k, v) in q.iter().filter(|(_, v)| !v.is_empty()) {
            p.append_pair(k, v);
        }
    }
    match u.query() {
        Some(q) if !q.is_empty() => format!("{}?{q}", u.path()),
        _ => u.path().to_string(),
    }
}

fn arg<'a>(args: &'a Value, k: &str) -> &'a str {
    args.get(k).and_then(Value::as_str).map(str::trim).unwrap_or_default()
}

fn id_arg(args: &Value, k: &str) -> Result<String, Value> {
    let v = arg(args, k);
    if v.is_empty() || v.len() > 200 || !v.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-')) {
        return Err(fail("INVALID_ARGUMENT", format!("`{k}` is missing or invalid")));
    }
    Ok(v.into())
}

/// 时间参数：ISO 8601（`2026-10-29T09:00:00+08:00`）或秒级时间戳 → 秒级时间戳
fn seconds(s: &str) -> Result<String, Value> {
    if s.is_empty() {
        return Ok(String::new());
    }
    if let Ok(n) = s.parse::<i64>() {
        return Ok((if n > 100_000_000_000 { n / 1000 } else { n }).to_string());
    }
    time::OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339)
        .map(|t| t.unix_timestamp().to_string())
        .map_err(|_| fail("INVALID_ARGUMENT", format!("time `{s}` must be ISO 8601 with a timezone, e.g. 2026-10-29T09:00:00+08:00, or unix seconds")))
}

fn iso(ms: &str) -> String {
    let ms: i64 = ms.parse().unwrap_or_default();
    time::OffsetDateTime::from_unix_timestamp(ms / 1000)
        .ok()
        .and_then(|t| t.format(&time::format_description::well_known::Rfc3339).ok())
        .unwrap_or_default()
}

fn page_size(args: &Value, max: u64, default: u64) -> String {
    args.get("page_size").and_then(Value::as_u64).unwrap_or(default).clamp(1, max).to_string()
}

// ---------------- 工具 ----------------

pub const TOOLS: [&str; 8] =
    ["feishu_list_chats", "feishu_read_messages", "feishu_chat_members", "feishu_download_file", "feishu_search_users", "feishu_send_message", "feishu_search_docs", "feishu_read_doc"];

pub const INSTRUCTIONS: &str = " The feishu_* tools act in Feishu (Lark) as the user themself: read their group chats, direct chats and topic threads, \
download files from messages, look up people, search and read cloud docs, and send messages as them. The first call may ask the user to authorize \
in Feishu; if a tool says authorization is pending, wait for the user and call it again. Every message you send as the user must be approved by \
them in Feishu first, so send only what they asked for.";

pub fn defs() -> Vec<Value> {
    let page_token = json!({ "type": "string", "description": "From the previous result's page_token, to get the next page" });
    vec![
        json!({
            "name": "feishu_list_chats",
            "title": "List or search Feishu chats",
            "description": "List the group chats the user is in (most recently active first), or search them by name or member with `query`. Returns chat_id, name and description. Direct (1:1) chats are not listed; read them with feishu_read_messages and user_id.",
            "inputSchema": { "type": "object", "properties": { "query": { "type": "string", "description": "Keyword; leave empty to list all" }, "page_token": page_token } },
            "annotations": { "readOnlyHint": true, "openWorldHint": true },
        }),
        json!({
            "name": "feishu_read_messages",
            "title": "Read Feishu messages",
            "description": "Read messages as the user from one of: a group chat (`chat_id`, oc_…), the direct chat with a person (`user_id`, ou_…), a topic thread with all its replies (`thread_id`, omt_…), or one message (`message_id`, om_…, expands forwarded chat history). A chat returns only top-level messages; messages that started a topic have a thread_id to read the replies. Files and images are listed with keys for feishu_download_file.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "chat_id": { "type": "string" },
                    "user_id": { "type": "string", "description": "open_id of the other person in a direct chat" },
                    "thread_id": { "type": "string" },
                    "message_id": { "type": "string" },
                    "start_time": { "type": "string", "description": "ISO 8601 with timezone, e.g. 2026-10-29T09:00:00+08:00 (chats only)" },
                    "end_time": { "type": "string", "description": "ISO 8601 with timezone (chats only)" },
                    "order": { "type": "string", "enum": ["newest_first", "oldest_first"], "description": "Default newest_first" },
                    "page_size": { "type": "integer", "minimum": 1, "maximum": 50, "description": "Default 30" },
                    "page_token": page_token
                }
            },
            "annotations": { "readOnlyHint": true, "openWorldHint": true },
        }),
        json!({
            "name": "feishu_chat_members",
            "title": "List members of a Feishu chat",
            "description": "List the members of a group chat (name and open_id).",
            "inputSchema": { "type": "object", "properties": { "chat_id": { "type": "string" }, "page_token": page_token }, "required": ["chat_id"] },
            "annotations": { "readOnlyHint": true, "openWorldHint": true },
        }),
        json!({
            "name": "feishu_download_file",
            "title": "Download a file or image from a Feishu message",
            "description": "Download an image or file attached to a message (keys come from feishu_read_messages) to this computer. Returns the local path. Max 100 MB.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "message_id": { "type": "string" },
                    "key": { "type": "string", "description": "image_key or file_key" },
                    "type": { "type": "string", "enum": ["image", "file"] },
                    "name": { "type": "string", "description": "File name to save as (optional)" }
                },
                "required": ["message_id", "key", "type"]
            },
            "annotations": { "readOnlyHint": true, "openWorldHint": true },
        }),
        json!({
            "name": "feishu_search_users",
            "title": "Find people in Feishu",
            "description": "Search people in the user's organization by name, and get their open_id (for feishu_read_messages or feishu_send_message).",
            "inputSchema": { "type": "object", "properties": { "query": { "type": "string" }, "page_token": page_token }, "required": ["query"] },
            "annotations": { "readOnlyHint": true, "openWorldHint": true },
        }),
        json!({
            "name": "feishu_send_message",
            "title": "Send a Feishu message as the user",
            "description": "Send a message as the user to a chat (`to` = chat_id) or a person (`to` = open_id), or reply to a message (`reply_to` = message_id). Markdown is supported. The user must approve each message in Feishu first; if they haven't yet when this returns, it is sent automatically once they approve — do not send it again.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "to": { "type": "string", "description": "chat_id (oc_…) or open_id (ou_…); not needed with reply_to" },
                    "reply_to": { "type": "string", "description": "message_id to reply to" },
                    "in_thread": { "type": "boolean", "description": "With reply_to: reply inside the message's topic thread" },
                    "text": { "type": "string", "maxLength": TEXT_MAX }
                },
                "required": ["text"]
            },
            "annotations": { "readOnlyHint": false, "destructiveHint": false, "openWorldHint": true },
        }),
        json!({
            "name": "feishu_search_docs",
            "title": "Search Feishu docs and wikis",
            "description": "Search the cloud docs and wiki pages the user can see. Returns titles, types and URLs.",
            "inputSchema": { "type": "object", "properties": { "query": { "type": "string" }, "page_token": page_token }, "required": ["query"] },
            "annotations": { "readOnlyHint": true, "openWorldHint": true },
        }),
        json!({
            "name": "feishu_read_doc",
            "title": "Read a Feishu doc",
            "description": "Read a cloud doc or wiki page as Markdown. `doc` is its URL or token. For long docs use offset/limit (characters).",
            "inputSchema": {
                "type": "object",
                "properties": { "doc": { "type": "string" }, "offset": { "type": "integer", "minimum": 0 }, "limit": { "type": "integer", "minimum": 1 } },
                "required": ["doc"]
            },
            "annotations": { "readOnlyHint": true, "openWorldHint": true },
        }),
    ]
}

/// 调一个飞书工具；不是飞书工具返回 None
pub async fn call(app: &App, name: &str, args: &Value) -> Option<Result<Value, Value>> {
    if !TOOLS.contains(&name) {
        return None;
    }
    Some(run(app, name, args).await)
}

async fn run(app: &App, name: &str, args: &Value) -> Result<Value, Value> {
    if name == "feishu_send_message" {
        return send(app, args).await;
    }
    let (f, tok) = token(app).await?;
    let f = &f;
    let tok = tok.as_str();
    match name {
        "feishu_list_chats" => {
            let q = arg(args, "query");
            let path = if q.is_empty() {
                with_query("/open-apis/im/v1/chats", &[("page_size", "50"), ("sort_type", "ByActiveTimeDesc"), ("page_token", arg(args, "page_token"))])
            } else {
                with_query("/open-apis/im/v1/chats/search", &[("query", q), ("page_size", "50"), ("page_token", arg(args, "page_token"))])
            };
            let d = get(app, f, tok, &path).await?;
            let chats: Vec<Value> = d["items"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|c| json!({ "chat_id": c["chat_id"], "name": c["name"], "description": c["description"], "external": c["external"] }))
                .collect();
            Ok(json!({ "chats": chats, "has_more": d["has_more"], "page_token": d["page_token"] }))
        }
        "feishu_read_messages" => read_messages(app, f, tok, args).await,
        "feishu_chat_members" => {
            let chat = id_arg(args, "chat_id")?;
            let path = with_query(&format!("/open-apis/im/v1/chats/{chat}/members"), &[("member_id_type", "open_id"), ("page_size", "100"), ("page_token", arg(args, "page_token"))]);
            let d = get(app, f, tok, &path).await?;
            let members: Vec<Value> = d["items"].as_array().into_iter().flatten().map(|m| json!({ "name": m["name"], "open_id": m["member_id"] })).collect();
            Ok(json!({ "members": members, "total": d["member_total"], "has_more": d["has_more"], "page_token": d["page_token"] }))
        }
        "feishu_download_file" => {
            let msg = id_arg(args, "message_id")?;
            let key = id_arg(args, "key")?;
            let kind = match arg(args, "type") {
                "image" => "image",
                "file" => "file",
                _ => return Err(fail("INVALID_ARGUMENT", "`type` must be image or file")),
            };
            let bytes = match user::download(&app.notify.http(), &f.domain, tok, &msg, &key, kind, FILE_MAX).await {
                Ok(b) => b,
                Err(e) => return Err(api_err(app, e).await),
            };
            let name = file_name(arg(args, "name"), &key, &bytes);
            let dir = app.paths.home.join("feishu-files").join(&msg);
            let path = dir.join(&name);
            let size = bytes.len();
            let saved = tokio::task::spawn_blocking(move || std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(&path, bytes)).map(|_| path)).await;
            match saved {
                Ok(Ok(p)) => Ok(json!({ "path": p, "bytes": size })),
                Ok(Err(e)) => Err(fail("WRITE_FAILED", e.to_string())),
                Err(e) => Err(fail("WRITE_FAILED", e.to_string())),
            }
        }
        "feishu_search_users" => {
            let q = arg(args, "query");
            if q.is_empty() {
                return Err(fail("INVALID_ARGUMENT", "`query` is required"));
            }
            let d = get(app, f, tok, &with_query("/open-apis/search/v1/user", &[("query", q), ("page_size", "20"), ("page_token", arg(args, "page_token"))])).await?;
            let users: Vec<Value> = d["users"].as_array().into_iter().flatten().map(|u| json!({ "name": u["name"], "open_id": u["open_id"], "departments": u["department_ids"] })).collect();
            Ok(json!({ "users": users, "has_more": d["has_more"], "page_token": d["page_token"] }))
        }
        "feishu_search_docs" => {
            let q = arg(args, "query");
            let mut body = json!({ "query": q, "page_size": 15, "doc_filter": {}, "wiki_filter": {} });
            if !arg(args, "page_token").is_empty() {
                body["page_token"] = json!(arg(args, "page_token"));
            }
            let d = post(app, f, tok, "/open-apis/search/v2/doc_wiki/search", &body).await?;
            Ok(json!({ "results": d["res_units"], "total": d["total"], "has_more": d["has_more"], "page_token": d["page_token"] }))
        }
        "feishu_read_doc" => {
            let doc = arg(args, "doc");
            if doc.is_empty() {
                return Err(fail("INVALID_ARGUMENT", "`doc` is required"));
            }
            let mut a = json!({ "doc_id": doc });
            for k in ["offset", "limit"] {
                if let Some(n) = args.get(k).and_then(Value::as_u64) {
                    a[k] = json!(n);
                }
            }
            let r = match user::remote_mcp(&app.notify.http(), &f.domain, tok, "fetch-doc", &a).await {
                Ok(r) => r,
                Err(e) => return Err(api_err(app, e).await),
            };
            let text: Vec<&str> = r["content"].as_array().into_iter().flatten().filter_map(|c| c["text"].as_str()).collect();
            let text = text.join("\n");
            if r["isError"] == true {
                return Err(fail("FEISHU_DOC", text));
            }
            Ok(serde_json::from_str::<Value>(&text).unwrap_or_else(|_| json!({ "markdown": text })))
        }
        _ => Err(fail("UNKNOWN_TOOL", name)),
    }
}

/// 存下来的文件名：给了就用（去掉路径），图片按内容猜扩展名
fn file_name(given: &str, key: &str, bytes: &[u8]) -> String {
    let clean: String = given.rsplit(['/', '\\']).next().unwrap_or_default().chars().filter(|c| !c.is_control()).take(120).collect();
    let clean = clean.trim().trim_start_matches('.').to_string();
    if !clean.is_empty() {
        return clean;
    }
    let ext = match bytes {
        [0x89, b'P', b'N', b'G', ..] => "png",
        [0xFF, 0xD8, 0xFF, ..] => "jpg",
        [b'G', b'I', b'F', ..] => "gif",
        [b'R', b'I', b'F', b'F', _, _, _, _, b'W', b'E', b'B', b'P', ..] => "webp",
        [b'%', b'P', b'D', b'F', ..] => "pdf",
        _ => "bin",
    };
    format!("{key}.{ext}")
}

async fn read_messages(app: &App, f: &Feishu, tok: &str, args: &Value) -> Result<Value, Value> {
    let order = if arg(args, "order") == "oldest_first" { "ByCreateTimeAsc" } else { "ByCreateTimeDesc" };
    let size = page_size(args, 50, 30);
    let page = arg(args, "page_token");
    let d = if !arg(args, "message_id").is_empty() {
        let id = id_arg(args, "message_id")?;
        get(app, f, tok, &format!("/open-apis/im/v1/messages/{id}")).await?
    } else if !arg(args, "thread_id").is_empty() {
        let id = id_arg(args, "thread_id")?;
        let q = [("container_id_type", "thread"), ("container_id", id.as_str()), ("sort_type", order), ("page_size", size.as_str()), ("page_token", page)];
        get(app, f, tok, &with_query("/open-apis/im/v1/messages", &q)).await?
    } else {
        let chat = if !arg(args, "user_id").is_empty() {
            let who = id_arg(args, "user_id")?;
            let r = post(app, f, tok, "/open-apis/im/v1/chat_p2p/batch_query?user_id_type=open_id", &json!({ "chatter_ids": [who] })).await?;
            r["p2p_chats"][0]["chat_id"].as_str().map(String::from).ok_or_else(|| fail("NOT_FOUND", "No direct chat with this person yet."))?
        } else {
            id_arg(args, "chat_id").map_err(|_| fail("INVALID_ARGUMENT", "Give one of chat_id, user_id, thread_id or message_id"))?
        };
        let (start, end) = (seconds(arg(args, "start_time"))?, seconds(arg(args, "end_time"))?);
        let q = [
            ("container_id_type", "chat"),
            ("container_id", chat.as_str()),
            ("sort_type", order),
            ("page_size", size.as_str()),
            ("start_time", start.as_str()),
            ("end_time", end.as_str()),
            ("page_token", page),
        ];
        let mut d = get(app, f, tok, &with_query("/open-apis/im/v1/messages", &q)).await?;
        d["chat_id"] = json!(chat);
        d
    };
    let items = d["items"].as_array().cloned().unwrap_or_default();
    names(app, f, tok, &items).await;
    let known = lock(&app.notify.tools.names).clone();
    let messages: Vec<Value> = items.iter().map(|m| message_view(m, &known)).collect();
    Ok(json!({ "chat_id": d["chat_id"], "messages": messages, "has_more": d["has_more"], "page_token": d["page_token"] }))
}

/// 查出发消息的人的名字（存起来，查不到不要紧）
async fn names(app: &App, f: &Feishu, tok: &str, items: &[Value]) {
    let me = &app.notify.tools;
    let mut ask: Vec<String> = Vec::new();
    {
        let mut known = lock(&me.names);
        for m in items {
            for x in m["mentions"].as_array().into_iter().flatten() {
                if let (Some(id), Some(n)) = (x["id"].as_str(), x["name"].as_str()) {
                    known.insert(id.into(), n.into());
                }
            }
            if let Some(id) = m["sender"]["id"].as_str().filter(|_| m["sender"]["sender_type"] == "user") {
                if !known.contains_key(id) && !ask.iter().any(|a| a == id) {
                    ask.push(id.into());
                }
            }
        }
    }
    for chunk in ask.chunks(50) {
        let mut q: Vec<(&str, &str)> = chunk.iter().map(|id| ("user_ids", id.as_str())).collect();
        q.push(("user_id_type", "open_id"));
        match user::call(&app.notify.http(), &f.domain, tok, reqwest::Method::GET, &with_query("/open-apis/contact/v3/users/batch", &q), &Value::Null).await {
            Ok(d) => {
                let mut known = lock(&me.names);
                for u in d["items"].as_array().into_iter().flatten() {
                    if let (Some(id), Some(n)) = (u["open_id"].as_str(), u["name"].as_str()) {
                        known.insert(id.into(), n.into());
                    }
                }
            }
            Err(e) => tracing::debug!(error = %e, "飞书：没查到发消息的人的名字"),
        }
    }
    let mut known = lock(&me.names);
    if known.len() > 5000 {
        known.clear();
    }
}

/// 开放接口的一条消息 → 给 AI 看的样子
fn message_view(m: &Value, names: &HashMap<String, String>) -> Value {
    let e = json!({ "message_id": m["message_id"], "message_type": m["msg_type"], "content": m["body"]["content"] });
    let content: Value = m["body"]["content"].as_str().and_then(|c| serde_json::from_str(c).ok()).unwrap_or_default();
    let mut text = match m["msg_type"].as_str().unwrap_or_default() {
        "text" | "post" => crate::feishu_agent::message_text(&e),
        "image" => "[image]".into(),
        "file" => format!("[file] {}", content["file_name"].as_str().unwrap_or_default()),
        "media" => format!("[video] {}", content["file_name"].as_str().unwrap_or_default()),
        "audio" => "[voice message]".into(),
        "sticker" => "[sticker]".into(),
        "merge_forward" => "[forwarded chat history — read it with feishu_read_messages message_id]".into(),
        "interactive" => format!("[card] {}", card_text(&content)),
        "share_chat" => format!("[shared chat] {}", content["chat_id"].as_str().unwrap_or_default()),
        "share_user" => format!("[shared contact] {}", content["user_id"].as_str().unwrap_or_default()),
        "system" => "[system message]".into(),
        t => format!("[{t}]"),
    };
    for x in m["mentions"].as_array().into_iter().flatten() {
        if let Some(key) = x["key"].as_str().filter(|k| !k.is_empty()) {
            text = text.replace(key, &format!("@{}", x["name"].as_str().unwrap_or_default()));
        }
    }
    if text.chars().count() > MSG_CHARS {
        text = text.chars().take(MSG_CHARS).collect::<String>() + "…";
    }
    let sender = m["sender"]["id"].as_str().unwrap_or_default();
    let from = match m["sender"]["sender_type"].as_str() {
        Some("user") => json!({ "open_id": sender, "name": names.get(sender) }),
        Some(t) => json!({ "type": t, "id": sender }),
        None => Value::Null,
    };
    let files: Vec<Value> = crate::feishu_agent::attachments(&e).into_iter().map(|r| json!({ "type": r.kind, "key": r.key, "name": r.name })).collect();
    let mut v = json!({ "message_id": m["message_id"], "time": iso(m["create_time"].as_str().unwrap_or_default()), "from": from, "text": text });
    for (k, src) in [("thread_id", "thread_id"), ("reply_to", "parent_id"), ("in_forward", "upper_message_id")] {
        if let Some(s) = m[src].as_str().filter(|s| !s.is_empty()) {
            v[k] = json!(s);
        }
    }
    if !files.is_empty() {
        v["files"] = json!(files);
    }
    if m["deleted"] == true {
        v["deleted"] = json!(true);
    }
    v
}

/// 卡片消息里的文字（标题、正文），最多 500 字
fn card_text(c: &Value) -> String {
    fn walk(v: &Value, out: &mut Vec<String>) {
        match v {
            Value::Object(m) => {
                for (k, x) in m {
                    match (k.as_str(), x) {
                        ("content" | "text" | "title", Value::String(s)) if !s.trim().is_empty() => out.push(s.trim().into()),
                        _ => walk(x, out),
                    }
                }
            }
            Value::Array(a) => a.iter().for_each(|x| walk(x, out)),
            _ => {}
        }
    }
    let mut out = Vec::new();
    walk(c, &mut out);
    out.join(" ").chars().take(500).collect()
}

// ---------------- 以你的身份发消息 ----------------

async fn send(app: &App, args: &Value) -> Result<Value, Value> {
    let text = args.get("text").and_then(Value::as_str).unwrap_or_default().trim().to_string();
    if text.is_empty() || text.chars().count() > TEXT_MAX {
        return Err(fail("INVALID_ARGUMENT", format!("`text` must be 1-{TEXT_MAX} characters")));
    }
    let reply_to = if arg(args, "reply_to").is_empty() { None } else { Some(id_arg(args, "reply_to")?) };
    let to = if reply_to.is_some() && arg(args, "to").is_empty() { String::new() } else { id_arg(args, "to")? };
    let kind = if to.starts_with("oc_") {
        "chat_id"
    } else if to.starts_with("ou_") {
        "open_id"
    } else if to.is_empty() {
        ""
    } else {
        return Err(fail("INVALID_ARGUMENT", "`to` must be a chat_id (oc_…) or an open_id (ou_…)"));
    };
    let in_thread = args.get("in_thread").and_then(Value::as_bool).unwrap_or(false);
    // 先确认授权了（没授权先授权，免得用户点了“发送”才发现发不出去）
    let (f, tok) = token(app).await?;
    let owner = f.open_id.clone().unwrap_or_default();
    let label = target_label(app, &f, &tok, &to, reply_to.as_deref()).await;
    let own = app.account.session().map(|s| s.device_id).unwrap_or_default();
    let device = crate::settings::Settings::load(&app.store).device_name();
    let en = en(app);
    let n = notify::nonce();
    let (tx, rx) = oneshot::channel();
    lock(&app.notify.tools.pending).insert(n.clone(), tx);
    let api = app.notify.api(&f);
    let msg = match api.send_card(&owner, &card::send_ask(en, &own, &n, &label, &text, &device)).await {
        Ok(m) => m,
        Err(e) => {
            lock(&app.notify.tools.pending).remove(&n);
            return Err(fail("FEISHU_API", format!("Could not ask the user in Feishu: {e}")));
        }
    };
    let (done_tx, done_rx) = oneshot::channel();
    let app2 = app.clone();
    tokio::spawn(async move {
        let decided = tokio::time::timeout(SEND_TTL, rx).await;
        lock(&app2.notify.tools.pending).remove(&n);
        let (result, state, ok) = match decided {
            Ok(Ok(true)) => match deliver(&app2, &to, kind, reply_to.as_deref(), in_thread, &text).await {
                Ok(id) => (Ok(json!({ "status": "sent", "message_id": id })), tr(en, "已发送", "sent"), true),
                Err(v) => {
                    let why = v["message"].as_str().unwrap_or_default().to_string();
                    (Err(v), tr(en, &format!("没发出去：{why}"), &format!("failed: {why}")), false)
                }
            },
            Ok(Ok(false)) => (Err(fail("DECLINED", "The user chose not to send this message. Do not send it again unless they ask.")), tr(en, "没发", "not sent"), false),
            _ => (Err(fail("EXPIRED", "The user did not approve the message in time; it was not sent.")), tr(en, "过期了，没发", "expired, not sent"), false),
        };
        if let Err(e) = app2.notify.api(&f).patch_card(&msg, &card::send_done(en, &label, &text, &state, ok)).await {
            tracing::debug!(error = %e, "飞书：发消息的卡片没改上");
        }
        let _ = done_tx.send(result);
    });
    match tokio::time::timeout(SEND_WAIT, done_rx).await {
        Ok(Ok(r)) => r,
        _ => Ok(json!({
            "status": "waiting_for_user",
            "message": "The user has been asked in Feishu to approve this message. It will be sent automatically if they approve within 30 minutes. Do not send it again.",
        })),
    }
}

/// 确认卡片上“发给谁”：群名、人名，查不到就用 id
async fn target_label(app: &App, f: &Feishu, tok: &str, to: &str, reply_to: Option<&str>) -> String {
    let http = app.notify.http();
    let got = if let Some(m) = reply_to {
        let d = user::call(&http, &f.domain, tok, reqwest::Method::GET, &format!("/open-apis/im/v1/messages/{m}"), &Value::Null).await.ok();
        let item = d.as_ref().map(|d| &d["items"][0]);
        let quoted = item.map(|i| message_view(i, &HashMap::new())["text"].as_str().unwrap_or_default().chars().take(60).collect::<String>()).unwrap_or_default();
        let chat = match item.and_then(|i| i["chat_id"].as_str()) {
            Some(c) => chat_name(app, f, tok, c).await,
            None => None,
        };
        Some(format!("{}（回复「{quoted}」）", chat.unwrap_or_default()))
    } else if to.starts_with("oc_") {
        chat_name(app, f, tok, to).await
    } else {
        let d = user::call(&http, &f.domain, tok, reqwest::Method::GET, &format!("/open-apis/contact/v3/users/{to}?user_id_type=open_id"), &Value::Null).await.ok();
        d.and_then(|d| d["user"]["name"].as_str().map(String::from))
    };
    match got.filter(|s| !s.trim().is_empty()) {
        Some(s) => s,
        None => reply_to.unwrap_or(to).to_string(),
    }
}

async fn chat_name(app: &App, f: &Feishu, tok: &str, chat: &str) -> Option<String> {
    let d = user::call(&app.notify.http(), &f.domain, tok, reqwest::Method::GET, &format!("/open-apis/im/v1/chats/{chat}"), &Value::Null).await.ok()?;
    d["name"].as_str().filter(|n| !n.is_empty()).map(String::from)
}

/// 真的发出去（用户点了“发送”以后；令牌可能在等的时候过期，重新拿一次）
async fn deliver(app: &App, to: &str, kind: &str, reply_to: Option<&str>, in_thread: bool, text: &str) -> Result<String, Value> {
    let (f, tok) = token(app).await?;
    let content = json!({ "zh_cn": { "content": [[{ "tag": "md", "text": text }]] } }).to_string();
    let d = match reply_to {
        Some(m) => post(app, &f, &tok, &format!("/open-apis/im/v1/messages/{m}/reply"), &json!({ "msg_type": "post", "content": content, "reply_in_thread": in_thread })).await?,
        None => post(app, &f, &tok, &format!("/open-apis/im/v1/messages?receive_id_type={kind}"), &json!({ "receive_id": to, "msg_type": "post", "content": content })).await?,
    };
    Ok(d["message_id"].as_str().unwrap_or_default().into())
}

/// 卡片上点了“发送 / 不发”（已经核对过是绑定的人）。消息在别的电脑上等着就转过去
pub async fn on_card(app: &App, act: &Act, operator: &str) -> Value {
    let en = en(app);
    let ok = act.a == SEND;
    let own = app.account.session().map(|s| s.device_id).unwrap_or_default();
    if act.dev == own {
        return if decide(app, &act.n, ok) {
            notify::toast("success", &if ok { tr(en, "好，正在发送", "Sending…") } else { tr(en, "好，不发", "OK, not sending") })
        } else {
            notify::toast("info", &tr(en, "已经处理过了，或者过期了", "Already handled or expired"))
        };
    }
    let (app, dev, n, operator) = (app.clone(), act.dev.clone(), act.n.clone(), operator.to_string());
    tokio::spawn(async move {
        if let Err(e) = crate::feishu_agent::forward_send(&app, &dev, &n, ok, &operator).await {
            tracing::warn!(error = ?e, dev, "飞书：发消息的确认没转给那台电脑");
        }
    });
    notify::toast("info", &tr(en, "已转给发起的电脑", "Passed to the computer that asked"))
}

/// 等着的那条消息：发还是不发。没找到（处理过、过期、电脑重启过）返回 false
pub fn decide(app: &App, n: &str, ok: bool) -> bool {
    match lock(&app.notify.tools.pending).remove(n) {
        Some(tx) => tx.send(ok).is_ok(),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_and_times() {
        assert_eq!(with_query("/open-apis/im/v1/chats/search", &[("query", "看看 群"), ("page_token", "")]), "/open-apis/im/v1/chats/search?query=%E7%9C%8B%E7%9C%8B+%E7%BE%A4");
        assert_eq!(with_query("/a", &[("x", "")]), "/a");
        assert_eq!(seconds("2026-10-29T09:00:00+08:00").unwrap(), "1793235600");
        assert_eq!(seconds("1793235600000").unwrap(), "1793235600");
        assert!(seconds("yesterday").is_err());
        assert_eq!(iso("1793235600123"), "2026-10-29T01:00:00Z");
    }

    #[test]
    fn messages_for_ai() {
        let m = json!({
            "message_id": "om_1", "msg_type": "text", "create_time": "1793235600000", "thread_id": "omt_1",
            "body": { "content": "{\"text\":\"@_user_1 看下这个\"}" },
            "mentions": [{ "key": "@_user_1", "id": "ou_b", "name": "小王" }],
            "sender": { "id": "ou_a", "sender_type": "user" },
        });
        let names = HashMap::from([("ou_a".to_string(), "小李".to_string())]);
        let v = message_view(&m, &names);
        assert_eq!(v["text"], "@小王 看下这个");
        assert_eq!(v["from"]["name"], "小李");
        assert_eq!(v["thread_id"], "omt_1");
        let file = json!({ "message_id": "om_2", "msg_type": "file", "body": { "content": "{\"file_key\":\"file_x\",\"file_name\":\"a.pdf\"}" }, "sender": { "id": "cli_1", "sender_type": "app" } });
        let v = message_view(&file, &names);
        assert_eq!((v["text"].as_str(), v["files"][0]["key"].as_str(), v["from"]["type"].as_str()), (Some("[file] a.pdf"), Some("file_x"), Some("app")));
        let card = json!({ "message_id": "om_3", "msg_type": "interactive", "body": { "content": "{\"header\":{\"title\":{\"content\":\"发布\"}},\"elements\":[{\"tag\":\"div\",\"text\":{\"content\":\"好了\"}}]}" } });
        assert!(message_view(&card, &names)["text"].as_str().unwrap().contains("发布"));
    }

    #[test]
    fn saved_names() {
        assert_eq!(file_name("../../etc/passwd", "k", b""), "passwd");
        assert_eq!(file_name("", "img_1", &[0x89, b'P', b'N', b'G']), "img_1.png");
        assert_eq!(file_name("..", "f", b"%PDF"), "f.pdf");
        assert_eq!(file_name(".bashrc", "f", b""), "bashrc");
    }
}
