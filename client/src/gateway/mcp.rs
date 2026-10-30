//! MCP 接口 `POST /mcp`（Streamable HTTP，只回 JSON，不开 SSE 流）：让 Codex、Claude Code、OpenCode、DSH 等 AI 助手
//! 在开发时自己查看、创建本机网页（隧道）。
//!
//! - 默认关闭；打开后生成一个**单独的** MCP 令牌（不是访问码），请求要带 `Authorization: Bearer <令牌>`。
//!   令牌只存在本机数据库（`mcp` 键），可以随时换新，换新后旧令牌立即失效。
//! - 只接受**这台机器自己**发来的请求：局域网、白名单和远程入口一律当作不存在（404）。
//! - 浏览器发来的请求（带 `Origin`）必须是本站，挡住 DNS 重绑定。
//!
//! 工具：`list_mappings`、`find_mapping`（只查）、`ensure_mapping`（有就复用并启用，没有就创建）；
//! `list_ssh_hosts`、`ssh_run`：在用户标了“允许 AI 使用”的 SSH 主机上运行命令（ssh.rs）。AI 只给出主机名，
//! 登录用的密码 / 私钥由看看在本机提供，不经过 AI；返回的输出去掉已存密码、私钥块和常见令牌。
//! 返回里附带端口探测结果：本机网页只转发到 127.0.0.1，开发服务器只监听 `::1` 时要提醒改监听地址。

use std::time::Duration;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use looklook_protocol::dto::{CreateTunnel, Tunnel, UpdateTunnel};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::access::Peer;
use super::{origin_matches, Access, App};
use crate::account::VERSION;
use crate::error::LocalError;
use crate::store::Store;

const KEY: &str = "mcp";
pub const PATH: &str = "/mcp";
/// 写进 Codex / Claude Code / OpenCode / DSH 配置里的服务名
pub const SERVER_NAME: &str = "looklook";
const PROTOCOL_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct McpConfig {
    pub enabled: bool,
    /// 只存在本机数据库；关闭时保留，重新打开继续用（已装好的配置不用重装）
    pub token: String,
    /// 关掉 SSH 工具（`list_ssh_hosts`、`ssh_run`）：不出现在工具列表里，调用也拒绝。默认开着，但只能用标了“允许 AI 使用”的主机
    pub ssh_disabled: bool,
    /// 原来的飞书工具开关；飞书工具已经拿掉，这里不再用到（管理台接口还在读写，跟着那边一起删）
    pub feishu_disabled: bool,
}

impl McpConfig {
    pub fn load(store: &Store) -> Self {
        store.get(KEY).ok().flatten().unwrap_or_default()
    }

    pub fn save(&self, store: &Store) -> anyhow::Result<()> {
        store.set(KEY, self)
    }

    pub fn token_matches(&self, bearer: &str) -> bool {
        self.enabled && !self.token.is_empty() && super::ct_eq(bearer, &self.token)
    }
}

/// `llmcp_` + 32 个 base32 字符（160 位随机数）
pub fn generate_token() -> String {
    let bytes: [u8; 20] = rand::random();
    format!("llmcp_{}", data_encoding::BASE32_NOPAD.encode(&bytes).to_ascii_lowercase())
}

/// AI 助手连接用的地址。管理台监听 0.0.0.0 时这里已经换成了 127.0.0.1。
pub fn endpoint(app: &App) -> String {
    format!("http://{}{PATH}", app.ui_addr)
}

// ---------------- HTTP 入口 ----------------

