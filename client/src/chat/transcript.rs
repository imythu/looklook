//! Claude Code 的 transcript（`~/.claude/projects/<项目>/<session>.jsonl`）：跟读，并整理成对话界面的条目。
//!
//! transcript 是 Claude Code 的内部格式，字段一直在增加（2.1.302 新出现了 `api-request` 一类记录），
//! 所以这里只认白名单里的几种记录和内容块，其余跳过；认不出的内容块显示成“其他内容”（docs/CLAUDE_UI.md §4.5）。
//!
//! 每行一条记录，靠 `uuid` / `parentUuid` 连成树：`/rewind`、编辑后重发会留下被丢弃的分支，
//! 所以显示的是“从最后一条记录沿着父节点往回走”得到的那一条链；压缩后的第一条记录 `parentUuid` 为空，
//! 用 `logicalParentUuid` 接回压缩前的历史。一次 assistant 回复拆成多行（每行一个内容块，共用 `message.id`），
//! 显示时合成一条。

use std::collections::{HashMap, HashSet};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::{Map, Value};

/// 单个文字内容（工具结果、用户消息、命令输出）最多保留这么多字符，完整内容按需另取
pub const TEXT_LIMIT: usize = 16_000;
/// 工具参数、结构化结果里单个字符串的上限
const FIELD_LIMIT: usize = 4_000;
/// 结构化结果（`toolUseResult`）整理后超过这么大就不带了
const EXTRA_LIMIT: usize = 32_000;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Item {
    /// 稳定的标识：assistant 是 `a:<message.id>`，其余是 `<uuid>:<序号>`
    pub key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ts: Option<String>,
    #[serde(flatten)]
    pub body: Body,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(tag = "k", rename_all = "snake_case")]
pub enum Body {
    User {
        text: String,
        images: u32,
        #[serde(skip_serializing_if = "std::ops::Not::not")]
        truncated: bool,
    },
    Assistant {
        #[serde(skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        blocks: Vec<Block>,
    },
    ToolResult {
        tool_use_id: String,
        is_error: bool,
        content: String,
        images: u32,
        #[serde(skip_serializing_if = "std::ops::Not::not")]
        truncated: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        extra: Option<Value>,
    },
    /// 用户按 Esc 停止了这一轮
    Interrupt,
    /// 斜杠命令、`!` 命令和它们的输出
    Command { text: String },
    /// 压缩分隔线
    Divider { kind: String },
    /// 压缩时生成的摘要（默认折叠）
    Summary { text: String },
    Info { text: String },
    /// 这一轮用了多久
    Turn { ms: u64 },
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Block {
    Text { text: String },
    Thinking { text: String },
    ToolUse {
        id: String,
        name: String,
        input: Value,
        #[serde(skip_serializing_if = "std::ops::Not::not")]
        truncated: bool,
    },
    Unknown { raw_type: String },
}

/// 一行记录里对界面有用的部分
#[derive(Debug, Default)]
struct Rec {
    uuid: Option<String>,
    parent: Option<String>,
    sidechain: bool,
    /// assistant 记录的 `message.id`（同一次回复的多行共用）
    message_id: Option<String>,
    items: Vec<Item>,
}

/// 文件内容的变化
#[derive(Debug, PartialEq)]
pub enum Change {
    None,
    /// 从第 `start` 条起的条目新增或更新了
    Upsert { start: usize, items: Vec<Item> },
    /// 前面的内容变了（`/rewind` 之类），要整个重新加载
    Reset,
}

#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct Stats {
    pub lines: u64,
    /// 解析不了的行
    pub failed: u64,
    /// 不在白名单里的记录类型
    pub unknown: u64,
}

pub struct Transcript {
    path: PathBuf,
    pos: u64,
    partial: Vec<u8>,
    recs: Vec<Rec>,
    by_uuid: HashMap<String, usize>,
    /// 显示的条目：每条是一组 (记录序号, 条目序号)，assistant 合并多行时有多个
    display: Vec<Vec<(usize, usize)>>,
    pub version: Option<String>,
    pub stats: Stats,
    /// 子代理自己的记录（`subagents/agent-<id>.jsonl`）：每条都标着 isSidechain，要照常显示
    sidechain: bool,
}

impl Transcript {
    pub fn new(path: PathBuf) -> Self {
        Self { path, pos: 0, partial: vec![], recs: vec![], by_uuid: HashMap::new(), display: vec![], version: None, stats: Stats::default(), sidechain: false }
    }

