//! Codex 的 rollout（`$CODEX_HOME/sessions/YYYY/MM/DD/rollout-<时间>-<id>.jsonl`）：跟读，并整理成对话界面的条目（docs/CODEX_UI.md §6）。
//!
//! rollout 里每一步写完都有一条 `event_msg: item_completed`，`item` 是结构化的（`UserMessage`、`AgentMessage`、
//! `CommandExecution`、`FileChange`……），比 `response_item` 好用（0.159 起工具调用在 `response_item` 里是一段 JS）。
//! 这里只认这些 item，再加 `task_complete`（一轮用时）、`turn_aborted`（中断）；其余记录跳过。
//!
//! 工具调用翻成 Claude Code 的说法（`Bash`、`Edit`、`Write`、`Read`、`WebSearch`、`mcp__…`），界面直接复用 Claude 的卡片。
//! 文件只追加、不改写，所以条目的 key 用行号：`r:<行号>:<序号>`。

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

use super::agent::TranscriptReader;
use super::transcript::{truncate, Block, Body, Change, Item, Stats, TEXT_LIMIT};

/// 记录里认识、但对话界面不显示的类型
const SKIPPED: &[&str] = &["response_item", "turn_context", "token_usage_record", "world_state", "compacted", "inter_agent_communication_metadata"];
const SKIPPED_EVENTS: &[&str] = &["token_count", "task_started", "thread_settings_applied", "user_message", "agent_message", "patch_apply_end", "web_search_end", "exec_command_end"];
const SKIPPED_ITEMS: &[&str] = &["SubAgentActivity", "CollabAgentToolCall", "Extension"];

pub struct Rollout {
    path: PathBuf,
    pos: u64,
    partial: Vec<u8>,
    /// 已经读过的行数（下一行的行号）
    line: usize,
    items: Vec<Item>,
    /// 工具调用 id → 参数（找子代理等用）
    inputs: HashMap<String, Value>,
    version: Option<String>,
    stats: Stats,
}

impl Rollout {
    pub fn new(path: PathBuf) -> Self {
        Self { path, pos: 0, partial: vec![], line: 0, items: vec![], inputs: HashMap::new(), version: None, stats: Stats::default() }
    }

    /// 喂入新读到的字节（测试直接用）。
    pub fn feed(&mut self, bytes: &[u8]) -> Change {
        self.partial.extend_from_slice(bytes);
        let Some(end) = self.partial.iter().rposition(|b| *b == b'\n') else { return Change::None };
        let complete: Vec<u8> = self.partial.drain(..=end).collect();
        let start = self.items.len();
        for raw in complete.split(|b| *b == b'\n') {
            if raw.iter().all(|b| b.is_ascii_whitespace()) {
                continue;
            }
            let n = self.line;
            self.line += 1;
            self.stats.lines += 1;
            match serde_json::from_slice::<Value>(raw) {
                Ok(v) => self.record(n, &v),
                Err(_) => self.stats.failed += 1,
            }
        }
        if self.items.len() == start {
            Change::None
        } else {
            Change::Upsert { start, items: self.items[start..].to_vec() }
        }
    }

    fn record(&mut self, line: usize, v: &Value) {
        let ty = v["type"].as_str().unwrap_or("");
        let p = &v["payload"];
        let ts = v["timestamp"].as_str().map(String::from);
        match ty {
            "session_meta" => {
                if self.version.is_none() {
                    self.version = p["cli_version"].as_str().map(String::from);
                }
            }
            "event_msg" => match p["type"].as_str().unwrap_or("") {
                "item_completed" => self.item(line, &p["item"], ts),
                "task_complete" => {
                    if let Some(ms) = p["duration_ms"].as_u64() {
                        self.push(line, 0, ts, Body::Turn { ms });
                    }
                }
                "turn_aborted" => self.push(line, 0, ts, Body::Interrupt),
                t if SKIPPED_EVENTS.contains(&t) => {}
                _ => self.stats.unknown += 1,
            },
            t if SKIPPED.contains(&t) => {}
            _ => self.stats.unknown += 1,
        }
    }

