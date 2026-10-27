//! Claude Code 对话界面（docs/CLAUDE_UI.md）。
//!
//! tmux 里照常运行交互式 `claude`（启动时用 `--settings` 注入 hooks），对话界面是同一个进程的第二个视图：
//!
//! - **hooks**（`hooks.rs`、`POST /hook`）：实时状态（运行中、空闲、等待审批）、绑定会话与 transcript、权限审批；
//! - **transcript**（`transcript.rs`）：对话内容的唯一来源，hook 丢了也能补回来；
//! - **tmux**：送输入（分块“打字”，图片路径用粘贴）、停止（Esc）、抓屏判断 TUI 当前是不是普通输入框（`screen.rs`）。
//!
//! 审批时 TUI 也同时弹出了自己的确认框，两边谁先回答算谁的（实测）；任何异常都交给 TUI，不会自动放行。
//!
//! Codex 不走这一套（docs/CODEX_UI.md）：对话模式的 Codex 由看看通过 `codex app-server` 驱动（`codex_chat.rs`），
//! 不跑 TUI、没有 hooks。快照、事件推送、读记录文件（rollout）和审批挂起两边共用；任务面板、后台命令只有 Claude Code 有。

pub mod agent;
pub mod board;
pub mod codex_app;
pub mod codex_artifacts;
pub mod codex_chat;
pub mod codex_launch;
pub mod codex_rollout;
pub mod codex_screen;
pub mod commands;
pub mod hooks;
pub mod models;
pub mod prompts;
pub mod risk;
pub mod screen;
pub mod transcript;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::{broadcast, oneshot, watch};

use crate::error::LocalError;
use crate::instances::Instances;
use crate::paths::Paths;
use crate::store::Store;
use agent::{Agent, PendingView, TranscriptReader};
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
/// 运行中多久没有 hook 就去看一眼屏幕（Esc 中断没有任何 hook）
const QUIET: Duration = Duration::from_secs(3);
/// 压缩上下文最多按这么久算“运行中”
const COMPACT_MAX: Duration = Duration::from_secs(600);
/// claude 在运行却一直没有 SessionStart：没带 hooks 启动，或者 hooks 被用户配置关掉了
const ATTACH_GRACE: Duration = Duration::from_secs(10);
/// Claude Code 启动后头几秒屏幕是空的（2.1.310 实测 3 秒多，登录 shell 慢时更久），之后才画出输入框：
/// 这段时间里认不出来的屏幕算“还在启动”，不弹“在等你选择”。读得出来的选择框（信任目录等）照常马上弹
const STARTUP_WAIT: Duration = Duration::from_secs(20);
/// 额度用完的菜单多久没人选，就替用户选“等重置后自动继续”
/// Claude Code 从这个版本起有“立即发送”（`chat:sendNow`，Ctrl+X Ctrl+S）
const SEND_NOW_SINCE: (u32, u32, u32) = (2, 1, 275);
const LIMIT_AUTO_AFTER: Duration = Duration::from_secs(60);
/// 额度用完（StopFailure）后多久还没看到菜单，就不再等它
const LIMIT_MENU_WAIT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// Claude Code 没在运行，或者没接上 hooks（`reason` 说明）
    #[default]
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

/// 对话界面记下的这个终端当前的 Claude Code 会话（`/clear`、`/resume` 后会换）。
pub fn binding(store: &crate::store::Store, id: &str) -> Option<Binding> {
    store.get(&binding_key(id)).ok().flatten()
}

struct Pending {
    id: u64,
    at: Instant,
    tool: String,
    /// `permission` / `question` / `plan`（[`Agent::pending_kind`]）
    kind: &'static str,
    input: Value,
    suggestions: Value,
    /// 命令规则要人确认的（规则的样子，见 guard.rs）：答复给 PreToolUse，不能“以后都允许”，也不能交给终端
    guard: Option<String>,
    tx: Option<oneshot::Sender<String>>,
}

impl Pending {
    fn kind(&self) -> &'static str {
        self.kind
    }

    fn resolve(&mut self, out: String) {
        if let Some(tx) = self.tx.take() {
            let _ = tx.send(out);
        }
    }
}

#[derive(Default)]
struct State {
    binding: Option<Binding>,
    phase: Phase,
    reason: Option<&'static str>,
    tool: Option<String>,
    mode: Option<String>,
    model: Option<String>,
    effort: Option<String>,
    /// 屏幕状态栏上的后台任务个数（`1 shell`、`2 subagents`）
    background: Vec<(u32, String)>,
    /// 正在运行的子代理（SubagentStart 到 SubagentStop）
    agents: HashSet<String>,
    error: Option<String>,
    pending: Option<Pending>,
    last_hook: Option<Instant>,
    /// 这次看到 claude 在运行的起点（判断“没接上 hooks”）
    claude_since: Option<Instant>,
    version: Option<String>,
    stats: transcript::Stats,
    /// 运行中屏幕上的状态行和正在输出的那段回复（transcript 里还没有）
    live: screen::Live,
    /// 正在替界面按键（切换模型时的确认框、后台任务列表）：这段时间里屏幕上的对话框不算“要到终端里操作”
    hold: Option<Instant>,
    /// 最近一条回复实际用的模型、那条回复的 message.id
    actual_model: Option<String>,
    model_msg: Option<String>,
    /// 在界面上选模型时最近一条回复是哪条：之后有了新回复，选的模型才算生效
    model_set_at: Option<Option<String>>,
    /// 最近一条回复时上下文里的 token 数
    context: Option<u64>,
    title: Option<String>,
    /// 运行中发的、还在 Claude Code 队列里的消息
    queued: Vec<String>,
    /// 额度用完、正在等重置后自动继续：屏幕上 `continuing automatically at 3pm` 里的 `at 3pm`
    limit_wait: Option<String>,
    /// 额度用完的菜单没人选时，看看在这个时间（毫秒）替用户选“等重置后自动继续”
    limit_auto_at: Option<i64>,
    /// TUI 上的选择框（phase 为 tui 时）
    dialog: Option<screen::Dialog>,
    /// 空闲时输入框里灰色的推荐回复（Claude Code 跑完一轮后给的下一步建议）
    suggestion: Option<String>,
    /// Codex（app-server）的这一轮、上下文窗口、累计 token
    codex: codex_chat::CodexSt,
    /// 连续几次抓屏都认不出输入框、也读不出选择框（重绘、缩放到一半时会抓到这种半截屏幕）
    odd_screens: u8,
    /// 这次启动后屏幕上出现过输入框（之前认不出来的屏幕多半是还在启动，见 [`STARTUP_WAIT`]）
    drawn: bool,
    /// 正在压缩上下文（PreCompact 到 PostCompact）：这期间屏幕上的转圈认不出来，不能按屏幕改成空闲
    compacting: Option<Instant>,
    /// 看板（board.rs）：进入当前状态的时间（毫秒）
    since: i64,
    /// 最近一次有进展的时间（毫秒）：hook 事件、Codex 事件、状态变化。卡住提醒用（docs/PLAN_2026-10-27.md §2）
    active_at: i64,
    /// 看板：这一轮、看过没有、标题（存在本机数据库里）
    board: board::Meta,
    /// `board` 改了、还没存
    board_dirty: bool,
    /// 这一轮怎么结束的（hook 先知道，切到空闲时用）
    end_hint: Option<board::Outcome>,
    /// 有人正在看这个会话（会话页可见且有焦点）到这个时间（毫秒）
    active_until: i64,
    /// 正在做的事（`board::doing`）
    doing: Option<Value>,
    /// 这一轮的摘要已经找过了
    summary_tried: bool,
    /// Codex 最后一段回复（摘要用）
    last_reply: Option<String>,
}

/// 推给界面的事件
#[derive(Debug, Clone)]
pub enum Event {
    Status(Value),
    Items { start: usize, items: Vec<Item> },
    /// 前面的内容变了或者换了会话：重新取快照
    Reset,
}

/// 跟读的记录文件：Claude Code 的 transcript（任务面板、子代理这些只有它有），或者别的工具的（Codex 的 rollout）
enum Reader {
    Claude(Transcript),
    Other(Box<dyn TranscriptReader>),
}

impl Reader {
    fn open(agent: &dyn Agent, path: PathBuf) -> Self {
        match agent.launch() {
            "claude" => Reader::Claude(Transcript::new(path)),
            _ => Reader::Other(agent.open_transcript(path)),
        }
    }

    fn get(&self) -> &dyn TranscriptReader {
        match self {
            Reader::Claude(t) => t,
            Reader::Other(r) => r.as_ref(),
        }
    }

    fn get_mut(&mut self) -> &mut dyn TranscriptReader {
        match self {
            Reader::Claude(t) => t,
            Reader::Other(r) => r.as_mut(),
        }
    }

    /// 只有 Claude Code 才有的功能（任务面板、后台命令）用
    fn claude(&self) -> Option<&Transcript> {
        match self {
            Reader::Claude(t) => Some(t),
            Reader::Other(_) => None,
        }
    }
}

