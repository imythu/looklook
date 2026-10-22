//! 和 `codex app-server` 说话（docs/CODEX_UI.md §4）：JSON-RPC，每条消息一帧，走本机 WebSocket。
//!
//! 看看起一个 app-server（`--listen ws://127.0.0.1:<端口>`，带随机口令），所有对话模式的 Codex 终端共用，
//! 每个终端是其中的一个 thread。终端标签里的 Codex TUI 用 `codex resume <thread> --remote <地址>` 接到同一个
//! app-server 上，两边是同一个会话的两个视图：谁发的消息、谁答的审批，另一边都实时看到。
//!
//! app-server 单独运行（不随客户端退出）：客户端重启时终端里的 TUI 不断，正在跑的这一轮也不停。
//! 地址、进程号记在 `run/codex-app.json`，口令在 `run/codex-app.token`；下次启动先试着接回去，接不上或者
//! 配置变了（程序、第三方服务商）才另起一个。
//!
//! 这里只管进程和收发：请求按 id 对上答复；通知和 app-server 发来的请求（审批）交给 [`Incoming`] 的接收方。
//! 连接断了所有等着的请求都返回错误，接收方收到 [`Incoming::Closed`]，下次用到时重新连（必要时重新起）。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::{client::IntoClientRequest, http::HeaderValue, Message};

use crate::error::LocalError;

/// 普通请求等答复的上限（`turn/start` 这些只是确认收到，很快；`thread/resume` 要读整份记录）
const CALL_TIMEOUT: Duration = Duration::from_secs(60);
/// 新起的 app-server 多久之内要能连上
const START_WAIT: Duration = Duration::from_secs(20);
/// 终端里的 TUI 从这个环境变量读口令（`--remote-auth-token-env`）
pub const TOKEN_ENV: &str = "LOOKLOOK_CODEX_TOKEN";

/// app-server 主动发来的
#[derive(Debug)]
pub enum Incoming {
    Notification { method: String, params: Value },
    /// 要看看答复的请求（审批、提问）：答复用 [`AppServer::respond`]
    Request { id: Value, method: String, params: Value },
    /// 连接断了
    Closed,
}

/// 终端里的 TUI 接哪里
#[derive(Debug, Clone, PartialEq)]
pub struct Endpoint {
    /// `ws://127.0.0.1:<端口>`
    pub url: String,
    pub token_file: PathBuf,
}

/// 当前连着的 app-server 的地址（终端拼启动命令用）；还没连过时 None
static ENDPOINT: Mutex<Option<Endpoint>> = Mutex::new(None);

pub fn endpoint() -> Option<Endpoint> {
    lock(&ENDPOINT).clone()
}

type Waiter = oneshot::Sender<Result<Value, Value>>;