    fn push(&mut self, line: usize, n: usize, ts: Option<String>, body: Body) {
        self.items.push(Item { key: format!("r:{line}:{n}"), ts, body });
    }

    /// 一次工具调用：assistant 条目里放一个（或几个）ToolUse，后面跟对应的结果
    fn tools(&mut self, line: usize, ts: Option<String>, calls: Vec<(String, String, Value, Outcome)>) {
        if calls.is_empty() {
            return;
        }
        let mut blocks = vec![];
        let mut results = vec![];
        for (id, name, input, res) in calls {
            let (input, truncated) = slim_input(input);
            self.inputs.insert(id.clone(), input.clone());
            blocks.push(Block::ToolUse { id: id.clone(), name, input, truncated });
            results.push((id, res));
        }
        self.push(line, 0, ts.clone(), Body::Assistant { model: None, blocks });
        for (i, (id, r)) in results.into_iter().enumerate() {
            let (content, truncated) = truncate(&r.content, TEXT_LIMIT);
            self.push(line, i + 1, ts.clone(), Body::ToolResult { tool_use_id: id, is_error: r.is_error, content, images: 0, truncated, extra: r.extra });
        }
    }

    fn item(&mut self, line: usize, it: &Value, ts: Option<String>) {
        let id = it["id"].as_str().unwrap_or("").to_string();
        match it["type"].as_str().unwrap_or("") {
            "UserMessage" => {
                let parts = it["content"].as_array().cloned().unwrap_or_default();
                let text: Vec<&str> = parts.iter().filter(|c| c["type"] == "text").filter_map(|c| c["text"].as_str()).collect();
                let images = parts.iter().filter(|c| c["type"].as_str().is_some_and(|t| t.contains("image"))).count() as u32;
                let (text, truncated) = truncate(&text.join("\n"), TEXT_LIMIT);
                self.push(line, 0, ts, Body::User { text, images, truncated });
            }
            "AgentMessage" => {
                let text: Vec<&str> = it["content"].as_array().map(|a| a.iter().filter_map(|c| c["text"].as_str()).collect()).unwrap_or_default();
                let text = file_citations(&text.join("\n"));
                if !text.trim().is_empty() {
                    self.push(line, 0, ts, Body::Assistant { model: None, blocks: vec![Block::Text { text }] });
                }
            }
            "Reasoning" => {
                // 只用 Codex 给的摘要；加密的原始推理不读，没有摘要就不显示
                let text: Vec<String> = it["summary_text"].as_array().map(|a| a.iter().filter_map(|s| s.as_str().map(String::from).or_else(|| s["text"].as_str().map(String::from))).collect()).unwrap_or_default();
                let text = text.join("\n\n");
                if !text.trim().is_empty() {
                    self.push(line, 0, ts, Body::Assistant { model: None, blocks: vec![Block::Thinking { text }] });
                }
            }
            "CommandExecution" => {
                let out = it["aggregated_output"].as_str().or_else(|| it["formatted_output"].as_str()).unwrap_or("").replace("\r\n", "\n");
                let failed = it["exit_code"].as_i64().is_some_and(|c| c != 0) || !matches!(it["status"].as_str(), Some("completed") | None);
                let content = if out.is_empty() && failed { it["status"].as_str().unwrap_or("failed").to_string() } else { out };
                self.tools(line, ts, vec![(id, "Bash".into(), json!({ "command": command_text(it) }), Outcome { content, is_error: failed, extra: None })]);
            }
            "FileChange" => {
                let ok = matches!(it["status"].as_str(), Some("completed") | None);
                let err = it["stderr"].as_str().unwrap_or("").to_string();
                let mut calls = vec![];
                if let Some(changes) = it["changes"].as_object() {
                    for (n, (path, ch)) in changes.iter().enumerate() {
                        let cid = format!("{id}#{n}");
                        let res = |extra| Outcome { content: if ok { String::new() } else { err.clone() }, is_error: !ok, extra };
                        match ch["type"].as_str().unwrap_or("") {
                            "add" => calls.push((cid, "Write".into(), json!({ "file_path": path, "content": ch["content"] }), res(None))),
                            "update" => {
                                let target = ch["move_path"].as_str().unwrap_or(path);
                                let patch = structured_patch(ch["unified_diff"].as_str().unwrap_or(""));
                                calls.push((cid, "Edit".into(), json!({ "file_path": target }), res(Some(json!({ "structuredPatch": patch })))));
                            }
                            "delete" => calls.push((cid, "Bash".into(), json!({ "command": format!("rm \"{path}\"") }), res(None))),
                            _ => self.stats.unknown += 1,
                        }
                    }
                }
                self.tools(line, ts, calls);
            }
            "McpToolCall" => {
                let name = format!("mcp__{}__{}", it["server"].as_str().unwrap_or("?"), it["tool"].as_str().unwrap_or("?"));
                let content: Vec<&str> = it["result"]["content"].as_array().map(|a| a.iter().filter_map(|c| c["text"].as_str()).collect()).unwrap_or_default();
                let is_error = it["result"]["isError"].as_bool().unwrap_or(false) || it["status"].as_str().is_some_and(|s| s != "completed");
                self.tools(line, ts, vec![(id, name, it["arguments"].clone(), Outcome { content: content.join("\n"), is_error, extra: None })]);
            }
            "WebSearch" => {
                let lines: Vec<String> = it["results"].as_array().map(|a| a.iter().filter_map(|r| Some(format!("{} {}", r["title"].as_str()?, r["url"].as_str().unwrap_or("")))).collect()).unwrap_or_default();
                self.tools(line, ts, vec![(id, "WebSearch".into(), json!({ "query": it["query"] }), Outcome { content: lines.join("\n"), is_error: false, extra: None })]);
            }
            "ImageView" => {
                let path = it["path"].as_str().unwrap_or("");
                self.tools(line, ts, vec![(id, "Read".into(), json!({ "file_path": file_url_path(path) }), Outcome { content: String::new(), is_error: false, extra: None })]);
            }
            "ContextCompaction" => self.push(line, 0, ts, Body::Divider { kind: "compact".into() }),
            // 生成图片（`image_gen`）：图片存在 `$CODEX_HOME/generated_images/<会话>/<id>.png`，
            // 记录里只留文件名和提示词（`result` 是整张图的 base64，不放进条目），界面按文件名从 `chat/file` 取
            "Extension" if it["kind"] == "image_gen.generation" => {
                let file = format!("{id}.png");
                let ok = it["status"].as_str().is_none_or(|s| s == "completed");
                let prompt = it["revisedPrompt"].as_str().or_else(|| it["prompt"].as_str()).unwrap_or("");
                let input = json!({ "prompt": truncate(prompt, 2000).0, "file": file });
                self.tools(line, ts, vec![(id, "ImageGen".into(), input, Outcome { content: String::new(), is_error: !ok, extra: None })]);
            }
            t if SKIPPED_ITEMS.contains(&t) => {}
            _ => self.stats.unknown += 1,
        }
    }
}