    /// 子代理的 transcript
    pub fn subagent(path: PathBuf) -> Self {
        Self { sidechain: true, ..Self::new(path) }
    }

    /// 某次工具调用的参数（找子代理用：没有结果之前只能按它的 prompt 去对）
    pub fn tool_input(&self, tool_use_id: &str) -> Option<&Value> {
        self.recs.iter().flat_map(|r| &r.items).find_map(|i| match &i.body {
            Body::Assistant { blocks, .. } => blocks.iter().find_map(|b| match b {
                Block::ToolUse { id, input, .. } if id == tool_use_id => Some(input),
                _ => None,
            }),
            _ => None,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn len(&self) -> usize {
        self.display.len()
    }

    /// 读新写入的部分。文件变短（被重写）时从头再来。
    pub fn poll(&mut self) -> std::io::Result<Change> {
        let mut f = match std::fs::File::open(&self.path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Change::None),
            Err(e) => return Err(e),
        };
        let size = f.metadata()?.len();
        if size < self.pos {
            *self = Self { sidechain: self.sidechain, ..Self::new(self.path.clone()) };
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

    /// 喂入新读到的字节（测试直接用）。
    pub fn feed(&mut self, bytes: &[u8]) -> Change {
        self.partial.extend_from_slice(bytes);
        let Some(end) = self.partial.iter().rposition(|b| *b == b'\n') else { return Change::None };
        let complete: Vec<u8> = self.partial.drain(..=end).collect();
        let mut added = false;
        for line in complete.split(|b| *b == b'\n') {
            if line.iter().all(|b| b.is_ascii_whitespace()) {
                continue;
            }
            self.stats.lines += 1;
            match serde_json::from_slice::<Value>(line) {
                Ok(v) => {
                    if let Some(rec) = self.parse(&v) {
                        if let Some(u) = &rec.uuid {
                            self.by_uuid.insert(u.clone(), self.recs.len());
                        }
                        self.recs.push(rec);
                        added = true;
                    }
                }
                Err(_) => self.stats.failed += 1,
            }
        }
        if !added {
            return Change::None;
        }
        let next = self.layout();
        let old = std::mem::replace(&mut self.display, next);
        let first_diff = old.iter().zip(&self.display).position(|(a, b)| a != b).unwrap_or(old.len().min(self.display.len()));
        if first_diff == old.len() && self.display.len() == old.len() {
            return Change::None;
        }
        // 只有末尾那条变了（assistant 又多了一个内容块）或者在末尾新增：增量更新；否则整个重新加载
        let tail_grew = first_diff >= old.len() || (first_diff + 1 == old.len() && old[first_diff].first() == self.display[first_diff].first());
        if tail_grew && self.display.len() >= old.len() {
            Change::Upsert { start: first_diff, items: self.items(first_diff, self.display.len()) }
        } else {
            Change::Reset
        }
    }

    /// 第 `from` 到 `to` 条（不含）显示条目。
    pub fn items(&self, from: usize, to: usize) -> Vec<Item> {
        let to = to.min(self.display.len());
        (from.min(to)..to).map(|i| self.materialize(&self.display[i])).collect()
    }

    /// 按 key 找条目，返回不截断的完整内容（从文件里重新读这一行）。
    pub fn full_item(&self, key: &str) -> Option<Value> {
        let uuid = key.split(':').next()?;
        let text = std::fs::read_to_string(&self.path).ok()?;
        text.lines().find(|l| l.contains(uuid)).and_then(|l| serde_json::from_str::<Value>(l).ok()).filter(|v| v["uuid"].as_str() == Some(uuid))
    }

    fn materialize(&self, parts: &[(usize, usize)]) -> Item {
        let (r0, i0) = parts[0];
        let mut item = self.recs[r0].items[i0].clone();
        if let Body::Assistant { blocks, .. } = &mut item.body {
            for (r, i) in &parts[1..] {
                if let Body::Assistant { blocks: more, .. } = &self.recs[*r].items[*i].body {
                    blocks.extend(more.iter().cloned());
                }
            }
        }
        item
    }

    /// 从最后一条主线记录沿父节点往回走得到要显示的链，再展开成条目（assistant 按 message.id 合并）。
    fn layout(&self) -> Vec<Vec<(usize, usize)>> {
        let Some(leaf) = self.recs.iter().rposition(|r| r.uuid.is_some() && (self.sidechain || !r.sidechain)) else { return vec![] };
        let mut chain = vec![];
        let mut seen = HashSet::new();
        let mut cur = Some(leaf);
        while let Some(i) = cur {
            if !seen.insert(i) {
                break;
            }
            chain.push(i);
            cur = self.recs[i].parent.as_ref().and_then(|p| self.by_uuid.get(p)).copied();
        }
        chain.reverse();
        let mut out: Vec<Vec<(usize, usize)>> = vec![];
        let mut last_msg: Option<&str> = None;
        for r in chain {
            let rec = &self.recs[r];
            for (i, item) in rec.items.iter().enumerate() {
                let msg = match &item.body {
                    Body::Assistant { .. } => rec.message_id.as_deref(),
                    _ => None,
                };
                match (msg, last_msg, out.last_mut()) {
                    (Some(m), Some(prev), Some(group)) if m == prev => group.push((r, i)),
                    _ => out.push(vec![(r, i)]),
                }
                last_msg = msg;
            }
        }
        out
    }

    fn parse(&mut self, v: &Value) -> Option<Rec> {
        let ty = v["type"].as_str()?;
        if let Some(ver) = v["version"].as_str() {
            if self.version.as_deref() != Some(ver) {
                self.version = Some(ver.to_string());
            }
        }
        let uuid = v["uuid"].as_str().map(String::from);
        // 链上的每条记录都要留着（附件、元信息也在链里），哪怕它本身不显示
        let mut rec = Rec {
            parent: v["parentUuid"].as_str().or_else(|| v["logicalParentUuid"].as_str()).map(String::from),
            sidechain: v["isSidechain"].as_bool().unwrap_or(false) && !self.sidechain,
            ..Default::default()
        };
        let ts = v["timestamp"].as_str().map(String::from);
        let key = |n: usize| format!("{}:{n}", uuid.as_deref().unwrap_or(""));
        match ty {
            "user" if !rec.sidechain => rec.items = user_items(v, &key, ts),
            "assistant" if !rec.sidechain => {
                let m = &v["message"];
                rec.message_id = m["id"].as_str().map(String::from).or_else(|| uuid.clone());
                let blocks = assistant_blocks(&m["content"]);
                if !blocks.is_empty() {
                    let model = m["model"].as_str().filter(|s| !s.starts_with('<')).map(String::from);
                    rec.items.push(Item { key: format!("a:{}", rec.message_id.as_deref().unwrap_or("")), ts, body: Body::Assistant { model, blocks } });
                }
            }
            "system" if !rec.sidechain => rec.items = system_items(v, &key, ts).into_iter().collect(),
            "user" | "assistant" | "system" | "attachment" => {}
            _ => {
                if uuid.is_none() {
                    // 不在链上、也不在白名单里的记录（标题、模式、费用、请求副本……）
                    self.stats.unknown += 1;
                    return None;
                }
            }
        }
        uuid.as_ref()?;
        rec.uuid = uuid;
        Some(rec)
    }
}

fn user_items(v: &Value, key: &dyn Fn(usize) -> String, ts: Option<String>) -> Vec<Item> {
    if v["isMeta"].as_bool() == Some(true) || v["isVisibleInTranscriptOnly"].as_bool() == Some(true) {
        return vec![];
    }
    let content = &v["message"]["content"];
    if v["isCompactSummary"].as_bool() == Some(true) {
        let (text, _, _) = join_text(content);
        return vec![Item { key: key(0), ts, body: Body::Summary { text: truncate(&text, TEXT_LIMIT).0 } }];
    }
    let mut out = vec![];
    if let Some(list) = content.as_array() {
        let results: Vec<&Value> = list.iter().filter(|b| b["type"] == "tool_result").collect();
        let extra = (results.len() == 1).then(|| slim_extra(&v["toolUseResult"])).flatten();
        for b in &results {
            let (text, images, truncated) = join_text(&b["content"]);
            out.push(Item {
                key: key(out.len()),
                ts: ts.clone(),
                body: Body::ToolResult {
                    tool_use_id: b["tool_use_id"].as_str().unwrap_or("").to_string(),
                    is_error: b["is_error"].as_bool().unwrap_or(false),
                    content: text,
                    images,
                    truncated,
                    extra: extra.clone(),
                },
            });
        }
        if !results.is_empty() {
            return out;
        }
    }
    let (text, images, truncated) = join_text(content);
    let trimmed = text.trim();
    if trimmed.starts_with("[Request interrupted by user") {
        return vec![Item { key: key(0), ts, body: Body::Interrupt }];
    }
    if let Some(cmd) = command_text(trimmed) {
        return vec![Item { key: key(0), ts, body: Body::Command { text: cmd } }];
    }
    if trimmed.is_empty() && images == 0 {
        return vec![];
    }
    vec![Item { key: key(0), ts, body: Body::User { text, images, truncated } }]
}

fn assistant_blocks(content: &Value) -> Vec<Block> {
    let Some(list) = content.as_array() else {
        return content.as_str().filter(|s| !s.is_empty()).map(|s| vec![Block::Text { text: s.to_string() }]).unwrap_or_default();
    };
    list.iter()
        .filter_map(|b| match b["type"].as_str()? {
            "text" => b["text"].as_str().filter(|t| !t.is_empty()).map(|t| Block::Text { text: t.to_string() }),
            // 只有签名、没有内容的 thinking（被隐藏的思考）不显示
            "thinking" => b["thinking"].as_str().filter(|t| !t.trim().is_empty()).map(|t| Block::Thinking { text: truncate(t, TEXT_LIMIT).0 }),
            "redacted_thinking" => None,
            "tool_use" | "server_tool_use" => {
                let (input, truncated) = slim_value(&b["input"], FIELD_LIMIT);
                Some(Block::ToolUse { id: b["id"].as_str().unwrap_or("").to_string(), name: b["name"].as_str().unwrap_or("").to_string(), input, truncated })
            }
            other => Some(Block::Unknown { raw_type: other.to_string() }),
        })
        .collect()
}

fn system_items(v: &Value, key: &dyn Fn(usize) -> String, ts: Option<String>) -> Option<Item> {
    let body = match v["subtype"].as_str()? {
        "compact_boundary" => Body::Divider { kind: "compact".into() },
        "turn_duration" => Body::Turn { ms: v["durationMs"].as_u64()? },
        "local_command" => Body::Command { text: command_text(v["content"].as_str()?)? },
        "informational" => Body::Info { text: truncate(v["content"].as_str()?, TEXT_LIMIT).0 },
        _ => return None,
    };
    Some(Item { key: key(0), ts, body })
}

/// 斜杠命令、`!` 命令的记录（`<command-name>/model</command-name>`、`<local-command-stdout>…`、`<bash-input>`）
/// 整理成一行文字；不是这类内容时返回 None。
fn command_text(s: &str) -> Option<String> {
    const TAGS: [&str; 7] = ["command-name", "command-args", "local-command-stdout", "local-command-stderr", "bash-input", "bash-stdout", "bash-stderr"];
    if !TAGS.iter().any(|t| s.starts_with(&format!("<{t}>"))) {
        return None;
    }
    let mut parts = vec![];
    for t in TAGS {
        let (open, close) = (format!("<{t}>"), format!("</{t}>"));
        if let Some(start) = s.find(&open) {
            let rest = &s[start + open.len()..];
            let inner = rest.find(&close).map(|e| &rest[..e]).unwrap_or(rest).trim();
            if !inner.is_empty() && !(t == "command-name" && parts.iter().any(|p: &String| p == inner)) {
                parts.push(match t {
                    "bash-input" => format!("! {inner}"),
                    _ => inner.to_string(),
                });
            }
        }
    }
    // `<command-message>` 与名字重复，不要
    let text = parts.join("\n");
    Some(truncate(&text, TEXT_LIMIT).0)
}

/// 内容（字符串或内容块数组）里的文字，以及图片个数。
fn join_text(content: &Value) -> (String, u32, bool) {
    let mut images = 0;
    let text = match content {
        Value::String(s) => s.clone(),
        Value::Array(list) => list
            .iter()
            .filter_map(|b| match b["type"].as_str() {
                Some("text") => b["text"].as_str().map(String::from),
                Some("image") => {
                    images += 1;
                    None
                }
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    };
    let (text, truncated) = truncate(&text, TEXT_LIMIT);
    (text, images, truncated)
}

pub(super) fn truncate(s: &str, limit: usize) -> (String, bool) {
    if s.len() <= limit {
        return (s.to_string(), false);
    }
    let mut end = limit;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    (s[..end].to_string(), true)
}

/// 工具参数：保留所有字段，过长的字符串截断。
fn slim_value(v: &Value, limit: usize) -> (Value, bool) {
    match v {
        Value::String(s) => {
            let (t, cut) = truncate(s, limit);
            (Value::String(t), cut)
        }
        Value::Array(a) => {
            let mut cut = false;
            let out = a.iter().map(|x| {
                let (y, c) = slim_value(x, limit);
                cut |= c;
                y
            });
            (Value::Array(out.collect()), cut)
        }
        Value::Object(m) => {
            let mut cut = false;
            let out: Map<String, Value> = m
                .iter()
                .map(|(k, x)| {
                    let (y, c) = slim_value(x, limit);
                    cut |= c;
                    (k.clone(), y)
                })
                .collect();
            (Value::Object(out), cut)
        }
        other => (other.clone(), false),
    }
}

/// 结构化的工具结果（`toolUseResult`，例如 Edit 的 `structuredPatch`）：只是用来把卡片画得更好，
/// 去掉很长的字符串字段（例如改动前的整个文件 `originalFile`），整理后还太大就不带。
fn slim_extra(v: &Value) -> Option<Value> {
    fn go(v: &Value) -> Option<Value> {
        match v {
            Value::String(s) if s.len() > 2_000 => None,
            Value::Array(a) => Some(Value::Array(a.iter().filter_map(go).collect())),
            Value::Object(m) => Some(Value::Object(m.iter().filter_map(|(k, x)| go(x).map(|y| (k.clone(), y))).collect())),
            other => Some(other.clone()),
        }
    }
    if !v.is_object() {
        return None;
    }
    let out = go(v)?;
    (serde_json::to_string(&out).map(|s| s.len()).unwrap_or(usize::MAX) <= EXTRA_LIMIT).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn line(v: Value) -> String {
        format!("{v}\n")
    }

    fn user(uuid: &str, parent: Option<&str>, content: Value) -> String {
        line(json!({"type":"user","uuid":uuid,"parentUuid":parent,"message":{"role":"user","content":content},"timestamp":"2026-10-17T00:00:00Z","version":"2.1.302"}))
    }

    fn assistant(uuid: &str, parent: &str, msg: &str, block: Value) -> String {
        line(json!({"type":"assistant","uuid":uuid,"parentUuid":parent,"message":{"id":msg,"model":"claude-haiku-5-5","content":[block]}}))
    }

    fn kinds(t: &Transcript) -> Vec<String> {
        t.items(0, t.len()).iter().map(|i| serde_json::to_value(i).unwrap()["k"].as_str().unwrap().to_string()).collect()
    }

    #[test]
    fn merges_assistant_blocks_and_pairs_tool_results() {
        let mut t = Transcript::new("/nonexistent".into());
        let mut s = user("u1", None, json!("用 Bash 执行 ls"));
        s += &assistant("a1", "u1", "msg_1", json!({"type":"thinking","thinking":"","signature":"x"}));
        s += &assistant("a2", "a1", "msg_1", json!({"type":"tool_use","id":"toolu_1","name":"Bash","input":{"command":"ls"}}));
        s += &line(json!({"type":"user","uuid":"r1","parentUuid":"a2","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_1","content":"calc.py\nREADME.md","is_error":false}]},"toolUseResult":{"stdout":"calc.py\nREADME.md","stderr":"","interrupted":false}}));
        s += &line(json!({"type":"api-request","id":"x","timestamp":"t"}));
        s += &assistant("a3", "r1", "msg_2", json!({"type":"text","text":"有 2 个文件"}));
        t.feed(s.as_bytes());
        assert_eq!(kinds(&t), ["user", "assistant", "tool_result", "assistant"]);
        let items = t.items(0, 4);
        let Body::Assistant { blocks, .. } = &items[1].body else { panic!() };
        assert_eq!(blocks.len(), 1, "空的 thinking 不显示");
        assert!(matches!(&blocks[0], Block::ToolUse { name, .. } if name == "Bash"));
        let Body::ToolResult { tool_use_id, extra, .. } = &items[2].body else { panic!() };
        assert_eq!(tool_use_id, "toolu_1");
        assert_eq!(extra.as_ref().unwrap()["stdout"], "calc.py\nREADME.md");
        assert_eq!(t.stats.unknown, 1);
        assert_eq!(t.version.as_deref(), Some("2.1.302"));
    }

    #[test]
    fn incremental_updates_and_partial_lines() {
        let mut t = Transcript::new("/nonexistent".into());
        assert!(matches!(t.feed(user("u1", None, json!("hi")).as_bytes()), Change::Upsert { start: 0, .. }));
        // 半行：等下一次
        let a = assistant("a1", "u1", "msg_1", json!({"type":"text","text":"第一段"}));
        let (head, tail) = a.split_at(20);
        assert_eq!(t.feed(head.as_bytes()), Change::None);
        let Change::Upsert { start, items } = t.feed(tail.as_bytes()) else { panic!() };
        assert_eq!((start, items.len()), (1, 1));
        // 同一条回复的下一个内容块：更新最后一条
        let Change::Upsert { start, items } = t.feed(assistant("a2", "a1", "msg_1", json!({"type":"text","text":"第二段"})).as_bytes()) else { panic!() };
        assert_eq!(start, 1);
        let Body::Assistant { blocks, .. } = &items[0].body else { panic!() };
        assert_eq!(blocks.len(), 2);
        assert_eq!(items[0].key, "a:msg_1");
    }

    #[test]
    fn rewind_shows_only_the_live_branch() {
        let mut t = Transcript::new("/nonexistent".into());
        let mut s = user("u1", None, json!("第一问"));
        s += &assistant("a1", "u1", "m1", json!({"type":"text","text":"答一"}));
        s += &user("u2", Some("a1"), json!("被丢弃的问题"));
        s += &assistant("a2", "u2", "m2", json!({"type":"text","text":"被丢弃的回答"}));
        t.feed(s.as_bytes());
        assert_eq!(t.len(), 4);
        // 从 a1 重新分叉
        assert_eq!(t.feed(user("u3", Some("a1"), json!("改过的问题")).as_bytes()), Change::Reset);
        let texts: Vec<String> = t.items(0, t.len()).iter().map(|i| serde_json::to_string(i).unwrap()).collect();
        assert_eq!(texts.len(), 3);
        assert!(texts[2].contains("改过的问题") && !texts.iter().any(|s| s.contains("被丢弃")));
    }

    #[test]
    fn compaction_keeps_history_via_logical_parent() {
        let mut t = Transcript::new("/nonexistent".into());
        let mut s = user("u1", None, json!("记住暗号"));
        s += &assistant("a1", "u1", "m1", json!({"type":"text","text":"好的"}));
        s += &line(json!({"type":"system","subtype":"compact_boundary","uuid":"c1","parentUuid":null,"logicalParentUuid":"a1","content":"Conversation compacted"}));
        s += &line(json!({"type":"user","uuid":"s1","parentUuid":"c1","isCompactSummary":true,"message":{"content":"摘要……"}}));
        s += &user("u2", Some("s1"), json!("暗号是什么"));
        t.feed(s.as_bytes());
        assert_eq!(kinds(&t), ["user", "assistant", "divider", "summary", "user"]);
    }

    #[test]
    fn commands_interrupts_and_meta() {
        let mut t = Transcript::new("/nonexistent".into());
        let mut s = user("u1", None, json!("跑一下"));
        s += &user("u2", Some("u1"), json!([{"type":"text","text":"[Request interrupted by user]"}]));
        s += &line(json!({"type":"system","subtype":"local_command","uuid":"c1","parentUuid":"u2","content":"<command-name>/model</command-name>\n            <command-message>model</command-message>\n            <command-args></command-args>"}));
        s += &line(json!({"type":"system","subtype":"local_command","uuid":"c2","parentUuid":"c1","content":"<local-command-stdout>Kept model as `Haiku 5.5`</local-command-stdout>"}));
        s += &line(json!({"type":"user","uuid":"m1","parentUuid":"c2","isMeta":true,"message":{"content":"Caveat: …"}}));
        s += &line(json!({"type":"system","subtype":"turn_duration","uuid":"d1","parentUuid":"m1","durationMs":4200}));
        s += &line(json!({"type":"system","subtype":"stop_hook_summary","uuid":"h1","parentUuid":"d1"}));
        t.feed(s.as_bytes());
        let items = t.items(0, t.len());
        assert_eq!(kinds(&t), ["user", "interrupt", "command", "command", "turn"]);
        assert_eq!(items[2].body, Body::Command { text: "/model".into() });
        assert_eq!(items[3].body, Body::Command { text: "Kept model as `Haiku 5.5`".into() });
    }

    #[test]
    fn long_content_is_truncated_and_big_extras_dropped() {
        let mut t = Transcript::new("/nonexistent".into());
        let big = "x".repeat(TEXT_LIMIT + 10);
        let mut s = user("u1", None, json!("改文件"));
        s += &assistant("a1", "u1", "m1", json!({"type":"tool_use","id":"t1","name":"Write","input":{"file_path":"/a","content":big}}));
        s += &line(json!({"type":"user","uuid":"r1","parentUuid":"a1","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":big}]},"toolUseResult":{"type":"update","filePath":"/a","originalFile":big,"structuredPatch":[{"oldStart":1,"lines":["-a","+b"]}]}}));
        t.feed(s.as_bytes());
        let items = t.items(0, t.len());
        let Body::Assistant { blocks, .. } = &items[1].body else { panic!() };
        let Block::ToolUse { input, truncated, .. } = &blocks[0] else { panic!() };
        assert!(*truncated && input["content"].as_str().unwrap().len() == FIELD_LIMIT && input["file_path"] == "/a");
        let Body::ToolResult { content, truncated, extra, .. } = &items[2].body else { panic!() };
        assert!(*truncated && content.len() == TEXT_LIMIT);
        let extra = extra.as_ref().unwrap();
        assert!(extra.get("originalFile").is_none() && extra["structuredPatch"][0]["lines"][1] == "+b");
    }

