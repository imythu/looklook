//! 和 `dsh web` 说话（docs/DSH_UI.md §2）：DSH 官方网页版用的同一套接口，看看只是换成自己的对话界面。
//!
//! - 登录：`dsh web` 启动时输出的口令换一个 Cookie（`GET /?token=` → 303 + `Set-Cookie`），之后所有请求带着它；
//!   DSH 只认本机地址的 `Host`、不认别的 `Origin`，这里都不带 `Origin`。
//! - 单次调用：`POST /api/<命名空间>/<方法>`，正文 `{"type":"client-request","rpcId","method","payload":{"args"}}`，
//!   答复 `{"type":"server-response","rpcId","result":{"ok":true,"value"}|{"ok":false,"error"}}`。
//! - 推送：一条 WebSocket（`/api/remote.mux`）上开多个逻辑流（`open` / `item` / `end` / `cancel` / `error`）。
//!   每条连接先开 `$events`：第一条是 `ready`（带 `clientId`），之后是 `emit`（通知）和 `waterfall`（要看看答复的：
//!   审批、提问），答复走 `POST /api/$events/result`。
//!
//! 这里只管进程、连接和收发；会话、状态、审批在 `dsh_chat.rs`。连接断了：所有流收到 [`StreamMsg::Closed`]，
//! 接收方收到 [`Incoming::Closed`]，下次用到时重新连（DSH 不在了就重新找或者重新起）。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::{client::IntoClientRequest, http::HeaderValue, Message};

use crate::error::LocalError;

/// 普通调用等答复的上限（`session/prompt` 这些只是确认收到；`session/page` 要读记录）
const CALL_TIMEOUT: Duration = Duration::from_secs(60);
/// 连上之后多久之内要收到 `ready`（DSH 默认也是 15 秒）
const READY_WAIT: Duration = Duration::from_secs(15);
/// 自己起的 `dsh web` 多久之内要输出口令（第一次启动要装内置插件，慢机器上二三十秒）
const START_WAIT: Duration = Duration::from_secs(90);

/// 一个逻辑流收到的
#[derive(Debug)]
pub enum StreamMsg {
    Item(Value),
    End,
    Error(Value),
    /// 连接断了（流也跟着没了）
    Closed,
}

/// `$events` 上收到的
#[derive(Debug)]
pub enum Incoming {
    /// 要答复的（`approval/request`、`user-questions/request`）：答复用 [`DshConn::reply`]
    Waterfall { event: String, event_id: String, agent: String, request: Value },
    Emit { event: String, args: Vec<Value> },
    /// 连接断了
    Closed,
}

