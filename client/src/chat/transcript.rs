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
        /// Claude 在忙时发的、这一步做完并进这一轮的消息（`queued_command`）
        #[serde(skip_serializing_if = "std::ops::Not::not")]
        mid: bool,
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
    /// 后台任务（后台命令、后台子代理）结束的通知（`<task-notification>`）
    Task { tool_use_id: String, status: String, summary: String },
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
    /// 压缩分界（`compact_boundary`）：它的 `logicalParentUuid` 有时指向文件里没有的记录
    boundary: bool,
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
    /// 每个 `message.id` 的第一行记录：同一次回复被工具结果隔开、拆成几条显示时，后面几条的 key 要另加后缀
    first_of_msg: HashMap<String, usize>,
    /// 显示的条目：每条是一组 (记录序号, 条目序号)，assistant 合并多行时有多个
    display: Vec<Vec<(usize, usize)>>,
    pub version: Option<String>,
    pub stats: Stats,
    /// 子代理自己的记录（`subagents/agent-<id>.jsonl`）：每条都标着 isSidechain，要照常显示
    sidechain: bool,
    /// 后台任务结束的通知，按工具调用 id。在终端里停掉的任务只有排队记录（`queue-operation`），不在对话链上，所以单独记
    pub notes: HashMap<String, Note>,
    /// 最近一条回复实际用的模型（API 返回的 `message.model`）
    pub model: Option<String>,
    /// 最近一条回复的 token 用量（`message.usage`，原样）和它的 `message.id`
    pub usage: Option<Value>,
    pub model_msg: Option<String>,
    /// 这个会话累计的 token：输入（含写缓存）、读缓存、输出。同一条回复拆成多行，每行带着到那时为止的用量，按 message.id 取最后一行的
    pub totals: Totals,
    per_msg: HashMap<String, Totals>,
    /// 会话标题：`/rename` 起的（`custom-title`），没有就用 Claude Code 生成的（`ai-title`）
    pub title: Option<String>,
    custom_title: bool,
    /// 运行中发的、还在 Claude Code 队列里的消息（`queue-operation`）。这一步做完会并进对话（`queued_command` 附件），
    /// 或者这一轮结束后当成下一条消息发出去
    pub queued: Vec<String>,
    /// Claude Code 的整个队列（含不显示的后台任务通知）：`dequeue` 不带内容，取的是最前面那条，要照着它的顺序记
    queue: std::collections::VecDeque<String>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Serialize)]
pub struct Totals {
    pub input: u64,
    pub cache_read: u64,
    pub output: u64,
}

/// 后台任务结束的通知
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Note {
    /// completed / failed / killed …
    pub status: String,
    pub summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_file: Option<String>,
}

/// 对话里的一次工具调用和它的结果（后台任务列表用）
pub struct ToolCall<'a> {
    pub ts: Option<&'a str>,
    pub id: &'a str,
    pub name: &'a str,
    pub input: &'a Value,
    pub result: Option<&'a Item>,
}

/// 结果显示这次 Bash 已经转到后台继续跑（参数里没有 `run_in_background`，Claude Code 2.1.310）：
/// 在终端里按了 Ctrl+B、运行中发来的消息要送达（`so that a message that arrived while it was running can reach you`）、
/// 超时。结果里带 `backgroundTaskId`；同一条记录里有几个结果时没有这个字段，只能看文字。
pub fn moved_to_background(result: Option<&Item>) -> bool {
    const SAYS: [&str; 3] = ["Command was manually backgrounded", "Command was moved to the background", "was moved to the background"];
    match result.map(|r| &r.body) {
        Some(Body::ToolResult { content, extra, .. }) => extra.as_ref().is_some_and(|e| e["backgroundTaskId"].as_str().is_some_and(|s| !s.is_empty())) || SAYS.iter().any(|s| content.contains(s)),
        _ => false,
    }
}

impl ToolCall<'_> {
    /// 在后台跑的 Bash：一开始就放到后台的，或者跑到一半转到后台的
    pub fn is_background_shell(&self) -> bool {
        self.name == "Bash" && (self.input["run_in_background"] == true || moved_to_background(self.result))
    }
}