pub async fn handle(State(app): State<App>, access: Option<Extension<Access>>, peer: Option<Extension<Peer>>, headers: HeaderMap, body: Bytes) -> Response {
    let local = access.is_none_or(|a| a.0.is_local()) && peer.is_none_or(|p| p.0 == Peer::Local);
    if !local {
        return StatusCode::NOT_FOUND.into_response();
    }
    if !origin_matches(&headers, &Access::Local) {
        return (StatusCode::FORBIDDEN, "origin").into_response();
    }
    let cfg = McpConfig::load(&app.store);
    let bearer = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer ").or_else(|| v.strip_prefix("bearer ")))
        .map(str::trim)
        .unwrap_or("");
    if !cfg.token_matches(bearer) {
        let why = if cfg.enabled { "invalid MCP token; reinstall Looklook MCP from the Looklook console (Local pages → AI assistants)" } else { "Looklook MCP is turned off; turn it on in the Looklook console (Local pages → AI assistants)" };
        return (StatusCode::UNAUTHORIZED, Json(json!({ "error": why }))).into_response();
    }
    let msg: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => return Json(rpc_error(Value::Null, -32700, "parse error")).into_response(),
    };
    match msg {
        Value::Array(list) => {
            let mut out = Vec::new();
            for m in list {
                if let Some(r) = dispatch(&app, m).await {
                    out.push(r);
                }
            }
            if out.is_empty() {
                StatusCode::ACCEPTED.into_response()
            } else {
                Json(Value::Array(out)).into_response()
            }
        }
        m => match dispatch(&app, m).await {
            Some(r) => Json(r).into_response(),
            // 通知与响应不需要回复
            None => StatusCode::ACCEPTED.into_response(),
        },
    }
}

/// 不提供 SSE 流，也没有会话可以删除。
pub async fn not_allowed() -> Response {
    (StatusCode::METHOD_NOT_ALLOWED, [(header::ALLOW, "POST")]).into_response()
}

fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

async fn dispatch(app: &App, msg: Value) -> Option<Value> {
    let id = msg.get("id").cloned()?;
    let Some(method) = msg.get("method").and_then(Value::as_str) else {
        // 客户端发来的响应（我们不发请求，不会有）
        return None;
    };
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    let result = match method {
        "initialize" => {
            let asked = params.get("protocolVersion").and_then(Value::as_str).unwrap_or("");
            let version = PROTOCOL_VERSIONS.iter().find(|v| **v == asked).copied().unwrap_or(PROTOCOL_VERSIONS[0]);
            json!({
                "protocolVersion": version,
                "capabilities": { "tools": { "listChanged": false } },
                "serverInfo": { "name": SERVER_NAME, "title": "Looklook: local pages and SSH servers", "version": VERSION },
                "instructions": instructions(app, &McpConfig::load(&app.store), params.pointer("/clientInfo/name").and_then(Value::as_str).unwrap_or("")),
            })
        }
        "ping" => json!({}),
        "tools/list" => {
            let c = McpConfig::load(&app.store);
            json!({ "tools": tool_defs(!c.ssh_disabled) })
        }
        "tools/call" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let args = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
            match call_tool(app, name, &args).await {
                Ok(v) => tool_result(v, false),
                Err(ToolError::Unknown) => return Some(rpc_error(id, -32602, &format!("unknown tool: {name}"))),
                Err(ToolError::Failed(v)) => tool_result(v, true),
            }
        }
        _ => return Some(rpc_error(id, -32601, "method not found")),
    };
    Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
}

fn tool_result(v: Value, is_error: bool) -> Value {
    let text = serde_json::to_string_pretty(&v).unwrap_or_default();
    json!({ "content": [{ "type": "text", "text": text }], "structuredContent": v, "isError": is_error })
}

/// 连上时给 AI 助手的总说明。Claude Code 只把工具名放进上下文、用到时才去查说明，所以这里要写清
/// “什么时候该用”，并按管理台里的设置写：这个助手的本机网页用法（自动 / 先问 / 只在要求时）、
/// 远程访问能不能用、能用的 SSH 主机名。
fn instructions(app: &App, c: &McpConfig, client: &str) -> String {
    let agent = crate::agents::from_client_name(client);
    let pages = Pages { skill: crate::agents::pages_skill(agent), remote: app.account.gate().reason().map(str::to_string) };
    let hosts = (!c.ssh_disabled).then(|| crate::ssh::list(&app.store).into_iter().filter(|h| h.allow_ai).map(|h| h.name).collect::<Vec<_>>());
    instructions_text(&pages, hosts.as_deref())
}

