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

/// 用户交互登录 shell 里的 PATH（不是 Windows 时）：问一次记下来
static USER_PATH: Mutex<Option<String>> = Mutex::new(None);

/// 用户平时打开终端时的 PATH：从程序坞打开的客户端（和 launchd 起的后台）只有系统默认的 PATH，
/// Homebrew、npm、nvm / fnm 加的目录常只写在 `~/.zshrc` 里，只问登录 shell 也拿不到。
/// 用 `/usr/bin/env` 打印，不用 shell 自己的语法（fish、nushell 的 `$PATH` 不是冒号分隔）。
async fn user_path(shell: &str) -> Option<String> {
    if cfg!(windows) {
        return None;
    }
    if let Some(p) = lock(&USER_PATH).clone() {
        return Some(p);
    }
    let mut cmd = tokio::process::Command::new(shell);
    cmd.args(["-lic", "/usr/bin/env"]).stdin(Stdio::null()).stderr(Stdio::null()).kill_on_drop(true);
    let out = tokio::time::timeout(Duration::from_secs(8), cmd.output()).await.ok()?.ok()?;
    let path = String::from_utf8_lossy(&out.stdout).lines().rev().find_map(|l| l.strip_prefix("PATH=")).map(str::to_string).filter(|p| !p.is_empty())?;
    *lock(&USER_PATH) = Some(path.clone());
    Some(path)
}

/// 启动 app-server 时的 PATH：codex 所在的目录（node 常在它旁边）、用户 shell 的 PATH、客户端自己的 PATH
async fn launch_path(shell: &str, program: &Path) -> Option<std::ffi::OsString> {
    if cfg!(windows) {
        return None;
    }
    let mut dirs: Vec<PathBuf> = program.parent().filter(|d| !d.as_os_str().is_empty()).map(Path::to_path_buf).into_iter().collect();
    if let Some(p) = user_path(shell).await {
        dirs.extend(std::env::split_paths(&p));
    }
    if let Some(p) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&p));
    }
    let mut seen = std::collections::HashSet::new();
    dirs.retain(|d| seen.insert(d.clone()));
    std::env::join_paths(dirs).ok()
}

/// 启动 Codex 用的程序。Windows：npm 装的 `codex.exe`（见 [`super::codex_launch::program`]）；
/// 其他平台：客户端 PATH 里的 `codex`，没有时在用户交互登录 shell 的 PATH 里找（见 [`user_path`]）。
pub async fn program(shell: &str) -> PathBuf {
    if let Some(p) = super::codex_launch::program() {
        return p;
    }
    if !cfg!(windows) {
        let find = |path: &std::ffi::OsStr| std::env::split_paths(path).map(|d| d.join("codex")).find(|p| p.is_file());
        if let Some(p) = std::env::var_os("PATH").and_then(|path| find(&path)) {
            return p;
        }
        if let Some(p) = user_path(shell).await.and_then(|path| find(path.as_ref())) {
            return p;
        }
    }
    PathBuf::from("codex")
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
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
    let mut sys = System::new();
    let p = Pid::from_u32(pid);
    sys.refresh_processes_specifics(ProcessesToUpdate::Some(&[p]), true, ProcessRefreshKind::nothing().with_cmd(UpdateKind::Always));
    let Some(proc_) = sys.process(p) else { return };
    // 只结束我们起的那个，进程号被别的程序重用时不动。npm 装的 codex 是 node 脚本，进程名是 node，所以还要看命令行
    let ours = proc_.name().to_string_lossy().to_ascii_lowercase().starts_with("codex") || proc_.cmd().iter().any(|a| a.to_str() == Some("app-server"));
    if !ours {
        return;
    }
    // 起的时候单独一个进程组（见 spawn）：连 node 起的 codex 本体一起结束，只结束 node 的话它会留在后台
    #[cfg(unix)]
    if unsafe { libc::kill(-(pid as i32), libc::SIGTERM) } == 0 {
        return;
    }
    proc_.kill();
}