pub struct AppServer {
    out: mpsc::UnboundedSender<Message>,
    waiting: Mutex<HashMap<i64, Waiter>>,
    next: AtomicI64,
    alive: AtomicBool,
    /// 起这个进程时的配置（程序、第三方服务商）；变了要换新进程
    pub signature: String,
    pid: Option<u32>,
    state_file: PathBuf,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// 启动 Codex 用的程序。Windows：npm 装的 `codex.exe`（见 [`super::codex_launch::program`]）；
/// 其他平台：PATH 里的 `codex`，找不到时问登录 shell（从程序坞打开的客户端拿不到用户 shell 里加的 PATH）。
pub fn program() -> PathBuf {
    if let Some(p) = super::codex_launch::program() {
        return p;
    }
    if !cfg!(windows) {
        if let Some(p) = std::env::var_os("PATH").and_then(|path| std::env::split_paths(&path).map(|d| d.join("codex")).find(|p| p.is_file())) {
            return p;
        }
        if let Some(p) = login_shell_which("codex") {
            return p;
        }
    }
    PathBuf::from("codex")
}

fn login_shell_which(name: &str) -> Option<PathBuf> {
    let shell = std::env::var("SHELL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "/bin/sh".into());
    let out = std::process::Command::new(shell).args(["-lc", &format!("command -v {name}")]).stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
    let p = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
    (out.status.success() && p.is_absolute() && p.is_file()).then_some(p)
}

/// 口令：第一次用时生成，之后一直用同一个（只有本机的看看和终端里的 TUI 读得到）
fn token(run: &Path) -> std::io::Result<(PathBuf, String)> {
    let path = run.join("codex-app.token");
    if let Ok(t) = std::fs::read_to_string(&path) {
        let t = t.trim().to_string();
        if t.len() >= 32 {
            return Ok((path, t));
        }
    }
    std::fs::create_dir_all(run)?;
    let t = crate::util::random_secret();
    std::fs::write(&path, &t)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    Ok((path, t))
}

async fn ready(port: u16) -> bool {
    let Ok(c) = reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(2)).build() else { return false };
    c.get(format!("http://127.0.0.1:{port}/readyz")).send().await.is_ok_and(|r| r.status().is_success())
}

fn free_port() -> Option<u16> {
    std::net::TcpListener::bind(("127.0.0.1", 0)).ok()?.local_addr().ok().map(|a| a.port())
}

fn kill_pid(pid: u32) {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};
    let mut sys = System::new();
    let p = Pid::from_u32(pid);
    sys.refresh_processes_specifics(ProcessesToUpdate::Some(&[p]), true, ProcessRefreshKind::nothing());
    if let Some(proc_) = sys.process(p) {
        // 只结束我们起的那个（名字对得上），进程号被别的程序重用时不动
        if proc_.name().to_string_lossy().to_ascii_lowercase().starts_with("codex") {
            proc_.kill();
        }
    }
}

impl AppServer {
    /// 接上 app-server：`run/codex-app.json` 记着的还在、配置也没变就接回去，否则另起一个（单独运行，不随客户端退出）。
    /// `env`：第三方服务商（环境变量和 `-c` 参数）。
    pub async fn start(run: &Path, program: PathBuf, env: Option<&crate::model::LaunchEnv>, signature: String, tx: mpsc::UnboundedSender<Incoming>) -> Result<Arc<Self>, LocalError> {
        let state_file = run.join("codex-app.json");
        let (token_file, token) = token(run).map_err(|e| LocalError::new("CODEX_APP_SERVER").with("detail", e.to_string()))?;
        let saved: Value = std::fs::read_to_string(&state_file).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        let saved_port = saved["port"].as_u64().and_then(|p| u16::try_from(p).ok());
        let saved_pid = saved["pid"].as_u64().map(|p| p as u32);
        let reuse = match saved_port {
            Some(port) if saved["signature"] == signature.as_str() && ready(port).await => Some(port),
            _ => None,
        };
        let (port, pid) = match reuse {
            Some(port) => (port, saved_pid),
            None => {
                if let Some(pid) = saved_pid {
                    kill_pid(pid);
                }
                let port = free_port().ok_or_else(|| LocalError::new("CODEX_APP_SERVER").with("detail", "no free port"))?;
                let pid = spawn(run, &program, env, port, &token_file)?;
                let deadline = tokio::time::Instant::now() + START_WAIT;
                while !ready(port).await {
                    if tokio::time::Instant::now() > deadline {
                        kill_pid(pid);
                        return Err(LocalError::new("CODEX_APP_SERVER").with("detail", "app-server did not start"));
                    }
                    tokio::time::sleep(Duration::from_millis(250)).await;
                }
                let _ = std::fs::write(&state_file, json!({ "port": port, "pid": pid, "signature": signature }).to_string());
                (port, Some(pid))
            }
        };
        let url = format!("ws://127.0.0.1:{port}");
        let me = Self::connect(&url, &token, signature, pid, state_file, tx).await?;
        *lock(&ENDPOINT) = Some(Endpoint { url, token_file });
        let init = json!({
            "clientInfo": { "name": "looklook", "title": "Looklook", "version": env!("CARGO_PKG_VERSION") },
            "capabilities": { "experimentalApi": true, "requestAttestation": false },
        });
        me.call("initialize", init).await?;
        me.notify("initialized", Value::Null).await?;
        Ok(me)
    }