struct Pages {
    skill: Option<crate::agents::Skill>,
    /// 远程访问用不了的原因（`NOT_LOGGED_IN`、`MEMBERSHIP_EXPIRED` 等）；能用是 `None`
    remote: Option<String>,
}

/// `hosts`：SSH 工具关掉时是 `None`
fn instructions_text(pages: &Pages, hosts: Option<&[String]>) -> String {
    use crate::agents::Skill;
    let mut s = String::from(
        "Looklook runs on this computer. Use these tools on your own when the situations below come up; the user will not name Looklook.\n\
\n\
Local pages (*_mapping): gives a local HTTP server (dev server on localhost:5173, API on :8080) a private address the owner can open from \
a phone or another computer, instead of ngrok, frp or `--host 0.0.0.0`. One mapping per port; forwards to 127.0.0.1 only. ",
    );
    let asked = "the user wants to open a local page elsewhere (\"on my phone\", \"remotely\", \"手机上看\", \"远程打开\", \"外网访问\")";
    match (&pages.remote, pages.skill) {
        (Some(r), _) => s.push_str(&format!(
            "Remote access is off right now ({r}): do not offer addresses yourself; if {asked}, tell them it needs Looklook remote access, which they can turn on in the Looklook console.\n"
        )),
        (None, Some(Skill::Auto)) => s.push_str(&format!(
            "User setting: automatic. When you start or notice a local web server (e.g. output shows http://localhost:5173), call ensure_mapping right away and give the URL, without asking; also when {asked}.\n"
        )),
        (None, Some(Skill::Ask)) => s.push_str(&format!(
            "User setting: ask first. When you start or notice a local web server, call find_mapping: give the address if there is one, otherwise ask once whether to add it and call ensure_mapping only if they agree. When {asked}, ensure_mapping directly.\n"
        )),
        (None, None) => s.push_str(&format!(
            "User setting: only on request. Do not bring it up yourself; when {asked}, call ensure_mapping and give the URL.\n"
        )),
    }
    if let Some(names) = hosts {
        s.push_str(
            "\nSSH servers (list_ssh_hosts, ssh_run): Looklook logs in to the user's saved servers for you. When a task needs a remote server \
(deploy, logs, disk, services, \"on the server\", \"服务器上\", \"线上\") or the user names a host below, use ssh_run, not `ssh` in a terminal \
(it would ask for a password); never ask for passwords or keys. Some hosts run Windows (cmd/PowerShell).",
        );
        if names.is_empty() {
            s.push_str(" No host is allowed for AI yet; the user can allow hosts in the Looklook console (SSH hosts).\n");
        } else {
            let shown: Vec<&str> = names.iter().take(12).map(String::as_str).collect();
            s.push_str(&format!(" Hosts you may use: {}{}.\n", shown.join(", "), if names.len() > shown.len() { ", …" } else { "" }));
        }
    }
    s
}

/// SSH 工具的名字（总开关关掉时从工具列表里拿掉）
const SSH_TOOLS: [&str; 2] = ["list_ssh_hosts", "ssh_run"];

/// 给管理台“AI 助手”页列出的工具：名字和分组（`pages` / `ssh`），说明由界面翻译。
pub fn tool_names() -> Value {
    let all = tool_defs(true);
    let items = all.as_array().map(Vec::as_slice).unwrap_or_default();
    let group = |n: &str| if SSH_TOOLS.contains(&n) { "ssh" } else { "pages" };
    Value::Array(items.iter().filter_map(|t| t["name"].as_str()).map(|n| json!({ "name": n, "group": group(n) })).collect())
}

fn tool_defs(ssh: bool) -> Value {
    let mut defs = all_tool_defs();
    if let Some(a) = defs.as_array_mut() {
        if !ssh {
            a.retain(|t| !t["name"].as_str().is_some_and(|n| SSH_TOOLS.contains(&n)));
        }
    }
    defs
}

