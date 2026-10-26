//! 对话模式的 Codex 走 `codex app-server`（docs/CODEX_UI.md）：不再在终端里跑 TUI、不用 hooks。
//!
//! - 每个对话模式的 Codex 终端对应 app-server 里的一个 thread（会话号记在 [`Binding`] 里，和 Claude Code 一样）；
//!   终端标签里是同一个工作目录下的普通 shell。
//! - 对话内容照旧读 rollout（`codex_rollout.rs`）：app-server 写的和 TUI 写的是同一种文件，以前的对话也能接着看。
//! - 状态、正在输出的回复、审批、提问、上下文和 token 用量来自 app-server 的通知和请求。
//! - 发消息是 `turn/start`（这一轮还在跑时是 `turn/steer`）；停止是 `turn/interrupt`；模型和思考强度随下一轮带上。
//! - 开启对话界面之前就在终端里跑着 Codex TUI 的（旧版本留下的）：找出它在用的会话，关掉 TUI，换成 app-server 接着。

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tokio::sync::{mpsc, oneshot};

use super::codex_app::{self, AppServer, Incoming};
use super::{lock, Binding, Chat, Inst, Pending, Phase, PermissionAnswer};
use crate::error::LocalError;

/// 正在输出的回复最多推这么多字（只留最后这段）
const PREVIEW_LIMIT: usize = 4000;
/// 正在输出时最多每隔这么久推一次状态
const PREVIEW_GAP: Duration = Duration::from_millis(120);
/// 模型列表、账号用量的缓存
const MODELS_TTL: Duration = Duration::from_secs(300);
const LIMITS_TTL: Duration = Duration::from_secs(30);
/// 以前的 Codex 会话等它空闲转到对话模式：多久看一次、最多等多久
const ADOPT_POLL: Duration = Duration::from_secs(5);
const ADOPT_WAIT: Duration = Duration::from_secs(30 * 60);
/// TUI 不带会话号启动后，多久之内它开的对话算这个终端的
const TUI_CLAIM: Duration = Duration::from_secs(120);

/// 这个终端的对话已经有记录文件（聊过至少一轮）：TUI 能接、app-server 能 resume
fn has_rollout(st: &super::State) -> bool {
    st.binding.as_ref().is_some_and(|b| b.transcript_path.is_file())
}
/// 界面上能选的思考强度（app-server 的 `ReasoningEffort`）
pub const EFFORTS: [&str; 8] = ["none", "minimal", "low", "medium", "high", "xhigh", "max", "ultra"];

#[derive(Default)]
pub struct Hub {
    pub(super) server: tokio::sync::Mutex<Option<Arc<AppServer>>>,
    /// 第几个 app-server 进程（0 表示还没起过）；thread 只在加载它的那个进程里有效
    generation: AtomicU64,
    /// thread id → 终端 id
    threads: Mutex<HashMap<String, String>>,
    /// 文件改动的审批里要显示改了什么：`item/started` 的 fileChange 先到，按 item id 记下
    patches: Mutex<HashMap<String, Value>>,
    models: Mutex<Option<(Instant, Value)>>,
    limits: Mutex<Option<(Instant, Value)>>,
    /// 每个终端最近一次算出的“产出的文件”（取文件时核对用）
    artifacts: Mutex<HashMap<String, (Instant, Vec<PathBuf>)>>,
    /// 已经自动打开过 TUI 的终端：只在第一次打开终端标签时自动接上，之后用“重新连接终端”
    term_opened: Mutex<HashSet<String>>,
    /// 正在等着转到对话模式的终端（见 [`Chat::codex_adopt_later`]），同一个终端同时只有一个
    pub(super) adopting: Mutex<HashSet<String>>,
    /// 刚让 TUI 不带会话号启动、等它自己开一段对话的终端（终端 id → 什么时候开始等）
    tui_waiting: Mutex<HashMap<String, Instant>>,
}

/// Codex 这边每个终端多记的状态
#[derive(Default)]
pub struct CodexSt {
    /// 这个 thread 加载在第几个 app-server 里（不等于当前的就要重新 `thread/resume`）
    pub generation: u64,
    /// 正在跑的这一轮
    pub turn: Option<String>,
    pub window: Option<u64>,
    /// 这个会话累计的 token（app-server 的 `tokenUsage.total`）
    pub totals: Option<Value>,
    preview_at: Option<Instant>,
    /// 运行中插话发出去、Codex 还没读到的（做完正在跑的这一步才会进对话），界面上显示成“排队中”
    pub steered: Vec<String>,
    /// 正在重连模型服务（`error` 带 `willRetry`）：活动文字是 Codex 的提示，有新进展就去掉
    retrying: bool,
}

fn resets_iso(secs: Option<i64>) -> Value {
    secs.and_then(|s| time::OffsetDateTime::from_unix_timestamp(s).ok())
        .and_then(|t| t.format(&time::format_description::well_known::Rfc3339).ok())
        .map(Value::String)
        .unwrap_or(Value::Null)
}

/// app-server 的限额窗口 → 界面用的 `{ used, resets_at }`；按窗口长度认 5 小时 / 7 天
fn limits_view(r: &Value) -> Value {
    let snap = &r["rateLimits"];
    let mut out = json!({ "five_hour": null, "seven_day": null, "plan": snap["planType"], "credits": snap["credits"], "reached": snap["rateLimitReachedType"] });
    for w in [&snap["primary"], &snap["secondary"]] {
        if w.is_null() {
            continue;
        }
        let view = json!({ "used": w["usedPercent"], "resets_at": resets_iso(w["resetsAt"].as_i64()), "minutes": w["windowDurationMins"] });
        let key = match w["windowDurationMins"].as_i64() {
            Some(m) if m <= 24 * 60 => "five_hour",
            _ => "seven_day",
        };
        out[key] = view;
    }
    out
}

