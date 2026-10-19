//! Claude Code 对话界面（docs/CLAUDE_UI.md）。
//!
//! tmux 里照常运行交互式 `claude`（启动时用 `--settings` 注入 hooks），对话界面是同一个进程的第二个视图：
//!
//! - **hooks**（`hooks.rs`、`POST /hook`）：实时状态（运行中、空闲、等待审批）、绑定会话与 transcript、权限审批；
//! - **transcript**（`transcript.rs`）：对话内容的唯一来源，hook 丢了也能补回来；
//! - **tmux**：送输入（分块“打字”，图片路径用粘贴）、停止（Esc）、抓屏判断 TUI 当前是不是普通输入框（`screen.rs`）。
//!
//! 审批时 TUI 也同时弹出了自己的确认框，两边谁先回答算谁的（实测）；任何异常都交给 TUI，不会自动放行。

pub mod commands;
pub mod hooks;
pub mod screen;
pub mod transcript;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::{broadcast, oneshot, watch};

use crate::error::LocalError;
use crate::instances::Instances;
use crate::paths::Paths;
use crate::store::Store;
use screen::Screen;
use transcript::{Change, Item, Transcript};

/// 客户端最多替界面挂起一个审批这么久（hook 的超时是 600 秒），到时间交给 TUI
const PERMISSION_WAIT: Duration = Duration::from_secs(590);
/// 打开对话时先发最后这么多条，更早的往上翻时再取
pub const SNAPSHOT_ITEMS: usize = 200;
/// “打字”：每块字符数和块间间隔（一次送太多会被 TUI 当成粘贴，包进 `<pasted_content>`）
const TYPE_CHUNK: usize = 64;
const TYPE_GAP: Duration = Duration::from_millis(30);
/// 超过这么长就直接粘贴（打字太慢），TUI 会把它显示成 `[Pasted text]`
pub const PASTE_ABOVE: usize = 8_000;
/// 送出后等 `UserPromptSubmit` 多久
const DELIVERY_WAIT: Duration = Duration::from_secs(3);
/// 运行中多久没有 hook 就去看一眼屏幕（Esc 中断没有任何 hook）
const QUIET: Duration = Duration::from_secs(3);
/// claude 在运行却一直没有 SessionStart：没带 hooks 启动，或者 hooks 被用户配置关掉了
const ATTACH_GRACE: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// Claude Code 没在运行，或者没接上 hooks（`reason` 说明）
    Detached,
    Idle,
    Running,
    /// 等待审批（界面上有审批条，TUI 上也有确认框）
    Approval,
    /// TUI 上是对话框、选择器之类，界面处理不了，要到终端里操作
    Tui,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Binding {
    pub session_id: String,
    pub transcript_path: PathBuf,
}

fn binding_key(id: &str) -> String {
    format!("chat.{id}")
}

struct Pending {
    id: u64,
    at: Instant,
    tool: String,
    input: Value,
    suggestions: Value,
    tx: Option<oneshot::Sender<String>>,
}

impl Pending {
    fn kind(&self) -> &'static str {
        match self.tool.as_str() {
            "AskUserQuestion" => "question",
            "ExitPlanMode" => "plan",
            _ => "permission",
        }
    }

    fn resolve(&mut self, out: String) {
        if let Some(tx) = self.tx.take() {
            let _ = tx.send(out);
        }
    }
}

struct State {
    binding: Option<Binding>,
    phase: Phase,
    reason: Option<&'static str>,
    tool: Option<String>,
    mode: Option<String>,
    model: Option<String>,
    error: Option<String>,
    pending: Option<Pending>,
    last_hook: Option<Instant>,
    /// 这次看到 claude 在运行的起点（判断“没接上 hooks”）
    claude_since: Option<Instant>,
    version: Option<String>,
    stats: transcript::Stats,
    /// 运行中屏幕上的状态行和正在输出的那段回复（transcript 里还没有）
    live: screen::Live,
}

/// 推给界面的事件
#[derive(Debug, Clone)]
pub enum Event {
    Status(Value),
    Items { start: usize, items: Vec<Item> },
    /// 前面的内容变了或者换了会话：重新取快照
    Reset,
}

pub struct Inst {
    id: String,
    st: Mutex<State>,
    tx: broadcast::Sender<Event>,
    tr: tokio::sync::Mutex<Option<Transcript>>,
    viewers: AtomicUsize,
    watching: AtomicBool,
    /// 收到的 `UserPromptSubmit` 个数（确认输入送达）
    prompts: watch::Sender<u64>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

pub struct Chat {
    store: Arc<Store>,
    paths: Paths,
    instances: Arc<Instances>,
    token: String,
    next_id: AtomicU64,
    insts: Mutex<HashMap<String, Arc<Inst>>>,
}

/// 一个打开着的对话视图；没有视图时不跟读 transcript、不抓屏。
pub struct Viewer {
    inst: Arc<Inst>,
    _terminal: crate::updater::TerminalGuard,
}

impl Drop for Viewer {
    fn drop(&mut self) {
        self.inst.viewers.fetch_sub(1, Ordering::SeqCst);
    }
}

impl Chat {
    pub fn new(store: Arc<Store>, paths: Paths, instances: Arc<Instances>) -> Arc<Self> {
        Arc::new(Self { store, paths, instances, token: crate::util::random_secret(), next_id: AtomicU64::new(1), insts: Mutex::new(HashMap::new()) })
    }

    /// 写 `run/hook.json`（本机地址与令牌）和 hooks 配置文件。
    pub fn publish(&self, addr: std::net::SocketAddr) -> std::io::Result<()> {
        hooks::write_endpoint(&self.paths, addr, &self.token)?;
        hooks::ensure_settings(&self.paths).map(|_| ())
    }

    pub fn token_ok(&self, token: &str) -> bool {
        crate::gateway::ct_eq(token, &self.token)
    }