fn all_tool_defs() -> Value {
    let port = json!({ "type": "integer", "minimum": 1, "maximum": 65535, "description": "Local TCP port of the HTTP server, e.g. 5173 for http://localhost:5173" });
    json!([
        {
            "name": "list_mappings",
            "title": "List local page mappings",
            "description": "List every local port already published through Looklook, with its access URL and whether it is enabled. Use when the user asks which local pages they have, or when the account is full.",
            "inputSchema": { "type": "object", "properties": {} },
            "annotations": { "readOnlyHint": true },
        },
        {
            "name": "find_mapping",
            "title": "Find the mapping for a port",
            "description": "Check whether a local web server's port (a dev server you started, http://localhost:PORT in output, a port the user mentions) already has a Looklook address the user can open from a phone or another computer; when to check on your own follows the user's setting in the server instructions. Never creates anything. Returns `mapping` (null if none) and `local` (whether the port currently answers HTTP on 127.0.0.1).",
            "inputSchema": { "type": "object", "properties": { "port": port }, "required": ["port"] },
            "annotations": { "readOnlyHint": true },
        },
        {
            "name": "ensure_mapping",
            "title": "Create or reuse a mapping",
            "description": "Give a local HTTP server a private Looklook address so the user can open it from a phone or another computer (instead of ngrok, frp or binding 0.0.0.0). Use when the user wants to see or test a local page remotely, or on your own as the user's setting in the server instructions says. If the port is already mapped, the existing mapping is reused (and re-enabled if it was disabled) instead of creating a duplicate. Returns the access URL.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "port": port,
                    "name": { "type": "string", "maxLength": 32, "description": "Short label shown in the Looklook console, e.g. the project name. Defaults to localhost:<port>. Ignored when reusing." }
                },
                "required": ["port"]
            },
            "annotations": { "readOnlyHint": false, "destructiveHint": false, "idempotentHint": true },
        },
        {
            "name": "list_ssh_hosts",
            "title": "List SSH hosts you may use",
            "description": "List the SSH servers the user saved in Looklook and allowed AI assistants to use. Call it whenever a task involves a remote server (deploy, logs, production, \"the server\") and you are not sure which host is meant. Only names and user@host:port are returned — never passwords or keys.",
            "inputSchema": { "type": "object", "properties": {} },
            "annotations": { "readOnlyHint": true },
        },
        {
            "name": "ssh_run",
            "title": "Run a command on an SSH host",
            "description": "Run one non-interactive shell command on a saved SSH host (by name from list_ssh_hosts) and return exit_code, stdout and stderr (each capped at 64 KB). Use this instead of `ssh` in a terminal for anything on the user's servers: checking services, logs, disk, deploys, config. Looklook logs in with the user's stored credentials; you never see or need them. Known secrets, private keys and common tokens are redacted from the output. Commands that wait for input will time out.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "host": { "type": "string", "description": "Host name as shown by list_ssh_hosts" },
                    "command": { "type": "string", "description": "Shell command to run on the host, e.g. `df -h` or `systemctl status nginx --no-pager`" },
                    "timeout_seconds": { "type": "integer", "minimum": 1, "maximum": 600, "description": "Stop the command after this many seconds (default 60)" }
                },
                "required": ["host", "command"]
            },
            "annotations": { "readOnlyHint": false, "destructiveHint": true, "openWorldHint": true },
        },
    ])
}

// ---------------- 工具 ----------------

enum ToolError {
    Unknown,
    Failed(Value),
}

impl From<LocalError> for ToolError {
    fn from(e: LocalError) -> Self {
        ToolError::Failed(json!({ "error": e.code, "params": e.params, "message": explain(&e.code) }))
    }
}