/// 从显示用的条目里配出每次工具调用和它的结果（别的工具的记录文件用，例如 Codex 的 rollout）
pub fn tool_calls_in(items: &[Item]) -> Vec<ToolCall<'_>> {
    let results: HashMap<&str, &Item> = items
        .iter()
        .filter_map(|i| match &i.body {
            Body::ToolResult { tool_use_id, .. } => Some((tool_use_id.as_str(), i)),
            _ => None,
        })
        .collect();
    items
        .iter()
        .flat_map(|i| match &i.body {
            Body::Assistant { blocks, .. } => blocks
                .iter()
                .filter_map(|b| match b {
                    Block::ToolUse { id, name, input, .. } => Some(ToolCall { ts: i.ts.as_deref(), id, name, input, result: results.get(id.as_str()).copied() }),
                    _ => None,
                })
                .collect(),
            _ => vec![],
        })
        .collect()
}

impl Transcript {
    pub fn new(path: PathBuf) -> Self {
        Self { path, pos: 0, partial: vec![], recs: vec![], by_uuid: HashMap::new(), first_of_msg: HashMap::new(), display: vec![], version: None, stats: Stats::default(), sidechain: false, notes: HashMap::new(), model: None, usage: None, model_msg: None, totals: Totals::default(), per_msg: HashMap::new(), title: None, custom_title: false, queued: vec![], queue: Default::default() }
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

    /// 显示的链上所有的工具调用（按顺序），带上各自的结果
    pub fn tool_calls(&self) -> Vec<ToolCall<'_>> {
        let mut results: HashMap<&str, &Item> = HashMap::new();
        let mut calls = vec![];
        for group in &self.display {
            for (r, i) in group {
                let item = &self.recs[*r].items[*i];
                match &item.body {
                    Body::Assistant { blocks, .. } => {
                        for b in blocks {
                            if let Block::ToolUse { id, name, input, .. } = b {
                                calls.push(ToolCall { ts: item.ts.as_deref(), id, name, input, result: None });
                            }
                        }
                    }
                    Body::ToolResult { tool_use_id, .. } => {
                        results.insert(tool_use_id, item);
                    }
                    _ => {}
                }
            }
        }
        for c in &mut calls {
            c.result = results.get(c.id).copied();
        }
        calls
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
                        if let Some(m) = rec.message_id.as_ref().filter(|_| !rec.items.is_empty()) {
                            self.first_of_msg.entry(m.clone()).or_insert(self.recs.len());
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
            // 同一次回复中间夹了工具结果会拆成几条，key 都是 `a:<message.id>`；前端按 key 渲染，重复会把卡片越画越多
            let rec = &self.recs[r0];
            if rec.message_id.as_ref().and_then(|m| self.first_of_msg.get(m)) != Some(&r0) {
                item.key = format!("{}:{}", item.key, rec.uuid.as_deref().unwrap_or(""));
            }
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
            let rec = &self.recs[i];
            cur = rec.parent.as_ref().and_then(|p| self.by_uuid.get(p)).copied();
            // 压缩分界指向的前一条记录不在文件里（实测 2.1.30x 的自动压缩会这样）：接到分界之前写下的最后一条主线记录，
            // 不然压缩前的对话在界面上整个消失
            if cur.is_none() && rec.boundary {
                cur = self.recs[..i].iter().rposition(|r| r.uuid.is_some() && (self.sidechain || !r.sidechain));
            }
        }
        chain.reverse();
        let chain = self.with_parallel_results(chain);
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

    /// 并行的工具调用：Claude Code 把第一个调用的结果挂在第一个调用下面，和第二个调用是兄弟，
    /// 沿父节点往回走的那条链上没有它（实测：两个 WebSearch 并行，只显示得出后一个的结果，前一张卡片一直在转）。
    /// 把这种“只有工具结果、父节点在链上、答的是链上还没有结果的调用”的兄弟记录接回链上，放在父节点后面。
    /// `/rewind` 丢掉的分支不会被带回来：那些分支里是用户消息和回复，不是单纯的工具结果。
    fn with_parallel_results(&self, chain: Vec<usize>) -> Vec<usize> {
        let on_chain: HashSet<usize> = chain.iter().copied().collect();
        let items = || chain.iter().flat_map(|&r| self.recs[r].items.iter());
        let uses: HashSet<&str> = items()
            .flat_map(|i| match &i.body {
                Body::Assistant { blocks, .. } => blocks.iter().filter_map(|b| if let Block::ToolUse { id, .. } = b { Some(id.as_str()) } else { None }).collect(),
                _ => vec![],
            })
            .collect();
        let answered: HashSet<&str> = items().filter_map(|i| if let Body::ToolResult { tool_use_id, .. } = &i.body { Some(tool_use_id.as_str()) } else { None }).collect();
        let mut extra: HashMap<usize, Vec<usize>> = HashMap::new();
        for (n, rec) in self.recs.iter().enumerate() {
            if on_chain.contains(&n) || rec.items.is_empty() {
                continue;
            }
            let only_missing_results = rec.items.iter().all(|i| matches!(&i.body, Body::ToolResult { tool_use_id, .. } if uses.contains(tool_use_id.as_str()) && !answered.contains(tool_use_id.as_str())));
            let parent = rec.parent.as_ref().and_then(|p| self.by_uuid.get(p)).copied();
            if let (true, Some(p)) = (only_missing_results, parent.filter(|p| on_chain.contains(p))) {
                extra.entry(p).or_default().push(n);
            }
        }
        if extra.is_empty() {
            return chain;
        }
        chain.into_iter().flat_map(|r| std::iter::once(r).chain(extra.remove(&r).unwrap_or_default())).collect()
    }

    fn parse(&mut self, v: &Value) -> Option<Rec> {
        let ty = v["type"].as_str()?;
        if let Some((id, note)) = task_note(v) {
            self.notes.insert(id, note);
        }
        if let Some(ver) = v["version"].as_str() {
            if self.version.as_deref() != Some(ver) {
                self.version = Some(ver.to_string());
            }
        }
        match ty {
            "custom-title" => {
                if let Some(t) = v["customTitle"].as_str().filter(|t| !t.trim().is_empty()) {
                    self.title = Some(t.trim().to_string());
                    self.custom_title = true;
                }
            }
            "queue-operation" => self.queue_op(v),
            // 压缩后上下文只剩摘要：用 `postTokens`（新版才写），没有就当作不知道，等下一条回复
            "system" if v["subtype"] == "compact_boundary" && v["isSidechain"] != true => {
                self.usage = v["compactMetadata"]["postTokens"].as_u64().map(|n| serde_json::json!({ "input_tokens": n }));
            }
            "ai-title" if !self.custom_title => self.title = v["aiTitle"].as_str().map(|t| t.trim().to_string()).filter(|t| !t.is_empty()),
            _ => {}
        }
        let uuid = v["uuid"].as_str().map(String::from);
        // 链上的每条记录都要留着（附件、元信息也在链里），哪怕它本身不显示
        let mut rec = Rec {
            parent: v["parentUuid"].as_str().or_else(|| v["logicalParentUuid"].as_str()).map(String::from),
            sidechain: v["isSidechain"].as_bool().unwrap_or(false) && !self.sidechain,
            boundary: v["subtype"] == "compact_boundary",
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
                // `<synthetic>`：Claude Code 自己写的提示（出错、停止），不是模型的回复
                let model = m["model"].as_str().filter(|s| !s.starts_with('<')).map(String::from);
                if model.is_some() {
                    self.model.clone_from(&model);
                    self.model_msg.clone_from(&rec.message_id);
                    let u = &m["usage"];
                    if u.is_object() {
                        self.usage = Some(u.clone());
                        if let Some(id) = &rec.message_id {
                            let n = |k: &str| u[k].as_u64().unwrap_or(0);
                            let now = Totals { input: n("input_tokens") + n("cache_creation_input_tokens"), cache_read: n("cache_read_input_tokens"), output: n("output_tokens") };
                            let old = self.per_msg.insert(id.clone(), now).unwrap_or_default();
                            self.totals.input = self.totals.input + now.input - old.input.min(self.totals.input);
                            self.totals.cache_read = self.totals.cache_read + now.cache_read - old.cache_read.min(self.totals.cache_read);
                            self.totals.output = self.totals.output + now.output - old.output.min(self.totals.output);
                        }
                    }
                }
                if !blocks.is_empty() {
                    rec.items.push(Item { key: format!("a:{}", rec.message_id.as_deref().unwrap_or("")), ts, body: Body::Assistant { model, blocks } });
                }
            }
            "system" if !rec.sidechain => rec.items = system_items(v, &key, ts).into_iter().collect(),
            // 运行中发的消息：这一步做完时并进对话，记成附件
            "attachment" if !rec.sidechain && v["attachment"]["type"] == "queued_command" => rec.items = queued_items(&v["attachment"], &key, ts),
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

impl Transcript {
    /// 照着 Claude Code 的队列记：`enqueue` 放到最后；`dequeue`（立即发送、这一轮结束后发出）不带内容，取最前面那条；
    /// `remove`（并进这一轮）按内容删；`popAll` / `clear`（按上键拿回去编辑等）清空。界面只显示用户发的。
    fn queue_op(&mut self, v: &Value) {
        let text = v["content"].as_str();
        match v["operation"].as_str() {
            Some("enqueue") => self.queue.push_back(text.unwrap_or("").to_string()),
            Some("dequeue") => {
                self.queue.pop_front();
            }
            Some("popAll" | "clear") => self.queue.clear(),
            _ => {
                if let Some(n) = text.and_then(|t| self.queue.iter().position(|q| q == t)) {
                    self.queue.remove(n);
                }
            }
        }
        self.queued = self.queue.iter().filter(|s| !s.contains("<task-notification>") && !s.trim().is_empty()).cloned().collect();
    }
}

/// 运行中发的消息并进对话时的附件（`attachment.type = queued_command`）：显示成用户消息。
/// 后台任务的通知、别的来源（不是人发的）不显示
fn queued_items(a: &Value, key: &dyn Fn(usize) -> String, ts: Option<String>) -> Vec<Item> {
    if a["commandMode"].as_str().is_some_and(|m| m != "prompt") || a["origin"]["kind"].as_str().is_some_and(|k| k != "human") {
        return vec![];
    }
    let (text, images, truncated) = join_text(&a["prompt"]);
    if text.contains("<task-notification>") || (text.trim().is_empty() && images == 0) {
        return vec![];
    }
    vec![Item { key: key(0), ts, body: Body::User { text, images, truncated, mid: true } }]
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
        // /clear（/new、/reset）是新会话记录里的第一条：对话界面上新会话应该是空的首页，不显示它
        if cmd.is_empty() || cmd == "/clear" || cmd.starts_with("/clear\n") {
            return vec![];
        }
        return vec![Item { key: key(0), ts, body: Body::Command { text: cmd } }];
    }
    if trimmed.contains("<task-notification>") {
        return match task_note(v) {
            Some((tool_use_id, n)) => vec![Item { key: key(0), ts, body: Body::Task { tool_use_id, status: n.status, summary: n.summary } }],
            None => vec![],
        };
    }
    if trimmed.is_empty() && images == 0 {
        return vec![];
    }
    vec![Item { key: key(0), ts, body: Body::User { text, images, truncated, mid: false } }]
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
        // 没有输出的命令（/clear 的 `<local-command-stdout></local-command-stdout>`）不显示
        "local_command" => Body::Command { text: command_text(v["content"].as_str()?).filter(|t| !t.is_empty())? },
        "informational" => Body::Info { text: truncate(v["content"].as_str()?, TEXT_LIMIT).0 },
        _ => return None,
    };
    Some(Item { key: key(0), ts, body })
}

/// 后台任务结束的通知：用户消息，或者排队记录（`queue-operation`，在终端里停掉的任务只有这个）。
/// ```text
/// <task-notification>
/// <task-id>b95azumrm</task-id>
/// <tool-use-id>toolu_01RB…</tool-use-id>
/// <output-file>/tmp/claude-0/…/tasks/b95azumrm.output</output-file>
/// <status>killed</status>
/// <summary>Task "Sleep for 600 seconds in background" was stopped by the user</summary>
/// </task-notification>
/// ```
fn task_note(v: &Value) -> Option<(String, Note)> {
    let text = match v["content"].as_str() {
        Some(s) => s.to_string(),
        None => join_text(&v["message"]["content"]).0,
    };
    let start = text.find("<task-notification>")?;
    let body = &text[start..];
    let tag = |t: &str| {
        let open = format!("<{t}>");
        let rest = &body[body.find(&open)? + open.len()..];
        Some(rest[..rest.find(&format!("</{t}>"))?].trim().to_string())
    };
    let id = tag("tool-use-id").filter(|s| !s.is_empty())?;
    Some((id, Note { status: tag("status").unwrap_or_default(), summary: truncate(&tag("summary").unwrap_or_default(), 2_000).0, output_file: tag("output-file") }))
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

/// 一条原始记录（`full_item` 取到的那一行）里用户消息的第 `n` 张图片：类型和内容（对话界面上点开看大图用）。
/// 和 `join_text` 的计数一样只看第一层内容块：`message.content`，运行中发的是 `attachment.prompt`。
pub fn user_image(rec: &Value, n: usize) -> Option<(&'static str, Vec<u8>)> {
    use base64::Engine;
    let roots = [&rec["message"]["content"], &rec["attachment"]["prompt"]];
    let img = roots.iter().filter_map(|r| r.as_array()).flatten().filter(|b| b["type"] == "image").nth(n)?;
    if img["source"]["type"] != "base64" {
        return None;
    }
    let mime = match img["source"]["media_type"].as_str()? {
        "image/png" => "image/png",
        "image/jpeg" | "image/jpg" => "image/jpeg",
        "image/gif" => "image/gif",
        "image/webp" => "image/webp",
        _ => return None,
    };
    let data = base64::engine::general_purpose::STANDARD.decode(img["source"]["data"].as_str()?.trim()).ok()?;
    Some((mime, data))
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
    #[test]
    fn user_image_from_records() {
        let img = |d: Option<(&str, Vec<u8>)>| d.map(|(m, b)| format!("{m}:{}", String::from_utf8_lossy(&b)));
        // 内容块里的 base64；第二张；不支持的类型不给
        let rec = serde_json::json!({ "message": { "content": [
            { "type": "text", "text": "看图" },
            { "type": "image", "source": { "type": "base64", "media_type": "image/png", "data": "YWJj" } },
            { "type": "image", "source": { "type": "base64", "media_type": "image/jpeg", "data": "eHl6" } },
            { "type": "image", "source": { "type": "base64", "media_type": "image/svg+xml", "data": "PHN2Zz4=" } },
        ] } });
        assert_eq!(img(user_image(&rec, 0)).unwrap(), "image/png:abc");
        assert_eq!(img(user_image(&rec, 1)).unwrap(), "image/jpeg:xyz");
        assert!(user_image(&rec, 2).is_none());
        assert!(user_image(&rec, 3).is_none());
        // 运行中发的（attachment.prompt）
        let q = serde_json::json!({ "attachment": { "prompt": [{ "type": "image", "source": { "type": "base64", "media_type": "image/webp", "data": "cQ==" } }] } });
        assert_eq!(img(user_image(&q, 0)).unwrap(), "image/webp:q");
    }

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

    /// 运行中发的消息（Claude Code 2.1.310 实测）：先进队列，这一步做完时作为附件并进对话
    #[test]
    fn queued_message_absorbed_mid_turn() {
        let mut t = Transcript::new("/nonexistent".into());
        let mut s = user("u1", None, json!("跑一下 sleep 20"));
        s += &assistant("a1", "u1", "m1", json!({"type":"tool_use","id":"t1","name":"Bash","input":{"command":"sleep 20"}}));
        s += &line(json!({"type":"queue-operation","operation":"enqueue","content":"顺便算一下 2+3"}));
        t.feed(s.as_bytes());
        assert_eq!(t.queued, ["顺便算一下 2+3"]);
        let mut s = line(json!({"type":"queue-operation","operation":"remove","content":"顺便算一下 2+3","reason":"absorbed_mid_turn"}));
        s += &line(json!({"type":"user","uuid":"r1","parentUuid":"a1","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"done"}]}}));
        s += &line(json!({"type":"attachment","uuid":"q1","parentUuid":"r1","attachment":{"type":"queued_command","prompt":"顺便算一下 2+3","commandMode":"prompt","origin":{"kind":"human"}}}));
        s += &assistant("a2", "q1", "m2", json!({"type":"text","text":"done，2+3=5"}));
        t.feed(s.as_bytes());
        assert!(t.queued.is_empty());
        assert_eq!(kinds(&t), ["user", "assistant", "tool_result", "user", "assistant"]);
        let Body::User { text, mid, .. } = &t.items(3, 4)[0].body else { panic!() };
        assert_eq!(text, "顺便算一下 2+3");
        assert!(mid);
        // 立即发送：dequeue 不带内容，取最前面的（后台任务通知也在队列里，只是不显示）
        t.feed(line(json!({"type":"queue-operation","operation":"enqueue","content":"<task-notification>t</task-notification>"})).as_bytes());
        t.feed(line(json!({"type":"queue-operation","operation":"enqueue","content":"马上看这个"})).as_bytes());
        assert_eq!(t.queued, ["马上看这个"]);
        t.feed(line(json!({"type":"queue-operation","operation":"dequeue"})).as_bytes());
        assert_eq!(t.queued, ["马上看这个"], "先取走的是通知");
        t.feed(line(json!({"type":"queue-operation","operation":"dequeue"})).as_bytes());
        assert!(t.queued.is_empty());
        // 按上键拿回去编辑：队列清空
        t.feed(line(json!({"type":"queue-operation","operation":"enqueue","content":"x"})).as_bytes());
        t.feed(line(json!({"type":"queue-operation","operation":"popAll"})).as_bytes());
        assert!(t.queued.is_empty());
    }

    /// 跑到一半转到后台的命令（运行中发来消息、Ctrl+B、超时）也算后台命令
    #[test]
    fn bash_moved_to_background() {
        let mut t = Transcript::new("/nonexistent".into());
        let mut s = user("u1", None, json!("跑测试"));
        s += &assistant("a1", "u1", "m1", json!({"type":"tool_use","id":"t1","name":"Bash","input":{"command":"npm test"}}));
        s += &line(json!({"type":"user","uuid":"r1","parentUuid":"a1","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"Command was moved to the background (ID: b7) so that a message that arrived while it was running can reach you; it was not interrupted. Output is being written to: /tmp/x"}]},"toolUseResult":{"backgroundTaskId":"b7","backgroundedToDeliverMessage":true}}));
        s += &assistant("a2", "r1", "m2", json!({"type":"tool_use","id":"t2","name":"Bash","input":{"command":"ls"}}));
        s += &line(json!({"type":"user","uuid":"r2","parentUuid":"a2","message":{"content":[{"type":"tool_result","tool_use_id":"t2","content":"a.txt"}]}}));
        s += &assistant("a3", "r2", "m3", json!({"type":"tool_use","id":"t3","name":"Bash","input":{"command":"sleep 99","run_in_background":true}}));
        t.feed(s.as_bytes());
        let bg: Vec<&str> = t.tool_calls().into_iter().filter(|c| c.is_background_shell()).map(|c| c.id).collect();
        assert_eq!(bg, ["t1", "t3"]);
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

    /// 后台任务：通知显示成一条简短的事件；在终端里停掉的只有排队记录，也要记下来
    #[test]
    fn background_task_notifications() {
        let mut t = Transcript::new("/nonexistent".into());
        let note = |id: &str, status: &str| format!("<task-notification>\n<task-id>b1</task-id>\n<tool-use-id>{id}</tool-use-id>\n<output-file>/tmp/claude-0/p/s/tasks/b1.output</output-file>\n<status>{status}</status>\n<summary>Task \"sleep\" {status}</summary>\n</task-notification>");
        let mut s = user("u1", None, json!("后台跑两个命令"));
        s += &assistant("a1", "u1", "m1", json!({"type":"tool_use","id":"toolu_1","name":"Bash","input":{"command":"sleep 600","run_in_background":true}}));
        s += &line(json!({"type":"user","uuid":"r1","parentUuid":"a1","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_1","content":"Command running in background with ID: b1."}]},"toolUseResult":{"backgroundTaskId":"b1"}}));
        s += &assistant("a2", "r1", "m2", json!({"type":"tool_use","id":"toolu_2","name":"Bash","input":{"command":"sleep 700","run_in_background":true}}));
        s += &line(json!({"type":"queue-operation","operation":"enqueue","content":note("toolu_2","killed")}));
        s += &line(json!({"type":"user","uuid":"n1","parentUuid":"a2","origin":{"kind":"task-notification"},"message":{"content":note("toolu_1","completed")}}));
        t.feed(s.as_bytes());
        assert_eq!(t.notes["toolu_2"].status, "killed");
        assert_eq!(t.notes["toolu_1"].output_file.as_deref(), Some("/tmp/claude-0/p/s/tasks/b1.output"));
        let last = t.items(t.len() - 1, t.len()).remove(0);
        assert_eq!(last.body, Body::Task { tool_use_id: "toolu_1".into(), status: "completed".into(), summary: "Task \"sleep\" completed".into() });
        let calls = t.tool_calls();
        assert_eq!(calls.iter().map(|c| (c.id, c.result.is_some())).collect::<Vec<_>>(), [("toolu_1", true), ("toolu_2", false)]);
    }

    #[test]
    fn reply_split_by_tool_result_gets_distinct_keys() {
        // 一次回复：思考、Write、（Write 的结果）、Artifact —— 结果夹在中间，显示成两条，key 不能一样
        let mut t = Transcript::new("/nonexistent".into());
        let mut s = user("u1", None, json!("做个网页"));
        s += &assistant("a1", "u1", "msg_1", json!({"type":"thinking","thinking":"","signature":"x"}));
        s += &assistant("a2", "a1", "msg_1", json!({"type":"tool_use","id":"t1","name":"Write","input":{"file_path":"/a.html"}}));
        s += &line(json!({"type":"user","uuid":"r1","parentUuid":"a2","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"ok"}]}}));
        s += &assistant("a3", "r1", "msg_1", json!({"type":"tool_use","id":"t2","name":"Artifact","input":{}}));
        t.feed(s.as_bytes());
        assert_eq!(kinds(&t), ["user", "assistant", "tool_result", "assistant"]);
        let items = t.items(0, t.len());
        assert_eq!(items[1].key, "a:msg_1");
        assert_eq!(items[3].key, "a:msg_1:a3");
        // 后面那条再多一个内容块，key 不变
        let Change::Upsert { start, items } = t.feed(assistant("a4", "a3", "msg_1", json!({"type":"text","text":"好了"})).as_bytes()) else { panic!() };
        assert_eq!((start, items[0].key.as_str()), (3, "a:msg_1:a3"));
    }

    #[test]
    fn parallel_tool_results_on_sibling_records() {
        // 实测的结构：两个并行的 WebSearch，第一个的结果挂在第一个调用下面（和第二个调用是兄弟）
        let mut t = Transcript::new("/nonexistent".into());
        let mut s = user("u1", None, json!("查一下"));
        s += &assistant("a1", "u1", "msg_1", json!({"type":"tool_use","id":"s1","name":"WebSearch","input":{"query":"a"}}));
        s += &assistant("a2", "a1", "msg_1", json!({"type":"tool_use","id":"s2","name":"WebSearch","input":{"query":"b"}}));
        s += &line(json!({"type":"user","uuid":"r1","parentUuid":"a1","message":{"content":[{"type":"tool_result","tool_use_id":"s1","content":"A"}]}}));
        s += &line(json!({"type":"user","uuid":"r2","parentUuid":"a2","message":{"content":[{"type":"tool_result","tool_use_id":"s2","content":"B"}]}}));
        s += &assistant("a3", "r2", "msg_2", json!({"type":"text","text":"好了"}));
        t.feed(s.as_bytes());
        let results: Vec<String> = t.items(0, t.len()).into_iter().filter_map(|i| if let Body::ToolResult { tool_use_id, .. } = i.body { Some(tool_use_id) } else { None }).collect();
        assert_eq!(results, ["s1", "s2"], "两个结果都要有");
        assert_eq!(kinds(&t).last().map(String::as_str), Some("assistant"));
        // /rewind 丢掉的分支（用户消息）不接回来
        let mut t = Transcript::new("/nonexistent".into());
        let mut s = user("u1", None, json!("第一版"));
        s += &assistant("a1", "u1", "msg_1", json!({"type":"text","text":"x"}));
        s += &user("u2", Some("u1"), json!("改过的问题"));
        t.feed(s.as_bytes());
        assert_eq!(kinds(&t), ["user", "user"], "被丢掉的回复不接回来");
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

    /// 手动压缩后（2.1.309 实测记录）：上下文用量换成 `postTokens`，不再是压缩前那条回复的
    #[test]
    fn compaction_resets_context_usage() {
        let mut t = Transcript::new("/nonexistent".into());
        let mut s = user("u1", None, json!("你好"));
        s += &line(json!({"type":"assistant","uuid":"a1","parentUuid":"u1","message":{"id":"m1","model":"claude-haiku-5-5","content":[{"type":"text","text":"好"}],"usage":{"input_tokens":3,"cache_read_input_tokens":452800}}}));
        s += &line(json!({"type":"system","subtype":"compact_boundary","uuid":"c1","parentUuid":null,"logicalParentUuid":"a1","compactMetadata":{"trigger":"manual","preTokens":452804,"postTokens":20313}}));
        t.feed(s.as_bytes());
        assert_eq!(t.usage, Some(json!({"input_tokens": 20313})));
        // 旧版没写 postTokens：不知道就不显示，等下一条回复
        t.feed(line(json!({"type":"system","subtype":"compact_boundary","uuid":"c2","parentUuid":null,"logicalParentUuid":"c1","compactMetadata":{"trigger":"auto","preTokens":1}})).as_bytes());
        assert_eq!(t.usage, None);
    }

    #[test]
    fn compaction_with_missing_logical_parent_keeps_history() {
        let mut t = Transcript::new("/nonexistent".into());
        let mut s = user("u1", None, json!("记住暗号"));
        s += &assistant("a1", "u1", "m1", json!({"type":"text","text":"好的"}));
        // 指向一条文件里没有的记录
        s += &line(json!({"type":"system","subtype":"compact_boundary","uuid":"c1","parentUuid":null,"logicalParentUuid":"gone","content":"Conversation compacted"}));
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
    fn clear_starts_an_empty_conversation() {
        // 2.1.310 的 /new（/clear 的别名）之后，新会话的记录开头是这几条
        let mut t = Transcript::new("/nonexistent".into());
        let mut s = line(json!({"type":"user","uuid":"m1","parentUuid":null,"isMeta":true,"message":{"content":"<local-command-caveat>The command below was run directly in Claude Code</local-command-caveat>"}}));
        s += &line(json!({"type":"user","uuid":"c1","parentUuid":"m1","message":{"content":"<command-name>/clear</command-name>\n            <command-message>clear</command-message>\n            <command-args></command-args>"}}));
        s += &line(json!({"type":"system","subtype":"local_command","uuid":"c2","parentUuid":"c1","content":"<local-command-stdout></local-command-stdout>"}));
        t.feed(s.as_bytes());
        assert!(kinds(&t).is_empty(), "{:?}", kinds(&t));
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
