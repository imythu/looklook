//! DSH 会话的记录（docs/DSH_UI.md §3）：`session/follow` 推来的事件 → 对话界面的条目。
//!
//! DSH 的记录文件是压缩过的、按版本迁移的内部格式，不直接读；内容全部来自它的公开接口：打开时的快照
//! （加上 `session/page` 往前翻的），之后是一条条事件。事件存在内存里（[`DshLog`]，按会话共用），
//! 对话界面的跟读（[`TranscriptReader::poll`]）取走新整理出来的条目。
//!
//! 只认“显示在对话里”的事件（`user/message` 里用户自己发的、`assistant/message`、`tool/result`、`turn/end`），
//! 其余（运行时上下文、计时、标题、日志投递……）跳过。工具调用翻成 Claude Code 的说法（`bash` → `Bash`，
//! `read` → `Read`……，参数名本来就一样），界面直接复用 Claude 的卡片。DSH 用 `surfaceOp: replace` 改写前面的内容
//! （压缩上下文时）：这时整个重新整理一遍，界面重新取快照。条目的 key 是 `d:<seq>:<序号>`。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use super::agent::TranscriptReader;
use super::transcript::{truncate, Block, Body, Change, Item, Stats, TEXT_LIMIT};

/// 对话界面里显示的事件；别的跳过
const SHOWN: &[&str] = &["user/message", "assistant/message", "tool/result", "turn/start", "turn/end"];

/// DSH 的工具名 → Claude Code 的（参数名一样）
pub fn tool_name(name: &str) -> &str {
    match name {
        "bash" => "Bash",
        "read" => "Read",
        "write" => "Write",
        "edit" => "Edit",
        "glob" => "Glob",
        "grep" => "Grep",
        "web_fetch" => "WebFetch",
        "web_search" => "WebSearch",
        "todo_write" => "TodoWrite",
        "exit_plan_mode" => "ExitPlanMode",
        "ask_user_question" => "AskUserQuestion",
        "job_output" => "BashOutput",
        "job_kill" => "KillShell",
        other => other,
    }
}

/// 记录在内存里的路径：`dsh/<会话>`（不是真的文件，只用来区分会话）
pub fn log_path(session: &str) -> PathBuf {
    PathBuf::from(format!("dsh/{session}"))
}

pub fn session_of(path: &Path) -> Option<&str> {
    path.to_str()?.strip_prefix("dsh/")
}