/// 给 AI 助手看的错误说明（它会转述给用户）
fn explain(code: &str) -> &'static str {
    match code {
        "NOT_LOGGED_IN" => "Opening local pages from outside is part of Looklook remote access, which needs a Looklook account (new accounts get their first year free). Tell the user they can turn it on from the Looklook console; everything else keeps working without signing in.",
        "LOCKED" => "Looklook remote access is paused (membership expired, account disabled, or offline too long). Tell the user they can extend their membership in the Looklook console (Membership: redeem a renewal code or invite friends). Everything on this computer keeps working.",
        "NETWORK" => "Looklook cannot reach its server right now. Try again later.",
        "PORT_RESERVED" => "This port belongs to Looklook itself and cannot be published.",
        "TUNNEL_LIMIT_REACHED" => "The account already has the maximum number of mappings. Ask the user which one to delete in the Looklook console (Local pages).",
        "TUNNEL_PORT_INVALID" => "The port must be between 1 and 65535.",
        "VALIDATION_FAILED" => "The name is invalid (1-32 characters, no control characters).",
        "SSH_AI_DISABLED" => "The user has turned off SSH access for AI assistants in the Looklook console (AI assistants page). Do not retry; tell the user if they need it.",
        "SSH_HOST_NOT_ALLOWED" => "No SSH host with that name is available to AI assistants. Call list_ssh_hosts; the user can allow more hosts in the Looklook console (SSH hosts).",
        "SSH_MISSING" => "The OpenSSH client (ssh) is not installed on this computer.",
        "SSH_FAILED" => "Looklook could not start the SSH connection.",
        _ => "The Looklook operation failed.",
    }
}

async fn call_tool(app: &App, name: &str, args: &Value) -> Result<Value, ToolError> {
    match name {
        "list_mappings" => {
            let list = tunnels(app).await?;
            Ok(json!({ "max": list.max, "count": list.items.len(), "mappings": list.items.iter().map(view).collect::<Vec<_>>() }))
        }
        "find_mapping" => {
            let port = port_arg(args)?;
            let list = tunnels(app).await?;
            let found = list.items.iter().find(|t| i64::from(t.target_port) == i64::from(port));
            Ok(json!({ "port": port, "mapping": found.map(view), "local": probe(port).await, "count": list.items.len(), "max": list.max }))
        }
        "ensure_mapping" => {
            let port = port_arg(args)?;
            let name = args.get("name").and_then(Value::as_str).map(clean_name).filter(|n| !n.is_empty()).unwrap_or_else(|| format!("localhost:{port}"));
            let (t, created, reenabled) = ensure(app, port, &name).await?;
            Ok(json!({ "port": port, "mapping": view(&t), "created": created, "reused": !created, "re_enabled": reenabled, "local": probe(port).await }))
        }
        n if SSH_TOOLS.contains(&n) && McpConfig::load(&app.store).ssh_disabled => Err(LocalError::new("SSH_AI_DISABLED").into()),
        "list_ssh_hosts" => {
            let all = crate::ssh::list(&app.store);
            let via = |h: &crate::ssh::SshHost| all.iter().find(|j| j.id == h.jump).map(|j| j.name.clone());
            let hosts: Vec<Value> = all.iter().filter(|h| h.allow_ai).map(|h| json!({ "name": h.name, "target": format!("{}@{}:{}", h.user, h.host, h.port), "via": via(h) })).collect();
            Ok(json!({ "hosts": hosts, "note": "Use ssh_run with a host name. Credentials are supplied by Looklook and are never shown to you; do not ask the user for passwords or keys." }))
        }
        "ssh_run" => {
            let name = args.get("host").and_then(Value::as_str).unwrap_or_default();
            let command = args.get("command").and_then(Value::as_str).unwrap_or_default().trim().to_string();
            if command.is_empty() || command.len() > 8000 {
                return Err(LocalError::invalid("command", "length").into());
            }
            let secs = args.get("timeout_seconds").and_then(Value::as_u64).unwrap_or(60).clamp(1, 600);
            let h = crate::ssh::find(&app.store, name).filter(|h| h.allow_ai).ok_or_else(|| LocalError::new("SSH_HOST_NOT_ALLOWED").with("host", name))?;
            tracing::info!(host = %h.name, "MCP 在 SSH 主机上运行命令");
            let r = crate::ssh::run(app.store.clone(), app.paths.home.clone(), h, command, Duration::from_secs(secs)).await?;
            Ok(serde_json::to_value(r).unwrap_or_default())
        }
        _ => Err(ToolError::Unknown),
    }
}