    fn inst(&self, id: &str) -> Arc<Inst> {
        let mut map = lock(&self.insts);
        map.entry(id.to_string())
            .or_insert_with(|| {
                let binding: Option<Binding> = self.store.get(&binding_key(id)).ok().flatten();
                Arc::new(Inst {
                    id: id.to_string(),
                    st: Mutex::new(State {
                        // 客户端重启后先按空闲处理，抓屏和下一个 hook 会纠正
                        phase: if binding.is_some() { Phase::Idle } else { Phase::Detached },
                        reason: None,
                        binding,
                        tool: None,
                        mode: None,
                        model: None,
                        error: None,
                        pending: None,
                        last_hook: None,
                        claude_since: None,
                        version: None,
                        stats: Default::default(),
                        live: Default::default(),
                    }),
                    tx: broadcast::channel(256).0,
                    tr: tokio::sync::Mutex::new(None),
                    viewers: AtomicUsize::new(0),
                    watching: AtomicBool::new(false),
                    prompts: watch::channel(0).0,
                })
            })
            .clone()
    }

    /// 终端删除时清掉记录；挂起的审批交给 TUI（进程也已经结束了）。
    pub fn forget(&self, id: &str) {
        if let Some(inst) = lock(&self.insts).remove(id) {
            if let Some(p) = lock(&inst.st).pending.as_mut() {
                p.resolve(String::new());
            }
        }
        let _ = self.store.remove(&binding_key(id));
    }

    fn require(&self, id: &str) -> Result<(), LocalError> {
        self.store.instance(id)?.ok_or_else(LocalError::not_found)?;
        if !self.instances.opts(id).chat_ui {
            return Err(LocalError::new("CHAT_UNAVAILABLE"));
        }
        Ok(())
    }

    // ---------------- 状态 ----------------

    fn status_of(st: &State) -> Value {
        json!({
            "phase": st.phase,
            "reason": st.reason,
            "tool": st.tool,
            "mode": st.mode,
            "model": st.model,
            "error": st.error,
            "session_id": st.binding.as_ref().map(|b| &b.session_id),
            "version": st.version,
            "stats": st.stats,
            "activity": st.live.activity,
            "preview": st.live.text,
            "pending": st.pending.as_ref().map(|p| json!({
                "id": p.id,
                "kind": p.kind(),
                "tool": p.tool,
                "input": p.input,
                "suggestions": p.suggestions,
            })),
        })
    }

    fn emit_status(inst: &Inst, st: &State) {
        let _ = inst.tx.send(Event::Status(Self::status_of(st)));
    }

    fn set_phase(inst: &Inst, st: &mut State, phase: Phase, reason: Option<&'static str>) {
        if st.phase != phase || st.reason != reason {
            st.phase = phase;
            st.reason = reason;
            if phase != Phase::Running {
                st.tool = None;
                st.live = Default::default();
            }
            Self::emit_status(inst, st);
        }
    }

    // ---------------- hooks ----------------

    /// 一个 hook 事件（`POST /hook`）。返回要写到 hook 程序 stdout 的内容（只有审批会有）。
    pub async fn hook(self: &Arc<Self>, id: &str, ev: Value) -> String {
        if self.store.instance(id).ok().flatten().is_none() {
            return String::new();
        }
        let inst = self.inst(id);
        let name = ev["hook_event_name"].as_str().unwrap_or("").to_string();
        let tool = ev["tool_name"].as_str().map(String::from);
        let rebound = {
            let mut st = lock(&inst.st);
            st.last_hook = Some(Instant::now());
            if let Some(m) = ev["permission_mode"].as_str() {
                st.mode = Some(m.to_string());
            }
            if let Some(m) = ev["model"].as_str() {
                st.model = Some(m.to_string());
            }
            let rebound = self.rebind(id, &mut st, &ev);
            match name.as_str() {
                "SessionStart" => {
                    // 自动压缩发生在一轮当中，不能改成空闲
                    if ev["source"] != "compact" {
                        st.error = None;
                        Self::set_phase(&inst, &mut st, Phase::Idle, None);
                    }
                }
                "SessionEnd" => {
                    Self::drop_pending(&mut st);
                    Self::set_phase(&inst, &mut st, Phase::Detached, Some("ended"));
                }
                "UserPromptSubmit" => {
                    Self::drop_pending(&mut st);
                    st.error = None;
                    inst.prompts.send_modify(|n| *n += 1);
                    Self::set_phase(&inst, &mut st, Phase::Running, None);
                }
                "PreToolUse" => {
                    if st.phase != Phase::Approval {
                        st.tool = tool.clone();
                        st.phase = Phase::Running;
                        st.reason = None;
                        Self::emit_status(&inst, &st);
                    }
                }
                "PostToolUse" | "PostToolUseFailure" | "PermissionDenied" => {
                    if st.pending.as_ref().is_some_and(|p| Some(&p.tool) == tool.as_ref()) {
                        Self::drop_pending(&mut st);
                    }
                    Self::set_phase(&inst, &mut st, Phase::Running, None);
                }
                "Stop" => {
                    Self::drop_pending(&mut st);
                    Self::set_phase(&inst, &mut st, Phase::Idle, None);
                }
                "StopFailure" => {
                    Self::drop_pending(&mut st);
                    st.error = ev["error"].as_str().or_else(|| ev["message"].as_str()).map(|s| s.chars().take(500).collect());
                    Self::set_phase(&inst, &mut st, Phase::Idle, None);
                    Self::emit_status(&inst, &st);
                }
                "Notification" => match ev["notification_type"].as_str() {
                    // 确认框出来了但不是我们挂起的审批（例如审批已经超时）：要到终端里回答
                    Some("permission_prompt" | "elicitation_dialog") if st.pending.is_none() => Self::set_phase(&inst, &mut st, Phase::Tui, None),
                    Some("idle_prompt") if st.pending.is_none() && st.phase == Phase::Running => Self::set_phase(&inst, &mut st, Phase::Idle, None),
                    _ => {}
                },
                // Stop 之后总会有一个 agent_type 为空的 SubagentStop（后台生成推荐提示），不算
                "SubagentStop" if ev["agent_type"].as_str().unwrap_or("").is_empty() => {}
                "SubagentStart" | "SubagentStop" | "PreCompact" => {
                    if st.phase != Phase::Approval {
                        Self::set_phase(&inst, &mut st, Phase::Running, None);
                    }
                }
                "PostCompact" if ev["trigger"] == "manual" => Self::set_phase(&inst, &mut st, Phase::Idle, None),
                _ => {}
            }
            rebound
        };
        if rebound {
            *inst.tr.lock().await = None;
            let _ = inst.tx.send(Event::Reset);
        }
        if name == "PermissionRequest" {
            return self.permission_request(&inst, tool.unwrap_or_default(), &ev).await;
        }
        String::new()
    }