    #[test]
    fn subagent_transcripts_show_sidechain_records() {
        let s = line(json!({"type":"user","uuid":"s1","parentUuid":null,"isSidechain":true,"agentId":"a1","message":{"content":"去查一下"}}))
            + &line(json!({"type":"assistant","uuid":"s2","parentUuid":"s1","isSidechain":true,"message":{"id":"m1","content":[{"type":"tool_use","id":"t1","name":"Grep","input":{"pattern":"x"}}]}}));
        let mut main = Transcript::new("/nonexistent".into());
        main.feed(s.as_bytes());
        assert_eq!(main.len(), 0, "主会话里不显示子代理的记录");
        let mut sub = Transcript::subagent("/nonexistent".into());
        sub.feed(s.as_bytes());
        assert_eq!(kinds(&sub), ["user", "assistant"]);
        assert_eq!(sub.tool_input("t1").unwrap()["pattern"], "x");
    }

    #[test]
    fn unknown_blocks_and_bad_lines() {
        let mut t = Transcript::new("/nonexistent".into());
        let mut s = user("u1", None, json!("hi"));
        s += "{not json\n";
        s += &assistant("a1", "u1", "m1", json!({"type":"brand_new_block","x":1}));
        t.feed(s.as_bytes());
        assert_eq!(t.stats.failed, 1);
        let items = t.items(0, t.len());
        assert_eq!(items[1].body, Body::Assistant { model: Some("claude-haiku-5-5".into()), blocks: vec![Block::Unknown { raw_type: "brand_new_block".into() }] });
    }