async fn tunnels(app: &App) -> Result<looklook_protocol::dto::TunnelList, LocalError> {
    if app.account.session().is_none() {
        return Err(LocalError::new("NOT_LOGGED_IN"));
    }
    Ok(app.account.tunnels().await?)
}

async fn ensure(app: &App, port: u16, name: &str) -> Result<(Tunnel, bool, bool), LocalError> {
    super::api::require_remote(app)?;
    super::api::check_tunnel_port(app, Some(i64::from(port)))?;
    let existing = |list: &looklook_protocol::dto::TunnelList| list.items.iter().find(|t| t.target_port == i32::from(port)).cloned();
    let list = tunnels(app).await?;
    let found = match existing(&list) {
        Some(t) => Some(t),
        None => match app.account.create_tunnel(&CreateTunnel { name: name.to_string(), target_port: port.into() }).await {
            Ok(t) => return Ok((t, true, false)),
            // 同时有别的请求刚建好了同一个端口
            Err(crate::platform::PlatformError::Api { code, .. }) if code == "TUNNEL_PORT_DUPLICATE" => existing(&tunnels(app).await?),
            Err(e) => return Err(e.into()),
        },
    };
    let t = found.ok_or_else(LocalError::not_found)?;
    if t.status == "enabled" {
        return Ok((t, false, false));
    }
    let req = UpdateTunnel { status: Some("enabled".into()), ..Default::default() };
    Ok((app.account.update_tunnel(&t.id, &req).await?, false, true))
}

fn view(t: &Tunnel) -> Value {
    json!({ "id": t.id, "name": t.name, "port": t.target_port, "url": t.url, "enabled": t.status == "enabled" })
}

fn port_arg(args: &Value) -> Result<u16, ToolError> {
    let p = args.get("port").and_then(|v| v.as_i64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok())));
    p.and_then(|p| u16::try_from(p).ok())
        .filter(|p| *p != 0)
        .ok_or_else(|| ToolError::Failed(json!({ "error": "TUNNEL_PORT_INVALID", "message": explain("TUNNEL_PORT_INVALID") })))
}

fn clean_name(s: &str) -> String {
    s.trim().chars().filter(|c| !c.is_control()).take(32).collect::<String>().trim().to_string()
}

/// 端口现在的情况：`http` 在 127.0.0.1 上回应 HTTP；`ipv6_only` 只有 `[::1]` 上有（本机网页打不开，
/// 要让开发服务器监听 127.0.0.1）；`not_http` 有程序在听但不是 HTTP；`closed` 没有程序在听。
async fn probe(port: u16) -> Value {
    let v4 = probe_at(&format!("127.0.0.1:{port}")).await;
    let state = match v4 {
        Probe::Http => "http",
        Probe::Other => "not_http",
        Probe::Closed => match probe_at(&format!("[::1]:{port}")).await {
            Probe::Closed => "closed",
            _ => "ipv6_only",
        },
    };
    let hint = match state {
        "ipv6_only" => Some("The server listens on [::1] only, but Looklook forwards to 127.0.0.1. Restart it bound to 127.0.0.1 (e.g. `vite --host 127.0.0.1`)."),
        "closed" => Some("Nothing is listening on this port yet; the address works once the server starts."),
        "not_http" => Some("Something is listening but did not answer HTTP; Looklook only publishes HTTP servers."),
        _ => None,
    };
    json!({ "state": state, "hint": hint })
}

enum Probe {
    Http,
    Other,
    Closed,
}