    /// hook 带来的会话和 transcript 路径（`/clear`、`/resume` 会换）。换了返回 true。
    fn rebind(&self, id: &str, st: &mut State, ev: &Value) -> bool {
        let (Some(sid), Some(path)) = (ev["session_id"].as_str(), ev["transcript_path"].as_str()) else { return false };
        let path = PathBuf::from(path);
        if !valid_transcript_path(&path) {
            tracing::warn!(path = %path.display(), "hook 带来的 transcript 路径不对，忽略");
            return false;
        }
        let b = Binding { session_id: sid.to_string(), transcript_path: path };
        if st.binding.as_ref() == Some(&b) {
            return false;
        }
        let _ = self.store.set(&binding_key(id), &b);
        st.binding = Some(b);
        st.version = None;
        true
    }

    fn drop_pending(st: &mut State) {
        if let Some(mut p) = st.pending.take() {
            p.resolve(String::new());
        }
    }

    /// 挂起审批，等界面回答。TUI 那边同时也能回答：答完会有 PostToolUse 等事件把这里放掉。
    async fn permission_request(&self, inst: &Arc<Inst>, tool: String, ev: &Value) -> String {
        let (tx, rx) = oneshot::channel();
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        {
            let mut st = lock(&inst.st);
            Self::drop_pending(&mut st);
            st.pending = Some(Pending { id, at: Instant::now(), tool, input: ev["tool_input"].clone(), suggestions: ev["permission_suggestions"].clone(), tx: Some(tx) });
            st.phase = Phase::Approval;
            st.reason = None;
            Self::emit_status(inst, &st);
        }
        let out = tokio::time::timeout(PERMISSION_WAIT, rx).await.ok().and_then(|r| r.ok()).unwrap_or_default();
        let mut st = lock(&inst.st);
        if st.pending.as_ref().is_some_and(|p| p.id == id) {
            // 超时：TUI 的确认框还在等，要到终端里回答
            st.pending = None;
            Self::set_phase(inst, &mut st, Phase::Tui, None);
        }
        out
    }

    // ---------------- 界面：快照、事件、历史 ----------------

    /// 跟读 transcript，把变化推给界面。
    async fn poll(&self, inst: &Inst) {
        let path = lock(&inst.st).binding.as_ref().map(|b| b.transcript_path.clone());
        let Some(path) = path else { return };
        let mut tr = inst.tr.lock().await;
        let fresh = tr.as_ref().is_none_or(|t| t.path() != path);
        if fresh {
            *tr = Some(Transcript::new(path));
        }
        let t = tr.as_mut().expect("transcript");
        let change = tokio::task::block_in_place(|| t.poll());
        let (version, stats) = (t.version.clone(), t.stats);
        drop(tr);
        {
            let mut st = lock(&inst.st);
            if st.version != version {
                st.version = version;
                Self::emit_status(inst, &st);
            }
            st.stats = stats;
        }
        match change {
            Ok(Change::Upsert { start, items }) if !fresh => {
                let _ = inst.tx.send(Event::Items { start, items });
            }
            Ok(Change::Reset) => {
                let _ = inst.tx.send(Event::Reset);
            }
            Ok(_) => {}
            Err(e) => tracing::debug!(error = %e, "读取 transcript 失败"),
        }
    }

    /// 对话的快照：状态和最后 [`SNAPSHOT_ITEMS`] 条。
    pub async fn snapshot(self: &Arc<Self>, id: &str) -> Result<Value, LocalError> {
        self.require(id)?;
        let inst = self.inst(id);
        self.poll(&inst).await;
        let tr = inst.tr.lock().await;
        let (total, items) = match tr.as_ref() {
            Some(t) => (t.len(), t.items(t.len().saturating_sub(SNAPSHOT_ITEMS), t.len())),
            None => (0, vec![]),
        };
        drop(tr);
        let status = Self::status_of(&lock(&inst.st));
        Ok(json!({ "status": status, "total": total, "start": total - items.len(), "items": items }))
    }

    /// 往上翻：`before` 之前的 `limit` 条。
    pub async fn history(&self, id: &str, before: usize, limit: usize) -> Result<Value, LocalError> {
        self.require(id)?;
        let inst = self.inst(id);
        let tr = inst.tr.lock().await;
        let Some(t) = tr.as_ref() else { return Ok(json!({ "start": 0, "items": [] })) };
        let before = before.min(t.len());
        let start = before.saturating_sub(limit.clamp(1, 500));
        Ok(json!({ "start": start, "items": t.items(start, before) }))
    }

    /// 某条记录不截断的原始内容。
    pub async fn item(&self, id: &str, key: &str) -> Result<Value, LocalError> {
        self.require(id)?;
        let inst = self.inst(id);
        let tr = inst.tr.lock().await;
        let t = tr.as_ref().ok_or_else(LocalError::not_found)?;
        tokio::task::block_in_place(|| t.full_item(key)).ok_or_else(LocalError::not_found)
    }

    /// 打开一个对话视图：订阅事件，没在跟读就开始跟读和抓屏。
    pub fn subscribe(self: &Arc<Self>, id: &str) -> Result<(broadcast::Receiver<Event>, Viewer), LocalError> {
        self.require(id)?;
        let inst = self.inst(id);
        let rx = inst.tx.subscribe();
        inst.viewers.fetch_add(1, Ordering::SeqCst);
        if !inst.watching.swap(true, Ordering::SeqCst) {
            tokio::spawn(self.clone().watch(inst.clone()));
        }
        Ok((rx, Viewer { inst, _terminal: crate::updater::TerminalGuard::new() }))
    }

    /// 有人看着的时候：每 300 毫秒跟读 transcript，每 0.9 秒看一眼屏幕。
    async fn watch(self: Arc<Self>, inst: Arc<Inst>) {
        let mut tick = 0u32;
        loop {
            while inst.viewers.load(Ordering::SeqCst) > 0 {
                self.poll(&inst).await;
                if tick.is_multiple_of(3) {
                    self.check_screen(&inst).await;
                }
                tick = tick.wrapping_add(1);
                tokio::time::sleep(Duration::from_millis(300)).await;
            }
            inst.watching.store(false, Ordering::SeqCst);
            // 刚好在退出时又有人打开：接着看
            if inst.viewers.load(Ordering::SeqCst) == 0 || inst.watching.swap(true, Ordering::SeqCst) {
                break;
            }
        }
    }