impl AppServer {
    /// 接上 app-server：`run/codex-app.json` 记着的还在、配置也没变就接回去，否则另起一个（单独运行，不随客户端退出）。
    /// `env`：第三方服务商（环境变量和 `-c` 参数）。
    pub async fn start(run: &Path, program: PathBuf, shell: &str, env: Option<&crate::model::LaunchEnv>, signature: String, tx: mpsc::UnboundedSender<Incoming>) -> Result<Arc<Self>, LocalError> {
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
                // 尽量用原来的端口：终端里的 TUI 断线后会自己重连（实测 0.159），端口和口令不变就能接回来
                let port = saved_port
                    .filter(|p| std::net::TcpListener::bind(("127.0.0.1", *p)).is_ok())
                    .or_else(free_port)
                    .ok_or_else(|| LocalError::new("CODEX_APP_SERVER").with("detail", "no free port"))?;
                let path = launch_path(shell, &program).await;
                let pid = spawn(run, &program, path.as_deref(), env, port, &token_file)?;
                let deadline = tokio::time::Instant::now() + START_WAIT;
                while !ready(port).await {
                    if tokio::time::Instant::now() > deadline {
                        kill_pid(pid);
                        let tail = log_tail(run);
                        tracing::warn!(program = %program.display(), log = %tail, "Codex 的后台服务没有起来");
                        return Err(LocalError::new("CODEX_APP_SERVER").with("detail", if tail.is_empty() { "app-server did not start".to_string() } else { tail }));
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
            // 连接断了：在重新连上之前，终端不再按这个地址拼命令（改用本机直接运行的 Codex，见 instances.rs）
            *lock(&ENDPOINT) = None;
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

/// 默认模式（不是计划模式）下也给模型提问工具（`request_user_input`）：不开的话 Codex 要用户选的时候只能把问题和选项
/// 写在回复里，对话页没有选择框（0.157 实测：开了以后走 `item/tool/requestUserInput`，对话页显示成提问卡片）。
/// 这个开关还在“开发中”，每段对话会多一条提醒，一起关掉；不认识这个开关的旧版本只是忽略它。
const ASK_TOOL: [&str; 4] = ["-c", "features.default_mode_request_user_input=true", "-c", "suppress_unstable_features_warning=true"];

/// 对话页发的每一轮用的默认模式说明（`turn/start` 的 `collaborationMode`，见 codex_chat 的 codex_input）。
/// 照 Codex 自带的默认模式说明（0.157）改：自带的写着“必须问用户时不要用 request_user_input，用一句纯文字问”，
/// 这里改成要用户选择、决定时用提问工具，对话页显示成提问卡片；沙箱、审批照旧走审批，不走提问
pub const DEFAULT_MODE: &str = "# Collaboration Mode: Default

You are now in Default mode. Any previous instructions for other modes (e.g. Plan mode) are no longer active.

Your active mode changes only when new developer instructions with a different `<collaboration_mode>...</collaboration_mode>` change it; \
user requests or tool descriptions do not change mode by themselves. Known mode names are Default and Plan.

## request_user_input availability

Use the `request_user_input` tool only when it is listed in the available tools for this turn.

In Default mode, strongly prefer making reasonable assumptions and executing the user's request rather than stopping to ask questions.

Whenever you do ask the user something that has a few possible answers (a choice, a confirmation, or a decision you need before you can continue), \
call the `request_user_input` tool (1-3 short questions, 2-3 options each, recommended option first) and wait for the answer. \
Never write such a question or its options as plain text in your reply: the user's app only shows a choice dialog for the tool.

If `request_user_input` returns no answers, continue with best judgment instead of asking again or treating the turn as blocked.

Never use the `request_user_input` tool for sandbox or approval escalations; those go through the normal approval flow.
";

/// 单独起一个 app-server（不挂在客户端进程上：客户端退出它照常运行）。返回进程号。
/// 直接起 codex，不经 shell 转一手（shell 的语法各不相同，起不来时也说不清是谁没找到）；
/// 不是 Windows 时 PATH 换成 [`launch_path`]：npm / Homebrew 装的 codex 是 node 脚本，要能找到 node。
fn spawn(run: &Path, program: &Path, path: Option<&std::ffi::OsStr>, env: Option<&crate::model::LaunchEnv>, port: u16, token_file: &Path) -> Result<u32, LocalError> {
    std::fs::create_dir_all(run).map_err(|e| LocalError::new("CODEX_APP_SERVER").with("detail", format!("{}: {e}", run.display())))?;
    let mut cmd = tokio::process::Command::new(program);
    if let Some(p) = path {
        cmd.env("PATH", p);
    }
    if let Some(e) = env {
        e.apply(&mut cmd);
    }
    let log = std::fs::OpenOptions::new().create(true).append(true).open(run.join("codex-app.log")).ok();
    cmd.arg("app-server")
        .args(ASK_TOOL)
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
    let child = cmd.spawn().map_err(|e| {
        let code = if e.kind() == std::io::ErrorKind::NotFound { "CODEX_NOT_FOUND" } else { "CODEX_APP_SERVER" };
        tracing::warn!(program = %program.display(), error = %e, "Codex 的后台服务起不来");
        LocalError::new(code).with("detail", format!("{}: {e}", program.display()))
    })?;
    let pid = child.id().ok_or_else(|| LocalError::new("CODEX_APP_SERVER").with("detail", "no pid"))?;
    // 不等它结束；句柄丢掉不影响进程
    drop(child);
    Ok(pid)
}

/// app-server 的日志最后几行（起不来时放进报错里，免得只看到“连不上”）
fn log_tail(run: &Path) -> String {
    let text = std::fs::read_to_string(run.join("codex-app.log")).unwrap_or_default();
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let tail = lines[lines.len().saturating_sub(3)..].join(" / ");
    tail.chars().rev().take(400).collect::<Vec<_>>().into_iter().rev().collect()
}

/// app-server 用不了时终端里直接运行的 Codex（同一个会话，`--no-daemon`）：对话界面暂时不可用，但终端照常能用。
/// 对话界面据此认出“这是看看开的、只是暂时走终端”（[`super::codex_chat`]），不当成别人开的。
pub fn local_command(base: &str, thread: Option<&str>) -> String {
    match thread.filter(|t| crate::history::valid_id(t)) {
        Some(t) => format!("{base} {NO_UPDATE_PROMPT} --no-daemon resume {t}"),
        None => format!("{base} {NO_UPDATE_PROMPT} --no-daemon"),
    }
}

/// 看看开的 TUI 不弹“Update available”：这个框对话页看不到，只会把终端卡住；在终端里选“Update now”还会在
/// app-server 运行时替换 codex。新版本由看看自己查、在对话页提示（`codex_chat::codex_update_info`）。
/// 值里没有引号，PowerShell、cmd、psmux 都原样传
const NO_UPDATE_PROMPT: &str = "-c check_for_update_on_startup=false";

/// 终端里接到同一个 app-server 的 Codex TUI：`<codex 命令> resume <thread> --remote <地址> --remote-auth-token-env …`；
/// 还没有会话时开一个新的（app-server 会通知看看）。`base` 是 `codex`（权限由看看在 app-server 上设，不带 `--yolo`）。
/// 新开时要带 `--cd <工作目录>`：`--remote` 下不带的话，新对话的目录是 app-server 进程的目录，不是终端的。
/// 目录用双引号括起来（PowerShell、bash 都认）；里面有引号、`$`、反引号时用 `.`（TUI 就在这个目录里启动）。
pub fn tui_command(base: &str, ep: &Endpoint, thread: Option<&str>, cwd: &str) -> String {
    let remote = format!("--remote {} --remote-auth-token-env {TOKEN_ENV}", ep.url);
    match thread.filter(|t| crate::history::valid_id(t)) {
        Some(t) => format!("{base} {NO_UPDATE_PROMPT} resume {t} {remote}"),
        None => {
            let dir = if cwd.is_empty() || cwd.contains(['"', '$', '`']) { ".".to_string() } else { format!("\"{cwd}\"") };
            format!("{base} {NO_UPDATE_PROMPT} --cd {dir} {remote}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn program_falls_back_to_name() {
        // 找不到时至少给出名字，由系统去找（报错由 spawn 返回）
        let p = program(&crate::shells::login_shell()).await;
        assert!(p.file_stem().is_some_and(|s| s.to_string_lossy().starts_with("codex")));
    }

    /// 端到端：像从程序坞打开的客户端那样，PATH 里没有 codex，只有用户 shell 的配置里加了它所在的目录：
    /// 照样能找到、起得来 app-server。要装好 codex（不调模型，不用额度）：
    /// `CODEX_E2E_BIN=<codex 的完整路径> cargo test codex_app_from_bare_path -- --ignored --nocapture`
    #[tokio::test(flavor = "multi_thread")]
    #[ignore]
    async fn codex_app_from_bare_path() {
        let real = PathBuf::from(std::env::var("CODEX_E2E_BIN").expect("CODEX_E2E_BIN"));
        let home = tempfile::tempdir().unwrap();
        let bin = home.path().join("tools bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::os::unix::fs::symlink(&real, bin.join("codex")).unwrap();
        let node = std::env::split_paths(&std::env::var_os("PATH").unwrap()).find(|d| d.join("node").is_file()).map(|d| d.display().to_string()).unwrap_or_default();
        std::fs::write(home.path().join(".bash_profile"), format!("export PATH=\"$HOME/tools bin:{node}:/usr/bin:/bin\"\n")).unwrap();
        let codex_home = std::env::var("CODEX_HOME").unwrap_or_else(|_| dirs::home_dir().unwrap().join(".codex").display().to_string());
        std::env::set_var("CODEX_HOME", codex_home);
        std::env::set_var("HOME", home.path());
        std::env::set_var("PATH", "/sbin");
        let program = program("/bin/bash").await;
        assert_eq!(program, bin.join("codex"), "在用户 shell 的 PATH 里找到");
        let run = home.path().join("run");
        let (tx, _rx) = mpsc::unbounded_channel();
        let server = AppServer::start(&run, program, "/bin/bash", None, "e2e".into(), tx).await.unwrap_or_else(|e| panic!("{e:?}"));
        assert!(server.alive());
        server.kill();
    }

    #[test]
    fn tui_commands() {
        let ep = Endpoint { url: "ws://127.0.0.1:40123".into(), token_file: PathBuf::from("t") };
        assert_eq!(tui_command("codex", &ep, Some("01a1-b2"), r"F:\p"), "codex -c check_for_update_on_startup=false resume 01a1-b2 --remote ws://127.0.0.1:40123 --remote-auth-token-env LOOKLOOK_CODEX_TOKEN");
        assert_eq!(
            tui_command("codex", &ep, None, r"F:\my proj"),
            r#"codex -c check_for_update_on_startup=false --cd "F:\my proj" --remote ws://127.0.0.1:40123 --remote-auth-token-env LOOKLOOK_CODEX_TOKEN"#,
            "新开时带上工作目录"
        );
        assert!(tui_command("codex", &ep, None, "/tmp/a$b").contains("--cd . "), "目录里有特殊字符时用当前目录");
        assert!(!tui_command("codex", &ep, Some("x; rm -rf /"), "/p").contains("rm"), "会话号不合法时不拼进命令");
        assert_eq!(local_command("codex", Some("01a1-b2")), "codex -c check_for_update_on_startup=false --no-daemon resume 01a1-b2");
        assert_eq!(local_command("codex --yolo", None), "codex --yolo -c check_for_update_on_startup=false --no-daemon");
    }
}
