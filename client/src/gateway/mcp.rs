//! MCP 接口 `POST /mcp`（Streamable HTTP，只回 JSON，不开 SSE 流）：让 Codex、Claude Code、OpenCode、DSH 等 AI 助手
//! 在开发时自己查看、创建本机网页（隧道）。
//!
//! - 默认关闭；打开后生成一个**单独的** MCP 令牌（不是访问码），请求要带 `Authorization: Bearer <令牌>`。
//!   令牌只存在本机数据库（`mcp` 键），可以随时换新，换新后旧令牌立即失效。
//! - 只接受**这台机器自己**发来的请求：局域网、白名单和远程入口一律当作不存在（404）。
//! - 浏览器发来的请求（带 `Origin`）必须是本站，挡住 DNS 重绑定。
//!
//! 工具：`list_mappings`、`find_mapping`（只查）、`ensure_mapping`（有就复用并启用，没有就创建）。
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
                "serverInfo": { "name": SERVER_NAME, "title": "Looklook local pages", "version": VERSION },
                "instructions": INSTRUCTIONS,
            })
        }
        "ping" => json!({}),
        "tools/list" => json!({ "tools": tool_defs() }),
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

const INSTRUCTIONS: &str = "Looklook publishes web pages running on this computer (e.g. a dev server on localhost:5173) at the user's private Looklook address, \
so they can open them from a phone or another computer. Only the signed-in owner can open these addresses. \
Use find_mapping to check whether a port already has an address, ensure_mapping to create one or reuse the existing one, list_mappings to see all. \
Each port has at most one mapping; the account has a small limit (usually 10). Looklook forwards to 127.0.0.1 only.";

fn tool_defs() -> Value {
    let port = json!({ "type": "integer", "minimum": 1, "maximum": 65535, "description": "Local TCP port of the HTTP server, e.g. 5173 for http://localhost:5173" });
    json!([
        {
            "name": "list_mappings",
            "title": "List local page mappings",
            "description": "List every local port already published through Looklook, with its public URL and whether it is enabled.",
            "inputSchema": { "type": "object", "properties": {} },
            "annotations": { "readOnlyHint": true },
        },
        {
            "name": "find_mapping",
            "title": "Find the mapping for a port",
            "description": "Check whether a local port already has a Looklook address. Never creates anything. Returns `mapping` (null if none) and `local` (whether the port currently answers HTTP on 127.0.0.1).",
            "inputSchema": { "type": "object", "properties": { "port": port }, "required": ["port"] },
            "annotations": { "readOnlyHint": true },
        },
        {
            "name": "ensure_mapping",
            "title": "Create or reuse a mapping",
            "description": "Publish a local HTTP port at the user's Looklook address. If the port is already mapped, the existing mapping is reused (and re-enabled if it was disabled) instead of creating a duplicate. Returns the access URL.",
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
        "NOT_LOGGED_IN" => "Looklook on this computer is not signed in. Ask the user to sign in from the Looklook console.",
        "LOCKED" => "The Looklook account cannot be used right now (membership expired, disabled or offline too long). Ask the user to check the Looklook console.",
        "NETWORK" => "Looklook cannot reach its server right now. Try again later.",
        "PORT_RESERVED" => "This port belongs to Looklook itself and cannot be published.",
        "TUNNEL_LIMIT_REACHED" => "The account already has the maximum number of mappings. Ask the user which one to delete in the Looklook console (Local pages).",
        "TUNNEL_PORT_INVALID" => "The port must be between 1 and 65535.",
        "VALIDATION_FAILED" => "The name is invalid (1-32 characters, no control characters).",
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
    super::api::require_allowed(app)?;
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
        let c = McpConfig { enabled: true, token: t.clone() };
        assert!(c.token_matches(&t));
        assert!(!c.token_matches("llmcp_x"));
        assert!(!McpConfig { enabled: false, token: t.clone() }.token_matches(&t));
        assert!(!McpConfig { enabled: true, token: String::new() }.token_matches(""));
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