    async fn connect(url: &str, token: &str, signature: String, pid: Option<u32>, state_file: PathBuf, tx: mpsc::UnboundedSender<Incoming>) -> Result<Arc<Self>, LocalError> {
        let err = |e: String| LocalError::new("CODEX_APP_SERVER").with("detail", e);
        let mut req = url.into_client_request().map_err(|e| err(e.to_string()))?;
        req.headers_mut().insert("authorization", HeaderValue::from_str(&format!("Bearer {token}")).map_err(|e| err(e.to_string()))?);
        // 不带 Origin（app-server 拒绝带 Origin 的连接，防网页连进来）
        let addr = url.trim_start_matches("ws://").to_string();
        let tcp = tokio::net::TcpStream::connect(&addr).await.map_err(|e| err(e.to_string()))?;
        let (ws, _) = tokio_tungstenite::client_async(req, tcp).await.map_err(|e| err(e.to_string()))?;
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
        let me = Arc::new(Self { out, waiting: Mutex::new(HashMap::new()), next: AtomicI64::new(1), alive: AtomicBool::new(true), signature, pid, state_file });
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
            for (_, w) in lock(&reader.waiting).drain() {
                let _ = w.send(Err(json!({ "message": "app-server connection closed" })));
            }
            let _ = tx.send(Incoming::Closed);
        });
        Ok(me)
    }

    fn dispatch(&self, msg: Value, tx: &mpsc::UnboundedSender<Incoming>) {
        let method = msg["method"].as_str().map(String::from);
        match (msg.get("id"), method) {
            (Some(id), None) => {
                let Some(n) = id.as_i64() else { return };
                if let Some(w) = lock(&self.waiting).remove(&n) {
                    let r = match msg.get("error") {
                        Some(e) => Err(e.clone()),
                        None => Ok(msg.get("result").cloned().unwrap_or(Value::Null)),
                    };
                    let _ = w.send(r);
                }
            }
            (Some(id), Some(method)) => {
                let _ = tx.send(Incoming::Request { id: id.clone(), method, params: msg["params"].clone() });
            }
            (None, Some(method)) => {
                let _ = tx.send(Incoming::Notification { method, params: msg["params"].clone() });
            }
            (None, None) => {}
        }
    }

    pub fn alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }

    fn send(&self, msg: Value) -> Result<(), LocalError> {
        self.out.send(Message::text(msg.to_string())).map_err(|_| LocalError::new("CODEX_APP_SERVER").with("detail", "closed"))
    }

    /// 发请求、等答复。app-server 答错误时返回 `CODEX_REQUEST_FAILED`（带上它的 message）。
    pub async fn call(&self, method: &str, params: Value) -> Result<Value, LocalError> {
        if !self.alive() {
            return Err(LocalError::new("CODEX_APP_SERVER").with("detail", "not running"));
        }
        let id = self.next.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        lock(&self.waiting).insert(id, tx);
        if let Err(e) = self.send(json!({ "id": id, "method": method, "params": params })) {
            lock(&self.waiting).remove(&id);
            return Err(e);
        }
        match tokio::time::timeout(CALL_TIMEOUT, rx).await {
            Ok(Ok(Ok(v))) => Ok(v),
            Ok(Ok(Err(e))) => {
                let msg = e["message"].as_str().unwrap_or("").chars().take(500).collect::<String>();
                tracing::debug!(method, error = %e, "app-server 请求失败");
                Err(LocalError::new("CODEX_REQUEST_FAILED").with("detail", msg))
            }
            Ok(Err(_)) => Err(LocalError::new("CODEX_APP_SERVER").with("detail", "closed")),
            Err(_) => {
                lock(&self.waiting).remove(&id);
                Err(LocalError::new("CODEX_APP_SERVER").with("detail", format!("{method} timeout")))
            }
        }
    }

    pub async fn notify(&self, method: &str, params: Value) -> Result<(), LocalError> {
        let mut msg = json!({ "method": method });
        if !params.is_null() {
            msg["params"] = params;
        }
        self.send(msg)
    }

    /// 答复 app-server 发来的请求
    pub async fn respond(&self, id: Value, result: Value) -> Result<(), LocalError> {
        self.send(json!({ "id": id, "result": result }))
    }

    /// 不支持的请求：答错误，app-server 按拒绝处理
    pub async fn respond_error(&self, id: Value, message: &str) -> Result<(), LocalError> {
        self.send(json!({ "id": id, "error": { "code": -32601, "message": message } }))
    }

    /// 换新进程时（配置变了）结束这一个
    pub fn kill(&self) {
        self.alive.store(false, Ordering::SeqCst);
        let _ = self.out.send(Message::Close(None));
        if let Some(pid) = self.pid {
            kill_pid(pid);
        }
        let _ = std::fs::remove_file(&self.state_file);
        *lock(&ENDPOINT) = None;
    }
}

