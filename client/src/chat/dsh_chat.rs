//! 对话模式的 DSH（docs/DSH_UI.md）：看看直接用 `dsh web` 的接口驱动对话，界面是看看自己的对话界面。
//!
//! - 一台电脑上所有对话模式的 DSH 终端共用一个 `dsh web`：本机已经有 DSH 在运行就用它（用户自己开的、DSH 桌面版），
//!   没有才在后台起一个（`dsh_app::start`）。每个终端是其中的一个会话（会话号记在 [`Binding`] 里）；终端标签里是同一个
//!   工作目录下的普通 shell。
//! - 对话内容来自 `session/follow`（`dsh_log.rs`），正在输出的回复来自同一个流里的 `assistant-stream`；
//!   标题、上下文、排队的消息、模型来自 `session/control` 的投影；是否在运行来自 `$events` 的 `api-session/status`。
//! - 审批、提问是 `$events` 上的 waterfall，挂起等界面回答；这个会话不归看看管（DSH 网页版里开的）时交给下一个。
//! - 发消息 `session/prompt`（这一轮在跑时 `mode: steer`），停止 `session/cancel`，`/clear` 新开一个会话，
//!   模型和思考强度 `session/selectModel`（DSH 自己记在会话里）。

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tokio::sync::{mpsc, oneshot};

use super::dsh_app::{self, DshConn, Incoming, StreamMsg};
use super::dsh_log;
use super::{lock, Binding, Chat, Inst, Pending, Phase, PermissionAnswer};
use crate::error::LocalError;

/// 正在输出的回复最多推这么多字（只留最后这段）、最多每隔这么久推一次
const PREVIEW_LIMIT: usize = 4000;
const PREVIEW_GAP: Duration = Duration::from_millis(120);
/// 打开时先取最后这么多条消息，更早的在后台往前翻（最多翻到这么多条事件）
const OPEN_MESSAGES: u64 = 200;
const HISTORY_MAX: usize = 5000;
const MODELS_TTL: Duration = Duration::from_secs(120);

#[derive(Default)]
pub struct Hub {
    conn: tokio::sync::Mutex<Option<Arc<DshConn>>>,
    /// 第几条连接（0 表示还没连过）；跟读的流只在打开它的那条连接上有效
    generation: AtomicU64,
    /// 会话 → 终端
    sessions: Mutex<HashMap<String, String>>,
    /// 终端 → 跟读的流
    follows: Mutex<HashMap<String, String>>,
    /// 还没对上终端的会话收到的投影（标题这些先到、终端后绑上时用）
    projections: Mutex<HashMap<String, Value>>,
    models: Mutex<Option<(Instant, Value)>>,
}

/// DSH 这边每个终端多记的状态
#[derive(Default)]
pub struct DshSt {
    /// 跟读的流是第几条连接上开的（不等于当前的就要重新开）
    pub generation: u64,
    /// 运行中插话发出去、DSH 还没放进对话的（DSH 的 `inbox` 投影）
    pub queued: Vec<String>,
    pub cwd: Option<String>,
    /// 这个会话累计的 token（`tokenUsage` 投影）、上下文窗口（`contextPressure`）
    pub usage: Option<Value>,
    pub window: Option<u64>,
    preview: String,
    preview_at: Option<Instant>,
}

/// 模型菜单里的值：`服务商/模型`
fn model_value(sel: &Value) -> Option<String> {
    Some(format!("{}/{}", sel["provider"].as_str()?, sel["model"].as_str()?))
}

/// `session/modelCatalog` → 模型菜单的选项（和 Claude Code 的同一个格式）
fn model_options(cat: &Value) -> Vec<Value> {
    let default = model_value(&cat["default"]);
    let mut out = vec![];
    for g in cat["groups"].as_array().into_iter().flatten() {
        let provider = g["id"].as_str().unwrap_or_default();
        for m in g["models"].as_array().into_iter().flatten() {
            let Some(id) = m["id"].as_str() else { continue };
            let value = format!("{provider}/{id}");
            let efforts: Vec<&str> = m["reasoning"]["efforts"].as_array().map(|e| e.iter().filter_map(|x| x["id"].as_str()).collect()).unwrap_or_default();
            out.push(json!({
                "value": value,
                "label": m["name"].as_str().unwrap_or(id),
                "description": m["description"].as_str().unwrap_or(""),
                "section": "main",
                "model": value,
                "efforts": efforts,
                "default_effort": m["reasoning"]["defaultEffort"],
                "default": default.as_deref() == Some(value.as_str()),
                "group": g["name"],
            }));
        }
    }
    out
}