pub fn valid_session(id: &str) -> bool {
    id.starts_with("session-") && id.len() < 80 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// 一个会话的事件和整理出来的条目
#[derive(Default)]
pub struct DshLog {
    /// 显示用的事件，按 seq 排
    events: Vec<Value>,
    /// 看到过的最大 seq（重连后的快照里重复的跳过）
    last_seq: i64,
    items: Vec<Item>,
    /// 工具调用 id → 参数
    inputs: HashMap<String, Value>,
    /// 这一轮开始的时间、这一轮里已经有过回复（之后再来的用户消息是插话；一起开始这一轮的几条不算）
    turn_at: Option<i64>,
    turn_replied: bool,
    /// 自上次取走以后：从第几条起变了、要不要整个重来
    dirty: Option<usize>,
    /// 收到过快照
    seeded: bool,
    reset: bool,
    stats: Stats,
}

impl DshLog {
    /// 快照或往前翻到的一批（按 seq 从小到大）：比已有的都早的放到前面，整个重新整理
    pub fn seed(&mut self, records: Vec<Value>) {
        self.seeded = true;
        // DSH 给的已经是显示的顺序（改写过的内容在被换掉的位置上，seq 不一定从小到大），不重新排
        let evs: Vec<Value> = records.into_iter().filter(|e| SHOWN.contains(&e["type"].as_str().unwrap_or(""))).collect();
        let first = self.events.iter().filter_map(|e| e["seq"].as_i64()).min().unwrap_or(i64::MAX);
        let (earlier, rest): (Vec<Value>, Vec<Value>) = evs.into_iter().partition(|e| e["seq"].as_i64().is_some_and(|s| s < first));
        let later: Vec<Value> = rest.into_iter().filter(|e| e["seq"].as_i64().is_some_and(|s| s > self.last_seq)).collect();
        if earlier.is_empty() && later.is_empty() {
            return;
        }
        self.last_seq = earlier.iter().chain(&later).filter_map(|e| e["seq"].as_i64()).max().unwrap_or(0).max(self.last_seq);
        let mut events = earlier;
        events.append(&mut self.events);
        events.extend(later);
        self.events = events;
        self.rebuild();
    }

    pub fn seeded(&self) -> bool {
        self.seeded
    }

    /// 最早的一条事件（往前翻时用）
    pub fn first_seq(&self) -> Option<i64> {
        self.events.iter().filter_map(|e| e["seq"].as_i64()).min()
    }

    /// 跟读到的一条事件
    pub fn event(&mut self, e: &Value) {
        let seq = e["seq"].as_i64().unwrap_or(0);
        if seq <= self.last_seq {
            return;
        }
        self.last_seq = seq;
        self.stats.lines += 1;
        let ty = e["type"].as_str().unwrap_or("");
        if !SHOWN.contains(&ty) {
            return;
        }
        // 改写前面的内容（压缩上下文）：被换掉的那段拿掉，整个重新整理
        if let Some((from, to)) = replaced(e) {
            self.events.retain(|x| x["seq"].as_i64().is_none_or(|s| s < from || s > to));
            // 放在被换掉的那段原来的位置
            let at = self.events.iter().position(|x| x["seq"].as_i64().is_some_and(|s| s > to)).unwrap_or(self.events.len());
            self.events.insert(at, e.clone());
            self.rebuild();
            return;
        }
        self.events.push(e.clone());
        let start = self.items.len();
        self.apply(e);
        if self.items.len() > start {
            self.dirty = Some(self.dirty.map_or(start, |d| d.min(start)));
        }
    }

    fn rebuild(&mut self) {
        self.items.clear();
        self.inputs.clear();
        self.turn_at = None;
        self.turn_replied = false;
        for e in std::mem::take(&mut self.events) {
            self.apply(&e);
            self.events.push(e);
        }
        self.reset = true;
        self.dirty = None;
    }

    fn push(&mut self, e: &Value, n: usize, body: Body) {
        let seq = e["seq"].as_i64().unwrap_or(0);
        self.items.push(Item { key: format!("d:{seq}:{n}"), ts: iso(e["time"].as_i64()), body });
    }

    fn apply(&mut self, e: &Value) {
        let d = &e["data"];
        match e["type"].as_str().unwrap_or("") {
            "turn/start" => {
                self.turn_at = e["time"].as_i64();
                self.turn_replied = false;
            }
            "user/message" => {
                // 只要用户自己发的（运行时上下文、时间这些也是 user/message，来源不同）
                if d["source"]["kind"] != "user" {
                    return;
                }
                let (text, images) = content_text(&d["content"]);
                let (text, truncated) = truncate(&text, TEXT_LIMIT);
                self.push(e, 0, Body::User { text, images, truncated, mid: self.turn_replied });
            }
            "assistant/message" => {
                let msg = &d["message"];
                let mut blocks = vec![];
                for b in msg["content"].as_array().into_iter().flatten() {
                    match b["type"].as_str().unwrap_or("") {
                        "text" => {
                            if let Some(t) = b["text"].as_str().filter(|t| !t.is_empty()) {
                                blocks.push(Block::Text { text: t.to_string() });
                            }
                        }
                        "reasoning" => {
                            if let Some(t) = b["text"].as_str().filter(|t| !t.trim().is_empty()) {
                                blocks.push(Block::Thinking { text: t.to_string() });
                            }
                        }
                        "tool-call" => {
                            let id = b["id"].as_str().unwrap_or_default().to_string();
                            let input = match &b["arguments"] {
                                Value::String(s) => serde_json::from_str(s).unwrap_or_else(|_| json!({ "arguments": s })),
                                v => v.clone(),
                            };
                            self.inputs.insert(id.clone(), input.clone());
                            blocks.push(Block::ToolUse { id, name: tool_name(b["name"].as_str().unwrap_or("")).to_string(), input, truncated: false });
                        }
                        other => blocks.push(Block::Unknown { raw_type: other.to_string() }),
                    }
                }
                self.turn_replied = true;
                if !blocks.is_empty() {
                    let model = msg["source"]["model"].as_str().map(String::from);
                    self.push(e, 0, Body::Assistant { model, blocks });
                }
            }
            "tool/result" => {
                let msg = &d["message"];
                let (text, images) = content_text(&msg["content"]);
                let (content, truncated) = truncate(&text, TEXT_LIMIT);
                let id = msg["toolCallId"].as_str().unwrap_or_default().to_string();
                self.push(e, 0, Body::ToolResult { tool_use_id: id, is_error: msg["isError"] == true, content, images, truncated, extra: None });
            }
            "turn/end" => {
                let ms = match (self.turn_at.take(), e["time"].as_i64()) {
                    (Some(a), Some(b)) if b >= a => Some((b - a) as u64),
                    _ => None,
                };
                self.turn_replied = false;
                match d["reason"]["kind"].as_str().unwrap_or("") {
                    "aborted" => self.push(e, 0, Body::Interrupt),
                    "completed" => {}
                    other => {
                        let why = d["reason"]["message"].as_str().or(d["reason"]["error"]["message"].as_str()).unwrap_or(other);
                        self.push(e, 0, Body::Info { text: why.chars().take(500).collect() });
                    }
                }
                if let Some(ms) = ms {
                    self.push(e, 1, Body::Turn { ms });
                }
            }
            _ => {}
        }
    }

    /// 取走变化（对话界面每次跟读时）
    pub fn take(&mut self) -> Change {
        if std::mem::take(&mut self.reset) {
            self.dirty = None;
            return Change::Reset;
        }
        match self.dirty.take() {
            Some(start) => Change::Upsert { start, items: self.items[start.min(self.items.len())..].to_vec() },
            None => Change::None,
        }
    }

    /// 最后一段回复的文字（摘要、提醒用）
    pub fn last_reply(&self) -> Option<String> {
        self.items.iter().rev().find_map(|i| match &i.body {
            Body::Assistant { blocks, .. } => blocks.iter().rev().find_map(|b| if let Block::Text { text } = b { Some(text.clone()) } else { None }),
            _ => None,
        })
    }

    /// 某次工具调用的参数
    pub fn input_of(&self, call: &str) -> Option<Value> {
        self.inputs.get(call).cloned()
    }

    fn raw(&self, key: &str) -> Option<Value> {
        let seq: i64 = key.strip_prefix("d:")?.split(':').next()?.parse().ok()?;
        self.events.iter().find(|e| e["seq"].as_i64() == Some(seq)).cloned()
    }
}

/// `surfaceOp: {op: "replace", startSeq, endSeq}`
fn replaced(e: &Value) -> Option<(i64, i64)> {
    let op = &e["surfaceOp"];
    (op["op"] == "replace").then(|| Some((op["startSeq"].as_i64()?, op["endSeq"].as_i64()?))).flatten()
}

/// 内容块里的文字（拼起来）和图片数
fn content_text(content: &Value) -> (String, u32) {
    let mut text = vec![];
    let mut images = 0;
    for c in content.as_array().into_iter().flatten() {
        match c["type"].as_str().unwrap_or("") {
            "text" => text.extend(c["text"].as_str()),
            "image" => images += 1,
            _ => {}
        }
    }
    (text.join("\n"), images)
}

fn iso(ms: Option<i64>) -> Option<String> {
    let t = time::OffsetDateTime::from_unix_timestamp_nanos(i128::from(ms?) * 1_000_000).ok()?;
    t.format(&time::format_description::well_known::Rfc3339).ok()
}

// ---------------- 按会话共用 ----------------

static LOGS: std::sync::LazyLock<Mutex<HashMap<String, Arc<Mutex<DshLog>>>>> = std::sync::LazyLock::new(Default::default);

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// 这个会话的记录（没有就建一个空的）
pub fn shared(session: &str) -> Arc<Mutex<DshLog>> {
    lock(&LOGS).entry(session.to_string()).or_default().clone()
}

/// 不再跟读这个会话了（换了会话、终端删了）
pub fn drop_log(session: &str) {
    lock(&LOGS).remove(session);
}

/// 对话界面的跟读：读的是 [`shared`] 里的那份
pub struct Reader {
    path: PathBuf,
    log: Arc<Mutex<DshLog>>,
    /// 给界面的条目（取走变化时同步过来，免得每次都锁着共用的那份）
    items: Vec<Item>,
    inputs: HashMap<String, Value>,
    stats: Stats,
}

impl Reader {
    pub fn new(path: PathBuf) -> Self {
        let log = shared(session_of(&path).unwrap_or_default());
        Self { path, log, items: vec![], inputs: HashMap::new(), stats: Stats::default() }
    }
}

impl TranscriptReader for Reader {
    fn path(&self) -> &Path {
        &self.path
    }

    fn poll(&mut self) -> std::io::Result<Change> {
        let mut log = lock(&self.log);
        let change = log.take();
        if !matches!(change, Change::None) || self.items.len() != log.items.len() {
            self.items = log.items.clone();
            self.inputs = log.inputs.clone();
        }
        self.stats = log.stats;
        Ok(change)
    }

    fn len(&self) -> usize {
        self.items.len()
    }

    fn items(&self, from: usize, to: usize) -> Vec<Item> {
        let to = to.min(self.items.len());
        self.items[from.min(to)..to].to_vec()
    }

    fn full_item(&self, key: &str) -> Option<Value> {
        lock(&self.log).raw(key)
    }

    fn tool_input(&self, tool_use_id: &str) -> Option<&Value> {
        self.inputs.get(tool_use_id)
    }

    fn version(&self) -> Option<String> {
        None
    }

    fn stats(&self) -> Stats {
        self.stats
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(seq: i64, ty: &str, data: Value) -> Value {
        json!({ "type": ty, "seq": seq, "time": 1_793_344_752_000i64 + seq * 10, "data": data, "surfaceOp": "append" })
    }

    /// 录下来的一轮（DSH 0.2.1-rc.4）：思考、回复、bash、结果、再回复
    fn turn() -> Vec<Value> {
        vec![
            ev(3, "agent/inbox/spliced", json!({})),
            ev(4, "turn/start", json!({ "turn": 1 })),
            ev(8, "user/message", json!({ "content": [{ "type": "text", "text": "#run" }], "source": { "kind": "user", "rpcId": "r1" }, "role": "user" })),
            ev(9, "user/message", json!({ "content": [{ "type": "text", "text": "Current runtime context." }], "source": { "kind": "runtime-context" }, "role": "user" })),
            ev(16, "assistant/message", json!({ "turn": 1, "step": 1, "message": { "role": "assistant", "content": [
                { "type": "reasoning", "text": "要跑个命令" },
                { "type": "text", "text": "我来运行一下。" },
                { "type": "tool-call", "id": "tu_0", "name": "bash", "arguments": "{\"command\":\"echo hi\",\"description\":\"say hello\"}" }
            ], "source": { "kind": "model", "provider": "deepseek-official", "model": "deepseek-flash" } } })),
            ev(17, "tool/call", json!({ "callId": "tu_0" })),
            ev(22, "tool/result", json!({ "message": { "role": "tool", "toolCallId": "tu_0", "content": [{ "type": "text", "text": "hi\n" }], "isError": false } })),
            ev(26, "assistant/message", json!({ "message": { "content": [{ "type": "text", "text": "好了" }], "source": { "model": "deepseek-flash" } } })),
            ev(28, "turn/end", json!({ "turn": 1, "reason": { "kind": "completed" } })),
        ]
    }

    #[test]
    fn one_turn() {
        let mut log = DshLog::default();
        for e in turn() {
            log.event(&e);
        }
        let Change::Upsert { start: 0, items } = log.take() else { panic!("要有新条目") };
        assert_eq!(items.len(), 5, "{items:#?}");
        assert!(matches!(&items[0].body, Body::User { text, mid: false, .. } if text == "#run"), "运行时上下文不显示");
        let Body::Assistant { model, blocks } = &items[1].body else { panic!() };
        assert_eq!(model.as_deref(), Some("deepseek-flash"));
        assert!(matches!(&blocks[0], Block::Thinking { text } if text == "要跑个命令"));
        assert!(matches!(&blocks[2], Block::ToolUse { id, name, input, .. } if id == "tu_0" && name == "Bash" && input["command"] == "echo hi"));
        assert!(matches!(&items[2].body, Body::ToolResult { tool_use_id, content, is_error: false, .. } if tool_use_id == "tu_0" && content == "hi\n"));
        assert!(matches!(&items[4].body, Body::Turn { ms: 240 }));
        assert_eq!(items[0].key, "d:8:0");
        assert!(matches!(log.take(), Change::None), "取走后没有变化");
        assert_eq!(log.last_reply().as_deref(), Some("好了"));
    }

    #[test]
    fn steer_abort_and_duplicates() {
        let mut log = DshLog::default();
        let mut evs = turn();
        evs.truncate(5);
        evs.push(ev(18, "user/message", json!({ "content": [{ "type": "text", "text": "插一句" }], "source": { "kind": "user" } })));
        evs.push(ev(20, "turn/end", json!({ "reason": { "kind": "aborted", "reason": { "kind": "user" } } })));
        for e in &evs {
            log.event(e);
        }
        // 重连后快照里又来一遍：不重复
        log.seed(evs.clone());
        let _ = log.take();
        let items = log.items.clone();
        assert!(matches!(&items[2].body, Body::User { mid: true, .. }), "回复之后再来的是插话");
        assert!(matches!(&items[3].body, Body::Interrupt));
        assert_eq!(items.len(), 5);
    }

    #[test]
    fn queued_messages_start_the_turn_together() {
        let mut log = DshLog::default();
        let user = |seq, text| ev(seq, "user/message", json!({ "content": [{ "type": "text", "text": text }], "source": { "kind": "user" } }));
        for e in [ev(1, "turn/start", json!({})), user(2, "停止前插的话"), user(3, "你好")] {
            log.event(&e);
        }
        let _ = log.take();
        assert!(log.items.iter().all(|i| matches!(&i.body, Body::User { mid: false, .. })), "一起开始这一轮的不算插话");
    }

    #[test]
    fn seed_earlier_page_and_replace() {
        let mut log = DshLog::default();
        let all = turn();
        log.seed(all[5..].to_vec());
        assert!(matches!(log.take(), Change::Reset));
        assert_eq!(log.first_seq(), Some(22), "tool/call 不显示，不记");
        // 往前翻到的
        log.seed(all[..5].to_vec());
        let _ = log.take();
        assert!(matches!(&log.items[0].body, Body::User { text, .. } if text == "#run"));
        assert_eq!(log.items.len(), 5);
        // 压缩：8..=22 换成一条
        let mut summary = ev(30, "user/message", json!({ "content": [{ "type": "text", "text": "摘要" }], "source": { "kind": "user" } }));
        summary["surfaceOp"] = json!({ "op": "replace", "startSeq": 8, "endSeq": 22 });
        log.event(&summary);
        assert!(matches!(log.take(), Change::Reset));
        assert!(matches!(&log.items[0].body, Body::User { text, .. } if text == "摘要"), "{:#?}", log.items);
    }

    #[test]
    fn paths() {
        let p = log_path("session-c4a0c54d-8671");
        assert_eq!(session_of(&p), Some("session-c4a0c54d-8671"));
        assert!(valid_session("session-c4a0c54d-8671-4b93-8298-65dbd49ce06a"));
        assert!(!valid_session("session-../x") && !valid_session("abc"));
    }
}