/// 单独起一个 app-server（不挂在客户端进程上：客户端退出它照常运行）。返回进程号。
fn spawn(run: &Path, program: &Path, env: Option<&crate::model::LaunchEnv>, port: u16, token_file: &Path) -> Result<u32, LocalError> {
    let mut cmd = tokio::process::Command::new(program);
    if let Some(e) = env {
        e.apply(&mut cmd);
    }
    let log = std::fs::OpenOptions::new().create(true).append(true).open(run.join("codex-app.log")).ok();
    cmd.arg("app-server")
        .args(["--listen", &format!("ws://127.0.0.1:{port}"), "--ws-auth", "capability-token", "--ws-token-file"])
        .arg(token_file)
        .current_dir(run)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log.map(Stdio::from).unwrap_or_else(Stdio::null))
        .kill_on_drop(false);
    // 没有窗口、单独一个进程组（客户端结束时不会被一起结束）
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000 | 0x0000_0200);
    #[cfg(unix)]
    cmd.process_group(0);
    let child = cmd.spawn().map_err(|e| LocalError::new("CODEX_NOT_FOUND").with("detail", format!("{}: {e}", program.display())))?;
    let pid = child.id().ok_or_else(|| LocalError::new("CODEX_APP_SERVER").with("detail", "no pid"))?;
    // 不等它结束；句柄丢掉不影响进程
    drop(child);
    Ok(pid)
}

/// 终端里接到同一个 app-server 的 Codex TUI：`<codex 命令> resume <thread> --remote <地址> --remote-auth-token-env …`；
/// 还没有会话时开一个新的（app-server 会通知看看）。`base` 是 `codex` / `codex --yolo`。
pub fn tui_command(base: &str, ep: &Endpoint, thread: Option<&str>) -> String {
    let remote = format!("--remote {} --remote-auth-token-env {TOKEN_ENV}", ep.url);
    match thread.filter(|t| crate::history::valid_id(t)) {
        Some(t) => format!("{base} resume {t} {remote}"),
        None => format!("{base} {remote}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn program_falls_back_to_name() {
        // 找不到时至少给出名字，由系统去找（报错由 spawn 返回）
        let p = program();
        assert!(p.file_stem().is_some_and(|s| s.to_string_lossy().starts_with("codex")));
    }

    #[test]
    fn tui_commands() {
        let ep = Endpoint { url: "ws://127.0.0.1:40123".into(), token_file: PathBuf::from("t") };
        assert_eq!(tui_command("codex", &ep, Some("01a1-b2")), "codex resume 01a1-b2 --remote ws://127.0.0.1:40123 --remote-auth-token-env LOOKLOOK_CODEX_TOKEN");
        assert_eq!(tui_command("codex --yolo", &ep, None), "codex --yolo --remote ws://127.0.0.1:40123 --remote-auth-token-env LOOKLOOK_CODEX_TOKEN");
        assert!(!tui_command("codex", &ep, Some("x; rm -rf /")).contains("rm"), "会话号不合法时不拼进命令");
    }
}