    /// 抓屏纠正状态：hooks 覆盖不到信任对话框、选择器、Esc 中断，也看不出 claude 退出了（docs/CLAUDE_UI.md §4.3）。
    async fn check_screen(&self, inst: &Inst) {
        let pane = self.instances.pane(&inst.id).await;
        let capture = match &pane {
            Some(p) if !is_shell(&p.command) => self.instances.screen(&inst.id).await,
            _ => None,
        };
        let screen = capture.as_deref().map(screen::classify);
        let mut st = lock(&inst.st);
        let quiet = st.last_hook.is_none_or(|t| t.elapsed() >= QUIET);
        match (&pane, screen) {
            (None, _) => {
                st.claude_since = None;
                Self::set_phase(inst, &mut st, Phase::Detached, Some("no_session"));
            }
            (Some(_), None) => {
                // 面板里是 shell：claude 没在运行
                st.claude_since = None;
                Self::drop_pending(&mut st);
                Self::set_phase(inst, &mut st, Phase::Detached, Some("not_running"));
            }
            (Some(_), Some(scr)) => {
                let since = *st.claude_since.get_or_insert_with(Instant::now);
                let attached = st.binding.is_some() || since.elapsed() < ATTACH_GRACE;
                let pending_age = st.pending.as_ref().map(|p| p.at.elapsed());
                match (st.phase, scr) {
                    // 确认框已经不在了（在终端里按了 Esc 或选了“否”：这两种都没有 hook）
                    (Phase::Approval, Screen::Input { busy, .. }) if pending_age.is_some_and(|a| a >= QUIET) => {
                        Self::drop_pending(&mut st);
                        Self::set_phase(inst, &mut st, if busy { Phase::Running } else { Phase::Idle }, None);
                    }
                    (Phase::Approval, _) => {}
                    (_, Screen::Modal) if st.phase == Phase::Tui => {}
                    (Phase::Running, Screen::Modal) if !quiet => {}
                    (_, Screen::Modal) => Self::set_phase(inst, &mut st, Phase::Tui, None),
                    (_, Screen::Input { .. }) if !attached => Self::set_phase(inst, &mut st, Phase::Detached, Some("not_attached")),
                    (Phase::Running, Screen::Input { busy, .. }) if quiet && !busy => Self::set_phase(inst, &mut st, Phase::Idle, None),
                    (Phase::Running, Screen::Input { .. }) => {}
                    (_, Screen::Input { busy, .. }) => Self::set_phase(inst, &mut st, if busy { Phase::Running } else { Phase::Idle }, None),
                }
                // 运行中：把屏幕上正在输出的内容推给界面（transcript 要等整段写完才有）
                if st.phase == Phase::Running {
                    let live = capture.as_deref().map(screen::live).unwrap_or_default();
                    if live != st.live {
                        st.live = live;
                        Self::emit_status(inst, &st);
                    }
                }
            }
        }
    }

    /// 会话列表上的角标：等待确认、要到终端里操作。没收到过这个终端的 hook 时为 None。
    pub fn brief(&self, id: &str) -> Option<Value> {
        let inst = lock(&self.insts).get(id).cloned()?;
        let st = lock(&inst.st);
        Some(json!({ "phase": st.phase, "pending": st.pending.as_ref().map(|p| p.kind()) }))
    }

    /// 输入 / 时提示的自定义命令和技能
    pub fn commands(&self, id: &str) -> Result<Value, LocalError> {
        self.require(id)?;
        let row = self.store.instance(id)?.ok_or_else(LocalError::not_found)?;
        Ok(json!({ "items": commands::scan(Path::new(&row.workdir)) }))
    }

    /// 子代理（Task / Agent 工具）自己的步骤。`agent`：工具结果里的 agentId；还没有结果时用 `tool`（工具调用 id）
    /// 按 prompt 找：子代理 transcript 的第一条就是它收到的 prompt。
    pub async fn agent(&self, id: &str, agent: Option<&str>, tool: Option<&str>) -> Result<Value, LocalError> {
        self.require(id)?;
        let inst = self.inst(id);
        let main = lock(&inst.st).binding.as_ref().map(|b| b.transcript_path.clone()).ok_or_else(LocalError::not_found)?;
        let dir = main.with_extension("").join("subagents");
        let file = match (agent, tool) {
            (Some(a), _) if !a.is_empty() && a.chars().all(|c| c.is_ascii_alphanumeric()) => dir.join(format!("agent-{a}.jsonl")),
            (_, Some(t)) => {
                let tr = inst.tr.lock().await;
                let prompt = tr.as_ref().and_then(|t2| t2.tool_input(t)).and_then(|v| v["prompt"].as_str()).map(String::from).ok_or_else(LocalError::not_found)?;
                drop(tr);
                tokio::task::block_in_place(|| find_subagent(&dir, &prompt)).ok_or_else(LocalError::not_found)?
            }
            _ => return Err(LocalError::invalid("agent", "required")),
        };
        if !file.is_file() {
            return Err(LocalError::not_found());
        }
        let mut t = Transcript::subagent(file.clone());
        tokio::task::block_in_place(|| t.poll())?;
        let start = t.len().saturating_sub(300);
        let agent_id = file.file_stem().and_then(|s| s.to_str()).and_then(|s| s.strip_prefix("agent-")).map(String::from);
        Ok(json!({ "agent": agent_id, "start": start, "items": t.items(start, t.len()) }))
    }

    // ---------------- 界面：操作 ----------------

