//! 和 `codex app-server` 说话（docs/CODEX_UI.md §4）：JSON-RPC，一行一条，走 stdio。
//!
//! 看看起一个 app-server 进程，所有对话模式的 Codex 终端共用（每个终端是其中的一个 thread）。
//! 这里只管进程和收发：请求按 id 对上答复；通知和 app-server 发来的请求（审批）交给 [`Incoming`] 的接收方。
//! 进程退出时所有等着的请求都返回错误，接收方收到 [`Incoming::Closed`]，下次用到时重新起。

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{mpsc, oneshot};

use crate::error::LocalError;

/// 普通请求等答复的上限（`turn/start` 这些只是确认收到，很快；`thread/resume` 要读整份记录）
const CALL_TIMEOUT: Duration = Duration::from_secs(60);

/// app-server 主动发来的
#[derive(Debug)]
pub enum Incoming {
    Notification { method: String, params: Value },
    /// 要看看答复的请求（审批、提问）：答复用 [`AppServer::respond`]
    Request { id: Value, method: String, params: Value },
    /// 进程退出了
    Closed,
}

type Waiter = oneshot::Sender<Result<Value, Value>>;

pub struct AppServer {
    stdin: tokio::sync::Mutex<ChildStdin>,
    waiting: Mutex<HashMap<i64, Waiter>>,
    next: AtomicI64,
    alive: AtomicBool,
    child: Mutex<Option<Child>>,
    /// 起这个进程时的配置（程序、第三方服务商）；变了要换新进程
    pub signature: String,
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

impl AppServer {
    /// 起进程、握手。`env`：第三方服务商（环境变量和 `-c` 参数）。
    pub async fn start(program: PathBuf, env: Option<&crate::model::LaunchEnv>, signature: String, tx: mpsc::UnboundedSender<Incoming>) -> Result<Arc<Self>, LocalError> {
        let mut cmd = Command::new(&program);
        if let Some(e) = env {
            e.apply(&mut cmd);
        }
        cmd.arg("app-server").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).kill_on_drop(true);
        #[cfg(windows)]
        cmd.creation_flags(0x0800_0000);
        let mut child = cmd.spawn().map_err(|e| LocalError::new("CODEX_NOT_FOUND").with("detail", format!("{}: {e}", program.display())))?;
        let stdin = child.stdin.take().ok_or_else(|| LocalError::new("CODEX_START_FAILED"))?;
        let stdout = child.stdout.take().ok_or_else(|| LocalError::new("CODEX_START_FAILED"))?;
        let me = Arc::new(Self {
            stdin: tokio::sync::Mutex::new(stdin),
            waiting: Mutex::new(HashMap::new()),
            next: AtomicI64::new(1),
            alive: AtomicBool::new(true),
            child: Mutex::new(Some(child)),
            signature,
        });
        let reader = me.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(msg) = serde_json::from_str::<Value>(&line) else { continue };
                reader.dispatch(msg, &tx);
            }
            reader.alive.store(false, Ordering::SeqCst);
            for (_, w) in lock(&reader.waiting).drain() {
                let _ = w.send(Err(json!({ "message": "app-server exited" })));
            }
            let _ = tx.send(Incoming::Closed);
        });
        let init = json!({
            "clientInfo": { "name": "looklook", "title": "Looklook", "version": env!("CARGO_PKG_VERSION") },
            "capabilities": { "experimentalApi": true, "requestAttestation": false },
        });
        me.call("initialize", init).await?;
        me.notify("initialized", Value::Null).await?;
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

    async fn send(&self, msg: Value) -> Result<(), LocalError> {
        let mut line = msg.to_string();
        line.push('\n');
        let mut w = self.stdin.lock().await;
        w.write_all(line.as_bytes()).await.map_err(|e| LocalError::new("CODEX_APP_SERVER").with("detail", e.to_string()))?;
        w.flush().await.map_err(|e| LocalError::new("CODEX_APP_SERVER").with("detail", e.to_string()))
    }

    /// 发请求、等答复。app-server 答错误时返回 `CODEX_REQUEST_FAILED`（带上它的 message）。
    pub async fn call(&self, method: &str, params: Value) -> Result<Value, LocalError> {
        if !self.alive() {
            return Err(LocalError::new("CODEX_APP_SERVER").with("detail", "not running"));
        }
        let id = self.next.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        lock(&self.waiting).insert(id, tx);
        let sent = self.send(json!({ "id": id, "method": method, "params": params })).await;
        if let Err(e) = sent {
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
        self.send(msg).await
    }

    /// 答复 app-server 发来的请求
    pub async fn respond(&self, id: Value, result: Value) -> Result<(), LocalError> {
        self.send(json!({ "id": id, "result": result })).await
    }

    /// 不支持的请求：答错误，app-server 按拒绝处理
    pub async fn respond_error(&self, id: Value, message: &str) -> Result<(), LocalError> {
        self.send(json!({ "id": id, "error": { "code": -32601, "message": message } })).await
    }

    pub fn kill(&self) {
        self.alive.store(false, Ordering::SeqCst);
        if let Some(mut c) = lock(&self.child).take() {
            let _ = c.start_kill();
        }
    }
}

impl Drop for AppServer {
    fn drop(&mut self) {
        self.kill();
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
}