/// `model/list` → 模型菜单的选项（和 Claude Code 的同一个格式，见 `models::Opt`）
fn model_options(list: &Value) -> Vec<Value> {
    list["data"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|m| {
                    let id = m["model"].as_str().or_else(|| m["id"].as_str())?;
                    let efforts: Vec<&str> = m["supportedReasoningEfforts"].as_array().map(|e| e.iter().filter_map(|x| x["reasoningEffort"].as_str()).collect()).unwrap_or_default();
                    Some(json!({
                        "value": id,
                        "label": m["displayName"].as_str().unwrap_or(id),
                        "description": m["description"].as_str().unwrap_or(""),
                        "section": if m["hidden"].as_bool() == Some(true) { "more" } else { "main" },
                        "model": id,
                        "efforts": efforts,
                        "default_effort": m["defaultReasoningEffort"],
                        "default": m["isDefault"].as_bool() == Some(true),
                    }))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// app-server 的提问 → 界面认的 AskUserQuestion 格式
fn questions_view(params: &Value) -> Value {
    let qs: Vec<Value> = params["questions"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|q| {
                    json!({
                        "id": q["id"],
                        "header": q["header"],
                        "question": q["question"],
                        "multiSelect": false,
                        "options": q["options"].as_array().cloned().unwrap_or_default(),
                        "secret": q["isSecret"],
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    json!({ "questions": qs })
}

/// 界面的回答（问题原文 → 选的答案）→ app-server 的 `{ answers: { <问题 id>: { answers: [..] } } }`
fn answers_result(input: &Value, answers: &Value) -> Value {
    let mut out = serde_json::Map::new();
    for q in input["questions"].as_array().into_iter().flatten() {
        let (Some(id), Some(text)) = (q["id"].as_str(), q["question"].as_str()) else { continue };
        let picked: Vec<Value> = match &answers[text] {
            Value::String(s) => s.split(", ").filter(|s| !s.is_empty()).map(|s| json!(s)).collect(),
            Value::Array(a) => a.clone(),
            _ => vec![],
        };
        out.insert(id.to_string(), json!({ "answers": picked }));
    }
    json!({ "answers": out })
}

/// 界面的回答 → app-server 请求的答复
pub fn decision(tool: &str, kind: &str, input: &Value, req: &PermissionAnswer) -> Result<Value, LocalError> {
    if kind == "question" {
        return match req.decision.as_str() {
            "answer" => Ok(answers_result(input, req.answers.as_ref().filter(|a| a.is_object()).ok_or_else(|| LocalError::invalid("answers", "required"))?)),
            // 不回答：空答案，Codex 照常往下走
            "deny" => Ok(json!({ "answers": {} })),
            _ => Err(LocalError::invalid("decision", "invalid")),
        };
    }
    let d = match req.decision.as_str() {
        "allow" => "accept",
        "allow_session" => "acceptForSession",
        "deny" => "decline",
        _ => return Err(LocalError::invalid("decision", "invalid")),
    };
    let _ = tool;
    Ok(json!({ "decision": d }))
}

/// 用户消息条目（`userMessage`）里的文字，和发出去时的一样拼起来
fn user_text(item: &Value) -> String {
    let parts: Vec<&str> = item["content"].as_array().map(|a| a.iter().filter(|c| c["type"] == "text").filter_map(|c| c["text"].as_str()).collect()).unwrap_or_default();
    parts.join("\n")
}

/// 运行中的命令显示成什么：Codex 拆出来的命令（`commandActions`），没有就是整条（带着 `bash -lc` 之类的外壳）
fn command_shown(item: &Value) -> Option<String> {
    let parts: Vec<&str> = item["commandActions"].as_array().map(|a| a.iter().filter_map(|c| c["command"].as_str()).collect()).unwrap_or_default();
    if parts.is_empty() {
        item["command"].as_str().map(String::from)
    } else {
        Some(parts.join(" && "))
    }
}

fn text_input(text: &str, images: &[String]) -> Value {
    let mut v = vec![];
    if !text.is_empty() {
        v.push(json!({ "type": "text", "text": text, "text_elements": [] }));
    }
    for p in images {
        v.push(json!({ "type": "localImage", "path": p }));
    }
    json!(v)
}

impl Chat {
    pub(super) fn is_codex(&self, id: &str) -> bool {
        self.inst(id).agent.launch() == "codex"
    }

    /// 全部权限的终端：不询问、不用沙箱（和 `codex --yolo` 一样）
    fn codex_policy(&self, id: &str) -> Value {
        if self.instances.opts(id).full_access {
            json!({ "approvalPolicy": "never", "sandbox": "danger-full-access" })
        } else {
            json!({})
        }
    }

    /// 拿到能用的 app-server：没起过、退出了、第三方服务商的设置变了（且没有在跑的对话）就起一个新的。
    async fn codex_server(&self) -> Result<Arc<AppServer>, LocalError> {
        let env = crate::model::launch_env(&self.store, &self.paths.home, "codex");
        let program = codex_app::program(self.instances.shell()).await;
        let signature = format!("{}|{}", program.display(), env.as_ref().map(|e| e.signature()).unwrap_or_default());
        let mut slot = self.codex.server.lock().await;
        if let Some(s) = slot.as_ref().filter(|s| s.alive()) {
            let busy = lock(&self.codex.threads).values().any(|id| lock(&self.inst(id).st).phase != Phase::Idle);
            if s.signature == signature || busy {
                return Ok(s.clone());
            }
            s.kill();
        }
        let (tx, rx) = mpsc::unbounded_channel();
        let server = AppServer::start(&self.paths.home.join("run"), program, env.as_ref(), signature, tx).await?;
        let gen = self.codex.generation.fetch_add(1, Ordering::SeqCst) + 1;
        lock(&self.codex.threads).clear();
        *slot = Some(server.clone());
        if let Some(me) = self.me.upgrade() {
            tokio::spawn(me.clone().codex_events(rx));
            // 不是第一次连（app-server 换过进程）：终端里重连上来的 TUI 要用的会话先加载好
            if gen > 1 {
                tokio::spawn(me.codex_reload_terminals(server.clone(), gen));
            }
        }
        Ok(server)
    }

    fn codex_inst_of(&self, params: &Value) -> Option<Arc<Inst>> {
        let thread = params["threadId"].as_str()?;
        let id = lock(&self.codex.threads).get(thread).cloned()?;
        Some(self.inst(&id))
    }

    /// 让这个终端的 thread 在当前 app-server 里加载着（打开对话、发消息、回答之前调用）。
    /// 还没有会话时什么都不做：第一条消息时再 `thread/start`。
    pub(super) async fn codex_attach(&self, id: &str) -> Result<Arc<AppServer>, LocalError> {
        // 终端里跑着的 Codex 不是接在看看 app-server 上的：两边会同时写一个会话，现在不接；
        // 在后台等它空闲时转到对话模式（接着同一段对话），转好后界面自己刷新
        match self.codex_terminal_kind(id).await {
            kind @ (TermCodex::Foreign | TermCodex::Local) => {
                if let Some(me) = self.me.upgrade() {
                    me.codex_adopt_later(id);
                }
                return Err(LocalError::new(if matches!(kind, TermCodex::Foreign) { "CHAT_FOREIGN" } else { "CHAT_TERMINAL_ONLY" }));
            }
            TermCodex::None | TermCodex::Remote => {}
        }
        let server = self.codex_server().await?;
        let gen = self.codex.generation.load(Ordering::SeqCst);
        let inst = self.inst(id);
        let (thread, loaded) = {
            let st = lock(&inst.st);
            (st.binding.as_ref().map(|b| b.session_id.clone()), st.codex.generation == gen)
        };
        if loaded {
            return Ok(server);
        }
        let row = self.store.instance(id)?.ok_or_else(LocalError::not_found)?;
        // 终端里的 TUI 刚开的、还没聊过的对话：app-server 里有它，但没有记录文件，resume 不了。
        // 先记下对应关系（状态通知能找到这个终端），第一轮写出记录后收到 thread/status/changed 再接（见 codex_note）
        let unwritten = {
            let st = lock(&inst.st);
            st.binding.as_ref().is_some_and(|b| !b.transcript_path.as_os_str().is_empty() && !b.transcript_path.is_file())
        };
        if let (Some(t), true) = (&thread, unwritten) {
            lock(&self.codex.threads).insert(t.clone(), id.to_string());
            let mut st = lock(&inst.st);
            if st.phase == Phase::Detached {
                Self::set_phase(&inst, &mut st, Phase::Idle, None);
            }
            return Ok(server);
        }
        match thread {
            Some(t) => {
                let mut params = json!({ "threadId": t, "cwd": row.workdir, "excludeTurns": true });
                merge(&mut params, self.codex_policy(id));
                let r = server.call("thread/resume", params).await?;
                self.codex_loaded(&inst, &r, gen);
            }
            None => {
                let mut st = lock(&inst.st);
                st.codex.generation = gen;
                if st.phase == Phase::Detached {
                    Self::set_phase(&inst, &mut st, Phase::Idle, None);
                }
            }
        }
        Ok(server)
    }

    /// `thread/start`、`thread/resume` 的答复：记下会话、记录文件、模型；按 thread 的状态设好空闲 / 运行中。
    fn codex_loaded(&self, inst: &Inst, r: &Value, gen: u64) {
        let t = &r["thread"];
        let Some(tid) = t["id"].as_str() else { return };
        lock(&self.codex.threads).insert(tid.to_string(), inst.id.clone());
        let path = t["path"].as_str().map(PathBuf::from).filter(|p| inst.agent.valid_transcript(p));
        let mut st = lock(&inst.st);
        let old_path = st.binding.as_ref().map(|b| b.transcript_path.clone()).filter(|p| !p.as_os_str().is_empty());
        let b = Binding { session_id: tid.to_string(), transcript_path: path.or(old_path).unwrap_or_default() };
        if st.binding.as_ref() != Some(&b) {
            let _ = self.store.set(&super::binding_key(&inst.id), &b);
            st.binding = Some(b);
        }
        st.codex.generation = gen;
        if st.model_set_at.is_none() {
            st.model = r["model"].as_str().map(String::from);
            st.effort = r["reasoningEffort"].as_str().map(String::from);
        }
        st.actual_model = r["model"].as_str().map(String::from).or(st.actual_model.take());
        st.title = t["name"].as_str().filter(|s| !s.is_empty()).map(String::from).or(st.title.take());
        st.version = t["cliVersion"].as_str().map(String::from).or(st.version.take());
        st.error = None;
        let active = t["status"]["type"] == "active";
        Self::set_phase(inst, &mut st, if active { Phase::Running } else { Phase::Idle }, None);
        Self::emit_status(inst, &st);
        // 上下文用量：app-server 要等下一轮才发，先从记录文件里最后一次的用量填上（输入框旁的小圆环、“用量”栏）
        let seed = st.context.is_none().then(|| st.binding.as_ref().map(|b| b.transcript_path.clone())).flatten().filter(|p| !p.as_os_str().is_empty());
        drop(st);
        if let Some(u) = seed.and_then(|p| super::codex_rollout::last_token_usage(&p)) {
            let mut st = lock(&inst.st);
            if st.context.is_none() {
                set_usage(&mut st, &u);
                Self::emit_status(inst, &st);
            }
        }
    }

    /// 还没有会话：建一个
    async fn codex_new_thread(&self, server: &AppServer, inst: &Inst) -> Result<String, LocalError> {
        let row = self.store.instance(&inst.id)?.ok_or_else(LocalError::not_found)?;
        let mut params = json!({ "cwd": row.workdir });
        merge(&mut params, self.codex_policy(&inst.id));
        let r = server.call("thread/start", params).await?;
        let tid = r["thread"]["id"].as_str().ok_or_else(|| LocalError::new("CODEX_REQUEST_FAILED"))?.to_string();
        self.codex_loaded(inst, &r, self.codex.generation.load(Ordering::SeqCst));
        let _ = inst.tx.send(super::Event::Reset);
        Ok(tid)
    }

    /// 换成接着以前的另一段对话：加载它、界面重新取；终端里开着同步的 TUI 时也换过去。
    async fn codex_resume(&self, id: &str, old: Option<String>, to: &str) -> Result<(), LocalError> {
        self.codex_forget(old).await;
        self.codex_bind(id, to);
        let inst = self.inst(id);
        {
            let mut st = lock(&inst.st);
            st.title = None;
            st.context = None;
        }
        self.codex_attach(id).await?;
        *inst.tr.lock().await = None;
        let _ = inst.tx.send(super::Event::Reset);
        if matches!(self.codex_terminal_kind(id).await, TermCodex::Remote) {
            if let Some(me) = self.me.upgrade() {
                let id = id.to_string();
                tokio::spawn(async move {
                    if let Err(e) = me.codex_reconnect_terminal(&id).await {
                        tracing::warn!(id, error = ?e, "换了对话后没能让终端里的 Codex 跟过去");
                    }
                });
            }
        }
        Ok(())
    }

    /// 记录文件路径还没有时（新建的会话第一轮开始后才写文件）问一下 app-server
    async fn codex_fill_path(&self, inst: &Inst) {
        let tid = {
            let st = lock(&inst.st);
            match &st.binding {
                Some(b) if b.transcript_path.as_os_str().is_empty() => b.session_id.clone(),
                _ => return,
            }
        };
        let Some(server) = self.codex.server.lock().await.clone() else { return };
        let Ok(r) = server.call("thread/read", json!({ "threadId": tid })).await else { return };
        let Some(path) = r["thread"]["path"].as_str().map(PathBuf::from).filter(|p| inst.agent.valid_transcript(p)) else { return };
        let mut st = lock(&inst.st);
        if let Some(b) = st.binding.as_mut().filter(|b| b.session_id == tid) {
            b.transcript_path = path;
            let b = b.clone();
            let _ = self.store.set(&super::binding_key(&inst.id), &b);
        }
    }

    // ---------------- app-server 发来的 ----------------

    async fn codex_events(self: Arc<Self>, mut rx: mpsc::UnboundedReceiver<Incoming>) {
        let gen = self.codex.generation.load(Ordering::SeqCst);
        while let Some(m) = rx.recv().await {
            match m {
                Incoming::Notification { method, params } => self.codex_note(&method, &params).await,
                Incoming::Request { id, method, params } => {
                    let me = self.clone();
                    tokio::spawn(async move { me.codex_request(id, &method, params).await });
                }
                Incoming::Closed => break,
            }
        }
        // 进程退出：这个进程里的会话都要重新加载；挂着的审批作废
        if self.codex.generation.load(Ordering::SeqCst) != gen {
            return;
        }
        let ids: Vec<String> = lock(&self.codex.threads).drain().map(|(_, id)| id).collect();
        for id in ids {
            let inst = self.inst(&id);
            let mut st = lock(&inst.st);
            Self::drop_pending(&mut st);
            st.codex.generation = 0;
            st.codex.turn = None;
            if st.phase != Phase::Idle {
                st.error = Some("Codex app-server exited".into());
                Self::set_phase(&inst, &mut st, Phase::Idle, None);
                Self::emit_status(&inst, &st);
            }
        }
    }

    async fn codex_note(&self, method: &str, p: &Value) {
        match method {
            "account/rateLimits/updated" => {
                *lock(&self.codex.limits) = None;
                return;
            }
            "item/started" if p["item"]["type"] == "fileChange" => {
                if let Some(item) = p["item"]["id"].as_str() {
                    lock(&self.codex.patches).insert(item.to_string(), p["item"]["changes"].clone());
                }
            }
            "thread/started" => {
                self.codex_claim_thread(p);
                return;
            }
            _ => {}
        }
        let Some(inst) = self.codex_inst_of(p) else { return };
        // 认领来的对话第一轮写出记录了：让界面重新加载，重新加载时 codex_attach 会 resume 它，
        // 之后逐条输出、审批才会推给看看（这里直接调 codex_attach 会让 async 的类型绕回自己）
        if method == "thread/status/changed" {
            let gen = self.codex.generation.load(Ordering::SeqCst);
            let late = {
                let st = lock(&inst.st);
                st.codex.generation != gen && has_rollout(&st)
            };
            if late {
                let _ = inst.tx.send(super::Event::Reset);
            }
            return;
        }
        let mut poll = false;
        {
            let mut st = lock(&inst.st);
            match method {
                "turn/started" => {
                    st.codex.turn = p["turn"]["id"].as_str().map(String::from);
                    st.codex.retrying = false;
                    st.error = None;
                    inst.prompts.send_modify(|n| *n += 1);
                    Self::set_phase(&inst, &mut st, Phase::Running, None);
                }
                "turn/completed" => {
                    st.codex.turn = None;
                    st.codex.steered.clear();
                    st.codex.retrying = false;
                    Self::drop_pending(&mut st);
                    if p["turn"]["status"] == "failed" {
                        st.error = p["turn"]["error"]["message"].as_str().map(|s| s.chars().take(500).collect());
                    }
                    if p["turn"]["status"] == "interrupted" {
                        st.end_hint = Some(super::board::Outcome::Interrupted);
                    }
                    Self::set_phase(&inst, &mut st, Phase::Idle, None);
                    Self::emit_status(&inst, &st);
                    poll = true;
                }
                "item/started" => {
                    let item = &p["item"];
                    let (tool, activity) = match item["type"].as_str() {
                        Some("commandExecution") => (Some("Bash"), command_shown(item)),
                        Some("fileChange") => (Some("Edit"), None),
                        Some("mcpToolCall") => (Some("mcp"), item["tool"].as_str().map(String::from)),
                        Some("webSearch") => (Some("WebSearch"), None),
                        Some("reasoning") => (None, Some("thinking".to_string())),
                        // 压缩（手动 /compact 和一轮当中自动压缩都是这个条目）
                        Some("contextCompaction") => (None, Some("compacting".to_string())),
                        // 等子代理交回结果
                        Some("collabAgentToolCall") if item["tool"] == "wait" => (Some("Agent"), Some("waiting_agents".to_string())),
                        _ => (None, None),
                    };
                    st.codex.retrying = false;
                    // 新的一段回复：正在输出的从头来。运行中插话时 Codex 会丢掉写了一半的那段（没有 item/completed）重新写
                    if item["type"] == "agentMessage" {
                        st.live.text = None;
                    }
                    if st.phase != Phase::Approval {
                        st.tool = tool.map(String::from);
                        if let Some(t) = tool {
                            let detail = match t {
                                "Edit" => super::board::changed_files(&item["changes"]),
                                _ => activity.clone(),
                            };
                            st.doing = Some(json!({ "tool": t, "detail": detail }));
                        }
                        st.live.activity = activity;
                        if st.phase != Phase::Running {
                            Self::enter(&mut st, Phase::Running);
                            st.phase = Phase::Running;
                            st.reason = None;
                        }
                        Self::emit_status(&inst, &st);
                    }
                }
                "item/agentMessage/delta" => {
                    if std::mem::take(&mut st.codex.retrying) {
                        st.live.activity = None;
                    }
                    let text = st.live.text.get_or_insert_with(String::new);
                    text.push_str(p["delta"].as_str().unwrap_or(""));
                    if text.chars().count() > PREVIEW_LIMIT {
                        let skip = text.chars().count() - PREVIEW_LIMIT;
                        *text = text.chars().skip(skip).collect();
                    }
                    if st.codex.preview_at.is_none_or(|t| t.elapsed() >= PREVIEW_GAP) {
                        st.codex.preview_at = Some(Instant::now());
                        Self::emit_status(&inst, &st);
                    }
                }
                "item/completed" => {
                    if p["item"]["type"] == "agentMessage" {
                        st.live.text = None;
                        if let Some(r) = p["item"]["text"].as_str().and_then(super::board::first_para) {
                            st.last_reply = Some(r);
                        }
                    }
                    // 插话进了对话：从“排队中”里拿掉。只按文字对：跟读记录时可能已经拿掉了（Chat::poll），
                    // 对不上就拿最早的那条会误删另一条还在排队的
                    if p["item"]["type"] == "userMessage" {
                        let text = user_text(&p["item"]);
                        if let Some(at) = st.codex.steered.iter().position(|q| *q == text) {
                            st.codex.steered.remove(at);
                        }
                    }
                    st.codex.retrying = false;
                    if let Some(item) = p["item"]["id"].as_str() {
                        lock(&self.codex.patches).remove(item);
                    }
                    st.tool = None;
                    st.live.activity = None;
                    Self::emit_status(&inst, &st);
                    poll = true;
                }
                "serverRequest/resolved" => {
                    // 审批在别处答了（或者这一轮被停了）
                    if st.phase == Phase::Approval {
                        Self::drop_pending(&mut st);
                        Self::set_phase(&inst, &mut st, Phase::Running, None);
                    }
                }
                "thread/tokenUsage/updated" => {
                    set_usage(&mut st, &p["tokenUsage"]);
                    Self::emit_status(&inst, &st);
                }
                "thread/name/updated" => {
                    st.title = p["threadName"].as_str().or_else(|| p["name"].as_str()).map(String::from);
                    Self::emit_status(&inst, &st);
                }
                "model/rerouted" => {
                    st.actual_model = p["toModel"].as_str().or_else(|| p["model"].as_str()).map(String::from).or(st.actual_model.take());
                    Self::emit_status(&inst, &st);
                }
                // 连不上模型服务、正在重试（“Reconnecting... 2/5”）：不算出错，运行中的活动文字换成它
                "error" if p["willRetry"] == true => {
                    if st.phase == Phase::Running {
                        st.codex.retrying = true;
                        st.live.activity = p["error"]["message"].as_str().map(|s| s.chars().take(120).collect());
                        Self::emit_status(&inst, &st);
                    }
                }
                "error" if p["willRetry"] != true => {
                    st.error = p["error"]["message"].as_str().map(|s| s.chars().take(500).collect());
                    Self::emit_status(&inst, &st);
                }
                "thread/closed" => {
                    st.codex.generation = 0;
                }
                _ => {}
            }
        }
        if poll {
            self.codex_fill_path(&inst).await;
            self.poll(&inst).await;
        }
    }

    /// app-server 要看看答复的请求：审批、提问挂起来等界面回答；其余不支持的答错误（Codex 按拒绝处理）。
    async fn codex_request(&self, rid: Value, method: &str, p: Value) {
        let Some(server) = self.codex.server.lock().await.clone() else { return };
        let inst = self.codex_inst_of(&p);
        let (tool, kind, input) = match method {
            "item/commandExecution/requestApproval" => {
                ("Bash", "permission", json!({ "command": p["command"], "cwd": p["cwd"], "description": p["reason"] }))
            }
            "item/fileChange/requestApproval" => {
                let changes = p["itemId"].as_str().and_then(|i| lock(&self.codex.patches).get(i).cloned()).unwrap_or(Value::Null);
                ("apply_patch", "permission", json!({ "changes": changes, "description": p["reason"], "grant_root": p["grantRoot"] }))
            }
            "item/tool/requestUserInput" => ("AskUserQuestion", "question", questions_view(&p)),
            _ => {
                let _ = server.respond_error(rid, &format!("{method} is not supported by Looklook")).await;
                return;
            }
        };
        let Some(inst) = inst else {
            let _ = server.respond(rid, json!({ "decision": "cancel" })).await;
            return;
        };
        let (tx, rx) = oneshot::channel();
        let pid = self.next_id.fetch_add(1, Ordering::Relaxed);
        {
            let mut st = lock(&inst.st);
            Self::drop_pending(&mut st);
            st.pending = Some(Pending { id: pid, at: Instant::now(), tool: tool.to_string(), kind, input, suggestions: Value::Null, tx: Some(tx) });
            Self::enter(&mut st, Phase::Approval);
            st.phase = Phase::Approval;
            st.reason = None;
            Self::emit_status(&inst, &st);
        }
        // 没有 TUI 可以兜底：一直等到界面回答、这一轮被停、或者 app-server 退出
        let out = rx.await.unwrap_or_default();
        let result = serde_json::from_str::<Value>(&out).unwrap_or_else(|_| if kind == "question" { json!({ "answers": {} }) } else { json!({ "decision": "cancel" }) });
        let _ = server.respond(rid, result).await;
    }

    // ---------------- 界面的操作 ----------------

    pub(super) async fn codex_input(&self, id: &str, text: &str, images: &[String]) -> Result<Value, LocalError> {
        let server = self.codex_attach(id).await?;
        let inst = self.inst(id);
        let (thread, turn, phase, model, effort) = {
            let st = lock(&inst.st);
            let chosen = st.model_set_at.is_some();
            (st.binding.as_ref().map(|b| b.session_id.clone()), st.codex.turn.clone(), st.phase, st.model.clone().filter(|_| chosen), st.effort.clone())
        };
        if phase == Phase::Approval {
            return Err(LocalError::new("CHAT_BUSY"));
        }
        // /resume <会话>：界面上选的以前的对话，在这个终端里接着（原来的留在“以前的对话”里）
        if let Some(to) = text.trim().strip_prefix("/resume ").map(str::trim).filter(|s| !s.is_empty() && images.is_empty()) {
            if phase == Phase::Running {
                return Err(LocalError::new("CHAT_RUNNING"));
            }
            if !to.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
                return Err(LocalError::invalid("resume", "invalid"));
            }
            if thread.as_deref() != Some(to) {
                self.codex_resume(id, thread, to).await?;
            }
            return Ok(json!({ "delivered": true }));
        }
        let thread = match thread {
            Some(t) => t,
            None => self.codex_new_thread(&server, &inst).await?,
        };
        // /clear、/new：开一个新会话（原来的留在“以前的对话”里）
        if matches!(text.trim(), "/clear" | "/new") && images.is_empty() {
            if phase == Phase::Running {
                return Err(LocalError::new("CHAT_RUNNING"));
            }
            self.codex_forget(Some(thread)).await;
            self.codex_new_thread(&server, &inst).await?;
            return Ok(json!({ "delivered": true }));
        }
        // 界面上的“压缩上下文”
        if text.trim() == "/compact" && images.is_empty() {
            server.call("thread/compact/start", json!({ "threadId": thread })).await?;
            let mut st = lock(&inst.st);
            Self::set_phase(&inst, &mut st, Phase::Running, None);
            return Ok(json!({ "delivered": true }));
        }
        let input = text_input(text, images);
        // 这一轮还在跑：插话（Codex 会在下一步之前看到）
        if let (Phase::Running, Some(turn)) = (phase, turn) {
            // 先记下再发：Codex 读到它的通知可能比这次请求的答复先到
            lock(&inst.st).codex.steered.push(text.to_string());
            let r = server.call("turn/steer", json!({ "threadId": thread, "input": input, "expectedTurnId": turn })).await;
            let mut st = lock(&inst.st);
            if r.is_err() {
                if let Some(at) = st.codex.steered.iter().rposition(|q| q == text) {
                    st.codex.steered.remove(at);
                }
            }
            Self::emit_status(&inst, &st);
            drop(st);
            r?;
            return Ok(json!({ "delivered": true }));
        }
        let mut params = json!({ "threadId": thread, "input": input });
        if let Some(m) = model {
            params["model"] = json!(m);
        }
        if let Some(e) = effort {
            params["effort"] = json!(e);
        }
        let r = server.call("turn/start", params).await?;
        let mut st = lock(&inst.st);
        st.codex.turn = r["turn"]["id"].as_str().map(String::from).or(st.codex.turn.take());
        st.error = None;
        Self::set_phase(&inst, &mut st, Phase::Running, None);
        Ok(json!({ "delivered": true }))
    }

    pub(super) async fn codex_interrupt(&self, id: &str) -> Result<(), LocalError> {
        let server = self.codex_attach(id).await?;
        let inst = self.inst(id);
        let (thread, turn) = {
            let st = lock(&inst.st);
            (st.binding.as_ref().map(|b| b.session_id.clone()), st.codex.turn.clone())
        };
        let (Some(thread), Some(turn)) = (thread, turn) else { return Ok(()) };
        server.call("turn/interrupt", json!({ "threadId": thread, "turnId": turn })).await?;
        Ok(())
    }

    /// 模型、思考强度：记下来，下一轮 `turn/start` 带上（之后的轮次都沿用）
    pub(super) fn codex_setting(&self, id: &str, kind: &str, value: &str) -> Result<(), LocalError> {
        let inst = self.inst(id);
        let mut st = lock(&inst.st);
        match kind {
            "model" => {
                if value.is_empty() || value.len() > 64 || !value.chars().all(|c| c.is_ascii_alphanumeric() || "._-[]:/".contains(c)) {
                    return Err(LocalError::invalid("value", "invalid"));
                }
                st.model = Some(value.to_string());
                st.model_set_at = Some(st.model_msg.clone());
            }
            "effort" => {
                if !EFFORTS.contains(&value) {
                    return Err(LocalError::invalid("value", "invalid"));
                }
                st.effort = Some(value.to_string());
                // 强度跟着模型一起带上：没选过模型时用会话当前的模型
                if st.model_set_at.is_none() {
                    st.model_set_at = Some(st.model_msg.clone());
                }
            }
            _ => return Err(LocalError::invalid("kind", "invalid")),
        }
        Self::emit_status(&inst, &st);
        Ok(())
    }

    pub(super) async fn codex_models(&self, id: &str) -> Result<Value, LocalError> {
        let cached = lock(&self.codex.models).as_ref().filter(|(t, _)| t.elapsed() < MODELS_TTL).map(|(_, v)| v.clone());
        let options = match cached {
            Some(v) => v,
            None => {
                let server = self.codex_server().await?;
                let r = server.call("model/list", json!({})).await?;
                let v = json!(model_options(&r));
                *lock(&self.codex.models) = Some((Instant::now(), v.clone()));
                v
            }
        };
        let inst = self.inst(id);
        let st = lock(&inst.st);
        let provider = crate::model::launch_env(&self.store, &self.paths.home, "codex").and_then(|_| crate::model::choice(&self.store, crate::model::ModelAgent::Codex)).map(|c| json!({ "name": c.name }));
        Ok(json!({ "options": options, "provider": provider, "model": st.model, "actual_model": st.actual_model, "effort": st.effort }))
    }

    /// 账号用量（5 小时、每周的限额）和这个会话的上下文、token
    pub(super) async fn codex_usage(&self, id: &str) -> Result<Value, LocalError> {
        let cached = lock(&self.codex.limits).as_ref().filter(|(t, _)| t.elapsed() < LIMITS_TTL).map(|(_, v)| v.clone());
        let limits = match cached {
            Some(v) => v,
            None => {
                let server = self.codex_server().await?;
                // 用第三方接口、没登录 ChatGPT 时没有限额
                let v = match server.call("account/rateLimits/read", Value::Null).await {
                    Ok(r) => limits_view(&r),
                    Err(_) => Value::Null,
                };
                let account = server.call("account/read", json!({})).await.ok();
                let mut v = v;
                if let (Some(a), Some(o)) = (account, v.as_object_mut()) {
                    o.insert("account".into(), json!({ "type": a["account"]["type"], "plan": a["account"]["planType"], "email": a["account"]["email"] }));
                }
                *lock(&self.codex.limits) = Some((Instant::now(), v.clone()));
                v
            }
        };
        let inst = self.inst(id);
        let st = lock(&inst.st);
        let t = st.codex.totals.clone().unwrap_or(Value::Null);
        let totals = json!({
            "input": t["inputTokens"].as_u64().unwrap_or(0).saturating_sub(t["cachedInputTokens"].as_u64().unwrap_or(0)),
            "cache_read": t["cachedInputTokens"].as_u64().unwrap_or(0),
            "output": t["outputTokens"].as_u64().unwrap_or(0),
        });
        Ok(json!({ "model": st.actual_model.clone().or(st.model.clone()), "context": st.context, "window": st.codex.window, "totals": totals, "limits": limits, "version": st.version }))
    }

    /// 这段对话里 Codex 生成的图片（`$CODEX_HOME/generated_images/<会话>/`），最新的在前。
    /// 界面用 `/api/instances/<终端>/chat/file?name=<文件名>` 取（完整内容，可以直接放进 `<img>`、复制链接、下载）。
    pub(super) fn codex_files(&self, id: &str) -> Value {
        let inst = self.inst(id);
        let Some(thread) = lock(&inst.st).binding.as_ref().map(|b| b.session_id.clone()) else { return json!({ "items": [] }) };
        if !crate::history::valid_id(&thread) {
            return json!({ "items": [] });
        }
        let dir = crate::agents::codex_home().join("generated_images").join(&thread);
        let mut files: Vec<(std::time::SystemTime, Value)> = std::fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                let p = e.path();
                let ext = p.extension()?.to_str()?.to_ascii_lowercase();
                if !matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "webp" | "gif") {
                    return None;
                }
                let m = e.metadata().ok().filter(|m| m.is_file())?;
                let t = m.modified().ok()?;
                let ms = t.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
                Some((t, json!({ "kind": "image", "path": p.display().to_string(), "name": p.file_name()?.to_string_lossy(), "size": m.len(), "mtime_ms": ms })))
            })
            .collect();
        files.sort_by_key(|(t, _)| std::cmp::Reverse(*t));
        json!({ "items": files.into_iter().map(|(_, v)| v).take(200).collect::<Vec<_>>() })
    }

    /// 这段对话生成的某张图片的完整路径：只认 `generated_images/<会话>/` 下、名字里没有路径分隔符的图片文件
    pub fn codex_file_path(&self, id: &str, name: &str) -> Option<PathBuf> {
        let ok_name = !name.is_empty() && name.len() <= 200 && !name.contains(['/', '\\', ':']) && !name.starts_with('.');
        let ext = Path::new(name).extension()?.to_str()?.to_ascii_lowercase();
        if !ok_name || !matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "webp" | "gif") {
            return None;
        }
        let thread = lock(&self.inst(id).st).binding.as_ref().map(|b| b.session_id.clone())?;
        if !crate::history::valid_id(&thread) {
            return None;
        }
        let p = crate::agents::codex_home().join("generated_images").join(thread).join(name);
        p.is_file().then_some(p)
    }

    /// 这段对话产出的文件（[`codex_artifacts`](super::codex_artifacts)），最近提到的在前
    async fn codex_artifact_list(&self, id: &str) -> Vec<(PathBuf, Vec<String>)> {
        let inst = self.inst(id);
        self.poll(&inst).await;
        let workdir = self.store.instance(id).ok().flatten().map(|r| PathBuf::from(r.workdir)).unwrap_or_default();
        let mut texts: Vec<String> = vec![];
        {
            let tr = inst.tr.lock().await;
            let Some(t) = tr.as_ref().map(super::Reader::get) else { return vec![] };
            for it in t.items(0, t.len()).iter().rev() {
                if let Ok(v) = serde_json::to_value(it) {
                    super::codex_artifacts::strings(&v, &mut texts);
                }
            }
        }
        let mut skip = super::codex_artifacts::system_dirs();
        skip.push(crate::agents::codex_home());
        let found = tokio::task::block_in_place(|| super::codex_artifacts::find(&texts, &workdir, &skip));
        lock(&self.codex.artifacts).insert(id.to_string(), (Instant::now(), found.iter().map(|(p, _)| p.clone()).collect()));
        found
    }

    pub(super) async fn codex_artifacts(&self, id: &str) -> Value {
        let found = self.codex_artifact_list(id).await;
        json!({ "items": found.iter().map(|(p, m)| super::codex_artifacts::view(p, m)).collect::<Vec<_>>() })
    }

    /// 取某个产出的文件：只认这段对话里出现过的（一分钟内算过就用算过的，否则重算一次）
    pub(super) async fn codex_artifact_path(&self, id: &str, path: &str) -> Option<PathBuf> {
        let want = super::codex_artifacts::normalize(Path::new(path));
        let known = lock(&self.codex.artifacts).get(id).filter(|(t, _)| t.elapsed() < super::codex_artifacts::TTL).is_some_and(|(_, v)| v.contains(&want));
        let ok = known || self.codex_artifact_list(id).await.iter().any(|(p, _)| *p == want);
        (ok && want.is_file()).then_some(want)
    }

    /// 从“以前的对话”接着、或者迁移时：直接记下要接着的会话（记录文件路径在加载时由 app-server 给）
    pub fn codex_bind(&self, id: &str, thread: &str) {
        let b = Binding { session_id: thread.to_string(), transcript_path: PathBuf::new() };
        let _ = self.store.set(&super::binding_key(id), &b);
        if let Some(inst) = lock(&self.insts).get(id).cloned() {
            let mut st = lock(&inst.st);
            st.binding = Some(b);
            st.codex.generation = 0;
        }
    }

    /// 新建的对话模式 Codex 终端：接着以前的对话（复制一份时用 `thread/fork`）、送第一句话。
    pub async fn codex_first(&self, id: &str, prompt: Option<String>, resume: Option<crate::instances::Resume>) {
        let r: Result<(), LocalError> = async {
            if let Some(r) = resume {
                if r.fork {
                    let server = self.codex_server().await?;
                    let row = self.store.instance(id)?.ok_or_else(LocalError::not_found)?;
                    let mut params = json!({ "threadId": r.id, "cwd": row.workdir });
                    merge(&mut params, self.codex_policy(id));
                    let res = server.call("thread/fork", params).await?;
                    self.codex_loaded(&self.inst(id), &res, self.codex.generation.load(Ordering::SeqCst));
                } else {
                    self.codex_bind(id, &r.id);
                }
            }
            if let Some(p) = prompt {
                self.codex_input(id, &p, &[]).await?;
            }
            Ok(())
        }
        .await;
        if let Err(e) = r {
            tracing::warn!(id, error = ?e, "Codex 对话没能开始");
            let inst = self.inst(id);
            let mut st = lock(&inst.st);
            st.error = Some(format!("{e:?}").chars().take(500).collect());
            Self::emit_status(&inst, &st);
        }
    }

    pub(super) async fn codex_forget(&self, thread: Option<String>) {
        let Some(t) = thread else { return };
        lock(&self.codex.threads).remove(&t);
        if let Some(server) = self.codex.server.lock().await.clone() {
            let _ = server.call("thread/unsubscribe", json!({ "threadId": t })).await;
        }
    }

    // ---------------- 终端里的 TUI（和对话界面同步） ----------------

    /// 终端标签连上之前：起好 app-server、加载好会话（还没有就建一个），这样终端里的 TUI 接的是同一个会话。
    pub async fn codex_prepare_terminal(&self, id: &str) {
        let r: Result<(), LocalError> = async {
            self.codex_attach(id).await?;
            self.codex_expect_tui(id);
            Ok(())
        }
        .await;
        if let Err(e) = r {
            tracing::warn!(id, error = ?e, "没能给终端准备好 Codex 会话");
        }
    }

    /// 第一次打开终端标签时：终端里停在 shell 上（终端是先开的 shell）就替用户打开同一个会话的 TUI。
    /// 之后再打开标签不再自动打开，要接回去用 [`Self::codex_reconnect_terminal`]。
    pub async fn codex_terminal_attached(self: Arc<Self>, id: String) {
        if !lock(&self.codex.term_opened).insert(id.clone()) {
            return;
        }
        // 等终端会话建好、刚启动的 TUI 跑起来
        tokio::time::sleep(Duration::from_millis(2500)).await;
        let Some(pane) = self.instances.pane(&id).await else { return };
        let inst = self.inst(&id);
        if super::agent_state(inst.agent, &pane).is_some() || !super::is_shell(&pane.command) {
            return;
        }
        let Some(line) = self.instances.codex_tui_line(&id) else { return };
        self.codex_expect_tui(&id);
        if self.instances.send_literal(&id, &line).await.is_ok() {
            let _ = self.instances.send_keys(&id, &["Enter"]).await;
        }
    }

    /// 终端标签上的“重新连接终端”：终端里还在跑别的程序（断了线的 TUI、本机版、自己开的 Codex）就先退出它，
    /// 再用 `--remote` 打开接在 app-server 上的同一个会话。
    pub async fn codex_reconnect_terminal(&self, id: &str) -> Result<(), LocalError> {
        let row = self.store.instance(id)?.ok_or_else(LocalError::not_found)?;
        if !self.instances.codex_app_mode(&row) {
            return Err(LocalError::new("CHAT_UNAVAILABLE"));
        }
        lock(&self.codex.term_opened).insert(id.to_string());
        // Codex TUI 空闲时要连按两次 Ctrl+C 才退出；在跑的话第一次是中断
        for _ in 0..3 {
            let pane = self.instances.pane(id).await.ok_or_else(|| LocalError::new("CODEX_TERMINAL_STOPPED"))?;
            if super::is_shell(&pane.command) {
                break;
            }
            let _ = self.instances.send_keys(id, &["C-c"]).await;
            tokio::time::sleep(Duration::from_millis(200)).await;
            let _ = self.instances.send_keys(id, &["C-c"]).await;
            tokio::time::sleep(Duration::from_millis(1500)).await;
        }
        let pane = self.instances.pane(id).await.ok_or_else(|| LocalError::new("CODEX_TERMINAL_STOPPED"))?;
        if !super::is_shell(&pane.command) {
            return Err(LocalError::new("CODEX_TERMINAL_BUSY"));
        }
        // 起好 app-server、加载好会话；--remote 要有地址
        self.codex_attach(id).await?;
        if codex_app::endpoint().is_none() {
            return Err(LocalError::new("CODEX_APP_SERVER"));
        }
        let line = self.instances.codex_tui_line(id).ok_or_else(|| LocalError::new("CHAT_UNAVAILABLE"))?;
        self.codex_expect_tui(id);
        self.instances.send_literal(id, &line).await?;
        self.instances.send_keys(id, &["Enter"]).await
    }

    /// 这个终端的对话还没有记录文件时，TUI 会不带会话号启动、自己开一段：记下来，等 thread/started 认领
    fn codex_expect_tui(&self, id: &str) {
        if !has_rollout(&lock(&self.inst(id).st)) {
            lock(&self.codex.tui_waiting).insert(id.to_string(), Instant::now());
        }
    }

    /// 不是看看开的对话开始了（终端里的 TUI 不带会话号启动后自己开的）：认领给在等它、同一个文件夹的终端；
    /// 没登记过时，同一个文件夹里只有一个还没聊过的对话模式终端也算它的。对话页和终端从此是同一段。
    fn codex_claim_thread(&self, p: &Value) {
        let t = &p["thread"];
        let (Some(tid), Some(cwd)) = (t["id"].as_str(), t["cwd"].as_str()) else { return };
        if lock(&self.codex.threads).contains_key(tid) {
            return;
        }
        let rows = self.store.instances().unwrap_or_default();
        let fits = |id: &str| {
            rows.iter().any(|r| r.id == id && self.instances.codex_app_mode(r) && crate::history::same_dir(&r.workdir, cwd)) && {
                let inst = self.inst(id);
                let st = lock(&inst.st);
                st.codex.turn.is_none() && !has_rollout(&st)
            }
        };
        let waiting: Vec<(String, Instant)> = lock(&self.codex.tui_waiting).iter().filter(|(_, at)| at.elapsed() < TUI_CLAIM).map(|(k, v)| (k.clone(), *v)).collect();
        let pick = waiting.iter().filter(|(id, _)| fits(id)).max_by_key(|(_, at)| *at).map(|(id, _)| id.clone()).or_else(|| {
            let c: Vec<&str> = rows.iter().map(|r| r.id.as_str()).filter(|id| fits(id)).collect();
            (c.len() == 1).then(|| c[0].to_string())
        });
        let Some(id) = pick else { return };
        lock(&self.codex.tui_waiting).remove(&id);
        lock(&self.codex.threads).insert(tid.to_string(), id.clone());
        let inst = self.inst(&id);
        let path = t["path"].as_str().map(PathBuf::from).filter(|p| inst.agent.valid_transcript(p)).unwrap_or_default();
        let b = Binding { session_id: tid.to_string(), transcript_path: path };
        let mut st = lock(&inst.st);
        let _ = self.store.set(&super::binding_key(&id), &b);
        st.binding = Some(b);
        st.codex.generation = 0;
        if st.phase == Phase::Detached {
            Self::set_phase(&inst, &mut st, Phase::Idle, None);
        }
        Self::emit_status(&inst, &st);
        drop(st);
        let _ = inst.tx.send(super::Event::Reset);
    }

    // ---------------- 以前的会话转到对话模式 ----------------

    /// 终端里是旧版看看开的、用户自己开的、或者 app-server 用不了时改成本机直接运行的 Codex：在后台等它空闲
    /// （输入框空着、没在干活），退出它，用 app-server 接着它那段对话，终端里换成 `--remote` 的 TUI。
    /// 正在干活时每隔几秒再看，不打断；最多等 [`ADOPT_WAIT`]，之后下次打开对话页再试。
    pub(super) fn codex_adopt_later(self: Arc<Self>, id: &str) {
        if !lock(&self.codex.adopting).insert(id.to_string()) {
            return;
        }
        let id = id.to_string();
        // 装箱成 dyn：转接里会再走到 codex_attach，不装箱的话 async 函数的类型会绕回自己
        let fut: std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> = Box::pin(async move {
            let deadline = Instant::now() + ADOPT_WAIT;
            loop {
                match self.codex_adopt(&id, false).await {
                    Ok(false) if Instant::now() < deadline => tokio::time::sleep(ADOPT_POLL).await,
                    Ok(_) => break,
                    Err(e) => {
                        tracing::warn!(id, error = ?e, "没能把以前的 Codex 会话转到对话模式");
                        break;
                    }
                }
            }
            lock(&self.codex.adopting).remove(&id);
        });
        tokio::spawn(fut);
    }

    /// 转到对话模式：转好了（或者本来就不用转）返回 true，旧的 Codex 还在干活、这次没转返回 false。
    /// `force`：不等空闲，直接退出旧的（界面上的“现在就转过来”，正在做的这一轮会被中断）。
    pub async fn codex_adopt(&self, id: &str, force: bool) -> Result<bool, LocalError> {
        if !matches!(self.codex_terminal_kind(id).await, TermCodex::Foreign | TermCodex::Local) {
            return Ok(true);
        }
        let inst = self.inst(id);
        if !force {
            let idle = self.instances.screen(id).await.map(|s| inst.agent.classify(&s));
            if !matches!(idle, Some(super::screen::Screen::Input { busy: false, ref text }) if text.trim().is_empty()) {
                return Ok(false);
            }
        }
        // 先确认 app-server 起得来，免得旧的退出了、新的又接不上
        self.codex_server().await?;
        let row = self.store.instance(id)?.ok_or_else(LocalError::not_found)?;
        if let Some(thread) = self.codex_foreign_thread(id, &row).await {
            let mut st = lock(&inst.st);
            if st.binding.as_ref().map(|b| b.session_id.as_str()) != Some(thread.as_str()) {
                let b = Binding { session_id: thread, transcript_path: PathBuf::new() };
                let _ = self.store.set(&super::binding_key(id), &b);
                st.binding = Some(b);
            }
            st.codex.generation = 0;
            Self::set_phase(&inst, &mut st, Phase::Detached, Some("adopting"));
        }
        self.codex_reconnect_terminal(id).await?;
        let _ = inst.tx.send(super::Event::Reset);
        Ok(true)
    }

    /// 终端里那个 Codex 用的是哪段对话：命令行里带着（`codex resume <id>`）就用它；已经记着的就用记着的；
    /// 旧版看看开的（`-p looklook-chat`，命令行里没有）按 [`super::guess_conversation`] 猜；猜不到（开着还没聊过）时
    /// 返回 None，转过去后开一段新的。
    async fn codex_foreign_thread(&self, id: &str, row: &crate::store::InstanceRow) -> Option<String> {
        let mut started_ms = 0;
        if let Some(pane) = self.instances.pane(id).await {
            if let Some((args, started)) = tokio::task::block_in_place(|| super::proc_under(pane.pid, &["codex"])) {
                if let Some(t) = args.iter().skip_while(|a| a.as_str() != "resume").nth(1).filter(|a| a.len() >= 32 && crate::history::valid_id(a)) {
                    return Some(t.clone());
                }
                started_ms = started;
            }
        }
        if let Some(b) = lock(&self.inst(id).st).binding.clone() {
            return Some(b.session_id);
        }
        super::guess_conversation(&self.store, id, &row.workdir, "codex", started_ms).await
    }

    // ---------------- 不是看看打开的 Codex ----------------

    /// 终端里跑的是哪种 Codex：看看开的、接在 app-server 上的（`--remote`）；app-server 用不了时看看开的
    /// 本机版（`--no-daemon resume <这个会话>`，不带旧版本的 `-p`）；别的都算不是看看开的
    async fn codex_terminal_kind(&self, id: &str) -> TermCodex {
        let Some(pane) = self.instances.pane(id).await else { return TermCodex::None };
        let Some(args) = tokio::task::block_in_place(|| super::find_under(pane.pid, &["codex"])) else { return TermCodex::None };
        let has = |s: &str| args.iter().any(|a| a == s);
        if has("--remote") {
            return TermCodex::Remote;
        }
        let thread = lock(&self.inst(id).st).binding.as_ref().map(|b| b.session_id.clone());
        let ours = has("--no-daemon") && !has("-p") && !has("--profile") && thread.as_deref().is_none_or(|t| has(t));
        if ours {
            TermCodex::Local
        } else {
            TermCodex::Foreign
        }
    }

    /// app-server 换了一个进程（崩了、配置变了）之后：终端里接着的 TUI 会自己重连到同一个端口，
    /// 但新进程里还没有这些会话（TUI 会显示“只读或不可用”），先替它们加载上
    async fn codex_reload_terminals(self: Arc<Self>, server: Arc<AppServer>, gen: u64) {
        let Ok(rows) = self.store.instances() else { return };
        for row in rows {
            let Some(b) = crate::chat::binding(&self.store, &row.id) else { continue };
            if !self.instances.codex_app_mode(&row) || !matches!(self.codex_terminal_kind(&row.id).await, TermCodex::Remote) {
                continue;
            }
            // 直接用这个新进程加载（不经 codex_attach：那里可能又起 app-server，会绕回这里）
            let mut params = json!({ "threadId": b.session_id, "cwd": row.workdir, "excludeTurns": true });
            merge(&mut params, self.codex_policy(&row.id));
            match server.call("thread/resume", params).await {
                Ok(r) => self.codex_loaded(&self.inst(&row.id), &r, gen),
                Err(e) => tracing::debug!(id = %row.id, error = ?e, "终端里的 Codex 会话没能重新加载"),
            }
        }
    }
}

/// 终端里跑着的 Codex
enum TermCodex {
    None,
    /// 看看开的、接在 app-server 上（和对话界面同步）
    Remote,
    /// 看看开的、app-server 用不了时本机直接运行的（对话界面暂时不可用）
    Local,
    /// 别人开的（旧版本留下的、用户在终端里自己开的）
    Foreign,
}

fn merge(a: &mut Value, b: Value) {
    if let (Some(a), Value::Object(b)) = (a.as_object_mut(), b) {
        a.extend(b);
    }
}

/// `thread/tokenUsage/updated` 的 `tokenUsage`（或从记录文件里换算的同样形状）
fn set_usage(st: &mut super::State, u: &Value) {
    st.context = u["last"]["inputTokens"].as_u64();
    st.codex.window = u["modelContextWindow"].as_u64().or(st.codex.window);
    st.codex.totals = Some(u["total"].clone());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits_map_windows() {
        let r = json!({ "rateLimits": { "planType": "plus", "primary": { "usedPercent": 24, "windowDurationMins": 300, "resetsAt": 1792583484 }, "secondary": { "usedPercent": 12, "windowDurationMins": 10080, "resetsAt": 1793082793 } } });
        let v = limits_view(&r);
        assert_eq!(v["five_hour"]["used"], 24);
        assert_eq!(v["seven_day"]["used"], 12);
        assert!(v["five_hour"]["resets_at"].as_str().unwrap().starts_with("2026-"));
        assert_eq!(v["plan"], "plus");
    }

    #[test]
    fn models_to_options() {
        let r = json!({ "data": [
            { "id": "gpt-6.1-sol", "model": "gpt-6.1-sol", "displayName": "GPT-6.1-Sol", "description": "d", "hidden": false, "isDefault": true,
              "supportedReasoningEfforts": [{ "reasoningEffort": "low" }, { "reasoningEffort": "high" }], "defaultReasoningEffort": "low" },
            { "id": "old", "model": "old", "displayName": "Old", "hidden": true, "supportedReasoningEfforts": [] }
        ] });
        let o = model_options(&r);
        assert_eq!(o.len(), 2);
        assert_eq!(o[0]["value"], "gpt-6.1-sol");
        assert_eq!(o[0]["efforts"], json!(["low", "high"]));
        assert_eq!(o[1]["section"], "more");
    }

    #[test]
    fn decisions_and_answers() {
        let ans = |d: &str, a: Option<Value>| PermissionAnswer { id: 1, decision: d.into(), message: None, answers: a };
        assert_eq!(decision("Bash", "permission", &Value::Null, &ans("allow", None)).unwrap(), json!({ "decision": "accept" }));
        assert_eq!(decision("Bash", "permission", &Value::Null, &ans("allow_session", None)).unwrap(), json!({ "decision": "acceptForSession" }));
        assert_eq!(decision("apply_patch", "permission", &Value::Null, &ans("deny", None)).unwrap(), json!({ "decision": "decline" }));
        assert!(decision("Bash", "permission", &Value::Null, &ans("allow_dirs", None)).is_err());
        let input = questions_view(&json!({ "questions": [{ "id": "q1", "header": "H", "question": "选哪个？", "isOther": false, "isSecret": false, "options": [{ "label": "A", "description": "" }] }] }));
        let r = decision("AskUserQuestion", "question", &input, &ans("answer", Some(json!({ "选哪个？": "A" })))).unwrap();
        assert_eq!(r, json!({ "answers": { "q1": { "answers": ["A"] } } }));
    }

    #[test]
    fn input_with_images() {
        let v = text_input("hi", &["C:\\a.png".into()]);
        assert_eq!(v[0]["type"], "text");
        assert_eq!(v[1], json!({ "type": "localImage", "path": "C:\\a.png" }));
    }
}