struct Outcome {
    content: String,
    is_error: bool,
    extra: Option<Value>,
}

/// 命令显示成什么：Codex 解析出来的命令（`parsed_cmd`），没有就取 argv 的最后一个（`powershell -Command <…>`、`bash -lc <…>`）
fn command_text(it: &Value) -> String {
    let parsed: Vec<&str> = it["parsed_cmd"].as_array().map(|a| a.iter().filter_map(|c| c["cmd"].as_str()).collect()).unwrap_or_default();
    if !parsed.is_empty() {
        return parsed.join(" && ");
    }
    match &it["command"] {
        Value::String(s) => s.clone(),
        Value::Array(a) => a.last().and_then(Value::as_str).unwrap_or("").to_string(),
        _ => String::new(),
    }
}

/// `file:///C:/x/y.png` → `C:/x/y.png`（Unix 是 `/x/y.png`）
/// Codex 在回复里引用文件的指令：`:codex-file-citation{path="F:/out/a.pdf" purpose="output"}`（也可能是 `::` 开头）。
/// 换成 Markdown 链接 `[a.pdf](<F:/out/a.pdf>)`，界面再把它指向这段对话产出的文件；没有 path 的原样留着。
fn file_citations(text: &str) -> String {
    const TAG: &str = "codex-file-citation{";
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(TAG) {
        let colons = rest[..at].len() - rest[..at].trim_end_matches(':').len();
        let Some(end) = rest[at..].find('}').map(|e| at + e) else { break };
        let attrs = &rest[at + TAG.len()..end];
        let path = attrs.split_once("path=\"").and_then(|(_, p)| p.split_once('"')).map(|(p, _)| p);
        match path.filter(|p| !p.is_empty()) {
            Some(p) => {
                out.push_str(&rest[..at - colons]);
                let name = p.rsplit(['/', '\\']).next().filter(|n| !n.is_empty()).unwrap_or(p);
                out.push_str(&format!("[{}](<{}>)", name.replace(['[', ']'], ""), p));
            }
            None => out.push_str(&rest[..=end]),
        }
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    out
}

fn file_url_path(s: &str) -> String {
    let rest = s.strip_prefix("file://").unwrap_or(s);
    let rest = if rest.len() > 2 && rest.as_bytes()[0] == b'/' && rest.as_bytes()[2] == b':' { &rest[1..] } else { rest };
    rest.replace("%20", " ")
}

/// unified diff（`@@ -13,3 +13,10 @@` 和 ` `/`+`/`-` 开头的行）→ Claude 的 `structuredPatch`，界面画 diff 用
fn structured_patch(diff: &str) -> Vec<Value> {
    let mut out: Vec<Value> = vec![];
    for line in diff.lines() {
        if let Some(h) = line.strip_prefix("@@ ") {
            let nums: Vec<(u64, u64)> = h
                .split_whitespace()
                .take(2)
                .map(|r| {
                    let r = r.trim_start_matches(['-', '+']);
                    let (a, b) = r.split_once(',').unwrap_or((r, "1"));
                    (a.parse().unwrap_or(0), b.parse().unwrap_or(1))
                })
                .collect();
            let (old, new) = (nums.first().copied().unwrap_or((0, 0)), nums.get(1).copied().unwrap_or((0, 0)));
            out.push(json!({ "oldStart": old.0, "oldLines": old.1, "newStart": new.0, "newLines": new.1, "lines": [] }));
        } else if let Some(Value::Array(lines)) = out.last_mut().map(|h| &mut h["lines"]) {
            if line.starts_with([' ', '+', '-']) {
                lines.push(json!(line));
            }
        }
    }
    out
}

/// 工具参数里太长的字符串截短（Write 的整个文件内容之类）
fn slim_input(v: Value) -> (Value, bool) {
    match v {
        Value::Object(m) => {
            let mut cut = false;
            let m: Map<String, Value> = m
                .into_iter()
                .map(|(k, v)| match v {
                    Value::String(s) => {
                        let (s, c) = truncate(&s, TEXT_LIMIT);
                        cut |= c;
                        (k, Value::String(s))
                    }
                    other => (k, other),
                })
                .collect();
            (Value::Object(m), cut)
        }
        other => (other, false),
    }
}

impl TranscriptReader for Rollout {
    fn path(&self) -> &Path {
        &self.path
    }

    /// 读新写入的部分。文件变短（被换掉）时从头再来。
    fn poll(&mut self) -> std::io::Result<Change> {
        let mut f = match std::fs::File::open(&self.path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Change::None),
            Err(e) => return Err(e),
        };
        let size = f.metadata()?.len();
        if size < self.pos {
            *self = Self::new(self.path.clone());
            self.poll()?;
            return Ok(Change::Reset);
        }
        if size == self.pos {
            return Ok(Change::None);
        }
        f.seek(SeekFrom::Start(self.pos))?;
        let mut buf = Vec::with_capacity((size - self.pos) as usize);
        f.take(size - self.pos).read_to_end(&mut buf)?;
        self.pos += buf.len() as u64;
        Ok(self.feed(&buf))
    }

    fn len(&self) -> usize {
        self.items.len()
    }

    fn items(&self, from: usize, to: usize) -> Vec<Item> {
        let to = to.min(self.items.len());
        self.items[from.min(to)..to].to_vec()
    }

    /// `r:<行号>:<序号>` → 那一行的原始记录
    fn full_item(&self, key: &str) -> Option<Value> {
        let n: usize = key.strip_prefix("r:")?.split(':').next()?.parse().ok()?;
        let text = std::fs::read_to_string(&self.path).ok()?;
        text.lines().filter(|l| !l.trim().is_empty()).nth(n).and_then(|l| serde_json::from_str(l).ok())
    }

    fn tool_input(&self, tool_use_id: &str) -> Option<&Value> {
        self.inputs.get(tool_use_id)
    }

    fn version(&self) -> Option<String> {
        self.version.clone()
    }

    fn stats(&self) -> Stats {
        self.stats
    }
}