    /// 发消息：文字按“打字”的方式送进去，图片路径用粘贴（TUI 才会变成图片附件），再按回车。
    /// 返回是否确认送达（收到了对应的 UserPromptSubmit）；没接上 hooks 时为 null。
    pub async fn input(self: &Arc<Self>, id: &str, text: &str, images: &[String]) -> Result<Value, LocalError> {
        self.require(id)?;
        let inst = self.inst(id);
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        let text = text.trim_end();
        if text.is_empty() && images.is_empty() {
            return Err(LocalError::invalid("text", "required"));
        }
        for p in images {
            if !Path::new(p).is_file() {
                return Err(LocalError::invalid("images", "not_found"));
            }
        }
        let hooked = {
            let st = lock(&inst.st);
            match st.phase {
                Phase::Detached => return Err(LocalError::new("CHAT_NOT_RUNNING")),
                Phase::Tui | Phase::Approval => return Err(LocalError::new("CHAT_BUSY")),
                _ => {}
            }
            st.binding.is_some()
        };
        // 确认输入框是普通输入框（选择器打开时打进去的字母会被当成快捷键）。
        // 刚启动时 SessionStart 比输入框先到，多看几次。
        let mut ready = false;
        for _ in 0..10 {
            if !matches!(self.instances.screen(id).await.map(|s| screen::classify(&s)), Some(Screen::Modal)) {
                ready = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
        if !ready {
            let mut st = lock(&inst.st);
            Self::set_phase(&inst, &mut st, Phase::Tui, None);
            return Err(LocalError::new("CHAT_BUSY"));
        }
        let mut prompts = inst.prompts.subscribe();
        prompts.borrow_and_update();
        if text.len() > PASTE_ABOVE {
            self.instances.paste(id, text).await?;
        } else {
            self.type_text(id, text).await?;
        }
        for p in images {
            self.instances.paste(id, &format!(" {p}")).await?;
        }
        tokio::time::sleep(Duration::from_millis(80)).await;
        self.instances.send_keys(id, &["Enter"]).await?;
        if !hooked {
            return Ok(json!({ "delivered": null }));
        }
        if tokio::time::timeout(DELIVERY_WAIT, prompts.changed()).await.is_ok() {
            return Ok(json!({ "delivered": true }));
        }
        // 没收到：文字还在输入框里就再按一次回车（TUI 刚启动、刚结束一轮时偶尔会吞掉回车）
        let first: String = text.lines().next().unwrap_or("").chars().take(20).collect();
        let still_there = matches!(self.instances.screen(id).await.map(|s| screen::classify(&s)), Some(Screen::Input { text: t, .. }) if !first.is_empty() && t.contains(&first));
        if still_there {
            self.instances.send_keys(id, &["Enter"]).await?;
            if tokio::time::timeout(DELIVERY_WAIT, prompts.changed()).await.is_ok() {
                return Ok(json!({ "delivered": true }));
            }
        }
        Ok(json!({ "delivered": false }))
    }

    /// 分块“打字”：每块 64 个字符、块间 30 毫秒，换行用 Ctrl-J（实测这样送进去的是普通文字，不会变成粘贴）。
    async fn type_text(&self, id: &str, text: &str) -> Result<(), LocalError> {
        for (i, line) in text.split('\n').enumerate() {
            if i > 0 {
                self.instances.send_keys(id, &["C-j"]).await?;
                tokio::time::sleep(TYPE_GAP).await;
            }
            let chars: Vec<char> = line.chars().collect();
            for chunk in chars.chunks(TYPE_CHUNK) {
                self.instances.send_literal(id, &chunk.iter().collect::<String>()).await?;
                tokio::time::sleep(TYPE_GAP).await;
            }
        }
        Ok(())
    }

    /// 停止这一轮（Esc）。没有 hook 会告诉我们停下来了，交给抓屏确认。
    pub async fn interrupt(&self, id: &str) -> Result<(), LocalError> {
        self.require(id)?;
        self.instances.send_keys(id, &["Escape"]).await?;
        let inst = self.inst(id);
        lock(&inst.st).last_hook = None;
        Ok(())
    }

    /// 回答审批。`decision`：`allow` / `allow_session` / `deny` / `answer`（AskUserQuestion）/ `terminal`。
    pub fn answer(&self, id: &str, req: &PermissionAnswer) -> Result<(), LocalError> {
        self.require(id)?;
        let inst = self.inst(id);
        let mut st = lock(&inst.st);
        let Some(p) = st.pending.as_mut().filter(|p| p.id == req.id) else { return Err(LocalError::new("CHAT_STALE")) };
        let decision = match req.decision.as_str() {
            "allow" => json!({ "behavior": "allow" }),
            "allow_session" => {
                let mut rule = json!({ "toolName": p.tool });
                if let Some(cmd) = p.input["command"].as_str() {
                    rule["ruleContent"] = json!(cmd);
                }
                json!({ "behavior": "allow", "updatedPermissions": [{ "type": "addRules", "rules": [rule], "behavior": "allow", "destination": "session" }] })
            }
            // 只回传“允许访问这个目录”那一条建议：建议里可能还有 setMode，会切换权限模式
            "allow_dirs" => {
                let dirs: Vec<Value> = p.suggestions.as_array().map(|a| a.iter().filter(|s| s["type"] == "addDirectories").cloned().collect()).unwrap_or_default();
                if dirs.is_empty() {
                    return Err(LocalError::invalid("decision", "invalid"));
                }
                json!({ "behavior": "allow", "updatedPermissions": dirs })
            }
            "deny" => {
                let msg = req.message.as_deref().map(str::trim).filter(|m| !m.is_empty()).unwrap_or("The user denied this in the Looklook chat view.");
                json!({ "behavior": "deny", "message": msg.chars().take(2000).collect::<String>() })
            }
            "answer" if p.kind() == "question" => {
                let answers = req.answers.as_ref().filter(|a| a.is_object()).ok_or_else(|| LocalError::invalid("answers", "required"))?;
                let mut input = p.input.clone();
                input["answers"] = answers.clone();
                json!({ "behavior": "allow", "updatedInput": input })
            }
            "terminal" => {
                p.resolve(String::new());
                st.pending = None;
                Self::set_phase(&inst, &mut st, Phase::Tui, None);
                return Ok(());
            }
            _ => return Err(LocalError::invalid("decision", "invalid")),
        };
        p.resolve(json!({ "hookSpecificOutput": { "hookEventName": "PermissionRequest", "decision": decision } }).to_string());
        st.pending = None;
        Self::set_phase(&inst, &mut st, Phase::Running, None);
        Ok(())
    }

    /// 计划审批：hook 批准不了（实测），只能在 TUI 的确认框里按数字键。先抓屏核对确认框还在、选项和预期一致。
    pub async fn plan(&self, id: &str, choice: u8) -> Result<(), LocalError> {
        self.require(id)?;
        let expect = match choice {
            1 => "1. Yes",
            2 => "2. Yes",
            _ => return Err(LocalError::invalid("choice", "invalid")),
        };
        let shown = self.instances.screen(id).await.unwrap_or_default();
        let plain = shown.lines().map(screen_plain).collect::<Vec<_>>().join("\n");
        if !(plain.contains("ready to execute") || plain.contains("Would you like to proceed")) || !plain.contains(expect) {
            let inst = self.inst(id);
            let mut st = lock(&inst.st);
            Self::set_phase(&inst, &mut st, Phase::Tui, None);
            return Err(LocalError::new("CHAT_SCREEN_CHANGED"));
        }
        self.instances.send_keys(id, &[&choice.to_string()]).await
    }

    /// 面板里是 shell（claude 已退出，或是没带 hooks 启动后退出了）：以对话模式接着上次的会话启动。
    pub async fn relaunch(&self, id: &str) -> Result<(), LocalError> {
        self.require(id)?;
        let pane = self.instances.pane(id).await.ok_or_else(|| LocalError::new("CHAT_NOT_RUNNING"))?;
        if !is_shell(&pane.command) {
            return Err(LocalError::new("CHAT_CLAUDE_RUNNING"));
        }
        let session = lock(&self.inst(id).st).binding.as_ref().map(|b| b.session_id.clone());
        let cmd = self.instances.chat_relaunch_command(id, session.as_deref())?;
        self.instances.send_literal(id, &cmd).await?;
        self.instances.send_keys(id, &["Enter"]).await
    }
}

#[derive(Debug, Deserialize)]
pub struct PermissionAnswer {
    pub id: u64,
    pub decision: String,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub answers: Option<Value>,
}

/// 按收到的 prompt 找子代理的 transcript（新的优先）。prompt 在参数里可能被截断过，只比开头。
fn find_subagent(dir: &Path, prompt: &str) -> Option<PathBuf> {
    use std::io::BufRead;
    let head: String = prompt.chars().take(300).collect();
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
        .filter_map(|p| Some((std::fs::metadata(&p).and_then(|m| m.modified()).ok()?, p)))
        .collect();
    files.sort_by_key(|f| std::cmp::Reverse(f.0));
    files.into_iter().take(200).map(|(_, p)| p).find(|p| {
        let Ok(f) = std::fs::File::open(p) else { return false };
        let mut first = String::new();
        if std::io::BufReader::new(f).read_line(&mut first).is_err() {
            return false;
        }
        let v: Value = serde_json::from_str(&first).unwrap_or_default();
        let content = &v["message"]["content"];
        let text = content.as_str().map(String::from).or_else(|| content.as_array().and_then(|a| a.iter().find_map(|b| b["text"].as_str().map(String::from)))).unwrap_or_default();
        !head.is_empty() && text.starts_with(&head)
    })
}

/// transcript 只能是 `…/projects/<项目>/<session>.jsonl`：防止借伪造的 hook 读任意文件（hook 本身还要令牌）。
fn valid_transcript_path(p: &Path) -> bool {
    p.is_absolute()
        && p.extension().is_some_and(|e| e == "jsonl")
        && p.parent().and_then(|d| d.parent()).and_then(|d| d.file_name()).is_some_and(|n| n == "projects")
        && !p.components().any(|c| matches!(c, std::path::Component::ParentDir))
}

/// 面板前台是 shell：claude 没在运行。
fn is_shell(command: &str) -> bool {
    let name = command.rsplit(['/', '\\']).next().unwrap_or("").trim_start_matches('-').to_ascii_lowercase();
    let name = name.strip_suffix(".exe").unwrap_or(&name);
    matches!(name, "bash" | "zsh" | "fish" | "sh" | "dash" | "ksh" | "mksh" | "tcsh" | "csh" | "nu" | "pwsh" | "powershell" | "cmd" | "login" | "")
}

fn screen_plain(line: &str) -> String {
    let mut out = String::new();
    let mut it = line.chars().peekable();
    while let Some(c) = it.next() {
        if c == '\x1b' {
            if it.peek() == Some(&'[') {
                it.next();
                for p in it.by_ref() {
                    if p.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_subagent_by_prompt() {
        let d = tempfile::tempdir().unwrap();
        let line = |p: &str| format!("{}\n", json!({"type":"user","isSidechain":true,"message":{"content":p}}));
        std::fs::write(d.path().join("agent-aaa.jsonl"), line("查一下 A")).unwrap();
        std::fs::write(d.path().join("agent-bbb.jsonl"), line("查一下 B 的很长的说明")).unwrap();
        assert_eq!(find_subagent(d.path(), "查一下 B").unwrap().file_name().unwrap(), "agent-bbb.jsonl");
        assert!(find_subagent(d.path(), "不存在").is_none());
        assert!(find_subagent(d.path(), "").is_none());
    }

    #[test]
    fn transcript_paths() {
        assert!(valid_transcript_path(Path::new("/root/.claude/projects/-tmp-proj/96e9b84c.jsonl")));
        assert!(!valid_transcript_path(Path::new("/etc/passwd")));
        assert!(!valid_transcript_path(Path::new("/root/.claude/projects/x/../../../etc/a.jsonl")));
        assert!(!valid_transcript_path(Path::new("relative/projects/x/a.jsonl")));
    }

    /// 端到端：真的 tmux + 真的 Claude Code。要登录好的 claude，并消耗少量额度，默认不跑：
    /// `CHAT_E2E_BIN=<放 claude 的目录> CHAT_E2E_DIR=<已信任的项目目录> cargo test chat_e2e -- --ignored --nocapture`
    #[tokio::test(flavor = "multi_thread")]
    #[ignore]
    async fn chat_e2e() {
        use std::sync::Arc;
        let bin = std::env::var("CHAT_E2E_BIN").expect("CHAT_E2E_BIN");
        let dir = std::env::var("CHAT_E2E_DIR").expect("CHAT_E2E_DIR");
        std::env::set_var("PATH", format!("{bin}:{}", std::env::var("PATH").unwrap_or_default()));
        std::env::set_var("LLV_CLAUDE_ARGS", "--permission-mode default");
        // hook 用 cargo build 出来的 looklook（测试程序在 target/debug/deps/ 里）
        let exe = std::env::current_exe().unwrap().parent().unwrap().parent().unwrap().join("looklook");
        assert!(exe.is_file(), "先 cargo build");
        std::env::set_var("LOOKLOOK_HOOK_EXE", exe);
        let home = tempfile::tempdir().unwrap();
        let paths = Paths::discover(Some(home.path().to_path_buf())).unwrap();
        let store = Arc::new(Store::memory().unwrap());
        let instances = Instances::new(store.clone(), paths.clone());
        assert!(matches!(instances.backend, crate::instances::Backend::Tmux(_)), "需要 tmux");
        let input = crate::instances::Input { name: Some("e2e".into()), workdir: Some(dir.clone()), launch: Some("claude".into()), ..Default::default() };
        let id = instances.create(input).await.unwrap().row.id;
        assert!(instances.opts(&id).chat_ui, "新建的 Claude Code 终端默认带对话界面");
        let chat = Chat::new(store.clone(), paths.clone(), instances.clone());
        // 只有 /hook 的小服务器
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let c2 = chat.clone();
        let app = axum::Router::new().route(
            hooks::PATH,
            axum::routing::post(move |headers: axum::http::HeaderMap, body: axum::body::Bytes| {
                let chat = c2.clone();
                async move {
                    let h = |k: &str| headers.get(k).and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
                    if !chat.token_ok(&h(hooks::TOKEN_HEADER)) {
                        return String::new();
                    }
                    chat.hook(&h(hooks::INSTANCE_HEADER), serde_json::from_slice(&body).unwrap_or_default()).await
                }
            }),
        );
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        chat.publish(addr).unwrap();
        let row = store.instance(&id).unwrap().unwrap();
        instances.ensure_session(&row).await.unwrap();
        let (_rx, viewer) = chat.subscribe(&id).unwrap();

        async fn wait(chat: &Arc<Chat>, id: &str, what: &str, secs: u64, f: impl Fn(&Value) -> bool) -> Value {
            for _ in 0..secs * 4 {
                let s = chat.snapshot(id).await.unwrap();
                if f(&s) {
                    return s;
                }
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
            panic!("等不到：{what}；最后的状态 {}", chat.snapshot(id).await.unwrap()["status"]);
        }
        let phase = |p: &'static str| move |s: &Value| s["status"]["phase"] == p;
        let last_text = |s: &Value| {
            s["items"].as_array().unwrap().iter().rev().find(|i| i["k"] == "assistant").map(|i| i.to_string()).unwrap_or_default()
        };

        let s = wait(&chat, &id, "SessionStart", 40, |s| s["status"]["session_id"].is_string() && s["status"]["phase"] == "idle").await;
        println!("已接上：{}", s["status"]);

        let r = chat.input(&id, "只回复两个字母 ok，不要别的", &[]).await.unwrap();
        assert_eq!(r["delivered"], true, "{r}");
        let s = wait(&chat, &id, "回复 ok", 60, |s| s["status"]["phase"] == "idle" && last_text(s).to_lowercase().contains("ok")).await;
        println!("回复：{}", last_text(&s));

        // 多行、长文字：作为普通文字送达（不是粘贴内容）
        let long = (1..=20).map(|i| format!("第{i}行：多行输入测试，内容要足够长才能触发粘贴判断。")).collect::<Vec<_>>().join("\n") + "\n最后：只回复你看到了几行（数字）";
        let r = chat.input(&id, &long, &[]).await.unwrap();
        assert_eq!(r["delivered"], true);
        let s = wait(&chat, &id, "多行回复", 60, |s| s["status"]["phase"] == "idle" && last_text(s).contains('2')).await;
        let users: Vec<&Value> = s["items"].as_array().unwrap().iter().filter(|i| i["k"] == "user").collect();
        let sent = users.last().unwrap()["text"].as_str().unwrap();
        assert!(sent.starts_with("第1行") && !sent.contains("pasted_content"), "{sent}");

        // 权限审批：界面上允许
        let _ = std::fs::remove_file(format!("{dir}/chat-e2e.txt"));
        chat.input(&id, "用 Bash 执行 touch chat-e2e.txt，然后只回复 done", &[]).await.unwrap();
        let s = wait(&chat, &id, "审批", 60, |s| s["status"]["pending"]["kind"] == "permission").await;
        let req = PermissionAnswer { id: s["status"]["pending"]["id"].as_u64().unwrap(), decision: "allow".into(), message: None, answers: None };
        chat.answer(&id, &req).unwrap();
        wait(&chat, &id, "审批后完成", 60, phase("idle")).await;
        assert!(Path::new(&format!("{dir}/chat-e2e.txt")).exists(), "允许后命令执行了");

        // 拒绝并附理由
        chat.input(&id, "用 Bash 执行 touch chat-e2e-2.txt", &[]).await.unwrap();
        let s = wait(&chat, &id, "第二次审批", 60, |s| s["status"]["pending"]["kind"] == "permission").await;
        let req = PermissionAnswer { id: s["status"]["pending"]["id"].as_u64().unwrap(), decision: "deny".into(), message: Some("不要建这个文件".into()), answers: None };
        chat.answer(&id, &req).unwrap();
        let s = wait(&chat, &id, "拒绝后停下", 60, phase("idle")).await;
        assert!(!Path::new(&format!("{dir}/chat-e2e-2.txt")).exists());
        assert!(s.to_string().contains("不要建这个文件"), "拒绝理由在工具结果里");

        // AskUserQuestion：界面上作答
        chat.input(&id, "请用 AskUserQuestion 工具问我喜欢哪种颜色，给红、蓝两个选项，然后根据回答只说一句话", &[]).await.unwrap();
        let s = wait(&chat, &id, "提问", 60, |s| s["status"]["pending"]["kind"] == "question").await;
        let q = &s["status"]["pending"]["input"]["questions"][0];
        let answers = json!({ q["question"].as_str().unwrap(): q["options"][1]["label"] });
        let req = PermissionAnswer { id: s["status"]["pending"]["id"].as_u64().unwrap(), decision: "answer".into(), message: None, answers: Some(answers) };
        chat.answer(&id, &req).unwrap();
        let s = wait(&chat, &id, "回答后完成", 60, phase("idle")).await;
        // Claude 收到的工具结果里就是界面上选的答案
        let answered = s["items"].as_array().unwrap().iter().any(|i| i["k"] == "tool_result" && i["content"].as_str().is_some_and(|c| c.contains(&format!("={}", q["options"][1]["label"]))));
        assert!(answered, "AskUserQuestion 的结果里没有界面上选的答案：{}", last_text(&s));

        // 选择器（/model）：变成 tui，界面拒绝输入；Esc 关掉后回到空闲
        chat.input(&id, "/model", &[]).await.unwrap();
        wait(&chat, &id, "选择器 → tui", 20, phase("tui")).await;
        assert_eq!(chat.input(&id, "hello", &[]).await.unwrap_err().code, "CHAT_BUSY");
        instances.send_keys(&id, &["Escape"]).await.unwrap();
        wait(&chat, &id, "关掉选择器", 20, phase("idle")).await;

        // 停止：Esc，没有 hook，靠抓屏回到空闲
        chat.input(&id, "用 Bash 在前台执行这条命令：for i in 1 2 3 4 5 6 7 8 9 10 11 12; do echo $i; sleep 2; done ，然后回复 完成", &[]).await.unwrap();
        let s = wait(&chat, &id, "循环命令的审批", 60, |s| s["status"]["pending"].is_object()).await;
        // 先在终端里按 Esc 取消确认框（没有 hook）：界面上的审批条要自己消失
        tokio::time::sleep(Duration::from_secs(2)).await;
        instances.send_keys(&id, &["Escape"]).await.unwrap();
        wait(&chat, &id, "终端里取消后审批条消失", 20, |s| s["status"]["pending"].is_null() && s["status"]["phase"] == "idle").await;
        let _ = s;
        chat.input(&id, "再执行一次刚才那条循环命令", &[]).await.unwrap();
        let s = wait(&chat, &id, "第二次循环命令的审批", 60, |s| s["status"]["pending"].is_object()).await;
        chat.answer(&id, &PermissionAnswer { id: s["status"]["pending"]["id"].as_u64().unwrap(), decision: "allow".into(), message: None, answers: None }).unwrap();
        wait(&chat, &id, "循环开始", 20, |s| s["status"]["phase"] == "running").await;
        tokio::time::sleep(Duration::from_secs(4)).await;
        chat.interrupt(&id).await.unwrap();
        let s = wait(&chat, &id, "停止后空闲", 30, phase("idle")).await;
        assert!(s["items"].as_array().unwrap().iter().any(|i| i["k"] == "interrupt"));

        // 正在输出的预览：长回复写完之前，状态里就有屏幕上的文字
        chat.input(&id, "不要使用任何工具，直接写一篇 500 字左右的中文短文，介绍 Rust 的所有权", &[]).await.unwrap();
        let mut seen_preview = false;
        let mut seen_activity = false;
        for _ in 0..240 {
            let s = chat.snapshot(&id).await.unwrap();
            seen_preview |= s["status"]["preview"].as_str().is_some_and(|p| p.chars().count() > 20);
            seen_activity |= s["status"]["activity"].is_string();
            if s["status"]["phase"] == "idle" && seen_preview {
                assert!(s["status"]["preview"].is_null(), "空闲时清掉预览");
                break;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        assert!(seen_preview, "运行中应该有正在输出的预览");
        println!("看到状态行：{seen_activity}");

        // 子代理：运行中就能按 prompt 找到它的步骤，结束后按 agentId 找
        chat.input(&id, "请使用 Agent 工具（subagent_type 用 general-purpose）启动一个子代理，让它用 Glob 工具列出当前目录下的 *.py 文件并汇报结果。你自己不要直接用 Glob。", &[]).await.unwrap();
        let mut found_running = false;
        let mut tool_id = String::new();
        for _ in 0..360 {
            let s = chat.snapshot(&id).await.unwrap();
            if let Some(p) = s["status"]["pending"]["id"].as_u64() {
                let _ = chat.answer(&id, &PermissionAnswer { id: p, decision: "allow".into(), message: None, answers: None });
            }
            let items = s["items"].as_array().unwrap();
            if let Some(b) = items.iter().rev().filter(|i| i["k"] == "assistant").flat_map(|i| i["blocks"].as_array().unwrap().iter()).find(|b| b["t"] == "tool_use" && (b["name"] == "Agent" || b["name"] == "Task")) {
                tool_id = b["id"].as_str().unwrap().to_string();
                if !found_running && s["status"]["phase"] != "idle" {
                    found_running = chat.agent(&id, None, Some(&tool_id)).await.is_ok();
                }
            }
            if s["status"]["phase"] == "idle" && !tool_id.is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        assert!(!tool_id.is_empty(), "主会话里有 Agent 工具调用");
        let s = chat.snapshot(&id).await.unwrap();
        let result = s["items"].as_array().unwrap().iter().find(|i| i["k"] == "tool_result" && i["tool_use_id"] == tool_id.as_str()).cloned().unwrap();
        let agent_id = result["extra"]["agentId"].as_str().map(String::from);
        // 子代理可能在后台运行（主会话先结束这一轮）：等它把步骤写出来
        let mut sub = json!({});
        for _ in 0..240 {
            sub = chat.agent(&id, agent_id.as_deref(), Some(&tool_id)).await.unwrap();
            if sub["items"].as_array().unwrap().iter().any(|i| i["k"] == "assistant") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        let steps = sub["items"].as_array().unwrap();
        println!("子代理 {:?}：{} 条，运行中就找到：{found_running}", sub["agent"], steps.len());
        assert!(steps.iter().any(|i| i["k"] == "assistant"), "子代理的步骤：{sub}");

        // 自定义命令
        std::fs::create_dir_all(format!("{dir}/.claude/commands")).unwrap();
        std::fs::write(format!("{dir}/.claude/commands/hello-e2e.md"), "---\ndescription: 端到端测试命令\n---\n说你好").unwrap();
        let cmds = chat.commands(&id).unwrap();
        assert!(cmds["items"].as_array().unwrap().iter().any(|c| c["name"] == "/hello-e2e" && c["description"] == "端到端测试命令"));
        let _ = std::fs::remove_file(format!("{dir}/.claude/commands/hello-e2e.md"));

        // 会话列表的角标
        // （子代理可能在后台运行，回来后主会话又会开始一轮，所以不限定状态）
        assert!(chat.brief(&id).unwrap()["phase"].is_string());

        drop(viewer);
        instances.stop(&id).await.unwrap();
        let _ = std::fs::remove_file(format!("{dir}/chat-e2e.txt"));
    }

    #[test]
    fn shells() {
        for s in ["bash", "-zsh", "/bin/fish", "pwsh.exe", "cmd.exe", ""] {
            assert!(is_shell(s), "{s}");
        }
        for s in ["claude", "2.1.302", "node", "vim"] {
            assert!(!is_shell(s), "{s}");
        }
    }
}