pub struct Inst {
    id: String,
    /// 这个终端运行的 AI 工具
    agent: &'static dyn Agent,
    st: Mutex<State>,
    tx: broadcast::Sender<Event>,
    tr: tokio::sync::Mutex<Option<Reader>>,
    /// 子代理的 transcript（“任务”面板用，增量跟读）
    agents: tokio::sync::Mutex<HashMap<PathBuf, Transcript>>,
    viewers: AtomicUsize,
    watching: AtomicBool,
    /// 收到的 `UserPromptSubmit` 个数（确认输入送达）
    prompts: watch::Sender<u64>,
    /// 正在看着额度用完的菜单（[`Chat::guard_limit`]）
    limit_guard: AtomicBool,
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
    /// Codex 的 app-server
    codex: codex_chat::Hub,
    me: Weak<Chat>,
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
        Arc::new_cyclic(|me| Self {
            store,
            paths,
            instances,
            token: crate::util::random_secret(),
            next_id: AtomicU64::new(1),
            insts: Mutex::new(HashMap::new()),
            codex: Default::default(),
            me: me.clone(),
        })
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
                let meta: board::Meta = self.store.get(&board::key(id)).ok().flatten().unwrap_or_default();
                // 只有带对话界面的启动类型才会走到这里（require 查过 chat_ui），认不出时按 Claude Code
                let agent = self.store.instance(id).ok().flatten().and_then(|r| agent::for_launch(&r.launch)).unwrap_or(&agent::Claude);
                Arc::new(Inst {
                    id: id.to_string(),
                    agent,
                    st: Mutex::new(State {
                        // 客户端重启后先按空闲处理，抓屏和下一个 hook 会纠正
                        phase: if binding.is_some() { Phase::Idle } else { Phase::Detached },
                        reason: None,
                        binding,
                        tool: None,
                        mode: None,
                        model: None,
                        effort: None,
                        background: vec![],
                        agents: HashSet::new(),
                        error: None,
                        pending: None,
                        last_hook: None,
                        claude_since: None,
                        version: None,
                        stats: Default::default(),
                        live: Default::default(),
                        hold: None,
                        actual_model: None,
                        model_msg: None,
                        model_set_at: None,
                        context: None,
                        title: None,
                        queued: vec![],
                        limit_wait: None,
                        limit_auto_at: None,
                        dialog: None,
                        suggestion: None,
                        codex: Default::default(),
                        odd_screens: 0,
                        drawn: false,
                        compacting: None,
                        since: crate::util::system_ms(),
                        active_at: 0,
                        board: meta,
                        board_dirty: false,
                        end_hint: None,
                        active_until: 0,
                        doing: None,
                        summary_tried: false,
                        last_reply: None,
                    }),
                    tx: broadcast::channel(256).0,
                    tr: tokio::sync::Mutex::new(None),
                    agents: tokio::sync::Mutex::new(HashMap::new()),
                    viewers: AtomicUsize::new(0),
                    watching: AtomicBool::new(false),
                    prompts: watch::channel(0).0,
                    limit_guard: AtomicBool::new(false),
                })
            })
            .clone()
    }

    /// 终端删除时清掉记录；挂起的审批交给 TUI（进程也已经结束了）。
    pub fn forget(&self, id: &str) {
        if let Some(inst) = lock(&self.insts).remove(id) {
            let mut st = lock(&inst.st);
            if let Some(p) = st.pending.as_mut() {
                p.resolve(String::new());
            }
            if inst.agent.launch() == "codex" {
                let thread = st.binding.as_ref().map(|b| b.session_id.clone());
                if let Some(me) = self.me.upgrade() {
                    tokio::spawn(async move { me.codex_forget(thread).await });
                }
            }
        }
        let _ = self.store.remove(&binding_key(id));
        let _ = self.store.remove(&board::key(id));
    }

    fn require(&self, id: &str) -> Result<(), LocalError> {
        self.store.instance(id)?.ok_or_else(LocalError::not_found)?;
        if !self.instances.opts(id).chat_ui {
            return Err(LocalError::new("CHAT_UNAVAILABLE"));
        }
        Ok(())
    }

    // ---------------- 状态 ----------------

    fn status_of(inst: &Inst, st: &State) -> Value {
        json!({
            "agent": inst.agent.launch(),
            "caps": inst.agent.caps(),
            "dialog": st.dialog,
            "phase": st.phase,
            "reason": st.reason,
            "tool": st.tool,
            "mode": st.mode,
            "model": st.model,
            "actual_model": st.actual_model,
            // 在界面上选了模型、还没有新的回复：模型按钮上显示选的那个
            "model_pending": st.model_set_at.as_ref().is_some_and(|at| *at == st.model_msg),
            // 模型是在界面上选的（之后回复用的模型和它对不上时，界面上提示）
            "model_chosen": st.model_set_at.is_some(),
            "context": st.context,
            "title": st.title,
            // Claude Code 队列里的（读 transcript）；Codex 是插话（`turn/steer`）发出去、还没进对话的
            "queued": st.queued.iter().chain(&st.codex.steered).collect::<Vec<_>>(),
            "limit": (st.limit_wait.is_some() || st.limit_auto_at.is_some()).then(|| json!({ "waiting": st.limit_wait, "auto_at": st.limit_auto_at })),
            "dialog": st.dialog,
            "effort": st.effort,
            "suggestion": st.suggestion,
            "background": st.background.iter().map(|(n, kind)| json!({ "n": n, "kind": kind })).collect::<Vec<_>>(),
            "agents": st.agents.len(),
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
                "guard": p.guard,
            })),
        })
    }

    fn emit_status(inst: &Inst, st: &State) {
        let _ = inst.tx.send(Event::Status(Self::status_of(inst, st)));
    }

    fn set_phase(inst: &Inst, st: &mut State, phase: Phase, reason: Option<&'static str>) {
        if st.phase != phase || st.reason != reason {
            Self::enter(st, phase);
            st.phase = phase;
            st.reason = reason;
            if phase != Phase::Running {
                st.tool = None;
                st.doing = None;
                st.live = Default::default();
            }
            if phase != Phase::Tui {
                st.dialog = None;
            }
            if phase != Phase::Idle {
                st.suggestion = None;
            }
            Self::emit_status(inst, st);
        }
    }

    /// 状态要换了（还没换）：记下时间，按需开始或结束一轮（看板，docs/TASK_BOARD.md §3.2）
    fn enter(st: &mut State, phase: Phase) {
        if st.phase == phase {
            return;
        }
        let now = crate::util::system_ms();
        let (was, since) = (st.phase, st.since);
        st.since = now;
        st.active_at = now;
        let active = st.board.turn.as_ref().is_some_and(board::Turn::active);
        match phase {
            Phase::Running | Phase::Approval if !active => {
                st.board.turn = Some(board::Turn { started_at: now, ..Default::default() });
                st.end_hint = None;
                st.summary_tried = false;
                st.last_reply = None;
                st.board_dirty = true;
            }
            // 没看到开始的那一轮（客户端重启时正在跑）也算一轮，从进入上个状态时算起
            Phase::Idle | Phase::Detached if active || matches!(was, Phase::Running | Phase::Approval) => {
                let mut turn = st.board.turn.take().filter(board::Turn::active).unwrap_or(board::Turn { started_at: since, ..Default::default() });
                let outcome = match st.end_hint.take() {
                    Some(o) => o,
                    None if st.error.is_some() => board::Outcome::Failed,
                    None if phase == Phase::Detached => board::Outcome::Interrupted,
                    None => board::Outcome::Done,
                };
                turn.ended_at = Some(now);
                turn.outcome = Some(outcome);
                turn.error = st.error.clone().filter(|_| outcome == board::Outcome::Failed);
                // 结束的时候有人正看着：不算“没看过”
                if st.active_until > now {
                    st.board.seen_at = now;
                }
                st.board.turn = Some(turn);
                st.board_dirty = true;
            }
            _ => {}
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
            st.active_at = crate::util::system_ms();
            if let Some(m) = ev["permission_mode"].as_str() {
                st.mode = Some(m.to_string());
            }
            if let Some(m) = ev["model"].as_str() {
                st.model = Some(m.to_string());
                st.model_set_at = None;
            }
            if let Some(e) = ev["effort"].as_str().or_else(|| ev["effort"]["level"].as_str()) {
                st.effort = Some(e.to_string());
            }
            let agent = ev["agent_id"].as_str().filter(|_| !ev["agent_type"].as_str().unwrap_or("").is_empty()).map(String::from);
            let rebound = self.rebind(&inst, &mut st, &ev, &name);
            match name.as_str() {
                "SessionStart" => {
                    // 自动压缩发生在一轮当中，不能改成空闲
                    if ev["source"] != "compact" {
                        st.error = None;
                        st.agents.clear();
                        Self::set_phase(&inst, &mut st, Phase::Idle, None);
                    }
                }
                "SessionEnd" => {
                    Self::drop_pending(&mut st);
                    st.compacting = None;
                    st.agents.clear();
                    Self::set_phase(&inst, &mut st, Phase::Detached, Some("ended"));
                }
                "UserPromptSubmit" => {
                    Self::drop_pending(&mut st);
                    st.error = None;
                    if let Some(p) = ev["prompt"].as_str() {
                        Self::note_prompt(&mut st, p);
                    }
                    inst.prompts.send_modify(|n| *n += 1);
                    Self::set_phase(&inst, &mut st, Phase::Running, None);
                }
                "PreToolUse" => {
                    if st.phase != Phase::Approval {
                        st.tool = tool.clone();
                        st.doing = tool.as_deref().map(|t| board::doing(t, &ev["tool_input"]));
                        Self::enter(&mut st, Phase::Running);
                        st.phase = Phase::Running;
                        st.reason = None;
                        Self::emit_status(&inst, &st);
                    }
                }
                "PostToolUse" | "PostToolUseFailure" | "PermissionDenied" => {
                    // 命令规则挂起的确认还在等：同时跑的另一条命令结束了，不能把它放掉
                    if st.pending.as_ref().is_some_and(|p| Some(&p.tool) == tool.as_ref() && p.guard.is_none()) {
                        Self::drop_pending(&mut st);
                    }
                    Self::set_phase(&inst, &mut st, Phase::Running, None);
                }
                // Codex 有 Interrupt（Esc 中断）；Claude Code 没有，靠抓屏
                "Stop" | "Interrupt" => {
                    st.end_hint = Some(if name == "Stop" { board::Outcome::Done } else { board::Outcome::Interrupted });
                    if let Some(r) = ev["last_assistant_message"].as_str().and_then(board::first_para) {
                        st.last_reply = Some(r);
                    }
                    Self::drop_pending(&mut st);
                    st.compacting = None;
                    Self::set_phase(&inst, &mut st, Phase::Idle, None);
                }
                "StopFailure" => {
                    // 额度用完时 Claude Code 可能弹菜单问怎么办：没人管的时候替用户选“等重置后自动继续”
                    self.guard_limit(&inst);
                    Self::drop_pending(&mut st);
                    st.compacting = None;
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
                // 后台运行的子代理在这一轮结束后才停：只更新个数，不改状态
                "SubagentStop" if st.phase != Phase::Running => {
                    if agent.is_some_and(|a| st.agents.remove(&a)) {
                        Self::emit_status(&inst, &st);
                    }
                }
                "SubagentStart" | "SubagentStop" | "PreCompact" => {
                    match (name.as_str(), agent) {
                        ("SubagentStart", Some(a)) => {
                            st.agents.insert(a);
                        }
                        ("SubagentStop", Some(a)) => {
                            st.agents.remove(&a);
                        }
                        _ => {}
                    }
                    if st.phase != Phase::Approval {
                        Self::set_phase(&inst, &mut st, Phase::Running, None);
                    }
                    if name == "PreCompact" {
                        st.compacting = Some(Instant::now());
                        st.live.activity = Some("compacting".into());
                        Self::emit_status(&inst, &st);
                    }
                }
                "PostCompact" => {
                    st.compacting = None;
                    // 手动压缩（/compact）做完就空闲了；自动压缩发生在一轮当中，接着跑
                    if ev["trigger"] == "manual" {
                        Self::set_phase(&inst, &mut st, Phase::Idle, None);
                    } else if st.live.activity.as_deref() == Some("compacting") {
                        st.live.activity = None;
                        Self::emit_status(&inst, &st);
                    }
                }
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
        if name == "UserPromptSubmit" {
            self.checkpoint_turn(id, ev["prompt"].as_str().unwrap_or_default(), true).await;
        }
        if name == "PreToolUse" && tool.as_deref() == Some("Bash") {
            let hit = ev["tool_input"]["command"].as_str().and_then(|c| crate::guard::check(&crate::guard::load(&self.store), c));
            if let Some(hit) = hit {
                return self.guard_request(&inst, hit, &ev).await;
            }
        }
        String::new()
    }

    /// hook 带来的会话和 transcript 路径（`/clear`、`/resume` 会换）。换了返回 true。
    fn rebind(&self, inst: &Inst, st: &mut State, ev: &Value, event: &str) -> bool {
        let id = inst.id.as_str();
        let (Some(sid), Some(path)) = (ev["session_id"].as_str(), ev["transcript_path"].as_str()) else { return false };
        if st.binding.is_some() && !inst.agent.rebinds_on(event) {
            return false;
        }
        let path = PathBuf::from(path);
        if !inst.agent.valid_transcript(&path) {
            tracing::warn!(path = %path.display(), "hook 带来的 transcript 路径不对，忽略");
            return false;
        }
        let b = Binding { session_id: sid.to_string(), transcript_path: path };
        if st.binding.as_ref() == Some(&b) {
            return false;
        }
        let _ = self.store.set(&binding_key(id), &b);
        // 换了一段对话（/clear、/resume）：看板上的标题跟着换
        if st.binding.is_some() {
            st.board.title = None;
            st.board.first_prompt = None;
            st.board_dirty = true;
        }
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
            let kind = inst.agent.pending_kind(&tool);
            st.pending = Some(Pending { id, at: Instant::now(), tool, kind, input: ev["tool_input"].clone(), suggestions: ev["permission_suggestions"].clone(), guard: None, tx: Some(tx) });
            Self::enter(&mut st, Phase::Approval);
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

    /// 每轮开始拍检查点（docs/PLAN_2026-10-27.md §3.3）。`wait`：最多等 2 秒（hook 要尽快答复，AI 改文件之前拍好），
    /// 没拍完就在后台接着拍
    pub(super) async fn checkpoint_turn(&self, id: &str, prompt: &str, wait: bool) {
        if !crate::guard::load(&self.store).checkpoints {
            return;
        }
        let Some(row) = self.store.instance(id).ok().flatten() else { return };
        let (store, id, prompt) = (self.store.clone(), id.to_string(), prompt.to_string());
        let h = tokio::task::spawn_blocking(move || {
            if let Err(e) = crate::checkpoint::take(&store, &id, Path::new(&row.workdir), &prompt, false) {
                tracing::debug!(error = ?e, "没拍上检查点");
            }
        });
        if wait {
            let _ = tokio::time::timeout(Duration::from_secs(2), h).await;
        }
    }

    /// 命令命中命令规则（docs/PLAN_2026-10-27.md §3.2）：拒绝的直接答复 PreToolUse；要确认的挂起来，和审批一样等界面回答。
    /// 没人确认（超时、这一轮停了、被别的审批顶掉）就拒绝：PreToolUse 不做决定 Claude Code 会照常执行。
    async fn guard_request(&self, inst: &Arc<Inst>, hit: crate::guard::Hit, ev: &Value) -> String {
        if hit.action == crate::guard::Action::Deny {
            return pre_tool_use("deny", &format!("A Looklook safety rule blocks this command ({}). Do not retry it or work around it; use another approach, or ask the user to run it themselves.", hit.rule));
        }
        let (tx, rx) = oneshot::channel();
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        {
            let mut st = lock(&inst.st);
            Self::drop_pending(&mut st);
            st.pending = Some(Pending { id, at: Instant::now(), tool: "Bash".into(), kind: "permission", input: ev["tool_input"].clone(), suggestions: Value::Null, guard: Some(hit.rule.clone()), tx: Some(tx) });
            Self::enter(&mut st, Phase::Approval);
            st.phase = Phase::Approval;
            st.reason = None;
            Self::emit_status(inst, &st);
        }
        let out = tokio::time::timeout(PERMISSION_WAIT, rx).await.ok().and_then(|r| r.ok()).unwrap_or_default();
        let mut st = lock(&inst.st);
        if st.pending.as_ref().is_some_and(|p| p.id == id) {
            st.pending = None;
            Self::set_phase(inst, &mut st, Phase::Running, None);
        }
        if out.is_empty() {
            return pre_tool_use("deny", &format!("This command matches a Looklook safety rule ({}) and nobody confirmed it in time, so it was not run. Ask the user before trying again.", hit.rule));
        }
        out
    }

    // ---------------- 界面：快照、事件、历史 ----------------

    /// 跟读 transcript，把变化推给界面。
    async fn poll(&self, inst: &Inst) {
        let path = lock(&inst.st).binding.as_ref().map(|b| b.transcript_path.clone());
        let Some(path) = path else { return };
        let mut tr = inst.tr.lock().await;
        let fresh = tr.as_ref().is_none_or(|t| t.get().path() != path);
        if fresh {
            *tr = Some(Reader::open(inst.agent, path));
        }
        let t = tr.as_mut().expect("transcript").get_mut();
        let change = tokio::task::block_in_place(|| t.poll());
        let (version, stats) = (t.version(), t.stats());
        // 标题、实际用的模型、上下文占用：只有 Claude Code 的 transcript 有
        let claude = tr.as_ref().and_then(Reader::claude);
        let title = claude.and_then(|t| t.title.clone());
        let queued = claude.map(|t| t.queued.clone()).unwrap_or_default();
        let (model, model_msg) = claude.map(|t| (t.model.clone(), t.model_msg.clone())).unwrap_or_default();
        let context = claude.and_then(|t| t.usage.as_ref()).map(|u| ["input_tokens", "cache_creation_input_tokens", "cache_read_input_tokens"].iter().map(|k| u[*k].as_u64().unwrap_or(0)).sum::<u64>());
        let is_claude = claude.is_some();
        drop(tr);
        {
            let mut st = lock(&inst.st);
            // 别的工具（Codex）的标题、模型、上下文由 app-server 给，这里只更新版本
            let (title, model_msg, context) = if is_claude { (title, model_msg, context) } else { (st.title.clone(), st.model_msg.clone(), st.context) };
            let version = if is_claude { version } else { version.or(st.version.clone()) };
            if st.version != version || st.title != title || st.model_msg != model_msg || st.context != context || st.queued != queued {
                st.version = version;
                st.queued = queued;
                st.title = title;
                if model_msg.is_some() {
                    st.actual_model = model;
                }
                st.model_msg = model_msg;
                st.context = context;
                Self::emit_status(inst, &st);
            }
            st.stats = stats;
            if matches!(change, Ok(Change::Upsert { .. })) {
                st.active_at = crate::util::system_ms();
            }
        }
        match change {
            Ok(Change::Upsert { start, items }) if !fresh => {
                // Codex 的插话：记录里先读到了（app-server 的通知还没到），一起从“排队中”拿掉，免得两边同时显示
                {
                    let mut st = lock(&inst.st);
                    let before = st.codex.steered.len();
                    for it in &items {
                        if let transcript::Body::User { text, mid: true, .. } = &it.body {
                            if let Some(at) = st.codex.steered.iter().position(|q| q == text) {
                                st.codex.steered.remove(at);
                            }
                        }
                    }
                    if st.codex.steered.len() != before {
                        Self::emit_status(inst, &st);
                    }
                }
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
        if inst.agent.launch() == "codex" {
            if let Err(e) = self.codex_attach(id).await {
                let mut st = lock(&inst.st);
                if e.code == "CHAT_FOREIGN" {
                    // 不是看看打开的：界面上说明，不算出错
                    Self::set_phase(&inst, &mut st, Phase::Detached, Some("not_attached"));
                } else if e.code == "CHAT_TERMINAL_ONLY" {
                    // 后台连不上时终端在本机直接跑：说明对话暂时不可用，终端照常能用
                    Self::set_phase(&inst, &mut st, Phase::Detached, Some("terminal_only"));
                } else {
                    st.error = Some(format!("{:?}", e).chars().take(500).collect());
                    Self::set_phase(&inst, &mut st, Phase::Detached, Some("app_server"));
                }
            }
        }
        self.poll(&inst).await;
        let tr = inst.tr.lock().await;
        let (total, items) = match tr.as_ref().map(Reader::get) {
            Some(t) => (t.len(), t.items(t.len().saturating_sub(SNAPSHOT_ITEMS), t.len())),
            None => (0, vec![]),
        };
        drop(tr);
        let status = Self::status_of(&inst, &lock(&inst.st));
        Ok(json!({ "status": status, "total": total, "start": total - items.len(), "items": items }))
    }

    /// 往上翻：`before` 之前的 `limit` 条。
    pub async fn history(&self, id: &str, before: usize, limit: usize) -> Result<Value, LocalError> {
        self.require(id)?;
        let inst = self.inst(id);
        let tr = inst.tr.lock().await;
        let Some(t) = tr.as_ref().map(Reader::get) else { return Ok(json!({ "start": 0, "items": [] })) };
        let before = before.min(t.len());
        let start = before.saturating_sub(limit.clamp(1, 500));
        Ok(json!({ "start": start, "items": t.items(start, before) }))
    }

    /// 某条记录不截断的原始内容。
    pub async fn item(&self, id: &str, key: &str) -> Result<Value, LocalError> {
        self.require(id)?;
        let inst = self.inst(id);
        let tr = inst.tr.lock().await;
        let t = tr.as_ref().ok_or_else(LocalError::not_found)?.get();
        tokio::task::block_in_place(|| t.full_item(key)).ok_or_else(LocalError::not_found)
    }

    /// 用户消息 `key` 里的第 `n` 张图片：类型和内容。Claude Code 的图片直接存在 transcript 里，Codex 的在 rollout 里
    pub async fn user_image(&self, id: &str, key: &str, n: usize) -> Result<(&'static str, Vec<u8>), LocalError> {
        self.require(id)?;
        if self.is_codex(id) {
            let inst = self.inst(id);
            let path = {
                let tr = inst.tr.lock().await;
                tr.as_ref().map(|r| r.get().path().to_path_buf()).ok_or_else(LocalError::not_found)?
            };
            let key = key.to_string();
            return tokio::task::block_in_place(|| codex_rollout::user_image(&path, &key, n)).ok_or_else(LocalError::not_found);
        }
        let rec = self.item(id, key).await?;
        transcript::user_image(&rec, n).ok_or_else(LocalError::not_found)
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
                // Codex 的状态来自 app-server，不用看屏幕
                if tick.is_multiple_of(3) && inst.agent.launch() != "codex" {
                    self.check_screen(&inst, true).await;
                    if lock(&inst.st).dialog.as_ref().is_some_and(screen::Dialog::is_limit) {
                        self.guard_limit(&inst);
                    }
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
    /// `adopt`：可以把没带 hooks 的 Claude 接到对话模式（要重启 claude，只在有人看着时做）
    async fn check_screen(&self, inst: &Inst, adopt_ok: bool) {
        let pane = self.instances.pane(&inst.id).await;
        let running = pane.as_ref().and_then(|p| agent_state(inst.agent, p));
        let capture = match running {
            Some(_) => self.instances.screen(&inst.id).await,
            None => None,
        };
        // 在运行、但不是带着看看的 hooks 启动的（开启对话界面之前就开着的、在终端里手动开的）
        let unhooked = running == Some(Some(false));
        let screen = capture.as_deref().map(|c| inst.agent.classify(c));
        let mut st = lock(&inst.st);
        if st.hold.is_some_and(|h| h > Instant::now()) {
            return;
        }
        let quiet = st.last_hook.is_none_or(|t| t.elapsed() >= QUIET);
        let mut adopt = false;
        match (&pane, screen) {
            (None, _) => {
                st.claude_since = None;
                st.drawn = false;
                Self::set_phase(inst, &mut st, Phase::Detached, Some("no_session"));
            }
            (Some(_), None) => {
                // 面板里是 shell：claude 没在运行
                st.claude_since = None;
                st.drawn = false;
                st.agents.clear();
                st.background.clear();
                Self::drop_pending(&mut st);
                Self::set_phase(inst, &mut st, Phase::Detached, Some("not_running"));
            }
            (Some(_), Some(scr)) => {
                let since = *st.claude_since.get_or_insert_with(Instant::now);
                let attached = !unhooked && (st.binding.is_some() || since.elapsed() < ATTACH_GRACE || inst.agent.session_starts_on_prompt());
                let pending_age = st.pending.as_ref().map(|p| p.at.elapsed());
                // 先认出对话框，切到 Tui 时和状态一起推给界面（界面看到能直接回答的对话框就不跳到终端）
                if scr == Screen::Modal {
                    st.dialog = capture.as_deref().and_then(|c| inst.agent.dialog(c));
                }
                // 读不出来的界面要连着看到两次才算：只看到一次多半是正在重绘，弹出“在等你选择”过一会儿又自己消失
                let odd = scr == Screen::Modal && st.dialog.is_none() && st.phase != Phase::Tui;
                st.odd_screens = if odd { st.odd_screens.saturating_add(1) } else { 0 };
                st.drawn |= matches!(scr, Screen::Input { .. });
                let starting = !st.drawn && since.elapsed() < STARTUP_WAIT;
                match (st.phase, scr) {
                    // 确认框已经不在了（在终端里按了 Esc 或选了“否”：这两种都没有 hook）。
                    // Codex 要等 hook 答复之后才在 TUI 上弹确认框，挂起时屏幕上本来就是输入框，不能据此放掉
                    (Phase::Approval, Screen::Input { busy, .. }) if pending_age.is_some_and(|a| a >= QUIET) && inst.agent.tui_asks_while_hook_waits() => {
                        Self::drop_pending(&mut st);
                        Self::set_phase(inst, &mut st, if busy { Phase::Running } else { Phase::Idle }, None);
                    }
                    (Phase::Approval, _) => {}
                    (_, Screen::Modal) if st.phase == Phase::Tui => {}
                    (Phase::Running, Screen::Modal) if !quiet => {}
                    (_, Screen::Modal) if st.odd_screens == 1 || (odd && starting) => {}
                    (_, Screen::Modal) => {
                        // 读不出来的界面：记下屏幕（最后 20 行、去掉转义），以后据此认出它或排查误判
                        if st.dialog.is_none() {
                            let shown: Vec<String> = capture.as_deref().unwrap_or("").lines().map(|l| screen::strip(l).0).filter(|l| !l.trim().is_empty()).collect();
                            let tail = shown[shown.len().saturating_sub(20)..].join("\n");
                            tracing::info!(id = %inst.id, drawn = st.drawn, since_ms = since.elapsed().as_millis() as u64, "认不出来的 Claude Code 界面，转到“在等你选择”\n{tail}");
                        }
                        Self::set_phase(inst, &mut st, Phase::Tui, None)
                    }
                    (_, Screen::Input { busy, ref text }) if !attached => {
                        Self::set_phase(inst, &mut st, Phase::Detached, Some("not_attached"));
                        // 以前的 Claude（不是带着看看的 hooks 启动的）空闲着：接着同一段对话转到对话模式
                        adopt = unhooked && !busy && text.trim().is_empty() && inst.agent.launch() == "claude";
                    }
                    // 压缩中（最多算 10 分钟，防止丢了 PostCompact 一直卡着）
                    (Phase::Running, Screen::Input { .. }) if st.compacting.is_some_and(|t| t.elapsed() < COMPACT_MAX) => {}
                    // 运行中没有 hook 就变回输入框：Esc 中断了（Claude Code 中断没有 hook）
                    (Phase::Running, Screen::Input { busy, .. }) if quiet && !busy => {
                        st.end_hint.get_or_insert(board::Outcome::Interrupted);
                        Self::set_phase(inst, &mut st, Phase::Idle, None)
                    }
                    (Phase::Running, Screen::Input { .. }) => {}
                    (_, Screen::Input { busy, .. }) => Self::set_phase(inst, &mut st, if busy { Phase::Running } else { Phase::Idle }, None),
                }
                // 选择框：读出来给界面显示成弹窗
                if st.phase == Phase::Tui {
                    let d = capture.as_deref().and_then(|c| inst.agent.dialog(c));
                    if d != st.dialog {
                        st.dialog = d;
                        Self::emit_status(inst, &st);
                    }
                }
                // 状态栏：权限模式、思考强度（hook 要等下一个事件才带）、后台任务个数（只有屏幕上有，只认 Claude Code 的状态栏）
                if let Some(f) = capture.as_deref().filter(|_| inst.agent.launch() == "claude").and_then(screen::footer) {
                    let effort = f.effort.or_else(|| st.effort.clone());
                    if st.mode != f.mode || st.effort != effort || st.background != f.background {
                        st.mode = f.mode;
                        st.effort = effort;
                        st.background = f.background;
                        Self::emit_status(inst, &st);
                    }
                }
                // 额度用完、正在等重置后自动继续（只有 Claude Code 有）
                let wait = capture.as_deref().filter(|_| inst.agent.launch() == "claude").and_then(screen::limit_wait);
                if st.limit_wait != wait {
                    st.limit_wait = wait;
                    Self::emit_status(inst, &st);
                }
                // 空闲时输入框里的推荐回复：界面上显示成一点就能用的建议（只认 Claude Code 的输入框）
                let sug = capture.as_deref().filter(|_| st.phase == Phase::Idle && inst.agent.launch() == "claude").and_then(screen::suggestion);
                if st.suggestion != sug {
                    st.suggestion = sug;
                    Self::emit_status(inst, &st);
                }
                // 运行中：把屏幕上正在输出的内容推给界面（transcript 要等整段写完才有）
                if st.phase == Phase::Running {
                    let mut live = capture.as_deref().map(|c| inst.agent.live(c)).unwrap_or_default();
                    if st.compacting.is_some() && live.activity.is_none() {
                        live.activity = Some("compacting".into());
                    }
                    if live != st.live {
                        st.live = live;
                        Self::emit_status(inst, &st);
                    }
                }
            }
        }
        drop(st);
        if adopt && adopt_ok {
            if let Some(me) = self.me.upgrade() {
                me.claude_adopt_later(&inst.id);
            }
        }
    }

    /// 会话列表的角标和任务看板（docs/TASK_BOARD.md §4.4）。没收到过这个终端的 hook 时为 None。
    pub fn brief(&self, id: &str) -> Option<Value> {
        let inst = lock(&self.insts).get(id).cloned()?;
        let st = lock(&inst.st);
        let attn = match (&st.pending, &st.dialog) {
            (Some(p), _) => {
                let (detail, options) = board::attn(&p.tool, p.kind, &p.input);
                let risk = (p.kind == "permission").then(|| risk::level(&p.tool, &p.input));
                // 回答提问要用问题原文
                let question = (p.kind == "question").then(|| &p.input["questions"][0]["question"]);
                Some(json!({ "kind": p.kind, "pending_id": p.id, "tool": p.tool, "detail": detail, "risk": risk, "options": options, "question": question, "guard": p.guard }))
            }
            (None, Some(d)) if st.phase == Phase::Tui => Some(json!({
                "kind": if d.is_limit() { "limit" } else { "dialog" },
                "detail": d.title,
                "title": d.title,
                "options": d.options.iter().map(|o| &o.label).collect::<Vec<_>>(),
            })),
            (None, None) if st.phase == Phase::Tui => Some(json!({ "kind": "dialog" })),
            _ => None,
        };
        let turn = st.board.turn.as_ref();
        let ended = turn.is_some_and(|t| t.ended_at.is_some_and(|e| e > st.board.seen_at) && t.outcome.is_some_and(|o| o != board::Outcome::Interrupted));
        let state = match st.phase {
            Phase::Approval | Phase::Tui => "attn",
            Phase::Running => "working",
            // 额度用完、正在等重置后自动继续
            _ if st.limit_wait.is_some() => "working",
            _ if ended => "ended",
            Phase::Idle => "idle",
            Phase::Detached => "stopped",
        };
        let title = st.title.clone().or_else(|| st.board.title.clone()).or_else(|| st.board.first_prompt.clone());
        Some(json!({
            "phase": st.phase,
            "pending": st.pending.as_ref().map(|p| p.kind()),
            "agent": inst.agent.launch(),
            "board": state,
            "since": st.since,
            "active_at": st.active_at.max(st.since),
            "attn": attn,
            "title": title,
            "doing": st.doing,
            "activity": st.live.activity,
            "turn": turn,
            "limit": (st.limit_wait.is_some() || st.limit_auto_at.is_some()).then(|| json!({ "waiting": st.limit_wait, "auto_at": st.limit_auto_at })),
            "seen_at": st.board.seen_at,
            // 有人正开着这个会话（会话页可见且有焦点）：不发通知
            "watched": st.active_until > crate::util::system_ms(),
        }))
    }

    /// 带对话界面的会话（通知按它们逐个检查）
    pub fn ids(&self) -> Vec<String> {
        lock(&self.insts).keys().cloned().collect()
    }

    /// 看过这个会话了。`active`：会话页可见且有焦点（每 20 秒报一次），这段时间里结束的一轮直接算看过
    pub fn seen(&self, id: &str, active: bool) -> Result<(), LocalError> {
        self.require(id)?;
        let inst = self.inst(id);
        let mut st = lock(&inst.st);
        let now = crate::util::system_ms();
        st.board.seen_at = now;
        if active {
            st.active_until = now + 30_000;
        }
        st.board_dirty = true;
        Ok(())
    }

    /// 第一条用户消息：没有标题时看板上用它（斜杠命令不算）
    fn note_prompt(st: &mut State, text: &str) {
        let text = text.trim();
        if st.board.first_prompt.is_none() && !text.is_empty() && !text.starts_with('/') {
            st.board.first_prompt = Some(board::clip(text, 40));
            st.board_dirty = true;
        }
    }

    /// 没人看着的会话也要知道准确的状态（docs/TASK_BOARD.md §4.1）。每 3 秒：
    /// - Claude Code 在运行、等审批、有选择框的会话抓一次屏（Esc 中断、选择框、额度菜单、claude 退出都没有 hook）；
    ///   空闲的 15 秒一次（看额度等重置、claude 退出）。额度菜单没人选时照样替用户选“等重置后自动继续”；
    /// - 补上一轮结束时的摘要、记下标题，改过的存进数据库。
    pub async fn background(self: Arc<Self>) {
        let mut tick = 0u32;
        loop {
            tokio::time::sleep(Duration::from_secs(3)).await;
            tick = tick.wrapping_add(1);
            let insts: Vec<Arc<Inst>> = lock(&self.insts).values().cloned().collect();
            for inst in insts {
                let claude = inst.agent.launch() != "codex";
                let phase = lock(&inst.st).phase;
                let due = match phase {
                    Phase::Running | Phase::Approval | Phase::Tui => true,
                    Phase::Idle => tick.is_multiple_of(5),
                    Phase::Detached => false,
                };
                if claude && due && inst.viewers.load(Ordering::SeqCst) == 0 {
                    self.check_screen(&inst, false).await;
                    if lock(&inst.st).dialog.as_ref().is_some_and(screen::Dialog::is_limit) {
                        self.guard_limit(&inst);
                    }
                }
                self.board_upkeep(&inst).await;
            }
        }
    }

    async fn board_upkeep(&self, inst: &Inst) {
        let path = {
            let mut st = lock(&inst.st);
            if st.title.is_some() && st.title != st.board.title {
                st.board.title = st.title.clone();
                st.board_dirty = true;
            }
            let wants = st.board.turn.as_ref().is_some_and(|t| t.ended_at.is_some() && t.summary.is_none()) && !st.summary_tried;
            if wants {
                st.summary_tried = true;
                match st.last_reply.clone() {
                    Some(r) => {
                        if let Some(t) = st.board.turn.as_mut() {
                            t.summary = Some(r);
                        }
                        st.board_dirty = true;
                        None
                    }
                    None if inst.agent.launch() == "claude" => st.binding.as_ref().map(|b| b.transcript_path.clone()),
                    None => None,
                }
            } else {
                None
            }
        };
        if let Some(path) = path {
            let reply = tokio::task::spawn_blocking(move || board::last_reply(&path)).await.ok().flatten();
            let mut st = lock(&inst.st);
            if let (Some(r), Some(t)) = (reply, st.board.turn.as_mut()) {
                t.summary = Some(r);
                st.board_dirty = true;
            }
        }
        let meta = {
            let mut st = lock(&inst.st);
            std::mem::take(&mut st.board_dirty).then(|| st.board.clone())
        };
        if let Some(m) = meta {
            if let Err(e) = self.store.set(&board::key(&inst.id), &m) {
                tracing::warn!(error = %e, "看板状态没存上");
            }
        }
    }

    /// 输入 / 时提示的自定义命令和技能
    pub fn commands(&self, id: &str) -> Result<Value, LocalError> {
        self.require(id)?;
        let row = self.store.instance(id)?.ok_or_else(LocalError::not_found)?;
        Ok(json!({ "items": self.inst(id).agent.commands(Path::new(&row.workdir)) }))
    }

    /// 输入历史（上下键、Ctrl+R）：Claude Code 自己的 `history.jsonl` 里这个项目的，各个账号的配置目录合起来。
    /// 别的工具没有按项目的输入历史，返回空的，界面用本机存着的。
    pub fn prompts(&self, id: &str) -> Result<Value, LocalError> {
        self.require(id)?;
        if self.inst(id).agent.launch() != "claude" {
            return Ok(json!({ "items": [] }));
        }
        let row = self.store.instance(id)?.ok_or_else(LocalError::not_found)?;
        let mut dirs: Vec<PathBuf> = vec![];
        for p in crate::claude_profiles::list(&self.store) {
            let dir = crate::claude_profiles::config_paths(&self.paths.home, &p).0;
            if !dirs.contains(&dir) {
                dirs.push(dir);
            }
        }
        let items = tokio::task::block_in_place(|| prompts::claude_prompts(&dirs, Path::new(&row.workdir), 500));
        Ok(json!({ "items": items }))
    }

    /// 子代理（Task / Agent 工具）自己的步骤。`agent`：工具结果里的 agentId；还没有结果时用 `tool`（工具调用 id）
    /// 按 prompt 找：子代理 transcript 的第一条就是它收到的 prompt。
    pub async fn agent(&self, id: &str, agent: Option<&str>, tool: Option<&str>) -> Result<Value, LocalError> {
        self.require(id)?;
        let inst = self.inst(id);
        let main = lock(&inst.st).binding.as_ref().map(|b| b.transcript_path.clone()).ok_or_else(LocalError::not_found)?;
        if self.is_codex(id) {
            return self.codex_agent(&inst, &main, agent, tool).await;
        }
        let dir = main.with_extension("").join("subagents");
        let file = match (agent, tool) {
            (Some(a), _) if !a.is_empty() && a.chars().all(|c| c.is_ascii_alphanumeric()) => dir.join(format!("agent-{a}.jsonl")),
            (_, Some(t)) => {
                let tr = inst.tr.lock().await;
                let prompt = tr.as_ref().and_then(|t2| t2.get().tool_input(t)).and_then(|v| v["prompt"].as_str()).map(String::from).ok_or_else(LocalError::not_found)?;
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

    /// Codex 的子代理：子会话自己的 rollout。`agent` 是子会话 id（卡片的结果里有），还没有结果时按工具调用 id 查参数里的。
    /// `done`：最后是一轮结束（或被中断），界面就不再刷新
    async fn codex_agent(&self, inst: &Inst, main: &Path, agent: Option<&str>, tool: Option<&str>) -> Result<Value, LocalError> {
        let thread = match (agent, tool) {
            (Some(a), _) if !a.is_empty() => a.to_string(),
            (_, Some(t)) => {
                let tr = inst.tr.lock().await;
                tr.as_ref().and_then(|r| r.get().tool_input(t)).and_then(|v| v["agent_thread_id"].as_str()).map(String::from).ok_or_else(LocalError::not_found)?
            }
            _ => return Err(LocalError::invalid("agent", "required")),
        };
        let file = tokio::task::block_in_place(|| codex_rollout::find_rollout(main, &thread)).ok_or_else(LocalError::not_found)?;
        let mut r = codex_rollout::Rollout::new(file);
        tokio::task::block_in_place(|| r.poll())?;
        let items = r.items(r.len().saturating_sub(300), r.len());
        let done = items.last().is_some_and(|i| matches!(i.body, transcript::Body::Turn { .. } | transcript::Body::Interrupt));
        Ok(json!({ "agent": thread, "start": r.len().saturating_sub(300), "items": items, "done": done }))
    }

    // ---------------- 界面：操作 ----------------

    /// 发消息：文字按“打字”的方式送进去，图片路径用粘贴（TUI 才会变成图片附件），再按回车。
    /// 返回是否确认送达（收到了对应的 UserPromptSubmit）；没接上 hooks 时为 null。
    pub async fn input(self: &Arc<Self>, id: &str, text: &str, images: &[String]) -> Result<Value, LocalError> {
        self.submit(id, text, images, false).await
    }

    /// 立即发送：Claude Code 的 `chat:sendNow`（Ctrl+X Ctrl+S，2.1.275 起）。排着的消息连同这条马上发出：
    /// Claude 在跑命令、子代理时，这些转到后台接着跑，Claude 在这一轮里马上读到；
    /// 只在写回复、或者在做转不到后台的事时，打断这一轮、先回应。没在运行时就是普通发送。
    /// `text` 可以为空：只把排着的发出去。
    pub async fn send_now(self: &Arc<Self>, id: &str, text: &str, images: &[String]) -> Result<Value, LocalError> {
        self.submit(id, text, images, true).await
    }

    async fn submit(self: &Arc<Self>, id: &str, text: &str, images: &[String], now: bool) -> Result<Value, LocalError> {
        self.require(id)?;
        let inst = self.inst(id);
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        let text = text.trim_end();
        Self::note_prompt(&mut lock(&inst.st), text);
        let now = now && inst.agent.launch() == "claude" && lock(&inst.st).phase == Phase::Running;
        if text.is_empty() && images.is_empty() && !now {
            return Err(LocalError::invalid("text", "required"));
        }
        if now && lock(&inst.st).version.as_deref().is_some_and(|v| !version_at_least(v, SEND_NOW_SINCE)) {
            return Err(LocalError::new("CHAT_SEND_NOW_UNAVAILABLE"));
        }
        for p in images {
            if !Path::new(p).is_file() {
                return Err(LocalError::invalid("images", "not_found"));
            }
        }
        if inst.agent.launch() == "codex" {
            return self.codex_input(id, text, images).await;
        }
        let (hooked, running) = {
            let st = lock(&inst.st);
            match st.phase {
                Phase::Detached => return Err(LocalError::new("CHAT_NOT_RUNNING")),
                Phase::Tui | Phase::Approval => return Err(LocalError::new("CHAT_BUSY")),
                _ => {}
            }
            (st.binding.is_some() || inst.agent.session_starts_on_prompt(), st.phase == Phase::Running)
        };
        // 确认输入框是普通输入框（选择器打开时打进去的字母会被当成快捷键）。
        // 刚启动时 SessionStart 比输入框先到，多看几次。
        let mut ready = false;
        for _ in 0..10 {
            if !matches!(self.instances.screen(id).await.map(|s| inst.agent.classify(&s)), Some(Screen::Modal)) {
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
            // 斜杠命令（/effort、/model……）刚执行完时 TUI 会重置输入框，这时打进去的开头会丢（实测 2.1.303，
            // 回车后 0.1 秒内打的字）。按回车前看一眼：开头不在就清空重打一次。
            if let Some(Screen::Input { text: shown, .. }) = self.instances.screen(id).await.map(|s| inst.agent.classify(&s)) {
                if !typed_ok(&shown, text) {
                    tracing::info!(id, "输入框里的内容和送进去的不一致，清空重打");
                    self.clear_input(id, shown.lines().count()).await?;
                    self.type_text(id, text).await?;
                }
            }
        }
        for p in images {
            self.instances.paste(id, &format!(" {p}")).await?;
        }
        tokio::time::sleep(Duration::from_millis(80)).await;
        if now {
            return self.confirm_send_now(&inst, id, text, prompts).await;
        }
        self.instances.send_keys(id, &["Enter"]).await?;
        // 斜杠命令（/clear、/new、/compact……）不会有 UserPromptSubmit：不等、不补按回车（补按会把下一条也送出去）
        if !hooked || text.starts_with('/') {
            return Ok(json!({ "delivered": null }));
        }
        // 运行中发的：Claude Code 先放进队列（没有 UserPromptSubmit），这一步做完再并进对话。进了队列就算送达
        let first: String = text.lines().next().unwrap_or("").chars().take(20).collect();
        if running && inst.agent.launch() == "claude" {
            let deadline = Instant::now() + inst.agent.delivery_wait();
            while Instant::now() < deadline {
                self.poll(&inst).await;
                if lock(&inst.st).queued.iter().any(|q| q.starts_with(&first)) {
                    return Ok(json!({ "delivered": true, "queued": true }));
                }
                if tokio::time::timeout(Duration::from_millis(250), prompts.changed()).await.is_ok() {
                    return Ok(json!({ "delivered": true }));
                }
            }
        } else if tokio::time::timeout(inst.agent.delivery_wait(), prompts.changed()).await.is_ok() {
            return Ok(json!({ "delivered": true }));
        }
        // 没收到：文字还在输入框里就再按一次回车（TUI 刚启动、刚结束一轮时偶尔会吞掉回车）
        let still_there = matches!(self.instances.screen(id).await.map(|s| inst.agent.classify(&s)), Some(Screen::Input { text: t, .. }) if !first.is_empty() && t.contains(&first));
        if still_there {
            self.instances.send_keys(id, &["Enter"]).await?;
            if tokio::time::timeout(inst.agent.delivery_wait(), prompts.changed()).await.is_ok() {
                return Ok(json!({ "delivered": true }));
            }
        }
        Ok(json!({ "delivered": false }))
    }

    /// 按下立即发送，看它送出去没有：打断了这一轮的会有 UserPromptSubmit；转后台、在这一轮里读到的没有，
    /// 就看输入框里的字还在不在。不补按（补按会把后面打的也发出去）。
    async fn confirm_send_now(&self, inst: &Inst, id: &str, text: &str, mut prompts: watch::Receiver<u64>) -> Result<Value, LocalError> {
        self.instances.send_keys(id, &["C-x", "C-s"]).await?;
        let first: String = text.lines().next().unwrap_or("").chars().take(20).collect();
        let deadline = Instant::now() + inst.agent.delivery_wait();
        while Instant::now() < deadline {
            if tokio::time::timeout(Duration::from_millis(300), prompts.changed()).await.is_ok() {
                return Ok(json!({ "delivered": true }));
            }
            match self.instances.screen(id).await.map(|s| inst.agent.classify(&s)) {
                Some(Screen::Input { text: shown, .. }) if first.is_empty() || !shown.contains(&first) => return Ok(json!({ "delivered": true })),
                _ => {}
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

    /// 清空 TUI 的输入框：Ctrl-U 删到行首，退格接上上一行，一行一行删（不用 Esc：输入框空着时连按两次 Esc 会打开回退菜单）。
    async fn clear_input(&self, id: &str, lines: usize) -> Result<(), LocalError> {
        let mut keys = vec![];
        for _ in 0..lines.clamp(1, 40) {
            keys.extend(["C-u", "BSpace"]);
        }
        keys.push("C-u");
        self.instances.send_keys(id, &keys).await?;
        tokio::time::sleep(Duration::from_millis(200)).await;
        Ok(())
    }

    /// 停止这一轮（Esc）。没有 hook 会告诉我们停下来了，交给抓屏确认。
    pub async fn interrupt(&self, id: &str) -> Result<(), LocalError> {
        self.require(id)?;
        if self.is_codex(id) {
            return self.codex_interrupt(id).await;
        }
        let inst = self.inst(id);
        // 已经停下来时不按：输入框空着时连按两次 Esc 会打开回退（Rewind）菜单
        // 额度用完、正在等重置后自动继续时也按（Esc 取消等待；这时输入框是空闲的）
        let idle = matches!(lock(&inst.st).phase, Phase::Idle | Phase::Detached);
        if idle {
            let cap = self.instances.screen(id).await;
            let busy = matches!(cap.as_deref().map(|s| inst.agent.classify(s)), Some(Screen::Input { busy: true, .. }));
            if !busy && cap.as_deref().and_then(screen::limit_wait).is_none() {
                return Ok(());
            }
        }
        self.instances.send_keys(id, &["Escape"]).await?;
        lock(&inst.st).last_hook = None;
        Ok(())
    }

    /// 回答审批。`decision`：`allow` / `allow_session` / `deny` / `answer`（AskUserQuestion）/ `terminal`。
    pub fn answer(&self, id: &str, req: &PermissionAnswer) -> Result<(), LocalError> {
        self.require(id)?;
        let inst = self.inst(id);
        let mut st = lock(&inst.st);
        let Some(p) = st.pending.as_mut().filter(|p| p.id == req.id) else { return Err(LocalError::new("CHAT_STALE")) };
        // Codex：答复直接是 app-server 请求的结果
        if inst.agent.launch() == "codex" {
            // 命令规则标出来的：只能这一次允许
            if p.guard.is_some() && req.decision == "allow_session" {
                return Err(LocalError::invalid("decision", "invalid"));
            }
            let result = codex_chat::decision(&p.tool, p.kind, &p.input, req)?;
            p.resolve(result.to_string());
            st.pending = None;
            Self::set_phase(&inst, &mut st, Phase::Running, None);
            // 拒绝时写的理由：Codex 的拒绝不带话，接着插一句给它
            let msg = req.message.as_deref().map(str::trim).filter(|m| !m.is_empty() && req.decision == "deny").map(String::from);
            if let Some(msg) = msg {
                self.codex_say_later(id, msg);
            }
            return Ok(());
        }
        if p.guard.is_some() {
            let out = match req.decision.as_str() {
                "allow" => pre_tool_use("allow", "The user confirmed this command in Looklook."),
                "deny" => {
                    let why = req.message.as_deref().map(str::trim).filter(|m| !m.is_empty());
                    pre_tool_use("deny", &format!("The user declined this command in Looklook.{}", why.map(|m| format!(" Their note: {m}")).unwrap_or_default()))
                }
                _ => return Err(LocalError::invalid("decision", "invalid")),
            };
            p.resolve(out);
            st.pending = None;
            Self::set_phase(&inst, &mut st, Phase::Running, None);
            return Ok(());
        }
        if req.decision == "terminal" {
            p.resolve(String::new());
            st.pending = None;
            Self::set_phase(&inst, &mut st, Phase::Tui, None);
            return Ok(());
        }
        let decision = inst.agent.decision(&PendingView { tool: &p.tool, input: &p.input, suggestions: &p.suggestions }, p.kind, req)?;
        p.resolve(json!({ "hookSpecificOutput": { "hookEventName": "PermissionRequest", "decision": decision } }).to_string());
        st.pending = None;
        Self::set_phase(&inst, &mut st, Phase::Running, None);
        Ok(())
    }

    /// 计划审批：hook 批准不了（实测），只能在 TUI 的确认框里按数字键。先抓屏核对确认框还在、选项和预期一致。
    /// 3 是“告诉 Claude 要改什么”：按 3 后光标停在这一项的输入框里，打字、回车，Claude 照着改计划（实测 2.1.304）。
    pub async fn plan(&self, id: &str, choice: u8, feedback: Option<&str>) -> Result<(), LocalError> {
        self.require(id)?;
        // 计划确认框是 Claude Code 的（[`Agent::plan_check`]；Codex 没有）
        if self.inst(id).agent.plan_check(1).is_none() {
            return Err(LocalError::invalid("choice", "invalid"));
        }
        let feedback = feedback.map(|f| f.replace(['\r', '\n'], " ")).map(|f| f.trim().to_string()).filter(|f| !f.is_empty());
        let expect = match choice {
            1 => "1. Yes",
            2 => "2. Yes",
            3 if feedback.is_some() => "3. ",
            _ => return Err(LocalError::invalid("choice", "invalid")),
        };
        let shown = self.instances.screen(id).await.unwrap_or_default();
        let plain = shown.lines().map(screen_plain).collect::<Vec<_>>().join("\n");
        if !(plain.contains("ready to execute") || plain.contains("Would you like to proceed")) || !plain.contains(expect) {
            return Err(LocalError::new("CHAT_SCREEN_CHANGED"));
        }
        self.instances.send_keys(id, &[&choice.to_string()]).await?;
        if let Some(text) = feedback {
            tokio::time::sleep(Duration::from_millis(150)).await;
            self.type_text(id, &text.chars().take(2000).collect::<String>()).await?;
            tokio::time::sleep(Duration::from_millis(80)).await;
            self.instances.send_keys(id, &["Enter"]).await?;
        }
        Ok(())
    }

    /// 面板里是 shell（claude 已退出，或是没带 hooks 启动后退出了）：以对话模式接着上次的会话启动。
    pub async fn relaunch(&self, id: &str) -> Result<(), LocalError> {
        self.require(id)?;
        // Codex：重新起 app-server、重新加载会话
        if self.is_codex(id) {
            return self.codex_attach(id).await.map(|_| ());
        }
        let pane = self.instances.pane(id).await.ok_or_else(|| LocalError::new("CHAT_NOT_RUNNING"))?;
        if agent_state(self.inst(id).agent, &pane).is_some() {
            return Err(LocalError::new("CHAT_CLAUDE_RUNNING"));
        }
        let session = lock(&self.inst(id).st).binding.as_ref().map(|b| b.session_id.clone());
        let cmd = self.instances.chat_relaunch_command(id, session.as_deref())?;
        self.instances.send_literal(id, &cmd).await?;
        self.instances.send_keys(id, &["Enter"]).await
    }

    // ---------------- 以前的会话转到对话模式 ----------------

    /// 界面上的“现在就转过来”：不等空闲，退出终端里以前的 Claude Code / Codex，接着同一段对话转到对话模式
    pub async fn adopt(&self, id: &str) -> Result<(), LocalError> {
        self.require(id)?;
        if self.is_codex(id) {
            return self.codex_adopt(id, true).await.map(|_| ());
        }
        self.claude_adopt(id, true).await
    }

    /// 以前的 Claude Code（不是带着看看的 hooks 启动的）空闲时：后台转到对话模式。同一个终端同时只跑一个。
    fn claude_adopt_later(self: Arc<Self>, id: &str) {
        if !lock(&self.codex.adopting).insert(id.to_string()) {
            return;
        }
        let id = id.to_string();
        tokio::spawn(async move {
            if let Err(e) = self.claude_adopt(&id, false).await {
                tracing::warn!(id, error = ?e, "没能把以前的 Claude Code 会话转到对话模式");
            }
            lock(&self.codex.adopting).remove(&id);
        });
    }

    /// 退出终端里以前的 Claude Code，用带 hooks 的命令接着同一段对话重新启动（`--resume <会话>`）。
    /// 找不到是哪段对话时：自动转的不转（免得丢了上下文）；“现在就转过来”就开一段新的。
    async fn claude_adopt(&self, id: &str, force: bool) -> Result<(), LocalError> {
        let inst = self.inst(id);
        let pane = self.instances.pane(id).await.ok_or_else(|| LocalError::new("CHAT_NOT_RUNNING"))?;
        let Some((args, started_ms)) = tokio::task::block_in_place(|| proc_under(pane.pid, inst.agent.process_names())) else { return Ok(()) };
        if !force {
            let idle = self.instances.screen(id).await.map(|s| inst.agent.classify(&s));
            if !matches!(idle, Some(Screen::Input { busy: false, ref text }) if text.trim().is_empty()) {
                return Ok(());
            }
        }
        let session = self.claude_session_of(id, &args, started_ms).await;
        if session.is_none() && !force {
            return Ok(());
        }
        Self::set_phase(&inst, &mut lock(&inst.st), Phase::Detached, Some("adopting"));
        self.quit_claude(id).await?;
        let cmd = self.instances.chat_relaunch_command(id, session.as_deref())?;
        self.instances.send_literal(id, &cmd).await?;
        self.instances.send_keys(id, &["Enter"]).await
    }

    /// 退出终端里的 Claude Code：连按两次 Ctrl+C（在跑的话第一次是中断）。退不掉时报 CHAT_CLAUDE_RUNNING。
    async fn quit_claude(&self, id: &str) -> Result<(), LocalError> {
        let agent = self.inst(id).agent;
        for _ in 0..3 {
            let _ = self.instances.send_keys(id, &["C-c"]).await;
            tokio::time::sleep(Duration::from_millis(200)).await;
            let _ = self.instances.send_keys(id, &["C-c"]).await;
            tokio::time::sleep(Duration::from_millis(1500)).await;
            if self.instances.pane(id).await.is_some_and(|p| agent_state(agent, &p).is_none()) {
                break;
            }
        }
        if self.instances.pane(id).await.is_some_and(|p| agent_state(agent, &p).is_some()) {
            return Err(LocalError::new("CHAT_CLAUDE_RUNNING"));
        }
        Ok(())
    }

    /// 额度用完时“换个账号继续”：退出 Claude Code，这个会话改用 `profile`（claude_profiles.rs），
    /// 接着同一段对话重新启动（各账号的对话记录是共用的），起来后发一句 `continue`。
    pub async fn switch_profile(self: &Arc<Self>, id: &str, profile: &str) -> Result<(), LocalError> {
        self.require(id)?;
        let row = self.store.instance(id)?.ok_or_else(LocalError::not_found)?;
        // 容器里的会话不用看看的账号（model_wrap）
        if row.launch != "claude" || self.instances.container(id).is_some() {
            return Err(LocalError::new("CHAT_UNAVAILABLE"));
        }
        crate::claude_profiles::get(&self.store, profile).ok_or_else(|| LocalError::invalid("claude_profile", "not_found"))?;
        let inst = self.inst(id);
        let session = lock(&inst.st).binding.as_ref().map(|b| b.session_id.clone()).ok_or_else(|| LocalError::new("CHAT_NOT_RUNNING"))?;
        self.instances.pane(id).await.ok_or_else(|| LocalError::new("CHAT_NOT_RUNNING"))?;
        Self::set_phase(&inst, &mut lock(&inst.st), Phase::Detached, Some("switching"));
        // 正在等额度重置时先按 Esc 取消（不然退出前它可能刚好到点接着跑）
        if lock(&inst.st).limit_wait.is_some() {
            let _ = self.instances.send_keys(id, &["Escape"]).await;
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
        self.quit_claude(id).await?;
        // 退出的过程中抓屏可能把旧的那个记成了空闲：重新记成在换，下面等的是新起来的那个
        Self::set_phase(&inst, &mut lock(&inst.st), Phase::Detached, Some("switching"));
        crate::claude_profiles::pin(&self.store, id, Some(profile))?;
        let cmd = self.instances.chat_relaunch_command(id, Some(&session))?;
        self.instances.send_literal(id, &cmd).await?;
        self.instances.send_keys(id, &["Enter"]).await?;
        // 起来（SessionStart 到了、空闲）后发 continue；一分钟还没起来就不发了，用户自己发
        let me = self.clone();
        let id = id.to_string();
        tokio::spawn(async move {
            let deadline = Instant::now() + Duration::from_secs(60);
            while Instant::now() < deadline {
                tokio::time::sleep(Duration::from_millis(500)).await;
                let inst = me.inst(&id);
                let up = me.instances.pane(&id).await.is_some_and(|p| agent_state(inst.agent, &p).is_some());
                if up && lock(&inst.st).phase == Phase::Idle {
                    if let Err(e) = me.input(&id, "continue", &[]).await {
                        tracing::warn!(id, error = ?e, "换账号后没能发出 continue");
                    }
                    return;
                }
            }
            tracing::warn!(id, "换账号后一分钟内 Claude Code 没起来，不发 continue");
        });
        Ok(())
    }

    /// 以前的 Claude Code 用的是哪段对话：命令行里的 `--resume` / `-r` / `--session-id`；已经记着的；
    /// 都没有时取这个文件夹里最近的、没被别的终端接着的那段，正在写的优先。
    async fn claude_session_of(&self, id: &str, args: &[String], started_ms: i64) -> Option<String> {
        let ok = |s: &String| s.len() >= 32 && crate::history::valid_id(s);
        for flag in ["--resume", "-r", "--session-id"] {
            if let Some(s) = args.iter().skip_while(|a| a.as_str() != flag).nth(1).filter(|s| ok(s)) {
                return Some(s.clone());
            }
            if let Some(s) = args.iter().find_map(|a| a.strip_prefix(&format!("{flag}="))).map(String::from).filter(ok) {
                return Some(s);
            }
        }
        if let Some(b) = lock(&self.inst(id).st).binding.clone() {
            return Some(b.session_id);
        }
        let row = self.store.instance(id).ok()??;
        guess_conversation(&self.store, id, &row.workdir, "claude", started_ms).await
    }

    // ---------------- 快捷设置 ----------------

    /// 输入框是空的普通输入框时才替界面按键（选择器里打字会被当成快捷键；用户在终端里打了一半的字不能接着往后打）。
    async fn ready_for_keys(&self, id: &str) -> Result<(), LocalError> {
        match self.instances.screen(id).await.map(|s| self.inst(id).agent.classify(&s)) {
            Some(Screen::Input { text, .. }) if text.is_empty() => Ok(()),
            Some(Screen::Input { .. }) => Err(LocalError::new("CHAT_INPUT_NOT_EMPTY")),
            _ => Err(LocalError::new("CHAT_BUSY")),
        }
    }

    fn hold(&self, id: &str, on: bool) {
        lock(&self.inst(id).st).hold = on.then(|| Instant::now() + Duration::from_secs(5));
    }

    /// 快捷设置，不用到终端里去：
    /// - `model`：`/model <名字>`。对话已经有缓存时 Claude Code 会先问“要切换吗”，替用户选第一项（实测 2.1.303）；
    /// - `effort`：`/effort <级别>`；
    /// - `mode`：权限模式只能用 Shift+Tab 轮换，按到状态栏显示目标模式为止。
    pub async fn setting(&self, id: &str, kind: &str, value: &str) -> Result<(), LocalError> {
        self.require(id)?;
        let inst = self.inst(id);
        if inst.agent.launch() == "codex" {
            return self.codex_setting(id, kind, value);
        }
        // 斜杠命令和状态栏都是按 Claude Code 认的
        if inst.agent.launch() != "claude" {
            return Err(LocalError::new("CHAT_UNAVAILABLE"));
        }
        match lock(&inst.st).phase {
            Phase::Detached => return Err(LocalError::new("CHAT_NOT_RUNNING")),
            Phase::Tui | Phase::Approval => return Err(LocalError::new("CHAT_BUSY")),
            // 运行中斜杠命令会排进队列，等这一轮做完才执行
            Phase::Running if kind != "mode" => return Err(LocalError::new("CHAT_RUNNING")),
            _ => {}
        }
        match kind {
            "model" | "effort" => {
                let ok = match kind {
                    "model" => !value.is_empty() && value.len() <= 64 && value.chars().all(|c| c.is_ascii_alphanumeric() || "._-[]".contains(c)),
                    _ => screen::EFFORTS.contains(&value),
                };
                if !ok {
                    return Err(LocalError::invalid("value", "invalid"));
                }
                self.ready_for_keys(id).await?;
                self.hold(id, true);
                let r = self.slash_setting(id, kind, value).await;
                self.hold(id, false);
                r?;
                let mut st = lock(&inst.st);
                if kind == "model" {
                    st.model = Some(value.to_string());
                    st.model_set_at = Some(st.model_msg.clone());
                } else {
                    st.effort = Some(value.to_string());
                }
                Self::emit_status(&inst, &st);
                Ok(())
            }
            "mode" => {
                const MODES: [&str; 5] = ["default", "acceptEdits", "plan", "auto", "bypassPermissions"];
                if !MODES.contains(&value) {
                    return Err(LocalError::invalid("value", "invalid"));
                }
                // 一圈最多 5 个模式；没开的模式（自动模式、全部权限）轮不到
                for _ in 0..6 {
                    let f = self.instances.screen(id).await.as_deref().and_then(screen::footer).ok_or_else(|| LocalError::new("CHAT_BUSY"))?;
                    if f.mode.as_deref() == Some(value) {
                        let mut st = lock(&inst.st);
                        st.mode = f.mode;
                        Self::emit_status(&inst, &st);
                        return Ok(());
                    }
                    self.instances.send_keys(id, &["BTab"]).await?;
                    tokio::time::sleep(Duration::from_millis(350)).await;
                }
                Err(LocalError::new("CHAT_MODE_UNAVAILABLE"))
            }
            _ => Err(LocalError::invalid("kind", "invalid")),
        }
    }

    async fn slash_setting(&self, id: &str, kind: &str, value: &str) -> Result<(), LocalError> {
        self.type_text(id, &format!("/{kind} {value}")).await?;
        tokio::time::sleep(Duration::from_millis(80)).await;
        self.instances.send_keys(id, &["Enter"]).await?;
        // 命令执行完 TUI 会重置输入框：等一下再返回，免得紧接着发的消息开头被吞掉
        let settle = || tokio::time::sleep(Duration::from_millis(500));
        if kind != "model" {
            settle().await;
            return Ok(());
        }
        for _ in 0..12 {
            tokio::time::sleep(Duration::from_millis(250)).await;
            let Some(scr) = self.instances.screen(id).await else { break };
            if matches!(screen::classify(&scr), Screen::Input { .. }) {
                break;
            }
            let plain: Vec<String> = scr.lines().map(screen_plain).collect();
            let yes = plain.iter().any(|l| l.trim_start().trim_start_matches('❯').trim_start().starts_with("1. Yes"));
            if plain.iter().any(|l| l.contains("Switch model?")) && yes {
                self.instances.send_keys(id, &["1"]).await?;
                break;
            }
        }
        settle().await;
        Ok(())
    }

    // ---------------- 模型、用量、改动 ----------------

    /// 这个会话的 Claude Code 配置目录和 `.claude.json`：跟着它用的账号（claude_profiles.rs）
    fn claude_paths(&self, id: &str) -> (PathBuf, PathBuf) {
        crate::claude_profiles::config_paths(&self.paths.home, &crate::claude_profiles::for_instance(&self.store, id))
    }

    /// 读模型菜单要用的上下文：claude 进程的环境变量（读得到时）和 settings 里的 `env`
    async fn model_ctx<'a>(&self, id: &str, workdir: &'a Path) -> models::Ctx<'a> {
        let env = match self.instances.pane(id).await {
            Some(p) if !is_shell(&p.command) => tokio::task::block_in_place(|| models::claude_env(p.pid)),
            _ => HashMap::new(),
        };
        let (config_dir, state_file) = self.claude_paths(id);
        let mut ctx = models::Ctx { config_dir, state_file, workdir, env };
        models::merge_settings_env(&mut ctx);
        ctx
    }

    /// 模型菜单：Claude Code 的选项（`/model` 里的那些），加上第三方接口自己列出的模型。
    pub async fn models(&self, id: &str) -> Result<Value, LocalError> {
        self.require(id)?;
        if self.is_codex(id) {
            return self.codex_models(id).await;
        }
        let row = self.store.instance(id)?.ok_or_else(LocalError::not_found)?;
        let workdir = PathBuf::from(&row.workdir);
        let ctx = self.model_ctx(id, &workdir).await;
        let mut options = tokio::task::block_in_place(|| models::options(&ctx));
        // 第三方接口：claude 进程环境里的地址（看看配置的接口也是这样传进去的）；读不到进程环境时用这个会话的配置
        let profile = crate::claude_profiles::for_instance(&self.store, id);
        let ours = profile.base_url.as_ref().filter(|_| crate::claude_profiles::launch_env(&self.paths.home, &profile).is_some());
        let base = ctx.env.get("ANTHROPIC_BASE_URL").cloned().or_else(|| ours.cloned());
        let mut provider = Value::Null;
        if let Some(base) = base.filter(|b| !b.contains("api.anthropic.com")) {
            let ours = ours.filter(|u| u.trim_end_matches('/') == base.trim_end_matches('/')).map(|_| &profile);
            let token = ctx.env.get("ANTHROPIC_AUTH_TOKEN").or_else(|| ctx.env.get("ANTHROPIC_API_KEY")).cloned().or_else(|| {
                ours.and_then(|p| std::fs::read_to_string(crate::claude_profiles::key_path(&self.paths.home, &p.id)).ok()).map(|k| k.trim().to_string())
            });
            if let Some(token) = token.filter(|t| !t.is_empty()) {
                let ids = models::provider_models(&base, &token).await;
                options.extend(models::provider_options(&options, &ids));
            }
            provider = json!({ "name": ours.map(|p| p.name.clone()) });
        }
        let inst = self.inst(id);
        let st = lock(&inst.st);
        Ok(json!({ "options": options, "provider": provider, "model": st.model, "actual_model": st.actual_model, "effort": st.effort }))
    }

    /// 用量：上下文、这个会话累计的 token、订阅的 5 小时 / 7 天用量
    pub async fn usage(&self, id: &str) -> Result<Value, LocalError> {
        self.require(id)?;
        if self.is_codex(id) {
            return self.codex_usage(id).await;
        }
        let row = self.store.instance(id)?.ok_or_else(LocalError::not_found)?;
        let workdir = PathBuf::from(&row.workdir);
        let inst = self.inst(id);
        self.poll(&inst).await;
        let totals = inst.tr.lock().await.as_ref().and_then(Reader::claude).map(|t| t.totals).unwrap_or_default();
        let (model, context, version) = {
            let st = lock(&inst.st);
            (st.actual_model.clone(), st.context, st.version.clone())
        };
        let profile = crate::claude_profiles::for_instance(&self.store, id);
        let (config_dir, state_file) = crate::claude_profiles::config_paths(&self.paths.home, &profile);
        let ctx = models::Ctx { config_dir, state_file, workdir: &workdir, env: HashMap::new() };
        let (window, mut limits) = tokio::task::block_in_place(|| (model.as_deref().map(|m| models::context_window(&ctx, m)), models::limits(&ctx)));
        // 订阅用量只读 Claude Code 自己的缓存；缓存没有或旧了，请它自己去取（第三方接口没有订阅用量）。
        // 一点都没有时等它取完再读，否则在后台取，下次来取就是新的
        // 第三方接口：服务商的套餐用量（认得的几家，见 provider_quota.rs），带着服务商的名字
        if profile.kind == crate::claude_profiles::Kind::Api {
            let r = crate::claude_profiles::api_usage(&self.paths.home, &profile).await;
            limits = r["limits"].clone();
        }
        if profile.kind == crate::claude_profiles::Kind::Official {
            let fetched = limits["fetched_at"].as_i64().unwrap_or(0);
            if crate::util::system_ms() - fetched > models::LIMITS_STALE_MS {
                let shell = self.instances.shell().to_string();
                let account = (profile.id != crate::claude_profiles::OFFICIAL).then(|| crate::claude_profiles::account_dir(&self.paths.home, &profile.id));
                let cwd = self.paths.home.join("run").join("usage");
                let dir = ctx.config_dir.clone();
                let refresh = async move { models::refresh_limits(&shell, &dir, account.as_deref(), &cwd).await };
                if limits.is_null() {
                    refresh.await;
                    limits = tokio::task::block_in_place(|| models::limits(&ctx));
                } else {
                    tokio::spawn(refresh);
                }
            }
        }
        Ok(json!({ "model": model, "context": context, "window": window, "totals": totals, "limits": limits, "version": version }))
    }

    /// 对话生成的文件（目前只有 Codex 生成的图片）
    pub fn files(&self, id: &str) -> Result<Value, LocalError> {
        self.require(id)?;
        if !self.is_codex(id) {
            return Ok(json!({ "items": [] }));
        }
        Ok(tokio::task::block_in_place(|| self.codex_files(id)))
    }

    /// 对话产出的文件（目前只有 Codex：新建、改过、提到过的本机文件）
    pub async fn artifacts(&self, id: &str) -> Result<Value, LocalError> {
        self.require(id)?;
        if !self.is_codex(id) {
            return Ok(json!({ "items": [] }));
        }
        Ok(self.codex_artifacts(id).await)
    }

    /// 要用户先处理的：待确认的 hook、还没登录（只有 Codex 有，见 `codex_chat::codex_setup`）
    pub async fn setup(&self, id: &str) -> Result<Value, LocalError> {
        self.require(id)?;
        if !self.is_codex(id) {
            return Ok(json!({ "hooks": [], "login_required": false }));
        }
        self.codex_setup(id).await
    }

    /// 信任或跳过界面上列出的 hook
    pub async fn hooks_answer(&self, id: &str, trust: bool, picked: &[(String, String)]) -> Result<(), LocalError> {
        self.require(id)?;
        if !self.is_codex(id) {
            return Err(LocalError::new("CHAT_UNAVAILABLE"));
        }
        self.codex_hooks_answer(id, trust, picked).await
    }

    /// 产出的某个文件的路径：只认这段对话里出现过的
    pub async fn artifact_path(&self, id: &str, path: &str) -> Result<PathBuf, LocalError> {
        self.require(id)?;
        if !self.is_codex(id) {
            return Err(LocalError::not_found());
        }
        self.codex_artifact_path(id, path).await.ok_or_else(LocalError::not_found)
    }

    /// 对话生成的某个文件的路径（`files` 列出的那些）
    pub fn file_path(&self, id: &str, name: &str) -> Result<PathBuf, LocalError> {
        self.require(id)?;
        if !self.is_codex(id) {
            return Err(LocalError::not_found());
        }
        self.codex_file_path(id, name).ok_or_else(LocalError::not_found)
    }

    /// 权限规则和 MCP 服务器（只读）
    pub fn config(&self, id: &str) -> Result<Value, LocalError> {
        self.require(id)?;
        let row = self.store.instance(id)?.ok_or_else(LocalError::not_found)?;
        let workdir = PathBuf::from(&row.workdir);
        let (config_dir, state_file) = self.claude_paths(id);
        let ctx = models::Ctx { config_dir, state_file, workdir: &workdir, env: HashMap::new() };
        Ok(tokio::task::block_in_place(|| models::config(&ctx)))
    }

    /// 这个会话改过的文件：每个文件的增删行数（成功的 Edit / MultiEdit / Write）
    pub async fn changes(&self, id: &str) -> Result<Value, LocalError> {
        self.require(id)?;
        let inst = self.inst(id);
        self.poll(&inst).await;
        let tr = inst.tr.lock().await;
        let Some(t) = tr.as_ref() else { return Ok(json!({ "files": [] })) };
        let items;
        let calls = match t {
            Reader::Claude(c) => c.tool_calls(),
            Reader::Other(r) => {
                items = r.items(0, r.len());
                transcript::tool_calls_in(&items)
            }
        };
        let mut files: Vec<(String, u64, u64, String)> = vec![];
        for c in calls {
            if !matches!(c.name, "Edit" | "MultiEdit" | "Write") {
                continue;
            }
            let Some(transcript::Body::ToolResult { is_error: false, extra, .. }) = c.result.map(|r| &r.body) else { continue };
            let Some(path) = c.input["file_path"].as_str() else { continue };
            let (add, del) = diff_stat(c.name, c.input, extra.as_ref());
            match files.iter_mut().find(|f| f.0 == path) {
                Some(f) => {
                    f.1 += add;
                    f.2 += del;
                    f.3 = c.ts.unwrap_or("").to_string();
                }
                None => files.push((path.to_string(), add, del, c.ts.unwrap_or("").to_string())),
            }
        }
        Ok(json!({ "files": files.iter().map(|(path, add, del, ts)| json!({ "path": path, "add": add, "del": del, "ts": ts })).collect::<Vec<_>>() }))
    }

    // ---------------- 额度用完 ----------------

    /// 额度用完时 Claude Code 弹的菜单（关掉了 autoContinueAtUsageLimit 时才有“等重置后自动继续”）：
    /// [`LIMIT_AUTO_AFTER`] 没人选就替用户选这一项，任务不会因为没人在电脑前一直停着。
    /// 不管有没有人开着对话页都要看（关着页面时没有抓屏），所以单独跑：每 2 秒看一眼，菜单不在了或者一直没出现就结束。
    fn guard_limit(self: &Arc<Self>, inst: &Arc<Inst>) {
        if inst.agent.launch() != "claude" || inst.limit_guard.swap(true, Ordering::SeqCst) {
            return;
        }
        let (me, inst) = (self.clone(), inst.clone());
        tokio::spawn(async move {
            let start = Instant::now();
            let mut seen: Option<Instant> = None;
            loop {
                let d = me.instances.screen(&inst.id).await.as_deref().and_then(|c| inst.agent.dialog(c)).filter(screen::Dialog::is_limit);
                match d {
                    Some(d) => {
                        // 没有“自动继续”这一项（例如已经在等）：留给人选
                        let Some(n) = d.limit_auto() else { break };
                        let first = *seen.get_or_insert_with(Instant::now);
                        {
                            let mut st = lock(&inst.st);
                            if st.limit_auto_at.is_none() {
                                st.limit_auto_at = Some(crate::util::system_ms() + LIMIT_AUTO_AFTER.as_millis() as i64);
                                Self::emit_status(&inst, &st);
                            }
                        }
                        if first.elapsed() >= LIMIT_AUTO_AFTER {
                            tracing::info!(id = %inst.id, option = %d.options[n].label, "额度用完的菜单没人选，替用户选“等重置后自动继续”");
                            if let Err(e) = me.dialog_choose(&inst.id, &d.title, &d.options[n].label, n).await {
                                tracing::warn!(code = %e.code, "没能替用户选“等重置后自动继续”");
                            }
                            break;
                        }
                    }
                    // 有人选过了，或者一直没出现
                    None if seen.is_some() || start.elapsed() >= LIMIT_MENU_WAIT => break,
                    None => {}
                }
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
            {
                let mut st = lock(&inst.st);
                if st.limit_auto_at.take().is_some() {
                    Self::emit_status(&inst, &st);
                }
            }
            inst.limit_guard.store(false, Ordering::SeqCst);
        });
    }

    // ---------------- TUI 上的选择框 ----------------

    /// 在界面上选了选择框的第 `index` 项：再抓一次屏，标题和选项数都对得上才按方向键移过去、回车。
    pub async fn dialog_choose(&self, id: &str, title: &str, label: &str, index: usize) -> Result<(), LocalError> {
        self.require(id)?;
        let agent = self.inst(id).agent;
        let d = self.instances.screen(id).await.as_deref().and_then(|c| agent.dialog(c)).ok_or_else(|| LocalError::new("CHAT_SCREEN_CHANGED"))?;
        // 标题是从选项往上找的第一行文字，说明里的时间、重绘时的折行都会让它和界面上看到的不一样；
        // 选项文字稳定得多：标题对不上时，只要这一项还是界面上选的那一项就照选
        let same_option = !label.is_empty() && d.options.get(index).is_some_and(|o| o.label == label);
        if index >= d.options.len() || (d.title != title && !same_option) {
            return Err(LocalError::new("CHAT_SCREEN_CHANGED"));
        }
        let key = if index > d.selected { "Down" } else { "Up" };
        let n = index.abs_diff(d.selected);
        let mut keys = vec![key; n];
        keys.push("Enter");
        for k in keys {
            self.instances.send_keys(id, &[k]).await?;
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
        let inst = self.inst(id);
        lock(&inst.st).last_hook = None;
        Ok(())
    }

    /// 关掉选择框（Esc）
    pub async fn dialog_cancel(&self, id: &str) -> Result<(), LocalError> {
        self.require(id)?;
        let inst = self.inst(id);
        // 界面上的弹窗和状态可能慢一步：只要屏幕上真有选择框就按 Esc（没有选择框时不按：
        // 输入框空着时连按两次 Esc 会打开回退菜单）
        let modal = matches!(self.instances.screen(id).await.map(|s| inst.agent.classify(&s)), Some(Screen::Modal));
        if !modal {
            let mut st = lock(&inst.st);
            if st.phase == Phase::Tui {
                Self::set_phase(&inst, &mut st, Phase::Idle, None);
            }
            return Ok(());
        }
        self.instances.send_keys(id, &["Escape"]).await
    }

    // ---------------- 后台任务和子代理 ----------------

    /// “任务”面板：这个会话的子代理（`<会话>/subagents/agent-*.meta.json`）和后台命令
    /// （`run_in_background` 的 Bash，以及跑到一半转到后台的，见 [`transcript::moved_to_background`]）。
    pub async fn tasks(&self, id: &str) -> Result<Value, LocalError> {
        self.require(id)?;
        let inst = self.inst(id);
        self.poll(&inst).await;
        let (main, alive, live_shells, hooked_agents) = {
            let st = lock(&inst.st);
            let alive = st.phase != Phase::Detached;
            // 状态栏上的个数只在普通输入框时看得到
            let shells = matches!(st.phase, Phase::Idle | Phase::Running).then(|| st.background.iter().filter(|(_, k)| k == "shell").map(|(n, _)| *n as usize).sum::<usize>());
            (st.binding.as_ref().map(|b| b.transcript_path.clone()), alive, shells, st.agents.clone())
        };
        let Some(main) = main else { return Ok(json!({ "agents": [], "shells": [] })) };
        let tr = inst.tr.lock().await;
        let Some(t) = tr.as_ref().and_then(Reader::claude) else { return Ok(json!({ "agents": [], "shells": [] })) };
        let calls = t.tool_calls();
        let now = time::OffsetDateTime::now_utc();
        let age = |ts: Option<&str>| ts.and_then(|s| time::OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339).ok()).map(|t| (now - t).whole_seconds());

        let mut shells: Vec<Value> = vec![];
        for c in calls.iter().filter(|c| c.is_background_shell()) {
            let note = t.notes.get(c.id);
            let failed = matches!(c.result.map(|r| &r.body), Some(transcript::Body::ToolResult { is_error: true, .. }));
            let state = match (note, failed) {
                (Some(n), _) => n.status.clone(),
                (None, true) => "failed".into(),
                (None, false) => "running".into(),
            };
            shells.push(json!({
                "tool_use_id": c.id,
                "command": c.input["command"].as_str().map(|s| s.chars().take(500).collect::<String>()),
                "description": c.input["description"],
                "started": c.ts,
                "state": state,
                "summary": note.map(|n| &n.summary),
                "age": age(c.ts),
            }));
        }
        // 没有结束通知的不一定还在跑（claude 重启过、通知还没写进来）：和状态栏上的个数对一下，多出来的算已经结束（旧的先算）
        let running: Vec<usize> = shells.iter().enumerate().filter(|(_, s)| s["state"] == "running").map(|(i, _)| i).collect();
        let keep = if !alive { 0 } else { live_shells.unwrap_or(running.len()) };
        for &i in running.iter().take(running.len().saturating_sub(keep)) {
            // 刚启动的命令，状态栏可能还没更新
            if shells[i]["age"].as_i64().is_none_or(|a| a > 5) {
                shells[i]["state"] = json!("ended");
            }
        }

        let by_tool: HashMap<&str, &transcript::ToolCall> = calls.iter().map(|c| (c.id, c)).collect();
        let dir = main.with_extension("").join("subagents");
        let mut metas: Vec<(std::time::SystemTime, PathBuf, Value)> = std::fs::read_dir(&dir)
            .map(|rd| {
                rd.flatten()
                    .filter_map(|e| {
                        let p = e.path();
                        let name = p.file_name()?.to_str()?;
                        let aid = name.strip_prefix("agent-")?.strip_suffix(".meta.json")?;
                        let jsonl = dir.join(format!("agent-{aid}.jsonl"));
                        let modified = std::fs::metadata(&jsonl).or_else(|_| e.metadata()).and_then(|m| m.modified()).ok()?;
                        let meta: Value = serde_json::from_slice(&std::fs::read(&p).ok()?).ok()?;
                        Some((modified, jsonl, meta))
                    })
                    .collect()
            })
            .unwrap_or_default();
        metas.sort_by_key(|m| std::cmp::Reverse(m.0));
        metas.truncate(40);
        let mut cache = inst.agents.lock().await;
        let mut agents: Vec<Value> = vec![];
        for (modified, jsonl, meta) in metas {
            let aid = jsonl.file_stem().and_then(|s| s.to_str()).and_then(|s| s.strip_prefix("agent-")).unwrap_or("").to_string();
            let sub = cache.entry(jsonl.clone()).or_insert_with(|| Transcript::subagent(jsonl.clone()));
            let _ = tokio::task::block_in_place(|| sub.poll());
            let sub_calls = sub.tool_calls();
            let last = sub.items(sub.len().saturating_sub(1), sub.len()).pop();
            let (last_tool, last_text, finished_reply) = match last.as_ref().map(|i| &i.body) {
                Some(transcript::Body::Assistant { blocks, .. }) => {
                    let tool = blocks.iter().rev().find_map(|b| match b {
                        transcript::Block::ToolUse { name, input, .. } => Some(json!({ "name": name, "input": slim_input(input) })),
                        _ => None,
                    });
                    let text = blocks.iter().rev().find_map(|b| match b {
                        transcript::Block::Text { text } => Some(text.chars().take(300).collect::<String>()),
                        _ => None,
                    });
                    let done = tool.is_none() && text.is_some();
                    (tool, text, done)
                }
                _ => (sub_calls.last().map(|c| json!({ "name": c.name, "input": slim_input(c.input) })), None, false),
            };
            let tool_id = meta["toolUseId"].as_str().unwrap_or("");
            let call = by_tool.get(tool_id);
            let note = t.notes.get(tool_id);
            let background = meta["requestShape"] == "background" || call.is_some_and(|c| c.input["run_in_background"] == true);
            let state = if meta["stoppedByUser"] == true || note.is_some_and(|n| n.status == "killed") {
                "stopped"
            } else if note.is_some_and(|n| n.status == "failed") {
                "failed"
            } else if hooked_agents.contains(&aid) {
                "running"
            } else if meta["finished"] == true || note.is_some() || finished_reply || (!background && call.is_some_and(|c| c.result.is_some())) {
                "done"
            } else if !alive {
                "ended"
            } else {
                "running"
            };
            let started = call.and_then(|c| c.ts).map(String::from).or_else(|| sub.items(0, 1).first().and_then(|i| i.ts.clone()));
            let updated = modified.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
            agents.push(json!({
                "id": aid,
                "type": meta["agentType"],
                "description": meta["description"].as_str().or_else(|| call.and_then(|c| c.input["description"].as_str())),
                "model": meta["model"],
                "tool_use_id": tool_id,
                "background": background,
                "worktree": meta["worktreePath"],
                "state": state,
                "started": started,
                "updated": updated,
                "steps": sub_calls.len(),
                "last_tool": last_tool,
                "last_text": last_text,
                "summary": note.map(|n| &n.summary),
            }));
        }
        // 这次没列出来的（太旧了）不再跟读
        let listed: HashSet<PathBuf> = agents.iter().filter_map(|a| a["id"].as_str()).map(|a| dir.join(format!("agent-{a}.jsonl"))).collect();
        cache.retain(|p, _| listed.contains(p));
        // 运行中的排前面，其余按开始时间新的在前（列表已经按最后修改时间排过）
        agents.sort_by_key(|a| a["state"] != "running");
        // 状态栏上说在跑、这段对话记录里却找不到的（子代理启动的、/clear 之前启动的）：告诉界面有几个
        let listed = shells.iter().filter(|s| s["state"] == "running").count();
        let unlisted = if alive { live_shells.unwrap_or(0).saturating_sub(listed) } else { 0 };
        Ok(json!({ "agents": agents, "shells": shells, "unlisted_shells": unlisted }))
    }

    /// 后台命令的输出（最后 32 KB）。输出文件的路径来自 transcript，不接受界面传路径。
    pub async fn task_output(&self, id: &str, tool: &str) -> Result<Value, LocalError> {
        self.require(id)?;
        let inst = self.inst(id);
        let tr = inst.tr.lock().await;
        let t = tr.as_ref().and_then(Reader::claude).ok_or_else(LocalError::not_found)?;
        let from_result = || {
            t.tool_calls().into_iter().find(|c| c.id == tool).and_then(|c| match c.result.map(|r| &r.body) {
                Some(transcript::Body::ToolResult { content, .. }) => output_path_in(content),
                _ => None,
            })
        };
        let path = t.notes.get(tool).and_then(|n| n.output_file.clone()).or_else(from_result).map(PathBuf::from).ok_or_else(LocalError::not_found)?;
        drop(tr);
        if !valid_output_path(&path) {
            return Err(LocalError::not_found());
        }
        const LIMIT: u64 = 32 * 1024;
        let (text, size) = tokio::task::block_in_place(|| -> std::io::Result<(String, u64)> {
            use std::io::{Read, Seek, SeekFrom};
            let mut f = std::fs::File::open(&path)?;
            let size = f.metadata()?.len();
            f.seek(SeekFrom::Start(size.saturating_sub(LIMIT)))?;
            let mut buf = vec![];
            f.take(LIMIT).read_to_end(&mut buf)?;
            Ok((String::from_utf8_lossy(&buf).into_owned(), size))
        })
        .map_err(|_| LocalError::not_found())?;
        Ok(json!({ "text": text, "size": size, "truncated": size > LIMIT }))
    }

    /// 停止一个后台任务：和在终端里一样，输入框里按 ↓ 选中状态栏上的任务、回车打开列表，选中这一项按 x。
    /// 每一步都抓屏核对，对不上就按 Esc 退出来、交给终端。
    pub async fn stop_task(&self, id: &str, tool: &str) -> Result<(), LocalError> {
        self.require(id)?;
        let inst = self.inst(id);
        if !matches!(lock(&inst.st).phase, Phase::Idle | Phase::Running) {
            return Err(LocalError::new("CHAT_BUSY"));
        }
        let label = {
            let tr = inst.tr.lock().await;
            let t = tr.as_ref().and_then(Reader::claude).ok_or_else(LocalError::not_found)?;
            let calls = t.tool_calls();
            let c = calls.iter().find(|c| c.id == tool).ok_or_else(LocalError::not_found)?;
            let text = if c.name == "Bash" { c.input["command"].as_str() } else { c.input["description"].as_str() };
            text.unwrap_or("").lines().next().unwrap_or("").split_whitespace().collect::<Vec<_>>().join(" ")
        };
        if label.is_empty() {
            return Err(LocalError::not_found());
        }
        self.ready_for_keys(id).await?;
        self.hold(id, true);
        let r = self.stop_task_keys(id, &label).await;
        // 不管成没成，都回到普通输入框
        for _ in 0..3 {
            if matches!(self.instances.screen(id).await.map(|s| inst.agent.classify(&s)), Some(Screen::Input { .. })) {
                break;
            }
            let _ = self.instances.send_keys(id, &["Escape"]).await;
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
        self.hold(id, false);
        r
    }

    async fn stop_task_keys(&self, id: &str, label: &str) -> Result<(), LocalError> {
        let wait = |ms| tokio::time::sleep(Duration::from_millis(ms));
        let norm = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
        // 列表里长的会被截断：比开头
        let head: String = label.chars().take(24).collect();
        let changed = || LocalError::new("CHAT_SCREEN_CHANGED");
        self.instances.send_keys(id, &["Down"]).await?;
        wait(400).await;
        let scr = self.instances.screen(id).await.unwrap_or_default();
        if !scr.lines().map(screen_plain).any(|l| l.contains("to view tasks")) {
            return Err(changed());
        }
        self.instances.send_keys(id, &["Enter"]).await?;
        wait(700).await;
        let scr = self.instances.screen(id).await.unwrap_or_default();
        if let Some(cmd) = screen::task_details(&scr) {
            if !norm(&cmd).starts_with(&head) {
                return Err(changed());
            }
        } else {
            let (rows, mut sel) = screen::task_list(&scr).ok_or_else(changed)?;
            let target = rows.iter().position(|r| norm(r).starts_with(&head)).ok_or_else(changed)?;
            while sel != target {
                self.instances.send_keys(id, &[if target > sel { "Down" } else { "Up" }]).await?;
                wait(150).await;
                let scr = self.instances.screen(id).await.unwrap_or_default();
                let (_, now) = screen::task_list(&scr).ok_or_else(changed)?;
                if now == sel {
                    return Err(changed());
                }
                sel = now;
            }
        }
        self.instances.send_keys(id, &["x"]).await?;
        wait(500).await;
        Ok(())
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

/// PreToolUse hook 的答复：`allow` 不再走 Claude Code 自己的权限确认，`deny` 时 Claude 会看到理由
fn pre_tool_use(decision: &str, reason: &str) -> String {
    json!({ "hookSpecificOutput": { "hookEventName": "PreToolUse", "permissionDecision": decision, "permissionDecisionReason": reason } }).to_string()
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

/// 输入框里显示的内容是不是送进去的那段（只比开头，不算空白：折行、续行的缩进都会变）
/// `2.1.310` 这样的版本号不低于 `min`（认不出来的当作够新）
pub(crate) fn version_at_least(v: &str, min: (u32, u32, u32)) -> bool {
    let mut it = v.trim().split('.').map(|p| p.chars().take_while(char::is_ascii_digit).collect::<String>().parse::<u32>());
    match (it.next(), it.next(), it.next()) {
        (Some(Ok(a)), Some(Ok(b)), Some(Ok(c))) => (a, b, c) >= min,
        _ => true,
    }
}

fn typed_ok(shown: &str, sent: &str) -> bool {
    let squash = |s: &str| s.chars().filter(|c| !c.is_whitespace()).take(24).collect::<String>();
    let (shown, sent) = (squash(shown), squash(sent));
    sent.is_empty() || shown.starts_with(&sent)
}

/// 工具参数里只留卡片摘要用得上的几个字段
fn slim_input(input: &Value) -> Value {
    let mut out = serde_json::Map::new();
    for k in ["command", "description", "file_path", "notebook_path", "path", "pattern", "url", "query", "prompt", "subagent_type"] {
        if let Some(s) = input[k].as_str() {
            out.insert(k.into(), json!(s.chars().take(200).collect::<String>()));
        }
    }
    Value::Object(out)
}

/// 后台命令的工具结果里写着输出文件：`… Output is being written to: /tmp/claude-0/…/tasks/b95azumrm.output. …`
fn output_path_in(content: &str) -> Option<String> {
    let rest = &content[content.find("written to: ")? + "written to: ".len()..];
    let end = rest.find(".output")? + ".output".len();
    Some(rest[..end].trim().to_string())
}

/// 后台命令的输出文件只能是 `…/tasks/<任务>.output`
fn valid_output_path(p: &Path) -> bool {
    p.is_absolute()
        && p.extension().is_some_and(|e| e == "output")
        && p.parent().and_then(|d| d.file_name()).is_some_and(|n| n == "tasks")
        && !p.components().any(|c| matches!(c, std::path::Component::ParentDir))
}

/// transcript 只能是 `…/projects/<项目>/<session>.jsonl`：防止借伪造的 hook 读任意文件（hook 本身还要令牌）。
fn valid_transcript_path(p: &Path) -> bool {
    p.is_absolute()
        && p.extension().is_some_and(|e| e == "jsonl")
        && p.parent().and_then(|d| d.parent()).and_then(|d| d.file_name()).is_some_and(|n| n == "projects")
        && !p.components().any(|c| matches!(c, std::path::Component::ParentDir))
}

/// 面板里的 AI 工具：没在运行时 None；在运行时 `Some(是不是带着看看的 hooks 启动的)`，看不出来为 `Some(None)`。
///
/// 一般看前台命令是不是 shell 就行；psmux（Windows）报的是最近启动的子进程，Codex 跑命令、跑 hook 时
/// 会报成 `powershell`，所以有进程名的工具（[`Agent::process_names`]）再看面板进程底下有没有它，顺便看它的命令行参数。
fn agent_state(agent: &dyn Agent, pane: &crate::instances::Pane) -> Option<Option<bool>> {
    let names = agent.process_names();
    if names.is_empty() {
        return (!is_shell(&pane.command)).then_some(None);
    }
    match tokio::task::block_in_place(|| find_under(pane.pid, names)) {
        Some(args) => Some(agent.launched_with_hooks(&args)),
        None => (!is_shell(&pane.command)).then_some(None),
    }
}

/// `root` 的子孙里叫这些名字的进程（不分大小写，不带 `.exe`）：返回它的命令行参数
fn find_under(root: u32, names: &[&str]) -> Option<Vec<String>> {
    proc_under(root, names).map(|(args, _)| args)
}

/// 同 [`find_under`]，另外给出进程的启动时间（毫秒）
fn proc_under(root: u32, names: &[&str]) -> Option<(Vec<String>, i64)> {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
    let mut sys = System::new();
    sys.refresh_processes_specifics(ProcessesToUpdate::All, true, ProcessRefreshKind::nothing());
    let root = Pid::from_u32(root);
    let pid = sys.processes().values().find_map(|p| {
        let name = p.name().to_string_lossy().to_ascii_lowercase();
        if !names.contains(&exe_stem(&name)) {
            return None;
        }
        let mut cur = p.parent();
        for _ in 0..32 {
            match cur {
                Some(pid) if pid == root => return Some(p.pid()),
                Some(pid) => cur = sys.process(pid).and_then(|x| x.parent()),
                None => return None,
            }
        }
        None
    })?;
    sys.refresh_processes_specifics(ProcessesToUpdate::Some(&[pid]), false, ProcessRefreshKind::nothing().with_cmd(UpdateKind::Always));
    let p = sys.process(pid)?;
    Some((p.cmd().iter().map(|a| a.to_string_lossy().into_owned()).collect(), p.start_time() as i64 * 1000))
}

/// 终端里以前的那个 Claude Code / Codex 用的是哪段对话（命令行里没有会话号、看看也没记着时）：这个文件夹里
/// 没被别的终端接着的对话中，正在写的、或者在它启动之后还更新过的，取最近的一段。都没有说明它开着还没聊过，返回 None。
async fn guess_conversation(store: &crate::store::Store, id: &str, workdir: &str, agent: &str, started_ms: i64) -> Option<String> {
    let taken: HashSet<String> = store.instances().ok()?.iter().filter(|r| r.id != id).filter_map(|r| binding(store, &r.id)).map(|b| b.session_id).collect();
    let q = crate::history::Query { workdir: Some(workdir.to_string()), agent: Some(agent.to_string()), limit: Some(20), ..Default::default() };
    crate::history::list(q).await.into_iter().filter(|c| !taken.contains(&c.id)).find(|c| c.live || c.updated_ms >= started_ms).map(|c| c.id)
}

/// 一次改动的增删行数：有 Claude Code 给的 structuredPatch 就数它，没有就按参数估
fn diff_stat(name: &str, input: &Value, extra: Option<&Value>) -> (u64, u64) {
    let lines = |s: &str| if s.is_empty() { 0 } else { s.lines().count() as u64 };
    if let Some(hunks) = extra.and_then(|e| e["structuredPatch"].as_array()).filter(|h| !h.is_empty()) {
        let (mut add, mut del) = (0, 0);
        for l in hunks.iter().flat_map(|h| h["lines"].as_array().into_iter().flatten()).filter_map(Value::as_str) {
            if l.starts_with('+') {
                add += 1;
            } else if l.starts_with('-') {
                del += 1;
            }
        }
        return (add, del);
    }
    match name {
        "Write" => (lines(input["content"].as_str().unwrap_or("")), 0),
        "Edit" => (lines(input["new_string"].as_str().unwrap_or("")), lines(input["old_string"].as_str().unwrap_or(""))),
        _ => input["edits"].as_array().into_iter().flatten().fold((0, 0), |(a, d), e| (a + lines(e["new_string"].as_str().unwrap_or("")), d + lines(e["old_string"].as_str().unwrap_or("")))),
    }
}

/// 进程名去掉 `.exe` 及后面的部分。Windows 上 Claude Code 自己更新时，会把正在运行的程序改名成
/// `claude.exe.old.<时间>.<进程号>`（新版本放到 `claude.exe`），这个进程还在跑，名字已经变了；
/// 只去掉结尾的 `.exe` 就认不出它，对话界面会以为 Claude Code 退出了。
fn exe_stem(name: &str) -> &str {
    match name.find(".exe") {
        Some(i) => &name[..i],
        None => name,
    }
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

    mod board_turns {
        use super::super::*;

        fn go(st: &mut State, p: Phase) {
            Chat::enter(st, p);
            st.phase = p;
        }

        fn outcome(st: &State) -> Option<board::Outcome> {
            st.board.turn.as_ref().and_then(|t| t.outcome)
        }

        #[test]
        fn turn_starts_and_ends() {
            let mut st = State { phase: Phase::Idle, ..Default::default() };
            go(&mut st, Phase::Running);
            assert!(st.board.turn.as_ref().unwrap().active());
            // 审批、选择框都还在这一轮里
            go(&mut st, Phase::Approval);
            go(&mut st, Phase::Running);
            go(&mut st, Phase::Tui);
            go(&mut st, Phase::Running);
            assert!(st.board.turn.as_ref().unwrap().active());
            st.end_hint = Some(board::Outcome::Done);
            go(&mut st, Phase::Idle);
            assert_eq!(outcome(&st), Some(board::Outcome::Done));
            assert!(st.board_dirty);
        }

        #[test]
        fn failure_interrupt_and_session_end() {
            let mut st = State { phase: Phase::Idle, ..Default::default() };
            go(&mut st, Phase::Running);
            st.error = Some("overloaded".into());
            go(&mut st, Phase::Idle);
            assert_eq!(outcome(&st), Some(board::Outcome::Failed));
            assert_eq!(st.board.turn.as_ref().unwrap().error.as_deref(), Some("overloaded"));
            st.error = None;
            go(&mut st, Phase::Running);
            st.end_hint = Some(board::Outcome::Interrupted);
            go(&mut st, Phase::Idle);
            assert_eq!(outcome(&st), Some(board::Outcome::Interrupted));
            go(&mut st, Phase::Running);
            go(&mut st, Phase::Detached);
            assert_eq!(outcome(&st), Some(board::Outcome::Interrupted));
        }

        #[test]
        fn startup_dialog_is_not_a_turn_but_unseen_running_is() {
            let mut st = State { phase: Phase::Detached, ..Default::default() };
            go(&mut st, Phase::Tui);
            go(&mut st, Phase::Idle);
            assert!(st.board.turn.is_none());
            // 客户端重启时正在跑（没看到开始）：结束时也算一轮
            let mut st = State { phase: Phase::Running, since: 5, ..Default::default() };
            go(&mut st, Phase::Idle);
            assert_eq!(st.board.turn.as_ref().unwrap().started_at, 5);
            assert_eq!(outcome(&st), Some(board::Outcome::Done));
        }

        #[test]
        fn ended_while_watched_counts_as_seen() {
            let mut st = State { phase: Phase::Idle, active_until: crate::util::system_ms() + 30_000, ..Default::default() };
            go(&mut st, Phase::Running);
            go(&mut st, Phase::Idle);
            assert!(st.board.seen_at >= st.board.turn.as_ref().unwrap().ended_at.unwrap());
        }
    }
    use super::*;

    /// 命令规则和检查点走 hook：不用真的 Claude Code，直接喂事件（docs/PLAN_2026-10-27.md §3）
    #[tokio::test(flavor = "multi_thread")]
    async fn guard_and_checkpoint_hooks() {
        let home = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        let dir = repo.path().canonicalize().unwrap();
        let git = |args: &[&str]| assert!(std::process::Command::new("git").arg("-C").arg(&dir).args(args).env("GIT_AUTHOR_NAME", "t").env("GIT_AUTHOR_EMAIL", "t@t").env("GIT_COMMITTER_NAME", "t").env("GIT_COMMITTER_EMAIL", "t@t").status().unwrap().success());
        if crate::shells::which("git").is_none() {
            return;
        }
        git(&["init", "-q"]);
        std::fs::write(dir.join("a.txt"), "one\n").unwrap();
        let paths = Paths::discover(Some(home.path().to_path_buf())).unwrap();
        let store = Arc::new(Store::memory().unwrap());
        let instances = Instances::new(store.clone(), paths.clone());
        let input = crate::instances::Input { name: Some("guard".into()), workdir: Some(dir.display().to_string()), launch: Some("claude".into()), ..Default::default() };
        let id = instances.create(input).await.unwrap().row.id;
        let chat = Chat::new(store.clone(), paths.clone(), instances.clone());
        let ev = |name: &str, extra: Value| {
            let mut v = json!({ "hook_event_name": name });
            v.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
            v
        };
        let bash = |cmd: &str| ev("PreToolUse", json!({ "tool_name": "Bash", "tool_input": { "command": cmd } }));

        // 每轮开始拍检查点
        assert_eq!(chat.hook(&id, ev("UserPromptSubmit", json!({ "prompt": "改一下 a.txt" }))).await, "");
        let cps = crate::checkpoint::list(&store, &id, &dir).unwrap();
        assert_eq!(cps["items"][0]["label"], "改一下 a.txt", "{cps}");

        // 没命中：立即放行，不输出
        assert_eq!(chat.hook(&id, bash("ls -la")).await, "");
        // 内置规则：挂起等确认，允许后答复 PreToolUse allow
        let c = chat.clone();
        let (i2, b) = (id.clone(), bash("git reset --hard HEAD~1"));
        let waiting = tokio::spawn(async move { c.hook(&i2, b).await });
        let mut pending = Value::Null;
        for _ in 0..40 {
            pending = chat.snapshot(&id).await.unwrap()["status"]["pending"].clone();
            if pending.is_object() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert_eq!(pending["guard"], "git reset --hard", "{pending}");
        assert_eq!(chat.brief(&id).unwrap()["attn"]["guard"], "git reset --hard");
        let pid = pending["id"].as_u64().unwrap();
        // 不能“以后都允许”
        let bad = PermissionAnswer { id: pid, decision: "allow_session".into(), message: None, answers: None };
        assert!(chat.answer(&id, &bad).is_err());
        chat.answer(&id, &PermissionAnswer { id: pid, decision: "allow".into(), message: None, answers: None }).unwrap();
        let out: Value = serde_json::from_str(&waiting.await.unwrap()).unwrap();
        assert_eq!(out["hookSpecificOutput"]["permissionDecision"], "allow");
        // 同时跑的另一条命令结束不会放掉等待中的确认；一轮结束（Stop）没人确认就拒绝
        let c = chat.clone();
        let (i2, b) = (id.clone(), bash("rm -rf ~"));
        let waiting = tokio::spawn(async move { c.hook(&i2, b).await });
        tokio::time::sleep(Duration::from_millis(200)).await;
        chat.hook(&id, ev("PostToolUse", json!({ "tool_name": "Bash", "tool_input": { "command": "ls" } }))).await;
        assert!(chat.snapshot(&id).await.unwrap()["status"]["pending"].is_object());
        chat.hook(&id, ev("Stop", json!({}))).await;
        let out: Value = serde_json::from_str(&waiting.await.unwrap()).unwrap();
        assert_eq!(out["hookSpecificOutput"]["permissionDecision"], "deny");

        // 用户的拒绝规则：直接拒绝，带理由
        let g = crate::guard::Guard { rules: vec![crate::guard::Rule { pattern: "npm publish".into(), action: crate::guard::Action::Deny }], ..Default::default() };
        crate::settings_sync::write_local(&store, 1, "guard", &[("config", Some(serde_json::to_value(&g).unwrap()))]).unwrap();
        let out: Value = serde_json::from_str(&chat.hook(&id, bash("npm publish --tag next")).await).unwrap();
        assert_eq!(out["hookSpecificOutput"]["permissionDecision"], "deny");
        assert!(out["hookSpecificOutput"]["permissionDecisionReason"].as_str().unwrap().contains("npm publish"));
    }

    #[test]
    fn exe_stem_sees_claude_after_self_update() {
        assert_eq!(exe_stem("claude.exe"), "claude");
        assert_eq!(exe_stem("claude.exe.old.1792799554136.91188"), "claude");
        assert_eq!(exe_stem("claude"), "claude");
        assert_eq!(exe_stem("codex.exe"), "codex");
    }

    #[test]
    fn send_now_needs_2_1_275() {
        assert!(version_at_least("2.1.310", SEND_NOW_SINCE));
        assert!(version_at_least("2.1.275", SEND_NOW_SINCE));
        assert!(!version_at_least("2.1.274", SEND_NOW_SINCE));
        assert!(!version_at_least("2.0.99", SEND_NOW_SINCE));
        assert!(version_at_least("3.0.0-beta", SEND_NOW_SINCE));
        assert!(version_at_least("weird", SEND_NOW_SINCE));
    }

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
    fn typed_text_check() {
        assert!(typed_ok("Use the Bash tool with run_in_background", "Use the Bash tool with run_in_background true"));
        assert!(typed_ok("第一行\n第二行", "第一行\n第二行"));
        // 开头被吞掉了
        assert!(!typed_ok("done; sleep 600 . Do not wait", "Use the Bash tool … done; sleep 600 . Do not wait"));
        assert!(!typed_ok("", "hello"));
    }

    #[test]
    fn background_output_paths() {
        let content = "Command running in background with ID: b95azumrm. Output is being written to: /tmp/claude-0/-tmp-x/cb08/tasks/b95azumrm.output. You will be notified when it completes";
        let p = output_path_in(content).unwrap();
        assert_eq!(p, "/tmp/claude-0/-tmp-x/cb08/tasks/b95azumrm.output");
        assert!(valid_output_path(Path::new(&p)));
        assert!(!valid_output_path(Path::new("/etc/passwd")));
        assert!(!valid_output_path(Path::new("/tmp/tasks/../../etc/x.output")));
        assert!(!valid_output_path(Path::new("/tmp/other/x.output")));
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

    /// 端到端：空的配置目录、没信任过的文件夹、全部权限首次启动：启动前已按官方文档写成都确认过了
    /// （claude_profiles::skip_first_run），选主题、安全提示、信任目录、Bypass 警告都不出现，直接到输入框。不发消息，不用额度。
    /// `CHAT_E2E_DIR` 可以是普通文件夹、git 仓库里的子目录，或者家目录（同时把 `HOME` 设成它）：都是写工作目录本身。
    /// `CHAT_E2E_BIN=… CHAT_E2E_DIR=… cargo test chat_e2e_onboarding -- --ignored --nocapture`
    #[tokio::test(flavor = "multi_thread")]
    #[ignore]
    async fn chat_e2e_onboarding() {
        let bin = std::env::var("CHAT_E2E_BIN").expect("CHAT_E2E_BIN");
        let dir = std::env::var("CHAT_E2E_DIR").expect("CHAT_E2E_DIR");
        std::env::set_var("PATH", format!("{bin}:{}", std::env::var("PATH").unwrap_or_default()));
        // tmux 的服务器在测试里是各自的（Instances::tmux_socket_args）；在 tmux 里跑测试时别带着外面的 TMUX
        std::env::remove_var("TMUX");
        // 空的 Claude Code 配置，只借用登录（链接，刷新令牌时写回同一个文件）
        let cfg = tempfile::tempdir().unwrap();
        let creds = dirs::home_dir().unwrap().join(".claude/.credentials.json");
        std::os::unix::fs::symlink(&creds, cfg.path().join(".credentials.json")).unwrap();
        std::env::set_var("CLAUDE_CONFIG_DIR", cfg.path());
        let exe = std::env::current_exe().unwrap().parent().unwrap().parent().unwrap().join("looklook");
        assert!(exe.is_file(), "先 cargo build");
        std::env::set_var("LOOKLOOK_HOOK_EXE", exe);
        let home = tempfile::tempdir().unwrap();
        let paths = Paths::discover(Some(home.path().to_path_buf())).unwrap();
        let store = Arc::new(Store::memory().unwrap());
        let instances = Instances::new(store.clone(), paths.clone());
        let input = crate::instances::Input { name: Some("e2e-onboarding".into()), workdir: Some(dir.clone()), launch: Some("claude".into()), ..Default::default() };
        let id = instances.create(input).await.unwrap().row.id;
        let chat = Chat::new(store.clone(), paths.clone(), instances.clone());
        let row = store.instance(&id).unwrap().unwrap();
        instances.ensure_session(&row).await.unwrap();
        let (_rx, viewer) = chat.subscribe(&id).unwrap();
        let mut seen = Vec::new();
        for _ in 0..100 {
            tokio::time::sleep(Duration::from_millis(300)).await;
            // 启动时屏幕先空着几秒：界面上要一直是“正在启动”，不能变成“在等你选择”
            let s = chat.snapshot(&id).await.unwrap();
            assert_ne!(s["status"]["phase"], "tui", "启动中不该当成选择框：{}", s["status"]);
            let raw = instances.screen(&id).await.unwrap_or_default();
            let cap = raw.lines().map(|l| screen::strip(l).0).collect::<Vec<_>>().join("\n");
            for (k, s) in [("theme", "Choose the text style"), ("security", "Security notes:"), ("trust", "trust this folder"), ("bypass", "Yes, I accept")] {
                if cap.contains(s) && !seen.contains(&k) {
                    seen.push(k);
                }
            }
            if matches!(screen::classify(&raw), Screen::Input { .. }) {
                break;
            }
        }
        println!("---- 屏幕\n{}", instances.screen(&id).await.unwrap_or_default().lines().map(|l| screen::strip(l).0).filter(|l| !l.trim().is_empty()).collect::<Vec<_>>().join("\n"));
        assert!(seen.is_empty(), "不该出现确认：{seen:?}");
        assert!(matches!(screen::classify(&instances.screen(&id).await.unwrap()), Screen::Input { .. }), "到了输入框");
        let state: Value = serde_json::from_slice(&std::fs::read(cfg.path().join(".claude.json")).unwrap()).unwrap();
        assert_eq!(state["hasCompletedOnboarding"], true);
        let key = std::fs::canonicalize(&dir).unwrap().display().to_string();
        assert_eq!(state["projects"][&key]["hasTrustDialogAccepted"], true, "{}", state["projects"]);
        assert_eq!(state["theme"], "auto");
        // 看看不写用户设置（Bypass 警告靠 `--settings` 传的 skipDangerousModePermissionPrompt）；
        // 用户设置里没选过主题时，Claude Code 启动时自己把 `.claude.json` 里的 `theme` 迁过去
        let settings: Value = std::fs::read(cfg.path().join("settings.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        assert!(settings.get("skipDangerousModePermissionPrompt").is_none(), "{settings}");
        assert!(settings.get("theme").is_none_or(|t| t == "auto"), "{settings}");
        // 界面开着时重启这个终端：新的 Claude Code 启动时屏幕也是先空着，同样要一直是“正在启动”
        instances.stop(&id).await.unwrap();
        instances.ensure_session(&row).await.unwrap();
        let mut back = false;
        for _ in 0..100 {
            tokio::time::sleep(Duration::from_millis(300)).await;
            let s = chat.snapshot(&id).await.unwrap();
            assert_ne!(s["status"]["phase"], "tui", "重启后启动中不该当成选择框：{}", s["status"]);
            if matches!(screen::classify(&instances.screen(&id).await.unwrap_or_default()), Screen::Input { .. }) {
                back = true;
                break;
            }
        }
        assert!(back, "重启后到了输入框");
        drop(viewer);
        instances.stop(&id).await.unwrap();
    }

    /// 端到端：运行中排队、立即发送（Claude Code 的 Ctrl+X Ctrl+S）。用 haiku（项目里 `.claude/settings.local.json` 定模型）：
    /// `CHAT_E2E_BIN=… CHAT_E2E_DIR=… cargo test chat_e2e_send_now -- --ignored --nocapture`
    #[tokio::test(flavor = "multi_thread")]
    #[ignore]
    async fn chat_e2e_send_now() {
        let bin = std::env::var("CHAT_E2E_BIN").expect("CHAT_E2E_BIN");
        let dir = std::env::var("CHAT_E2E_DIR").expect("CHAT_E2E_DIR");
        std::env::set_var("PATH", format!("{bin}:{}", std::env::var("PATH").unwrap_or_default()));
        std::env::set_var("ANTHROPIC_MODEL", "haiku");
        std::env::remove_var("TMUX");
        std::fs::create_dir_all(format!("{dir}/.claude")).unwrap();
        std::fs::write(format!("{dir}/.claude/settings.local.json"), r#"{"model":"haiku","permissions":{"allow":["Bash"]}}"#).unwrap();
        let exe = std::env::current_exe().unwrap().parent().unwrap().parent().unwrap().join("looklook");
        assert!(exe.is_file(), "先 cargo build");
        std::env::set_var("LOOKLOOK_HOOK_EXE", exe);
        let home = tempfile::tempdir().unwrap();
        let paths = Paths::discover(Some(home.path().to_path_buf())).unwrap();
        let store = Arc::new(Store::memory().unwrap());
        let instances = Instances::new(store.clone(), paths.clone());
        let input = crate::instances::Input { name: Some("e2e-now".into()), workdir: Some(dir.clone()), launch: Some("claude".into()), ..Default::default() };
        let id = instances.create(input).await.unwrap().row.id;
        let chat = Chat::new(store.clone(), paths.clone(), instances.clone());
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
        let texts = |s: &Value| -> String {
            s["items"].as_array().unwrap().iter().filter(|i| i["k"] == "assistant").map(|i| i.to_string()).collect::<Vec<_>>().join("\n")
        };
        // 启动时的确认已经在启动前跳过（claude_profiles::skip_first_run）；万一还有信任目录，替用户点
        for _ in 0..60 {
            let s = chat.snapshot(&id).await.unwrap();
            if s["status"]["session_id"].is_string() && s["status"]["phase"] == "idle" {
                break;
            }
            let cap = instances.screen(&id).await.unwrap_or_default();
            let cap = cap.lines().map(|l| screen::strip(l).0).collect::<Vec<_>>().join("\n");
            if cap.contains("trust this folder") {
                instances.send_keys(&id, &["Down"]).await.unwrap();
                tokio::time::sleep(Duration::from_millis(800)).await;
                instances.send_keys(&id, &["Enter"]).await.unwrap();
            } else if cap.contains("Enter to confirm") {
                instances.send_keys(&id, &["Enter"]).await.unwrap();
            } else {
                println!("---- 屏幕\n{}", cap.lines().filter(|l| !l.trim().is_empty()).collect::<Vec<_>>().join("\n"));
            }
            tokio::time::sleep(Duration::from_millis(1500)).await;
        }
        let s = wait(&chat, &id, "SessionStart", 30, |s| s["status"]["session_id"].is_string() && s["status"]["phase"] == "idle").await;
        println!("已接上：{}", s["status"]["version"]);

        // 1. 跑一条 40 秒的前台命令，中途“立即发送”：命令转到后台接着跑，Claude 在这一轮里马上回答
        let cmd = "用 Bash 在前台执行这条命令（不要放后台）：for i in $(seq 1 40); do echo tick $i; sleep 1; done 。执行完只回复 done";
        assert_eq!(chat.input(&id, cmd, &[]).await.unwrap()["delivered"], true);
        let s = wait(&chat, &id, "开始跑命令", 60, |s| s["status"]["tool"] == "Bash" && s["status"]["actual_model"].is_string()).await;
        let model = s["status"]["actual_model"].as_str().unwrap_or("").to_string();
        assert!(model.contains("haiku"), "要用 haiku 测：{model}");
        tokio::time::sleep(Duration::from_secs(3)).await;
        let t0 = Instant::now();
        let r = chat.send_now(&id, "先别管那条命令，现在马上告诉我 7+8 等于几，只回复数字", &[]).await.unwrap();
        println!("立即发送：{r}，{:?}", t0.elapsed());
        assert_eq!(r["delivered"], true, "{r}");
        let s = wait(&chat, &id, "马上回答 15", 25, |s| texts(s).contains("15")).await;
        let took = t0.elapsed();
        println!("{took:?} 后回答了 15");
        assert!(took < Duration::from_secs(30), "没等那条 40 秒的命令跑完");
        assert!(s["items"].as_array().unwrap().iter().any(|i| i["k"] == "user" && i["mid"] == true && i["text"].as_str().is_some_and(|t| t.contains("7+8"))), "并进了这一轮");
        let moved = s["items"].as_array().unwrap().iter().any(|i| i["k"] == "tool_result" && i.to_string().contains("background"));
        println!("命令转到后台：{moved}");
        assert!(moved);
        wait(&chat, &id, "这一轮结束", 90, |s| s["status"]["phase"] == "idle").await;

        // 2. 先排队（回车），再空着输入框点“立即发送”：排着的那条马上发出
        assert_eq!(chat.input(&id, cmd, &[]).await.unwrap()["delivered"], true);
        wait(&chat, &id, "又开始跑命令", 60, |s| s["status"]["tool"] == "Bash").await;
        tokio::time::sleep(Duration::from_secs(3)).await;
        let r = chat.input(&id, "顺便告诉我 2+3 等于几，只回复数字", &[]).await.unwrap();
        assert_eq!(r["delivered"], true, "{r}");
        // 运行中回车是排队：在 Claude Code 的队列里等这一步做完（命令要跑 40 秒）
        wait(&chat, &id, "进了队列", 10, |s| s["status"]["queued"].as_array().is_some_and(|q| q.iter().any(|t| t.as_str().is_some_and(|t| t.contains("2+3"))))).await;
        tokio::time::sleep(Duration::from_secs(2)).await;
        assert_eq!(chat.snapshot(&id).await.unwrap()["status"]["phase"], "running", "排队时不打断这一轮");
        let t0 = Instant::now();
        let r = chat.send_now(&id, "", &[]).await.unwrap();
        assert_eq!(r["delivered"], true, "{r}");
        wait(&chat, &id, "马上回答 5", 25, |s| {
            let a = s["items"].as_array().unwrap();
            let at = a.iter().rposition(|i| i["k"] == "user" && i["text"].as_str().is_some_and(|t| t.contains("2+3")));
            at.is_some_and(|n| a[n..].iter().any(|i| i["k"] == "assistant" && i.to_string().contains('5')))
        })
        .await;
        println!("{:?} 后回答了 5", t0.elapsed());
        assert!(t0.elapsed() < Duration::from_secs(30));
        wait(&chat, &id, "这一轮结束", 90, |s| s["status"]["phase"] == "idle").await;

        drop(viewer);
        instances.stop(&id).await.unwrap();
    }

    /// 端到端：/new（Claude Code 里是 /clear 的别名）之后对话界面回到空的首页：换成新会话、条目清空、空闲（不是“已结束”）。
    /// `CHAT_E2E_BIN=… CHAT_E2E_DIR=… cargo test chat_e2e_new -- --ignored --nocapture`
    #[tokio::test(flavor = "multi_thread")]
    #[ignore]
    async fn chat_e2e_new() {
        let bin = std::env::var("CHAT_E2E_BIN").expect("CHAT_E2E_BIN");
        let dir = std::env::var("CHAT_E2E_DIR").expect("CHAT_E2E_DIR");
        std::env::set_var("PATH", format!("{bin}:{}", std::env::var("PATH").unwrap_or_default()));
        std::env::set_var("ANTHROPIC_MODEL", "haiku");
        std::env::remove_var("TMUX");
        std::fs::create_dir_all(format!("{dir}/.claude")).unwrap();
        std::fs::write(format!("{dir}/.claude/settings.local.json"), r#"{"model":"haiku"}"#).unwrap();
        let exe = std::env::current_exe().unwrap().parent().unwrap().parent().unwrap().join("looklook");
        assert!(exe.is_file(), "先 cargo build");
        std::env::set_var("LOOKLOOK_HOOK_EXE", exe);
        let home = tempfile::tempdir().unwrap();
        let paths = Paths::discover(Some(home.path().to_path_buf())).unwrap();
        let store = Arc::new(Store::memory().unwrap());
        let instances = Instances::new(store.clone(), paths.clone());
        let input = crate::instances::Input { name: Some("e2e-new".into()), workdir: Some(dir.clone()), launch: Some("claude".into()), ..Default::default() };
        let id = instances.create(input).await.unwrap().row.id;
        let chat = Chat::new(store.clone(), paths.clone(), instances.clone());
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
                    let ev: Value = serde_json::from_slice(&body).unwrap_or_default();
                    println!("hook {} {} {}", ev["hook_event_name"], ev["source"], ev["reason"]);
                    chat.hook(&h(hooks::INSTANCE_HEADER), ev).await
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
        let s = wait(&chat, &id, "SessionStart", 60, |s| s["status"]["session_id"].is_string() && s["status"]["phase"] == "idle").await;
        let first = s["status"]["session_id"].clone();
        assert_eq!(chat.input(&id, "只回复两个字母 ok", &[]).await.unwrap()["delivered"], true);
        wait(&chat, &id, "回复", 60, |s| s["status"]["phase"] == "idle" && s["items"].as_array().is_some_and(|a| a.iter().any(|i| i["k"] == "assistant"))).await;
        chat.input(&id, "/new", &[]).await.unwrap();
        let s = wait(&chat, &id, "换成新会话", 30, |s| s["status"]["session_id"].is_string() && s["status"]["session_id"] != first).await;
        // 两个 hook 都到齐再看最后的状态
        tokio::time::sleep(Duration::from_secs(3)).await;
        let s2 = chat.snapshot(&id).await.unwrap();
        println!("换会话时：{}\n3 秒后：{} 条目 {}", s["status"], s2["status"], s2["items"]);
        assert_eq!(s2["status"]["phase"], "idle", "{}", s2["status"]);
        assert!(s2["items"].as_array().unwrap().is_empty(), "{}", s2["items"]);
        drop(viewer);
        instances.stop(&id).await.unwrap();
    }

    /// 端到端：快捷设置（权限模式、思考强度、模型）、后台命令（列表、输出、停止）、子代理列表。
    /// 和 chat_e2e 一样要登录好的 claude：`CHAT_E2E_BIN=… CHAT_E2E_DIR=… cargo test chat_e2e_tasks -- --ignored --nocapture`
    #[tokio::test(flavor = "multi_thread")]
    #[ignore]
    async fn chat_e2e_tasks() {
        let bin = std::env::var("CHAT_E2E_BIN").expect("CHAT_E2E_BIN");
        let dir = std::env::var("CHAT_E2E_DIR").expect("CHAT_E2E_DIR");
        std::env::set_var("PATH", format!("{bin}:{}", std::env::var("PATH").unwrap_or_default()));
        std::env::set_var("LLV_CLAUDE_ARGS", "--permission-mode default");
        let exe = std::env::current_exe().unwrap().parent().unwrap().parent().unwrap().join("looklook");
        assert!(exe.is_file(), "先 cargo build");
        std::env::set_var("LOOKLOOK_HOOK_EXE", exe);
        let home = tempfile::tempdir().unwrap();
        let paths = Paths::discover(Some(home.path().to_path_buf())).unwrap();
        let store = Arc::new(Store::memory().unwrap());
        let instances = Instances::new(store.clone(), paths.clone());
        let input = crate::instances::Input { name: Some("e2e-tasks".into()), workdir: Some(dir.clone()), launch: Some("claude".into()), ..Default::default() };
        let id = instances.create(input).await.unwrap().row.id;
        let chat = Chat::new(store.clone(), paths.clone(), instances.clone());
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
        async fn approve_until(chat: &Arc<Chat>, id: &str, what: &str, secs: u64, f: impl Fn(&Value) -> bool) -> Value {
            for _ in 0..secs * 4 {
                let s = chat.snapshot(id).await.unwrap();
                if let Some(p) = s["status"]["pending"]["id"].as_u64() {
                    let _ = chat.answer(id, &PermissionAnswer { id: p, decision: "allow".into(), message: None, answers: None });
                }
                if f(&s) {
                    return s;
                }
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
            panic!("等不到：{what}；最后的状态 {}", chat.snapshot(id).await.unwrap()["status"]);
        }
        wait(&chat, &id, "SessionStart", 40, |s| s["status"]["session_id"].is_string() && s["status"]["phase"] == "idle").await;
        // 状态栏读出来的模式
        wait(&chat, &id, "状态栏上的模式", 10, |s| s["status"]["mode"] == "default").await;

        // 权限模式：Shift+Tab 轮换到目标
        chat.setting(&id, "mode", "plan").await.unwrap();
        assert_eq!(chat.snapshot(&id).await.unwrap()["status"]["mode"], "plan");
        chat.setting(&id, "mode", "default").await.unwrap();
        assert_eq!(chat.snapshot(&id).await.unwrap()["status"]["mode"], "default");
        assert_eq!(chat.setting(&id, "mode", "nope").await.unwrap_err().code, "VALIDATION_FAILED");
        // 思考强度
        chat.setting(&id, "effort", "low").await.unwrap();
        wait(&chat, &id, "强度 low", 10, |s| s["status"]["effort"] == "low").await;
        assert_eq!(chat.snapshot(&id).await.unwrap()["status"]["phase"], "idle");

        // 后台命令：状态栏上的个数、列表、输出、停止
        chat.input(&id, "Use the Bash tool with run_in_background true to run exactly this command: for i in 1 2 3 4 5 6 7 8 9; do echo tick$i; sleep 1; done; sleep 600 . Do not wait for it. Then reply only: started", &[]).await.unwrap();
        let s = approve_until(&chat, &id, "状态栏上的后台命令", 90, |s| s["status"]["phase"] == "idle" && s["status"]["background"].as_array().is_some_and(|b| b.iter().any(|x| x["kind"] == "shell"))).await;
        println!("后台：{}", s["status"]["background"]);
        let tasks = chat.tasks(&id).await.unwrap();
        let shell = tasks["shells"].as_array().unwrap().iter().find(|x| x["state"] == "running").cloned().unwrap_or_else(|| panic!("{tasks}"));
        let tool = shell["tool_use_id"].as_str().unwrap().to_string();
        tokio::time::sleep(Duration::from_secs(3)).await;
        let out = chat.task_output(&id, &tool).await.unwrap();
        assert!(out["text"].as_str().unwrap().contains("tick1"), "{out}");
        chat.stop_task(&id, &tool).await.unwrap();
        let s = wait(&chat, &id, "停止后", 15, |s| !s["status"]["background"].as_array().unwrap().iter().any(|x| x["kind"] == "shell")).await;
        assert_eq!(s["status"]["phase"], "idle");
        let tasks = chat.tasks(&id).await.unwrap();
        let state = tasks["shells"].as_array().unwrap().iter().find(|x| x["tool_use_id"] == tool.as_str()).unwrap()["state"].clone();
        assert!(state == "killed" || state == "ended", "{state}");

        // 子代理：列表里有它，结束后是 done
        chat.input(&id, "Use the Agent tool (subagent_type general-purpose, description \"List py files\") to have a subagent run the Glob tool for *.py and report. Do not use Glob yourself. Then reply only: ok", &[]).await.unwrap();
        approve_until(&chat, &id, "子代理做完", 120, |s| s["status"]["phase"] == "idle" && s["items"].as_array().unwrap().iter().any(|i| i["k"] == "assistant" && i.to_string().contains("\"Agent\""))).await;
        let mut agents = json!([]);
        for _ in 0..80 {
            agents = chat.tasks(&id).await.unwrap()["agents"].clone();
            if agents.as_array().unwrap().iter().any(|a| a["state"] == "done") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        let a = agents.as_array().unwrap().first().cloned().unwrap_or_else(|| panic!("没有子代理"));
        println!("子代理：{a}");
        assert_eq!(a["state"], "done");
        assert!(a["steps"].as_u64().unwrap() >= 1);

        // 模型：/model sonnet（对话有缓存时会先问要不要切换，自动确认）。子代理的结束通知会让主会话再开始一轮，等它做完
        wait(&chat, &id, "空闲", 60, |s| s["status"]["phase"] == "idle").await;
        tokio::time::sleep(Duration::from_secs(2)).await;
        wait(&chat, &id, "空闲", 60, |s| s["status"]["phase"] == "idle").await;
        chat.setting(&id, "model", "sonnet").await.unwrap();
        let s = wait(&chat, &id, "切换模型后回到空闲", 15, |s| s["status"]["phase"] == "idle").await;
        assert_eq!(s["status"]["model"], "sonnet");
        tokio::time::sleep(Duration::from_secs(2)).await;
        let s = chat.snapshot(&id).await.unwrap();
        assert!(s.to_string().contains("Set model to"), "transcript 里有切换的结果");

        drop(viewer);
        instances.stop(&id).await.unwrap();
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

    /// 端到端：真的 Codex app-server。要登录好的 codex，并消耗少量额度，默认不跑：
    /// `CHAT_E2E_DIR=<项目目录> [CHAT_E2E_CODEX_MODEL=<便宜的模型>] cargo test chat_e2e_codex -- --ignored --nocapture`
    /// 看的是：用户发的图片能取出来、运行中的命令、插话先显示“排队中”再变成“中途补充”、压缩。
    #[tokio::test(flavor = "multi_thread")]
    #[ignore]
    async fn chat_e2e_codex() {
        use std::sync::Arc;
        let dir = std::env::var("CHAT_E2E_DIR").expect("CHAT_E2E_DIR");
        let home = tempfile::tempdir().unwrap();
        let paths = Paths::discover(Some(home.path().to_path_buf())).unwrap();
        let store = Arc::new(Store::memory().unwrap());
        let instances = Instances::new(store.clone(), paths.clone());
        let input = crate::instances::Input { name: Some("e2e".into()), workdir: Some(dir.clone()), launch: Some("codex".into()), full_access: Some(true), chat_ui: Some(true), ..Default::default() };
        let id = instances.create(input).await.unwrap().row.id;
        let chat = Chat::new(store.clone(), paths.clone(), instances.clone());
        let row = store.instance(&id).unwrap().unwrap();
        instances.ensure_session(&row).await.unwrap();
        let (_rx, viewer) = chat.subscribe(&id).unwrap();
        if let Ok(m) = std::env::var("CHAT_E2E_CODEX_MODEL") {
            chat.setting(&id, "model", &m).await.unwrap();
            chat.setting(&id, "effort", "low").await.unwrap();
        }

        async fn wait(chat: &Arc<Chat>, id: &str, what: &str, secs: u64, mut f: impl FnMut(&Value) -> bool) -> Value {
            for _ in 0..secs * 10 {
                let s = chat.snapshot(id).await.unwrap();
                if f(&s) {
                    return s;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            panic!("等不到：{what}；最后的状态 {}", chat.snapshot(id).await.unwrap()["status"]);
        }
        let users = |s: &Value| s["items"].as_array().unwrap().iter().filter(|i| i["k"] == "user").cloned().collect::<Vec<_>>();

        // 发一张图：记录里的用户消息带一张，按 key 取得出来
        let att = paths.attach_dir(&id);
        std::fs::create_dir_all(&att).unwrap();
        let png = att.join("red.png");
        std::fs::write(&png, base64::Engine::decode(&base64::engine::general_purpose::STANDARD, "iVBORw0KGgoAAAANSUhEUgAAABAAAAAQCAIAAACQkWg2AAAAF0lEQVR4nGP4z8BAEiJN9aiGUQ1DSgMAkPn/Afnh+ngAAAAASUVORK5CYII=").unwrap()).unwrap();
        let r = chat.input(&id, "What colour is this image? Answer in one word.", &[png.to_string_lossy().into_owned()]).await.unwrap();
        assert_eq!(r["delivered"], true, "{r}");
        let s = wait(&chat, &id, "看图回复", 120, |s| s["status"]["phase"] == "idle" && users(s).first().is_some_and(|u| u["images"] == 1)).await;
        let key = users(&s)[0]["key"].as_str().unwrap().to_string();
        let (mime, bytes) = chat.user_image(&id, &key, 0).await.unwrap();
        assert_eq!(mime, "image/png");
        assert!(bytes.starts_with(b"\x89PNG"), "取出来的是那张图");
        println!("图片 ok：{key}");

        // 运行中的命令（去掉 bash -lc 外壳）；这时插话先显示在排队里
        chat.input(&id, "Run the shell command `sleep 12 && echo done`, then reply with one word.", &[]).await.unwrap();
        let s = wait(&chat, &id, "命令在跑", 120, |s| s["status"]["tool"] == "Bash").await;
        println!("运行中：{}", s["status"]["activity"]);
        assert!(s["status"]["activity"].as_str().is_some_and(|a| a.contains("sleep 12") && !a.contains("-lc")), "{}", s["status"]);
        chat.input(&id, "Also include the word banana.", &[]).await.unwrap();
        // 命令这一步已经做完时 Codex 马上就读到了，看不到排队
        let s = chat.snapshot(&id).await.unwrap();
        let read = users(&s).iter().any(|u| u["text"] == "Also include the word banana.");
        println!("插话后：queued {}，已进对话 {read}", s["status"]["queued"]);
        assert!(read != (s["status"]["queued"] == json!(["Also include the word banana."])), "{}", s["status"]);
        let s = wait(&chat, &id, "插话进了对话", 120, |s| {
            let read = users(s).iter().any(|u| u["text"] == "Also include the word banana.");
            assert!(!(read && s["status"]["queued"] != json!([])), "同时显示在排队和对话里：{}", s["status"]);
            read
        })
        .await;
        assert_eq!(s["status"]["queued"], json!([]), "进了对话就不再排队");
        let u = users(&s).into_iter().find(|u| u["text"] == "Also include the word banana.").unwrap();
        assert_eq!(u["mid"], true, "{u}");
        wait(&chat, &id, "这一轮结束", 120, |s| s["status"]["phase"] == "idle").await;

        // 子代理：一张 Agent 卡片，结果是它交回的答复；步骤从它自己的 rollout 取得到，做完了 done
        chat.input(&id, "Spawn ONE sub-agent (spawn_agent) whose task is: reply with the number 4. Wait for it, then reply with its answer.", &[]).await.unwrap();
        let s = wait(&chat, &id, "子代理做完", 180, |s| {
            s["status"]["phase"] == "idle" && s["items"].as_array().unwrap().iter().any(|i| i["k"] == "assistant" && i["blocks"].as_array().is_some_and(|b| b.iter().any(|b| b["name"] == "Agent")))
        })
        .await;
        let card = s["items"].as_array().unwrap().iter().flat_map(|i| i["blocks"].as_array().cloned().unwrap_or_default()).find(|b| b["name"] == "Agent").unwrap();
        let tool = card["id"].as_str().unwrap().to_string();
        let result = s["items"].as_array().unwrap().iter().find(|i| i["k"] == "tool_result" && i["tool_use_id"] == tool.as_str()).cloned();
        println!("子代理卡片：{card}；结果：{:?}", result.as_ref().map(|r| &r["content"]));
        assert!(result.is_some_and(|r| r["content"].as_str().is_some_and(|c| c.contains('4'))));
        let steps = chat.agent(&id, None, Some(&tool)).await.unwrap();
        assert_eq!(steps["done"], true, "{steps}");
        assert!(steps["items"].as_array().unwrap().iter().any(|i| i["k"] == "assistant"), "{steps}");

        // 压缩：运行中显示“正在压缩”，做完有分隔线
        chat.input(&id, "/compact", &[]).await.unwrap();
        let mut seen = false;
        let s = wait(&chat, &id, "压缩完成", 180, |s| {
            if s["status"]["activity"] == "compacting" {
                seen = true;
            }
            s["status"]["phase"] == "idle" && s["items"].as_array().unwrap().iter().any(|i| i["k"] == "divider")
        })
        .await;
        println!("压缩中看到 compacting：{seen}；条目 {}", s["items"].as_array().unwrap().len());
        drop(viewer);
        instances.stop(&id).await.ok();
        // app-server 是单独的进程组（看看重启时留着接着用），测试完要自己结束
        let server = chat.codex.server.lock().await.take();
        if let Some(server) = server {
            server.kill();
        }
    }
}