pub struct DshConn {
    pub port: u16,
    cookie: String,
    http: reqwest::Client,
    out: mpsc::UnboundedSender<Message>,
    /// 这条连接的 `$events` 给的标识：答复 waterfall 时要带上
    client_id: String,
    streams: Mutex<HashMap<String, mpsc::UnboundedSender<StreamMsg>>>,
    next: AtomicU64,
    alive: AtomicBool,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn err(code: &'static str, detail: impl std::fmt::Display) -> LocalError {
    LocalError::new(code).with("detail", detail.to_string().chars().take(500).collect::<String>())
}

impl DshConn {
    /// 用换到的 Cookie 连上 `dsh web`：打开 WebSocket、等到 `$events` 的 `ready`。
    pub async fn connect(port: u16, cookie: String, tx: mpsc::UnboundedSender<Incoming>) -> Result<Arc<Self>, LocalError> {
        let host = format!("127.0.0.1:{port}");
        let mut req = format!("ws://{host}/api/remote.mux").into_client_request().map_err(|e| err("DSH_CONNECT", e))?;
        req.headers_mut().insert("cookie", HeaderValue::from_str(&cookie).map_err(|e| err("DSH_CONNECT", e))?);
        let tcp = tokio::net::TcpStream::connect(&host).await.map_err(|e| err("DSH_CONNECT", e))?;
        let (ws, _) = tokio_tungstenite::client_async(req, tcp).await.map_err(|e| match e {
            tokio_tungstenite::tungstenite::Error::Http(r) if r.status() == 401 || r.status() == 403 => err("DSH_UNAUTHORIZED", r.status()),
            e => err("DSH_CONNECT", e),
        })?;
        let (mut sink, mut stream) = ws.split();
        let (out, mut out_rx) = mpsc::unbounded_channel::<Message>();
        tokio::spawn(async move {
            while let Some(m) = out_rx.recv().await {
                if sink.send(m).await.is_err() {
                    break;
                }
            }
            let _ = sink.close().await;
        });
        out.send(Message::text(json!({ "type": "open", "streamId": "ev", "endpoint": "$events", "payload": { "args": {} } }).to_string()))
            .map_err(|_| err("DSH_CONNECT", "closed"))?;
        // 第一条必须是 ready
        let first = tokio::time::timeout(READY_WAIT, async {
            while let Some(Ok(m)) = stream.next().await {
                if let Message::Text(t) = &m {
                    return serde_json::from_str::<Value>(t.as_str()).ok();
                }
            }
            None
        })
        .await
        .ok()
        .flatten();
        let client_id = match first.as_ref().map(|f| (&f["streamId"], &f["value"])) {
            Some((s, v)) if s == "ev" && v["type"] == "ready" => v["clientId"].as_str().unwrap_or_default().to_string(),
            _ => return Err(err("DSH_CONNECT", format!("no ready: {}", first.unwrap_or_default()))),
        };
        let http = reqwest::Client::builder().no_proxy().timeout(CALL_TIMEOUT).build().map_err(|e| err("DSH_CONNECT", e))?;
        let me = Arc::new(Self { port, cookie, http, out, client_id, streams: Mutex::new(HashMap::new()), next: AtomicU64::new(1), alive: AtomicBool::new(true) });
        let reader = me.clone();
        tokio::spawn(async move {
            while let Some(Ok(m)) = stream.next().await {
                let text = match &m {
                    Message::Text(t) => t.as_str(),
                    Message::Close(_) => break,
                    _ => continue,
                };
                let Ok(msg) = serde_json::from_str::<Value>(text) else { continue };
                reader.dispatch(msg, &tx);
            }
            reader.alive.store(false, Ordering::SeqCst);
            for (_, s) in lock(&reader.streams).drain() {
                let _ = s.send(StreamMsg::Closed);
            }
            let _ = tx.send(Incoming::Closed);
        });
        Ok(me)
    }

    fn dispatch(&self, msg: Value, tx: &mpsc::UnboundedSender<Incoming>) {
        let sid = msg["streamId"].as_str().unwrap_or_default();
        let ty = msg["type"].as_str().unwrap_or_default();
        if sid == "ev" {
            let v = &msg["value"];
            match (ty, v["type"].as_str()) {
                ("item", Some("waterfall")) => {
                    let _ = tx.send(Incoming::Waterfall {
                        event: v["event"].as_str().unwrap_or_default().to_string(),
                        event_id: v["eventId"].as_str().unwrap_or_default().to_string(),
                        agent: v["agentId"].as_str().unwrap_or_default().to_string(),
                        request: v["request"].clone(),
                    });
                }
                ("item", Some("emit")) => {
                    let args = v["args"].as_array().cloned().unwrap_or_default();
                    let _ = tx.send(Incoming::Emit { event: v["event"].as_str().unwrap_or_default().to_string(), args });
                }
                _ => {}
            }
            return;
        }
        let mut streams = lock(&self.streams);
        let msg = match ty {
            "item" => StreamMsg::Item(msg["value"].clone()),
            "end" => StreamMsg::End,
            "error" => StreamMsg::Error(msg["error"].clone()),
            _ => return,
        };
        let done = !matches!(msg, StreamMsg::Item(_));
        let gone = streams.get(sid).is_some_and(|s| s.send(msg).is_err());
        if done || gone {
            streams.remove(sid);
        }
    }

    pub fn alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }

