//! 对话模式的 Codex 走 `codex app-server`（docs/CODEX_UI.md）：不再在终端里跑 TUI、不用 hooks。
//!
//! - 每个对话模式的 Codex 终端对应 app-server 里的一个 thread（会话号记在 [`Binding`] 里，和 Claude Code 一样）；
//!   终端标签里是同一个工作目录下的普通 shell。
//! - 对话内容照旧读 rollout（`codex_rollout.rs`）：app-server 写的和 TUI 写的是同一种文件，以前的对话也能接着看。
//! - 状态、正在输出的回复、审批、提问、上下文和 token 用量来自 app-server 的通知和请求。
//! - 发消息是 `turn/start`（这一轮还在跑时是 `turn/steer`）；停止是 `turn/interrupt`；模型和思考强度随下一轮带上。
//! - 开启对话界面之前就在终端里跑着 Codex TUI 的（旧版本留下的）：找出它在用的会话，关掉 TUI，换成 app-server 接着。

use std::collections::HashMap;
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
/// 界面上能选的思考强度（app-server 的 `ReasoningEffort`）
pub const EFFORTS: [&str; 8] = ["none", "minimal", "low", "medium", "high", "xhigh", "max", "ultra"];

#[derive(Default)]
pub struct Hub {
    server: tokio::sync::Mutex<Option<Arc<AppServer>>>,
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
        let program = codex_app::program();
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
        let server = AppServer::start(program, env.as_ref(), signature, tx).await?;
        self.codex.generation.fetch_add(1, Ordering::SeqCst);
        lock(&self.codex.threads).clear();
        *slot = Some(server.clone());
        if let Some(me) = self.me.upgrade() {
            tokio::spawn(me.codex_events(rx));
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
        // 终端里还跑着旧版本留下的 Codex TUI：先换成普通 shell，免得两边同时写一个会话
        let thread = match self.codex_migrate(id, thread.as_deref()).await {
            Some(t) => Some(t),
            None => thread,
        };
        let row = self.store.instance(id)?.ok_or_else(LocalError::not_found)?;
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
            _ => {}
        }
        let Some(inst) = self.codex_inst_of(p) else { return };
        let mut poll = false;
        {
            let mut st = lock(&inst.st);
            match method {
                "turn/started" => {
                    st.codex.turn = p["turn"]["id"].as_str().map(String::from);
                    st.error = None;
                    inst.prompts.send_modify(|n| *n += 1);
                    Self::set_phase(&inst, &mut st, Phase::Running, None);
                }
                "turn/completed" => {
                    st.codex.turn = None;
                    Self::drop_pending(&mut st);
                    if p["turn"]["status"] == "failed" {
                        st.error = p["turn"]["error"]["message"].as_str().map(|s| s.chars().take(500).collect());
                    }
                    Self::set_phase(&inst, &mut st, Phase::Idle, None);
                    Self::emit_status(&inst, &st);
                    poll = true;
                }
                "item/started" => {
                    let item = &p["item"];
                    let (tool, activity) = match item["type"].as_str() {
                        Some("commandExecution") => (Some("Bash"), item["command"].as_str().map(String::from)),
                        Some("fileChange") => (Some("Edit"), None),
                        Some("mcpToolCall") => (Some("mcp"), item["tool"].as_str().map(String::from)),
                        Some("webSearch") => (Some("WebSearch"), None),
                        Some("reasoning") => (None, Some("thinking".to_string())),
                        _ => (None, None),
                    };
                    if st.phase != Phase::Approval {
                        st.tool = tool.map(String::from);
                        st.live.activity = activity;
                        if st.phase != Phase::Running {
                            st.phase = Phase::Running;
                            st.reason = None;
                        }
                        Self::emit_status(&inst, &st);
                    }
                }
                "item/agentMessage/delta" => {
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
                    }
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
                    let u = &p["tokenUsage"];
                    st.context = u["last"]["inputTokens"].as_u64();
                    st.codex.window = u["modelContextWindow"].as_u64().or(st.codex.window);
                    st.codex.totals = Some(u["total"].clone());
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
        let thread = match thread {
            Some(t) => t,
            None => self.codex_new_thread(&server, &inst).await?,
        };
        // /clear：开一个新会话（原来的留在“以前的对话”里）
        if text.trim() == "/clear" && images.is_empty() {
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
            server.call("turn/steer", json!({ "threadId": thread, "input": input, "expectedTurnId": turn })).await?;
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

    // ---------------- 旧版本留下的 TUI ----------------

    /// 终端里还在跑 Codex TUI（以前用 hooks 接入时启动的，或者手动开的）：找出它的会话，重启终端换成普通 shell。
    /// 返回找到的会话号；终端里没有 Codex 时 None。
    async fn codex_migrate(&self, id: &str, bound: Option<&str>) -> Option<String> {
        let pane = self.instances.pane(id).await?;
        let inst = self.inst(id);
        super::agent_state(inst.agent, &pane)?;
        // 只迁移“终端就是为了跑 Codex 开的”那种（旧版本启动的）；在 shell 里手动敲的 codex 不动
        if !tokio::task::block_in_place(|| started_with_codex(pane.pid)) {
            return None;
        }
        let row = self.store.instance(id).ok().flatten()?;
        let thread = match bound {
            Some(t) => Some(t.to_string()),
            None => self.store.get::<String>(&crate::instances::resume_key(id)).ok().flatten().or_else(|| latest_rollout_thread(Path::new(&row.workdir), pane_started(pane.pid))),
        };
        tracing::info!(id, thread = ?thread, "终端里还在跑 Codex TUI，换成 app-server");
        if let Err(e) = self.instances.restart(id).await {
            tracing::warn!(id, error = ?e, "重启终端失败");
        }
        if let Some(t) = &thread {
            self.codex_bind(id, t);
        }
        thread
    }
}

fn merge(a: &mut Value, b: Value) {
    if let (Some(a), Value::Object(b)) = (a.as_object_mut(), b) {
        a.extend(b);
    }
}

/// 面板进程（shell）的命令行里带着 codex：终端是用来跑 Codex 的（`powershell -Command codex …`、`sh -c 'codex …'`）
fn started_with_codex(pid: u32) -> bool {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
    let mut sys = System::new();
    let pid = Pid::from_u32(pid);
    sys.refresh_processes_specifics(ProcessesToUpdate::Some(&[pid]), true, ProcessRefreshKind::nothing().with_cmd(UpdateKind::Always));
    let Some(p) = sys.process(pid) else { return false };
    let name = p.name().to_string_lossy().to_ascii_lowercase();
    if name.trim_end_matches(".exe") == "codex" {
        return true;
    }
    p.cmd().iter().skip(1).any(|a| a.to_string_lossy().to_ascii_lowercase().contains("codex"))
}

/// 面板进程是什么时候启动的（找它之后才开始写的记录）
fn pane_started(pid: u32) -> Option<std::time::SystemTime> {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};
    let mut sys = System::new();
    let pid = Pid::from_u32(pid);
    sys.refresh_processes_specifics(ProcessesToUpdate::Some(&[pid]), true, ProcessRefreshKind::nothing());
    let secs = sys.process(pid)?.start_time();
    Some(std::time::UNIX_EPOCH + Duration::from_secs(secs))
}

/// 这个工作目录下、在 `since` 之后还写过的最近一份 Codex 记录的会话号
fn latest_rollout_thread(workdir: &Path, since: Option<std::time::SystemTime>) -> Option<String> {
    let convs = tokio::task::block_in_place(|| crate::history::codex_recent(workdir, since));
    convs.into_iter().next()
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