    /// 把一份 transcript 整理后的条目写成 JSON（前端界面检查用）：
    /// `CHAT_DUMP=<transcript> CHAT_DUMP_OUT=<输出> cargo test dump_items -- --ignored`
    #[test]
    #[ignore]
    fn dump_items() {
        let path: PathBuf = std::env::var("CHAT_DUMP").unwrap().into();
        // 子代理的 transcript（subagents/agent-*.jsonl）用子代理模式
        let mut t = if path.parent().is_some_and(|p| p.ends_with("subagents")) { Transcript::subagent(path) } else { Transcript::new(path) };
        t.poll().unwrap();
        let items = t.items(0, t.len());
        std::fs::write(std::env::var("CHAT_DUMP_OUT").unwrap(), serde_json::to_string_pretty(&items).unwrap()).unwrap();
    }

    /// 本机真实的 transcript（有就跑）：整份能解析，失败行为 0。
    #[test]
    fn real_transcripts_if_present() {
        let Some(home) = dirs::home_dir() else { return };
        let Ok(dirs) = std::fs::read_dir(home.join(".claude/projects")) else { return };
        for d in dirs.flatten().take(5) {
            for f in std::fs::read_dir(d.path()).into_iter().flatten().flatten().take(3) {
                if f.path().extension().is_some_and(|e| e == "jsonl") {
                    let mut t = Transcript::new(f.path());
                    t.poll().unwrap();
                    assert_eq!(t.stats.failed, 0, "{}", f.path().display());
                    let _ = serde_json::to_string(&t.items(0, t.len())).unwrap();
                }
            }
        }
    }
}