    /// 单次调用：`endpoint` 是 `命名空间/方法`，`args` 按 DSH 的参数名（大多是 `{"request": {…}}`）。
    /// DSH 答 `ok: false` 时返回 `DSH_REQUEST_FAILED`（带上它的错误码和说明）。
    pub async fn call(&self, endpoint: &str, args: Value) -> Result<Value, LocalError> {
        let rpc = format!("ll-{}", self.next.fetch_add(1, Ordering::SeqCst));
        let body = json!({ "type": "client-request", "rpcId": rpc, "method": endpoint, "payload": { "args": args } });
        let r = self
            .http
            .post(format!("http://127.0.0.1:{}/api/{endpoint}", self.port))
            .header("cookie", &self.cookie)
            .header("content-type", "application/json")
            .body(body.to_string())
            .send()
            .await
            .map_err(|e| err("DSH_CONNECT", e))?;
        match r.status().as_u16() {
            200 => {}
            401 | 403 => return Err(err("DSH_UNAUTHORIZED", r.status())),
            s => return Err(err("DSH_REQUEST_FAILED", format!("{endpoint}: HTTP {s}"))),
        }
        let v: Value = r.json().await.map_err(|e| err("DSH_REQUEST_FAILED", e))?;
        let res = &v["result"];
        if res["ok"] == true {
            return Ok(res.get("value").cloned().unwrap_or(Value::Null));
        }
        let e = &res["error"];
        tracing::debug!(endpoint, error = %e, "DSH 调用失败");
        Err(LocalError::new("DSH_REQUEST_FAILED").with("code", e["code"].clone()).with("detail", e["message"].as_str().unwrap_or_default().chars().take(500).collect::<String>()))
    }

    /// 开一个逻辑流（`session/follow` 之类），返回流的标识和收到的内容
    pub fn open(&self, endpoint: &str, args: Value) -> Result<(String, mpsc::UnboundedReceiver<StreamMsg>), LocalError> {
        if !self.alive() {
            return Err(err("DSH_CONNECT", "closed"));
        }
        let sid = format!("s{}", self.next.fetch_add(1, Ordering::SeqCst));
        let (tx, rx) = mpsc::unbounded_channel();
        lock(&self.streams).insert(sid.clone(), tx);
        let msg = json!({ "type": "open", "streamId": sid, "endpoint": endpoint, "payload": { "args": args } });
        if self.out.send(Message::text(msg.to_string())).is_err() {
            lock(&self.streams).remove(&sid);
            return Err(err("DSH_CONNECT", "closed"));
        }
        Ok((sid, rx))
    }

    /// 不要这个流了
    pub fn cancel(&self, sid: &str) {
        if lock(&self.streams).remove(sid).is_some() {
            let _ = self.out.send(Message::text(json!({ "type": "cancel", "streamId": sid }).to_string()));
        }
    }

    /// 答复一个 waterfall：`Some(结果)` 是看看替用户答了，`None` 是交给下一个（DSH 网页版之类）
    pub async fn reply(&self, event_id: &str, value: Option<Value>) -> Result<(), LocalError> {
        let outcome = match value {
            Some(v) => json!({ "kind": "result", "value": v }),
            None => json!({ "kind": "next" }),
        };
        self.call("$events/result", json!({ "clientId": self.client_id, "eventId": event_id, "outcome": outcome })).await.map(|_| ())
    }
}

// ---------------- 找到或者起一个 dsh web ----------------

/// 能连上的 `dsh web`：端口和换到的 Cookie
pub struct Login {
    pub port: u16,
    pub cookie: String,
}

/// 本机已经在运行的 DSH（看看的插件报上来的，不管是谁启动的），能登录上的第一个
pub async fn running() -> Option<Login> {
    let _ = tokio::task::spawn_blocking(|| crate::agents::ensure_dsh_web_plugin(true)).await;
    let found = tokio::task::spawn_blocking(crate::agents::dsh_web_running).await.unwrap_or_default();
    for w in found {
        match exchange(w.port, &w.token).await {
            Ok(cookie) => return Some(Login { port: w.port, cookie }),
            Err(e) => tracing::debug!(port = w.port, error = %e, "DSH 登录不上"),
        }
    }
    None
}

/// 口令换 Cookie
pub async fn exchange(port: u16, token: &str) -> Result<String, String> {
    let client = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).timeout(Duration::from_secs(5)).no_proxy().build().map_err(|e| e.to_string())?;
    let r = client.get(format!("http://127.0.0.1:{port}/?token={token}")).send().await.map_err(|e| e.to_string())?;
    if r.status() != reqwest::StatusCode::SEE_OTHER {
        return Err(format!("HTTP {}", r.status()));
    }
    r.headers()
        .get_all(reqwest::header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find_map(|c| c.split(';').next().map(str::trim).filter(|p| p.split_once('=').is_some_and(|(k, v)| !k.is_empty() && !v.is_empty())).map(String::from))
        .ok_or_else(|| "no cookie".into())
}