/// DSH 的提问 → 界面认的 AskUserQuestion 格式
fn questions_view(req: &Value) -> Value {
    let qs: Vec<Value> = req["questions"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|q| {
                    json!({
                        "id": q["id"],
                        "header": q["header"],
                        "question": q["question"],
                        "multiSelect": q["multiSelect"].as_bool().unwrap_or(false),
                        "options": q["options"].as_array().cloned().unwrap_or_default(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    json!({ "questions": qs })
}

/// 界面的回答（问题原文 → 选的答案）→ DSH 的 `{ answers: [{ id, selected, custom? }] }`：
/// 不是选项里的算用户自己写的
fn answers_result(input: &Value, answers: &Value) -> Value {
    let mut out = vec![];
    for q in input["questions"].as_array().into_iter().flatten() {
        let (Some(id), Some(text)) = (q["id"].as_str(), q["question"].as_str()) else { continue };
        let picked: Vec<String> = match &answers[text] {
            Value::String(s) => s.split(", ").filter(|s| !s.is_empty()).map(String::from).collect(),
            Value::Array(a) => a.iter().filter_map(|v| v.as_str().map(String::from)).collect(),
            _ => vec![],
        };
        let labels: Vec<&str> = q["options"].as_array().map(|o| o.iter().filter_map(|x| x["label"].as_str()).collect()).unwrap_or_default();
        let (selected, custom): (Vec<String>, Vec<String>) = picked.into_iter().partition(|p| labels.contains(&p.as_str()));
        let mut a = json!({ "id": id, "selected": selected });
        if !custom.is_empty() {
            a["custom"] = json!(custom.join(", "));
        }
        out.push(a);
    }
    json!({ "answers": out })
}

/// 界面的回答 → waterfall 的结果。`None`：不替用户回答（交给 DSH 自己的界面）
pub fn decision(kind: &str, input: &Value, req: &PermissionAnswer) -> Result<Option<Value>, LocalError> {
    // 计划卡片被拒（例如停止）：接着做计划
    if kind == "plan" {
        return match req.decision.as_str() {
            "deny" => Ok(Some(plan_result(input, false, None))),
            _ => Err(LocalError::invalid("decision", "invalid")),
        };
    }
    if kind == "question" {
        return match req.decision.as_str() {
            "answer" => Ok(Some(answers_result(input, req.answers.as_ref().filter(|a| a.is_object()).ok_or_else(|| LocalError::invalid("answers", "required"))?))),
            // 不回答：每个问题都是空的，DSH 照常往下走
            "deny" => Ok(Some(answers_result(input, &json!({})))),
            _ => Err(LocalError::invalid("decision", "invalid")),
        };
    }
    match req.decision.as_str() {
        "allow" => Ok(Some(json!("allowed-once"))),
        "deny" => Ok(Some(json!("rejected"))),
        _ => Err(LocalError::invalid("decision", "invalid")),
    }
}

/// DSH 的计划确认（`intent.kind == "plan-review"` 的提问）→ 计划卡片的内容：计划正文、两个选项的原文
fn plan_review(req: &Value) -> Option<Value> {
    let q = req["questions"].as_array().filter(|a| a.len() == 1)?.first()?;
    if q["intent"]["kind"] != "plan-review" {
        return None;
    }
    let approve = q["intent"]["approve"].as_str()?;
    let keep = q["options"].as_array()?.iter().filter_map(|o| o["label"].as_str()).find(|l| *l != approve)?;
    Some(json!({ "plan": q["detail"].as_str().unwrap_or_default(), "id": q["id"], "approve": approve, "keep": keep }))
}

/// 计划卡片的回答：批准，或者接着做计划（可以带意见）
pub fn plan_result(input: &Value, approve: bool, feedback: Option<&str>) -> Value {
    let label = if approve { &input["approve"] } else { &input["keep"] };
    let mut a = json!({ "id": input["id"], "selected": [label] });
    if let Some(f) = feedback.filter(|_| !approve) {
        a["custom"] = json!(f);
    }
    json!({ "answers": [a] })
}

/// 一张图片最大多少字节（DSH 自己会缩小过大的图，这里只挡住明显不对的）
const IMAGE_LIMIT: u64 = 20 << 20;

/// 发给 DSH 的内容：文字和图片（图片直接带 base64，DSH 存成附件，见 docs/DSH_UI.md §4）
fn prompt_content(text: &str, images: &[String]) -> Result<Value, LocalError> {
    use base64::Engine;
    let mut parts = vec![];
    if !text.trim().is_empty() || images.is_empty() {
        parts.push(json!({ "type": "text", "text": text }));
    }
    for p in images {
        let path = std::path::Path::new(p);
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
        let media = image_type(&ext).ok_or_else(|| LocalError::invalid("images", "unsupported"))?;
        if std::fs::metadata(path).map_err(|_| LocalError::invalid("images", "not_found"))?.len() > IMAGE_LIMIT {
            return Err(LocalError::invalid("images", "too_large"));
        }
        let data = std::fs::read(path).map_err(|_| LocalError::invalid("images", "not_found"))?;
        let name: String = path.file_name().and_then(|n| n.to_str()).unwrap_or("image").chars().take(200).collect();
        parts.push(json!({ "type": "image", "mediaType": media, "data": base64::engine::general_purpose::STANDARD.encode(data), "name": name }));
    }
    Ok(Value::Array(parts))
}

/// DSH 收的图片类型
fn image_type(ext: &str) -> Option<&'static str> {
    match ext {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "webp" => Some("image/webp"),
        "gif" => Some("image/gif"),
        _ => None,
    }
}

impl Chat {
    pub(super) fn is_dsh(&self, id: &str) -> bool {
        self.inst(id).agent.launch() == "dsh"
    }

    fn dsh_inst_of(&self, session: &str) -> Option<Arc<Inst>> {
        let id = lock(&self.dsh.sessions).get(session).cloned()?;
        Some(self.inst(&id))
    }

    /// 连上 `dsh web`（已经连着就直接用）
    async fn dsh_conn(&self) -> Result<Arc<DshConn>, LocalError> {
        let mut slot = self.dsh.conn.lock().await;
        if let Some(c) = slot.as_ref().filter(|c| c.alive()) {
            return Ok(c.clone());
        }
        let login = match dsh_app::running().await {
            Some(l) => l,
            None => dsh_app::start(&self.paths.home.join("run")).await?,
        };
        let (tx, rx) = mpsc::unbounded_channel();
        let conn = DshConn::connect(login.port, login.cookie, tx).await?;
        let gen = self.dsh.generation.fetch_add(1, Ordering::SeqCst) + 1;
        *slot = Some(conn.clone());
        drop(slot);
        if let Some(me) = self.me.upgrade() {
            tokio::spawn(me.clone().dsh_events(rx, gen));
            match conn.open("session/control", json!({})) {
                Ok((_, rx)) => {
                    tokio::spawn(me.dsh_control(rx));
                }
                Err(e) => tracing::debug!(error = ?e, "DSH 的 session/control 开不了"),
            }
        }
        tracing::info!(port = conn.port, "已连上 DSH");
        Ok(conn)
    }

    /// 这个终端的会话在跟读了（没有会话就新建一个）
    pub(super) async fn dsh_attach(&self, id: &str) -> Result<Arc<DshConn>, LocalError> {
        let conn = self.dsh_conn().await?;
        let gen = self.dsh.generation.load(Ordering::SeqCst);
        let inst = self.inst(id);
        let (session, loaded) = {
            let st = lock(&inst.st);
            (st.binding.as_ref().map(|b| b.session_id.clone()), st.dsh.generation == gen)
        };
        if loaded && session.is_some() {
            return Ok(conn);
        }
        let session = match session {
            Some(s) => s,
            None => self.dsh_new_session(&conn, &inst).await?,
        };
        self.dsh_follow(&conn, &inst, &session, gen)?;
        Ok(conn)
    }

    async fn dsh_new_session(&self, conn: &DshConn, inst: &Inst) -> Result<String, LocalError> {
        let row = self.store.instance(&inst.id)?.ok_or_else(LocalError::not_found)?;
        let r = conn.call("session/create", json!({ "request": { "cwd": row.workdir } })).await?;
        let sid = r["sessionId"].as_str().filter(|s| dsh_log::valid_session(s)).ok_or_else(|| LocalError::new("DSH_REQUEST_FAILED").with("detail", "no sessionId"))?.to_string();
        self.dsh_bind(inst, &sid);
        Ok(sid)
    }

    /// 记下这个终端用的会话（换会话时，原来那个不再跟读）
    fn dsh_bind(&self, inst: &Inst, session: &str) {
        let b = Binding { session_id: session.to_string(), transcript_path: dsh_log::log_path(session) };
        let old = {
            let mut st = lock(&inst.st);
            let old = st.binding.as_ref().map(|b| b.session_id.clone()).filter(|s| s != session);
            if st.binding.as_ref() != Some(&b) {
                let _ = self.store.set(&super::binding_key(&inst.id), &b);
                st.binding = Some(b);
                st.dsh = DshSt::default();
                st.title = None;
                st.context = None;
                st.error = None;
                Self::set_phase(inst, &mut st, Phase::Idle, None);
                Self::emit_status(inst, &st);
            }
            old
        };
        if let Some(old) = old {
            lock(&self.dsh.sessions).remove(&old);
            dsh_log::drop_log(&old);
        }
        lock(&self.dsh.sessions).insert(session.to_string(), inst.id.clone());
        if let Some(p) = lock(&self.dsh.projections).remove(session) {
            let mut st = lock(&inst.st);
            for (k, v) in p.as_object().into_iter().flatten() {
                Self::dsh_projection(&mut st, k, v);
            }
            Self::emit_status(inst, &st);
        }
    }

    /// 开始跟读会话：快照、往前翻、之后的事件和正在输出的回复
    fn dsh_follow(&self, conn: &Arc<DshConn>, inst: &Arc<Inst>, session: &str, gen: u64) -> Result<(), LocalError> {
        if let Some(old) = lock(&self.dsh.follows).remove(&inst.id) {
            conn.cancel(&old);
        }
        let args = json!({ "request": { "address": { "kind": "session", "sessionId": session }, "assistantStream": true, "maxMessages": OPEN_MESSAGES } });
        let (sid, rx) = conn.open("session/follow", args)?;
        lock(&self.dsh.follows).insert(inst.id.clone(), sid);
        lock(&self.dsh.sessions).insert(session.to_string(), inst.id.clone());
        {
            let mut st = lock(&inst.st);
            st.dsh.generation = gen;
            if st.phase == Phase::Detached {
                Self::set_phase(inst, &mut st, Phase::Idle, None);
            }
        }
        if let Some(me) = self.me.upgrade() {
            tokio::spawn(me.dsh_reader(conn.clone(), inst.clone(), session.to_string(), rx, gen));
        }
        Ok(())
    }

    /// 一个会话的跟读流
    async fn dsh_reader(self: Arc<Self>, conn: Arc<DshConn>, inst: Arc<Inst>, session: String, mut rx: mpsc::UnboundedReceiver<StreamMsg>, gen: u64) {
        let log = dsh_log::shared(&session);
        while let Some(m) = rx.recv().await {
            let v = match m {
                StreamMsg::Item(v) => v,
                StreamMsg::Error(e) => {
                    tracing::debug!(error = %e, session, "DSH 跟读出错");
                    let mut st = lock(&inst.st);
                    st.error = e["message"].as_str().map(|s| s.chars().take(500).collect());
                    st.dsh.generation = 0;
                    Self::set_phase(&inst, &mut st, Phase::Detached, Some("dsh_follow"));
                    return;
                }
                StreamMsg::End | StreamMsg::Closed => break,
            };
            match v["type"].as_str().unwrap_or("") {
                "snapshot" => {
                    let records: Vec<Value> = v["records"].as_array().map(|a| a.iter().map(|r| r["event"].clone()).collect()).unwrap_or_default();
                    let running = running_in(&records);
                    lock(&log).seed(records);
                    {
                        let mut st = lock(&inst.st);
                        st.dsh.cwd = v["header"]["cwd"].as_str().map(String::from);
                        if st.phase != Phase::Approval {
                            Self::set_phase(&inst, &mut st, if running { Phase::Running } else { Phase::Idle }, None);
                        }
                        Self::emit_status(&inst, &st);
                    }
                    if v["hasMore"] == true {
                        if let Some(cursor) = v["cursor"].as_i64() {
                            tokio::spawn(older(conn.clone(), session.clone(), cursor));
                        }
                    }
                }
                "event" => self.dsh_event(&inst, &log, &v["event"]),
                "assistant-stream" => self.dsh_stream(&inst, &v["frame"]),
                _ => {}
            }
        }
        // 流断了（连接断了、DSH 重启）：下次打开、下次跟读时重新开
        let mut st = lock(&inst.st);
        if st.dsh.generation == gen {
            st.dsh.generation = 0;
        }
    }

    fn dsh_event(&self, inst: &Arc<Inst>, log: &Arc<Mutex<dsh_log::DshLog>>, e: &Value) {
        lock(log).event(e);
        let d = &e["data"];
        let mut st = lock(&inst.st);
        st.active_at = crate::util::system_ms();
        match e["type"].as_str().unwrap_or("") {
            "turn/start" => {
                st.error = None;
                inst.prompts.send_modify(|n| *n += 1);
                Self::set_phase(inst, &mut st, Phase::Running, None);
            }
            "turn/end" => {
                Self::drop_pending(&mut st);
                st.dsh.preview.clear();
                match d["reason"]["kind"].as_str().unwrap_or("") {
                    "aborted" => st.end_hint = Some(super::board::Outcome::Interrupted),
                    "completed" => {}
                    other => st.error = Some(d["reason"]["message"].as_str().unwrap_or(other).chars().take(500).collect()),
                }
                st.last_reply = lock(log).last_reply();
                Self::set_phase(inst, &mut st, Phase::Idle, None);
                Self::emit_status(inst, &st);
            }
            "assistant/message" => {
                st.actual_model = d["message"]["source"]["model"].as_str().map(String::from).or(st.actual_model.take());
                st.dsh.preview.clear();
                st.live.text = None;
                st.live.activity = None;
                Self::emit_status(inst, &st);
            }
            "tool/call" => {
                st.tool = d["name"].as_str().map(|n| dsh_tool_label(n).to_string());
                Self::emit_status(inst, &st);
            }
            "tool/result" => {
                st.tool = None;
                Self::emit_status(inst, &st);
            }
            "session/title" => {
                st.title = d["title"].as_str().filter(|t| !t.is_empty()).map(String::from);
                Self::emit_status(inst, &st);
            }
            "llm/retry" => {
                // 和 Codex 的重连提示同一个说法（界面认 `Reconnecting… n/m`）
                st.live.activity = Some(format!("Reconnecting... {}/{}", d["retry"].as_u64().unwrap_or(0), d["maxRetries"].as_u64().unwrap_or(0)));
                Self::emit_status(inst, &st);
            }
            "llm/retry-started" => {}
            _ => {}
        }
    }

    /// 正在输出的回复（只用文字部分；思考、工具参数等写完了从事件里看）
    fn dsh_stream(&self, inst: &Arc<Inst>, f: &Value) {
        let mut st = lock(&inst.st);
        match f["type"].as_str().unwrap_or("") {
            "start" => {
                st.dsh.preview.clear();
                st.live.activity = None;
            }
            "chunk" => {
                let c = &f["chunk"];
                match c["type"].as_str().unwrap_or("") {
                    "text-delta" => st.dsh.preview.push_str(c["text"].as_str().unwrap_or("")),
                    "reasoning-delta" if st.dsh.preview.is_empty() => st.live.activity = Some("thinking".into()),
                    "block-start" if c["blockType"] == "tool-call" => st.live.activity = None,
                    _ => return,
                }
                let n = st.dsh.preview.chars().count();
                if n > PREVIEW_LIMIT {
                    st.dsh.preview = st.dsh.preview.chars().skip(n - PREVIEW_LIMIT).collect();
                }
                if st.dsh.preview_at.is_some_and(|t| t.elapsed() < PREVIEW_GAP) {
                    return;
                }
                st.dsh.preview_at = Some(Instant::now());
                st.live.text = (!st.dsh.preview.is_empty()).then(|| st.dsh.preview.clone());
            }
            "end" => {
                st.dsh.preview.clear();
                st.live.text = None;
            }
            _ => return,
        }
        Self::emit_status(inst, &st);
    }

    /// `session/control`：每个会话的投影（标题、用量、上下文、排队、模型、目录）
    async fn dsh_control(self: Arc<Self>, mut rx: mpsc::UnboundedReceiver<StreamMsg>) {
        while let Some(StreamMsg::Item(v)) = rx.recv().await {
            match v["type"].as_str().unwrap_or("") {
                "baseline" => {
                    for (sid, p) in v["value"]["projections"].as_object().into_iter().flatten() {
                        for (k, val) in p["values"].as_object().into_iter().flatten() {
                            self.dsh_project(sid, k, val);
                        }
                    }
                }
                "projection" => {
                    if let (Some(sid), Some(k)) = (v["sessionId"].as_str(), v["key"].as_str()) {
                        self.dsh_project(sid, k, &v["value"]);
                    }
                }
                _ => {}
            }
        }
    }

    fn dsh_project(&self, session: &str, key: &str, val: &Value) {
        if !matches!(key, "title" | "tokenUsage" | "contextPressure" | "inbox" | "modelSelection" | "workingDirectory" | "plan") {
            return;
        }
        let Some(inst) = self.dsh_inst_of(session) else {
            // 还没对上终端：先记下（数量有上限，只留会用到的几项）
            let mut p = lock(&self.dsh.projections);
            if p.len() < 500 || p.contains_key(session) {
                p.entry(session.to_string()).or_insert_with(|| json!({}))[key] = val.clone();
            }
            return;
        };
        let mut st = lock(&inst.st);
        if Self::dsh_projection(&mut st, key, val) {
            Self::emit_status(&inst, &st);
        }
    }

    /// 投影 → 状态；返回有没有变
    fn dsh_projection(st: &mut super::State, key: &str, val: &Value) -> bool {
        match key {
            "title" => {
                let t = val.as_str().filter(|t| !t.is_empty()).map(String::from);
                std::mem::replace(&mut st.title, t.clone()) != t
            }
            "tokenUsage" => {
                st.dsh.usage = Some(val.clone());
                false
            }
            "contextPressure" => {
                st.dsh.window = val["contextWindow"].as_u64().or(st.dsh.window);
                let c = val["pressureTokens"].as_u64();
                c.is_some() && std::mem::replace(&mut st.context, c) != c
            }
            // 计划模式（`/plan` 开关）：界面上显示成模式 plan
            "plan" => {
                let m = (val["active"] == true).then(|| "plan".to_string());
                std::mem::replace(&mut st.mode, m.clone()) != m
            }
            "inbox" => {
                let q: Vec<String> = ["next-step", "next-turn"]
                    .iter()
                    .flat_map(|k| val[*k].as_array().cloned().unwrap_or_default())
                    .filter(|m| m["source"]["kind"] == "user")
                    .map(|m| dsh_log_text(&m["content"]))
                    .collect();
                std::mem::replace(&mut st.dsh.queued, q.clone()) != q
            }
            "modelSelection" => {
                let next = &val["next"];
                if st.model_set_at.is_none() || model_value(next).is_some() {
                    st.model = model_value(next);
                    st.effort = next["reasoningEffort"].as_str().map(String::from);
                }
                st.actual_model = val["lastUsed"]["model"].as_str().map(String::from).or(st.actual_model.take());
                true
            }
            "workingDirectory" => {
                st.dsh.cwd = val.as_str().map(String::from);
                false
            }
            _ => false,
        }
    }

    /// `$events`：运行状态、出错、审批、提问
    async fn dsh_events(self: Arc<Self>, mut rx: mpsc::UnboundedReceiver<Incoming>, gen: u64) {
        while let Some(m) = rx.recv().await {
            match m {
                Incoming::Emit { event, args } => {
                    let Some(inst) = args.first().and_then(Value::as_str).and_then(|s| self.dsh_inst_of(s)) else { continue };
                    let mut st = lock(&inst.st);
                    match event.as_str() {
                        "api-session/status" => match args.get(1).and_then(Value::as_bool) {
                            Some(true) if st.phase == Phase::Idle => Self::set_phase(&inst, &mut st, Phase::Running, None),
                            Some(false) if st.phase == Phase::Running => Self::set_phase(&inst, &mut st, Phase::Idle, None),
                            _ => {}
                        },
                        "api-session/error" => {
                            st.error = args.get(1).and_then(Value::as_str).map(|s| s.chars().take(500).collect());
                            Self::emit_status(&inst, &st);
                        }
                        _ => {}
                    }
                }
                Incoming::Waterfall { event, event_id, agent, request } => {
                    let me = self.clone();
                    tokio::spawn(async move { me.dsh_waterfall(&event, &event_id, &agent, request).await });
                }
                Incoming::Closed => break,
            }
        }
        // 连接断了：这条连接上开的跟读都要重开；挂着的审批作废（DSH 那边重连后会重新发）
        if self.dsh.generation.load(Ordering::SeqCst) != gen {
            return;
        }
        let ids: Vec<String> = lock(&self.dsh.sessions).values().cloned().collect();
        lock(&self.dsh.follows).clear();
        for id in ids {
            let inst = self.inst(&id);
            let mut st = lock(&inst.st);
            Self::drop_pending(&mut st);
            st.dsh.generation = 0;
            if st.phase != Phase::Idle && st.phase != Phase::Detached {
                st.error = Some("DSH disconnected".into());
                Self::set_phase(&inst, &mut st, Phase::Idle, None);
            }
        }
    }

    async fn dsh_waterfall(&self, event: &str, event_id: &str, agent: &str, request: Value) {
        let Some(conn) = self.dsh.conn.lock().await.clone() else { return };
        let inst = self.dsh_inst_of(agent);
        let (tool, kind, input) = match (event, &inst) {
            (_, None) => {
                // 不是看看的会话：交给 DSH 自己的界面
                let _ = conn.reply(event_id, None).await;
                return;
            }
            ("approval/request", Some(_)) => {
                let call = request["callId"].as_str().unwrap_or_default();
                let log = dsh_log::shared(agent);
                let mut input = lock(&log).input_of(call).filter(Value::is_object).unwrap_or_else(|| json!({}));
                // DSH 给了中英文两种说明：都带上，界面按语言挑
                input["description"] = request["reason"].clone();
                input["display_reason"] = request["displayReason"].clone();
                (dsh_tool_label(request["toolName"].as_str().unwrap_or("")).to_string(), "permission", input)
            }
            // 计划写完了：DSH 用一个“批准计划吗”的提问来问，换成看看的计划卡片
            ("user-questions/request", Some(_)) if plan_review(&request).is_some() => ("ExitPlanMode".to_string(), "plan", plan_review(&request).expect("checked")),
            ("user-questions/request", Some(_)) => ("AskUserQuestion".to_string(), "question", questions_view(&request)),
            _ => {
                let _ = conn.reply(event_id, None).await;
                return;
            }
        };
        let inst = inst.expect("checked");
        // 命令规则（docs/PLAN_2026-10-27.md §3.2）：拒绝的替用户拒绝
        let guard = match input["command"].as_str().filter(|_| tool == "Bash").and_then(|c| crate::guard::check(&crate::guard::load(&self.store), c)) {
            Some(hit) if hit.action == crate::guard::Action::Deny => {
                let _ = conn.reply(event_id, Some(json!("rejected"))).await;
                return;
            }
            hit => hit.map(|h| h.rule),
        };
        let (tx, rx) = oneshot::channel();
        let pid = self.next_id.fetch_add(1, Ordering::Relaxed);
        {
            let mut st = lock(&inst.st);
            Self::drop_pending(&mut st);
            st.pending = Some(Pending { id: pid, at: Instant::now(), tool, kind, input, suggestions: Value::Null, guard, tx: Some(tx) });
            Self::enter(&mut st, Phase::Approval);
            st.phase = Phase::Approval;
            st.reason = None;
            Self::emit_status(&inst, &st);
        }
        // 一直等到界面回答、这一轮停了、或者连接断了；没答（被顶掉、停了）就交给 DSH 自己的界面
        let out = rx.await.unwrap_or_default();
        let value = serde_json::from_str::<Value>(&out).ok();
        if let Err(e) = conn.reply(event_id, value).await {
            tracing::debug!(error = ?e, "DSH 的审批答复没送到");
        }
    }

    // ---------------- 界面的操作 ----------------

    pub(super) async fn dsh_input(&self, id: &str, text: &str, images: &[String]) -> Result<Value, LocalError> {
        let inst = self.inst(id);
        let phase = lock(&inst.st).phase;
        if phase == Phase::Approval {
            return Err(LocalError::new("CHAT_BUSY"));
        }
        let trimmed = text.trim();
        let resume_to = trimmed.strip_prefix("/resume ").map(str::trim).filter(|s| !s.is_empty());
        if resume_to.is_some() || matches!(trimmed, "/clear" | "/new") {
            if phase == Phase::Running {
                return Err(LocalError::new("CHAT_RUNNING"));
            }
            let conn = self.dsh_conn().await?;
            let gen = self.dsh.generation.load(Ordering::SeqCst);
            let session = match resume_to {
                Some(s) if dsh_log::valid_session(s) => s.to_string(),
                Some(_) => return Err(LocalError::invalid("resume", "invalid")),
                None => self.dsh_new_session(&conn, &inst).await?,
            };
            self.dsh_bind(&inst, &session);
            self.dsh_follow(&conn, &inst, &session, gen)?;
            let _ = inst.tx.send(super::Event::Reset);
            return Ok(json!({ "delivered": true }));
        }
        let conn = self.dsh_attach(id).await?;
        let session = lock(&inst.st).binding.as_ref().map(|b| b.session_id.clone()).ok_or_else(LocalError::not_found)?;
        // 斜杠命令（`/compact` 这些）：DSH 自己执行
        if images.is_empty() && trimmed.starts_with('/') && !trimmed.contains('\n') {
            conn.call("commands/execute", json!({ "agentId": session, "line": trimmed, "submittedAttachments": [] })).await?;
            return Ok(json!({ "delivered": true }));
        }
        let running = lock(&inst.st).phase == Phase::Running;
        if !running {
            self.checkpoint_turn(id, text, true).await;
        }
        let content = tokio::task::block_in_place(|| prompt_content(text, images))?;
        let rid = format!("ll-{}", crate::util::random_secret());
        let req = json!({ "requestId": rid, "sessionId": session, "mode": if running { "steer" } else { "queue" }, "content": content });
        // 当前模型不收图片：DSH 整条拒收，说清楚是模型的问题（界面会把输入框里的字放回来）
        conn.call("session/prompt", json!({ "request": req })).await.map_err(|e| {
            if !images.is_empty() && e.params.get("code").and_then(Value::as_str) == Some("session/attachment-invalid") {
                return LocalError::new("DSH_IMAGES_UNSUPPORTED");
            }
            e
        })?;
        let mut st = lock(&inst.st);
        st.error = None;
        Self::set_phase(&inst, &mut st, Phase::Running, None);
        Ok(json!({ "delivered": true }))
    }

    /// 用户消息里的第 `n` 张图：记录里只有附件 id，内容找 DSH 要（`session/attachment`）
    pub(super) async fn dsh_user_image(&self, id: &str, rec: &Value, n: usize) -> Result<(&'static str, Vec<u8>), LocalError> {
        use base64::Engine;
        let img = rec["data"]["content"].as_array().into_iter().flatten().filter(|c| c["type"] == "image").nth(n).ok_or_else(LocalError::not_found)?;
        let attachment = img["attachment"]["attachmentId"].as_str().ok_or_else(LocalError::not_found)?;
        let conn = self.dsh_attach(id).await?;
        let session = lock(&self.inst(id).st).binding.as_ref().map(|b| b.session_id.clone()).ok_or_else(LocalError::not_found)?;
        let r = conn.call("session/attachment", json!({ "request": { "sessionId": session, "attachmentId": attachment } })).await?;
        let media = r["attachment"]["mediaType"].as_str().and_then(|m| image_type(m.trim_start_matches("image/"))).ok_or_else(LocalError::not_found)?;
        let data = base64::engine::general_purpose::STANDARD.decode(r["data"].as_str().unwrap_or_default()).map_err(|_| LocalError::not_found())?;
        Ok((media, data))
    }

    /// 用量栏：DSH 推来的累计 token 和上下文占用，没有订阅额度
    pub(super) fn dsh_usage(&self, id: &str) -> Value {
        let inst = self.inst(id);
        let st = lock(&inst.st);
        let u = st.dsh.usage.clone().unwrap_or(Value::Null);
        let totals = json!({
            "input": u["uncachedInputTokens"].as_u64().unwrap_or(0) + u["cacheWriteTokens"].as_u64().unwrap_or(0),
            "cache_read": u["cacheReadTokens"].as_u64().unwrap_or(0),
            "output": u["outputTokens"].as_u64().unwrap_or(0),
        });
        json!({ "model": st.actual_model.clone().or(st.model.clone()), "context": st.context, "window": st.dsh.window, "totals": totals, "limits": null, "version": null })
    }

    /// 斜杠命令菜单：DSH 自己的命令（`/compact`、`/plan`、`/permission`……）
    pub(super) async fn dsh_commands(&self, id: &str) -> Result<Value, LocalError> {
        let conn = self.dsh_attach(id).await?;
        let Some(session) = lock(&self.inst(id).st).binding.as_ref().map(|b| b.session_id.clone()) else { return Ok(json!({ "items": [] })) };
        let list = conn.call("commands/list", json!({ "agentId": session })).await?;
        let items: Vec<Value> = list
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|c| Some(json!({ "name": format!("/{}", c["name"].as_str()?), "description": c["description"], "hint": c["input"]["hint"] })))
            .collect();
        Ok(json!({ "items": items }))
    }

    /// 计划卡片：1、2 都是批准（DSH 不分自动、手动），3 是带着意见接着做计划
    pub(super) fn dsh_plan(&self, id: &str, choice: u8, feedback: Option<&str>) -> Result<(), LocalError> {
        let inst = self.inst(id);
        let mut st = lock(&inst.st);
        let Some(p) = st.pending.as_mut().filter(|p| p.kind == "plan") else { return Err(LocalError::new("CHAT_STALE")) };
        let result = match choice {
            1 | 2 => plan_result(&p.input, true, None),
            3 if feedback.is_some() => plan_result(&p.input, false, feedback),
            _ => return Err(LocalError::invalid("choice", "invalid")),
        };
        p.resolve(result.to_string());
        st.pending = None;
        Self::set_phase(&inst, &mut st, Phase::Running, None);
        Ok(())
    }

    pub(super) async fn dsh_interrupt(&self, id: &str) -> Result<(), LocalError> {
        let inst = self.inst(id);
        if !matches!(lock(&inst.st).phase, Phase::Running | Phase::Approval) {
            return Ok(());
        }
        let conn = self.dsh_attach(id).await?;
        let Some(session) = lock(&inst.st).binding.as_ref().map(|b| b.session_id.clone()) else { return Ok(()) };
        conn.call("session/cancel", json!({ "request": { "sessionId": session } })).await?;
        Ok(())
    }

    /// 模型、思考强度：DSH 记在会话里，之后的轮次都用它
    pub(super) async fn dsh_setting(&self, id: &str, kind: &str, value: &str) -> Result<(), LocalError> {
        let inst = self.inst(id);
        // 换模型时思考强度用新模型的默认值（不同模型能选的不一样）
        let model = lock(&inst.st).model.clone();
        let (model, effort) = match kind {
            "model" => (Some(value.to_string()), None),
            "effort" => (model, Some(value.to_string())),
            _ => return Err(LocalError::invalid("kind", "invalid")),
        };
        let Some((provider, name)) = model.as_deref().and_then(|m| m.split_once('/')) else { return Err(LocalError::invalid("value", "invalid")) };
        let ok = |s: &str| !s.is_empty() && s.len() <= 80 && s.chars().all(|c| c.is_ascii_alphanumeric() || "._-:".contains(c));
        if !ok(provider) || !ok(name) || effort.as_deref().is_some_and(|e| !ok(e)) {
            return Err(LocalError::invalid("value", "invalid"));
        }
        let conn = self.dsh_attach(id).await?;
        let session = lock(&inst.st).binding.as_ref().map(|b| b.session_id.clone()).ok_or_else(LocalError::not_found)?;
        let mut req = json!({ "sessionId": session, "provider": provider, "model": name });
        if let Some(e) = &effort {
            req["reasoningEffort"] = json!(e);
        }
        let r = conn.call("session/selectModel", json!({ "request": req })).await?;
        let mut st = lock(&inst.st);
        st.model = model_value(&r["selected"]).or(model);
        st.effort = r["selected"]["reasoningEffort"].as_str().map(String::from).or(effort);
        st.model_set_at = Some(st.model_msg.clone());
        Self::emit_status(&inst, &st);
        Ok(())
    }

    pub(super) async fn dsh_models(&self, id: &str) -> Result<Value, LocalError> {
        let cached = lock(&self.dsh.models).as_ref().filter(|(t, _)| t.elapsed() < MODELS_TTL).map(|(_, v)| v.clone());
        let options = match cached {
            Some(v) => v,
            None => {
                let conn = self.dsh_conn().await?;
                let cat = conn.call("session/modelCatalog", json!({})).await?;
                let v = json!(model_options(&cat));
                *lock(&self.dsh.models) = Some((Instant::now(), v.clone()));
                v
            }
        };
        let inst = self.inst(id);
        let st = lock(&inst.st);
        Ok(json!({ "options": options, "provider": null, "model": st.model, "actual_model": st.actual_model, "effort": st.effort }))
    }

    /// 有人看着的时候：连接断了（DSH 重启了）就重新接上
    pub(super) async fn dsh_keep(&self, id: &str) {
        let inst = self.inst(id);
        let gen = self.dsh.generation.load(Ordering::SeqCst);
        if lock(&inst.st).dsh.generation == gen && gen != 0 && self.dsh.conn.lock().await.as_ref().is_some_and(|c| c.alive()) {
            return;
        }
        if let Err(e) = self.dsh_attach(id).await {
            let mut st = lock(&inst.st);
            st.error = e.params.get("detail").and_then(Value::as_str).map(String::from).or(Some(e.code.to_string()));
            Self::set_phase(&inst, &mut st, Phase::Detached, Some("dsh_unavailable"));
        }
    }

    /// 终端删了：不再跟读（DSH 里的会话留着）
    pub(super) async fn dsh_forget(&self, id: &str, session: Option<String>) {
        let follow = lock(&self.dsh.follows).remove(id);
        if let Some(sid) = follow {
            if let Some(c) = self.dsh.conn.lock().await.as_ref() {
                c.cancel(&sid);
            }
        }
        if let Some(s) = session {
            lock(&self.dsh.sessions).remove(&s);
            dsh_log::drop_log(&s);
        }
    }
}

/// 状态栏上显示的工具名（和对话里的卡片一样）
fn dsh_tool_label(name: &str) -> &str {
    dsh_log::tool_name(name)
}

fn dsh_log_text(content: &Value) -> String {
    content.as_array().into_iter().flatten().filter(|c| c["type"] == "text").filter_map(|c| c["text"].as_str()).collect::<Vec<_>>().join("\n")
}

/// 快照里最后一轮还没结束：正在运行
fn running_in(records: &[Value]) -> bool {
    records.iter().rev().find_map(|e| match e["type"].as_str() {
        Some("turn/start") => Some(true),
        Some("turn/end") => Some(false),
        _ => None,
    }) == Some(true)
}

/// 往前翻：快照之前的记录，一页一页取，放进记录里（界面往上翻时就有了）
async fn older(conn: Arc<DshConn>, session: String, through: i64) {
    let log = dsh_log::shared(&session);
    let mut fetched = 0;
    while fetched < HISTORY_MAX {
        let Some(before) = lock(&log).first_seq() else { return };
        let req = json!({ "address": { "kind": "session", "sessionId": session }, "throughSeq": through, "beforeSeq": before, "maxMessages": OPEN_MESSAGES });
        let page = match conn.call("session/page", json!({ "request": req })).await {
            Ok(p) => p,
            Err(e) => {
                tracing::debug!(error = ?e, "DSH 往前翻失败");
                return;
            }
        };
        let records: Vec<Value> = page["records"].as_array().map(|a| a.iter().map(|r| r["event"].clone()).collect()).unwrap_or_default();
        if records.is_empty() {
            return;
        }
        fetched += records.len();
        lock(&log).seed(records);
        if page["hasMore"] != true {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn models_from_catalog() {
        let cat = json!({ "default": { "provider": "deepseek-official", "model": "deepseek-flash" }, "groups": [{ "id": "deepseek-official", "name": "DeepSeek", "models": [
            { "id": "deepseek-flash", "name": "DeepSeek-V4.1-Flash", "reasoning": { "efforts": [{ "id": "off" }, { "id": "high" }], "defaultEffort": "high" } },
            { "id": "deepseek-v4-pro", "name": "DeepSeek-V4-Pro", "description": "Stronger" }
        ] }] });
        let o = model_options(&cat);
        assert_eq!(o.len(), 2);
        assert_eq!(o[0]["value"], "deepseek-official/deepseek-flash");
        assert_eq!(o[0]["efforts"], json!(["off", "high"]));
        assert_eq!(o[0]["default"], true);
        assert_eq!(o[1]["label"], "DeepSeek-V4-Pro");
    }

    #[test]
    fn question_answers() {
        let req = json!({ "questions": [{ "id": "q1", "question": "用哪个数据库？", "header": "数据库", "options": [{ "label": "Postgres" }, { "label": "SQLite" }] }] });
        let input = questions_view(&req);
        assert_eq!(input["questions"][0]["multiSelect"], false);
        let a = answers_result(&input, &json!({ "用哪个数据库？": "SQLite" }));
        assert_eq!(a, json!({ "answers": [{ "id": "q1", "selected": ["SQLite"] }] }));
        let a = answers_result(&input, &json!({ "用哪个数据库？": "MySQL" }));
        assert_eq!(a, json!({ "answers": [{ "id": "q1", "selected": [], "custom": "MySQL" }] }), "不是选项里的算自己写的");
    }

    #[test]
    fn running_from_snapshot() {
        let e = |t: &str| json!({ "type": t });
        assert!(running_in(&[e("turn/end"), e("turn/start"), e("user/message")]));
        assert!(!running_in(&[e("turn/start"), e("turn/end")]));
        assert!(!running_in(&[]));
    }

    #[test]
    fn plan_review_card() {
        // DSH 0.2.1-rc.4 写完计划时问的
        let req = json!({ "questions": [{ "id": "plan-review", "header": "Plan review", "question": "Approve this plan and leave plan mode?", "detail": "# 计划\n1. 改 a",
            "options": [{ "label": "Approve" }, { "label": "Keep planning" }], "intent": { "kind": "plan-review", "approve": "Approve", "callId": "tu_2" } }] });
        let input = plan_review(&req).expect("认得出计划确认");
        assert_eq!(input["plan"], "# 计划\n1. 改 a");
        assert_eq!(plan_result(&input, true, Some("忽略")), json!({ "answers": [{ "id": "plan-review", "selected": ["Approve"] }] }));
        assert_eq!(plan_result(&input, false, Some("第二步换掉")), json!({ "answers": [{ "id": "plan-review", "selected": ["Keep planning"], "custom": "第二步换掉" }] }));
        assert!(plan_review(&json!({ "questions": [{ "id": "q", "question": "?", "options": [] }] })).is_none(), "普通提问不是计划");
    }

    #[test]
    fn prompt_with_images() {
        let dir = std::env::temp_dir().join(format!("ll-dsh-img-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let png = dir.join("a.PNG");
        std::fs::write(&png, b"abc").unwrap();
        let c = prompt_content("看图", &[png.display().to_string()]).unwrap();
        assert_eq!(c[0], json!({ "type": "text", "text": "看图" }));
        assert_eq!(c[1], json!({ "type": "image", "mediaType": "image/png", "data": "YWJj", "name": "a.PNG" }));
        // 只有图片：不带空的文字
        assert_eq!(prompt_content("", &[png.display().to_string()]).unwrap().as_array().unwrap().len(), 1);
        let bmp = dir.join("b.bmp");
        std::fs::write(&bmp, b"x").unwrap();
        assert!(prompt_content("", &[bmp.display().to_string()]).is_err(), "DSH 不收的类型");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