async fn probe_at(addr: &str) -> Probe {
    let attempt = async {
        let Ok(mut s) = tokio::net::TcpStream::connect(addr).await else { return Probe::Closed };
        if s.write_all(b"HEAD / HTTP/1.0\r\nHost: localhost\r\n\r\n").await.is_err() {
            return Probe::Other;
        }
        let mut buf = [0u8; 5];
        match s.read_exact(&mut buf).await {
            Ok(_) if &buf == b"HTTP/" => Probe::Http,
            _ => Probe::Other,
        }
    };
    tokio::time::timeout(Duration::from_millis(1500), attempt).await.unwrap_or(Probe::Other)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens() {
        let t = generate_token();
        assert!(t.starts_with("llmcp_") && t.len() == 38);
        assert_ne!(t, generate_token());
        let c = McpConfig { enabled: true, token: t.clone(), ..Default::default() };
        assert!(c.token_matches(&t));
        assert!(!c.token_matches("llmcp_x"));
        assert!(!McpConfig { enabled: false, token: t.clone(), ..Default::default() }.token_matches(&t));
        assert!(!McpConfig { enabled: true, token: String::new(), ..Default::default() }.token_matches(""));
    }

    #[test]
    fn ssh_switch_hides_ssh_tools() {
        let names = |v: Value| v.as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap().to_string()).collect::<Vec<_>>();
        assert!(names(tool_defs(true)).contains(&"ssh_run".to_string()));
        let off = names(tool_defs(false));
        assert!(!off.iter().any(|n| SSH_TOOLS.contains(&n.as_str())) && off.contains(&"ensure_mapping".to_string()));
        assert_eq!(tool_names().as_array().unwrap().iter().filter(|t| t["group"] == "ssh").count(), 2);
        assert!(!names(tool_defs(true)).iter().any(|n| n.starts_with("feishu_")));
        // 旧版本存的配置没有这个字段：SSH 工具照旧可用
        let old: McpConfig = serde_json::from_str(r#"{"enabled":true,"token":"x"}"#).unwrap();
        assert!(!old.ssh_disabled);
    }

    #[test]
    fn instructions_fit() {
        use crate::agents::Skill;
        let on = |skill| Pages { skill, remote: None };
        let hosts: Vec<String> = (0..30).map(|i| format!("prod-web-{i}")).collect();
        for p in [on(Some(Skill::Auto)), on(Some(Skill::Ask)), on(None), Pages { skill: None, remote: Some("MEMBERSHIP_EXPIRED".into()) }] {
            let full = instructions_text(&p, Some(&hosts));
            // Claude Code 只取前 2048 个字符
            assert!(full.chars().count() < 2048, "{}", full.chars().count());
            assert!(!full.to_lowercase().contains("feishu"));
        }
        let full = instructions_text(&on(Some(Skill::Auto)), Some(&hosts));
        assert!(full.contains("prod-web-11, …") && !full.contains("prod-web-12"));
        assert!(full.contains("without asking"));
        assert!(instructions_text(&on(None), None).contains("Do not bring it up yourself"));
        assert!(instructions_text(&Pages { skill: Some(Skill::Auto), remote: Some("NOT_LOGGED_IN".into()) }, None).contains("Remote access is off"));
        assert!(!instructions_text(&on(Some(Skill::Ask)), None).contains("ssh_run"));
        assert!(instructions_text(&on(None), Some(&[])).contains("No host is allowed"));
    }

    #[test]
    fn args() {
        assert_eq!(port_arg(&json!({ "port": 5173 })).ok(), Some(5173));
        assert_eq!(port_arg(&json!({ "port": "8080" })).ok(), Some(8080));
        assert!(port_arg(&json!({ "port": 0 })).is_err());
        assert!(port_arg(&json!({ "port": 70000 })).is_err());
        assert!(port_arg(&json!({})).is_err());
        assert_eq!(clean_name("  my\napp  "), "myapp");
        assert_eq!(clean_name(&"x".repeat(40)).len(), 32);
    }

    #[tokio::test]
    async fn probes() {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut s, _) = l.accept().await.unwrap();
            let mut buf = [0u8; 64];
            let _ = s.read(&mut buf).await;
            s.write_all(b"HTTP/1.0 200 OK\r\n\r\n").await.unwrap();
        });
        assert_eq!(probe(port).await["state"], "http");
        let closed = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap().local_addr().unwrap().port();
        assert_eq!(probe(closed).await["state"], "closed");
    }
}