/// `dsh web: http://127.0.0.1:<端口>/?token=<口令>` 行里的端口和口令（最后一行算）
pub fn parse_launch(text: &str) -> Option<(u16, String)> {
    text.lines().rev().filter(|l| l.contains("dsh web:")).find_map(|l| {
        let url = l.split("http://").nth(1)?;
        let port: u16 = url.split(['/', '?']).next()?.rsplit(':').next()?.parse().ok()?;
        let token: String = url.split("token=").nth(1)?.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').collect();
        (token.len() >= 16).then_some((port, token))
    })
}

/// 启动 DSH 用的程序：客户端 PATH 里的 `dsh`，没有时在用户交互登录 shell 的 PATH 里找；都没有就给名字
pub async fn program(shell: &str) -> PathBuf {
    let name = if cfg!(windows) { "dsh.cmd" } else { "dsh" };
    let find = |path: &std::ffi::OsStr| std::env::split_paths(path).map(|d| d.join(name)).find(|p| p.is_file());
    if let Some(p) = std::env::var_os("PATH").and_then(|path| find(&path)) {
        return p;
    }
    if let Some(p) = super::codex_app::user_path(shell).await.and_then(|path| find(path.as_ref())) {
        return p;
    }
    PathBuf::from(name)
}

/// 本机没有 DSH 在运行：看看自己起一个（单独运行，不随客户端退出；之后插件会报上它，下次直接接上）。
/// 只听本机地址，端口让系统挑，输出写到 `run/dsh-web.log`，从里面读口令。
pub async fn start(run: &Path) -> Result<Login, LocalError> {
    std::fs::create_dir_all(run).map_err(|e| err("DSH_START", format!("{}: {e}", run.display())))?;
    let shell = crate::shells::login_shell();
    let program = program(&shell).await;
    let log_path = run.join("dsh-web.log");
    let log = std::fs::File::create(&log_path).map_err(|e| err("DSH_START", e))?;
    let mut cmd = tokio::process::Command::new(&program);
    if let Some(p) = super::codex_app::launch_path(&shell, &program).await {
        cmd.env("PATH", p);
    }
    let home = dirs::home_dir().unwrap_or_else(|| run.to_path_buf());
    cmd.args(["web", "--no-open", "--host", "127.0.0.1", "--port", "0"])
        .current_dir(home)
        .stdin(Stdio::null())
        .stdout(log.try_clone().map(Stdio::from).unwrap_or_else(|_| Stdio::null()))
        .stderr(Stdio::from(log))
        .kill_on_drop(false);
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000 | 0x0000_0200);
    #[cfg(unix)]
    cmd.process_group(0);
    let child = cmd.spawn().map_err(|e| {
        let code = if e.kind() == std::io::ErrorKind::NotFound { "DSH_NOT_FOUND" } else { "DSH_START" };
        err(code, format!("{}: {e}", program.display()))
    })?;
    let pid = child.id();
    drop(child);
    let deadline = tokio::time::Instant::now() + START_WAIT;
    loop {
        let text = std::fs::read_to_string(&log_path).unwrap_or_default();
        if let Some((port, token)) = parse_launch(&text) {
            let cookie = exchange(port, &token).await.map_err(|e| err("DSH_START", e))?;
            let _ = std::fs::write(run.join("dsh-web.json"), json!({ "pid": pid, "port": port }).to_string());
            return Ok(Login { port, cookie });
        }
        let exited = pid.is_some_and(|p| crate::agents::alive_pids(&[p]).is_empty());
        if exited || tokio::time::Instant::now() > deadline {
            let tail: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
            let tail = tail[tail.len().saturating_sub(3)..].join(" / ");
            tracing::warn!(program = %program.display(), log = %tail, "dsh web 没有起来");
            return Err(err("DSH_START", if tail.is_empty() { "dsh web did not start".into() } else { tail }));
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_line() {
        let out = "starting\ndsh web: http://127.0.0.1:39801/?token=XfECja94M6ZRyraHGetnigwBLmnKH6kypwKVEFBoNc8\n";
        assert_eq!(parse_launch(out), Some((39801, "XfECja94M6ZRyraHGetnigwBLmnKH6kypwKVEFBoNc8".into())));
        assert_eq!(parse_launch("dsh web: http://127.0.0.1:1/?token=short"), None, "口令太短不算");
        let two = "dsh web: http://127.0.0.1:1/?token=aaaaaaaaaaaaaaaaaaaa\ndsh web: http://127.0.0.1:2/?token=bbbbbbbbbbbbbbbbbbbb";
        assert_eq!(parse_launch(two).map(|p| p.0), Some(2), "最后一行算");
    }
}