/// 记录里最后一条 `token_count`，换成 app-server `thread/tokenUsage/updated` 的 `tokenUsage` 的样子。
/// 刚接上（看看重启、重新加载会话）时 app-server 要等下一轮才发用量，先用它把上下文用量填上。只读文件末尾一段。
pub fn last_token_usage(path: &Path) -> Option<Value> {
    const TAIL: u64 = 1 << 20;
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    f.seek(SeekFrom::Start(len.saturating_sub(TAIL))).ok()?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).ok()?;
    token_usage_in(&String::from_utf8_lossy(&buf))
}

fn token_usage_in(text: &str) -> Option<Value> {
    text.lines().rev().filter(|l| l.contains("\"token_count\"")).find_map(|l| {
        let v: Value = serde_json::from_str(l).ok()?;
        let info = v["payload"]["info"].as_object()?;
        let n = |u: &Value, k: &str| u[k].as_u64().unwrap_or(0);
        let (last, total) = (&info["last_token_usage"], &info["total_token_usage"]);
        Some(json!({
            "last": { "inputTokens": n(last, "input_tokens") },
            "total": { "inputTokens": n(total, "input_tokens"), "cachedInputTokens": n(total, "cached_input_tokens"), "outputTokens": n(total, "output_tokens") },
            "modelContextWindow": info["model_context_window"],
        }))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_usage_from_last_token_count() {
        let text = concat!(
            r#"{"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":10,"cached_input_tokens":4,"output_tokens":2},"last_token_usage":{"input_tokens":10},"model_context_window":1000}}}"#, "
",
            r#"{"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":66424,"cached_input_tokens":34432,"output_tokens":253},"last_token_usage":{"input_tokens":21502},"model_context_window":258400}}}"#, "
",
            r#"{"type":"event_msg","payload":{"type":"token_count","info":null}}"#, "
",
            r#"{"type":"event_msg","payload":{"type":"task_complete"}}"#, "
",
        );
        let u = token_usage_in(text).unwrap();
        assert_eq!(u["last"]["inputTokens"], 21502);
        assert_eq!(u["total"]["cachedInputTokens"], 34432);
        assert_eq!(u["modelContextWindow"], 258400);
        assert!(token_usage_in("{}").is_none());
    }

    fn line(v: Value) -> String {
        format!("{v}\n")
    }

    fn done(item: Value) -> String {
        line(json!({ "timestamp": "2026-10-18T04:35:41Z", "type": "event_msg", "payload": { "type": "item_completed", "item": item } }))
    }

    #[test]
    fn items_in_claude_vocabulary() {
        let mut r = Rollout::new(PathBuf::from("x.jsonl"));
        let text = [
            line(json!({ "type": "session_meta", "payload": { "id": "s1", "cli_version": "0.159.3" } })),
            line(json!({ "type": "response_item", "payload": { "type": "message", "role": "user", "content": [] } })),
            done(json!({ "type": "UserMessage", "id": "u", "content": [{ "type": "text", "text": "跑测试" }, { "type": "local_image", "path": "a.png" }] })),
            done(json!({ "type": "Reasoning", "id": "r", "summary_text": [] })),
            done(json!({ "type": "AgentMessage", "id": "m", "content": [{ "type": "Text", "text": "好的" }] })),
            done(json!({ "type": "CommandExecution", "id": "c", "command": ["powershell.exe", "-Command", "npm test"], "parsed_cmd": [], "aggregated_output": "fail", "exit_code": 1, "status": "failed" })),
            done(json!({ "type": "FileChange", "id": "f", "status": "completed", "changes": {
                "b.txt": { "type": "add", "content": "hi" },
                "a.rs": { "type": "update", "unified_diff": "@@ -2,2 +2,3 @@\n a\n-x\n+y\n+z\n" } } })),
            done(json!({ "type": "McpToolCall", "id": "p", "server": "looklook", "tool": "add_page", "arguments": { "title": "t" }, "result": { "content": [{ "type": "text", "text": "ok" }] }, "status": "completed" })),
            done(json!({ "type": "ImageView", "id": "i", "path": "file:///C:/x/my%20pic.png" })),
            done(json!({ "type": "ContextCompaction", "id": "k" })),
            line(json!({ "type": "event_msg", "payload": { "type": "turn_aborted", "reason": "interrupted" } })),
            line(json!({ "type": "event_msg", "payload": { "type": "task_complete", "duration_ms": 8558 } })),
            done(json!({ "type": "SomethingNew", "id": "z" })),
            line(json!({ "type": "brand_new_record" })),
        ]
        .concat();
        assert!(matches!(r.feed(text.as_bytes()), Change::Upsert { start: 0, .. }));
        assert_eq!(r.version().as_deref(), Some("0.159.3"));
        assert_eq!(r.stats().unknown, 2);
        let items = r.items(0, r.len());
        assert_eq!(items[0].key, "r:2:0");
        assert_eq!(items[0].body, Body::User { text: "跑测试".into(), images: 1, truncated: false });
        assert!(matches!(&items[1].body, Body::Assistant { blocks, .. } if blocks == &[Block::Text { text: "好的".into() }]), "空的思考摘要不显示");
        assert!(matches!(&items[2].body, Body::Assistant { blocks, .. } if matches!(&blocks[0], Block::ToolUse { name, input, .. } if name == "Bash" && input["command"] == "npm test")));
        assert!(matches!(&items[3].body, Body::ToolResult { is_error: true, content, .. } if content == "fail"));
        let Body::Assistant { blocks, .. } = &items[4].body else { panic!() };
        let names: Vec<&str> = blocks.iter().map(|b| if let Block::ToolUse { name, .. } = b { name.as_str() } else { "" }).collect();
        assert_eq!(names, ["Write", "Edit"], "按补丁里的顺序");
        let Body::ToolResult { extra: Some(extra), .. } = &items[6].body else { panic!("{:?}", items[6]) };
        assert_eq!(extra["structuredPatch"][0], json!({ "oldStart": 2, "oldLines": 2, "newStart": 2, "newLines": 3, "lines": [" a", "-x", "+y", "+z"] }));
        assert!(items.iter().any(|i| matches!(&i.body, Body::Assistant { blocks, .. } if matches!(&blocks[0], Block::ToolUse { name, .. } if name == "mcp__looklook__add_page"))));
        assert_eq!(r.tool_input("i").unwrap()["file_path"], "C:/x/my pic.png");
        let tail: Vec<&Body> = items.iter().rev().take(3).map(|i| &i.body).collect();
        assert_eq!(tail, [&Body::Turn { ms: 8558 }, &Body::Interrupt, &Body::Divider { kind: "compact".into() }]);
    }

    #[test]
    fn file_citations_become_links() {
        let t = "已制作《灯》：四页。\n\n:codex-file-citation{path=\"F:/out/pdf/替远行的人留一盏灯.pdf\" purpose=\"output\"}";
        assert_eq!(file_citations(t), "已制作《灯》：四页。\n\n[替远行的人留一盏灯.pdf](<F:/out/pdf/替远行的人留一盏灯.pdf>)");
        assert_eq!(file_citations("a ::codex-file-citation{path=\"/tmp/a b.txt\"} b"), "a [a b.txt](</tmp/a b.txt>) b");
        assert_eq!(file_citations("no path :codex-file-citation{purpose=\"x\"}"), "no path :codex-file-citation{purpose=\"x\"}");
        assert_eq!(file_citations("plain"), "plain");
    }

    #[test]
    fn image_generation_shows_file_not_base64() {
        let mut r = Rollout::new(PathBuf::from("x.jsonl"));
        let big = "A".repeat(200_000);
        let text = done(json!({ "type": "Extension", "kind": "image_gen.generation", "id": "exec-1174", "status": "completed", "revisedPrompt": "a water town at blue hour", "result": big }));
        r.feed(text.as_bytes());
        let items = r.items(0, r.len());
        let Body::Assistant { blocks, .. } = &items[0].body else { panic!("{:?}", items[0]) };
        let Block::ToolUse { name, input, .. } = &blocks[0] else { panic!() };
        assert_eq!(name, "ImageGen");
        assert_eq!(input["file"], "exec-1174.png");
        assert_eq!(input["prompt"], "a water town at blue hour");
        assert!(serde_json::to_string(&items).unwrap().len() < 2000, "base64 不能进条目");
        // 别的 Extension 照旧跳过
        let mut r2 = Rollout::new(PathBuf::from("y.jsonl"));
        r2.feed(done(json!({ "type": "Extension", "kind": "other", "id": "e" })).as_bytes());
        assert_eq!(r2.len(), 0);
    }

    #[test]
    fn incremental() {
        let mut r = Rollout::new(PathBuf::from("x.jsonl"));
        let a = done(json!({ "type": "UserMessage", "id": "u", "content": [{ "type": "text", "text": "hi" }] }));
        let (head, tail) = a.split_at(20);
        assert_eq!(r.feed(head.as_bytes()), Change::None, "半行先留着");
        assert!(matches!(r.feed(tail.as_bytes()), Change::Upsert { start: 0, .. }));
        let b = done(json!({ "type": "AgentMessage", "id": "m", "content": [{ "type": "Text", "text": "yo" }] }));
        assert!(matches!(r.feed(b.as_bytes()), Change::Upsert { start: 1, .. }));
        assert_eq!(r.len(), 2);
    }

    /// 本机真实的 rollout（有就读）：不能有解析失败的行
    #[test]
    fn real_rollouts_if_present() {
        let Some(home) = dirs::home_dir() else { return };
        let mut files = vec![];
        let mut stack = vec![home.join(".codex").join("sessions")];
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else if p.extension().is_some_and(|x| x == "jsonl") {
                    files.push(p);
                }
            }
        }
        files.sort();
        for f in files.iter().rev().take(20) {
            let mut r = Rollout::new(f.clone());
            r.poll().unwrap();
            assert_eq!(r.stats().failed, 0, "{}", f.display());
        }
    }
}
