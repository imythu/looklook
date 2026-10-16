//! 对话迁移：把一个 AI 编程助手里的对话变成另一个助手里能接着聊的原生会话。
//!
//! 做法参照 session-migrate（xhluca/session-migrate，MIT）：读取器把各自的原生记录投影成一个很小的
//! 中间表示（用户 / 助手文字、工具调用与结果，按顺序），写入器只写目标工具认得的结构，再用目标工具
//! 自己的“继续会话”打开。这里用 Rust 实现，不需要 Python，Windows / macOS / Linux 一样。
//!
//! - Codex：读 `~/.codex/sessions/**/rollout-*.jsonl`；写入走 Codex 自己的 app-server
//!   （`thread/start` + `thread/inject_items`），由 Codex 落盘，`codex resume <id>` 接着聊。
//! - DSH（DeepSeek Harness）：读写 `$DSH_HOME/sessions/<项目>/<会话>/session.v4.jsonl.zstd`，
//!   并登记到 `storages/workspace.json`（DSH 没有导入接口，只能按它的 v4 格式写）。
//!
//! 不迁移（只记数量，见 [`Manifest`]）：推理过程（加密或私有）、图片与附件、加密的压缩摘要、
//! 系统 / 开发者提示和各工具注入的上下文；钩子、MCP、权限等运行配置留在原工具里。
//! 原会话从不修改；目标得到一个新的会话 id。
//!
//! 每个工具可以有多个数据目录（[`Source`]：自动找到的默认目录，以及用户登记的，见 [`registry`]）。
//! Claude Code 读写 `projects/<项目>/<id>.jsonl`（见 [`claude`]）。OpenCode 现在只能登记、列出，还不能读写（[`Harness::can_export`] / [`Harness::can_import`]）。

pub mod claude;
pub mod codex;
pub mod dsh;
pub mod live;
pub mod registry;

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Harness {
    Codex,
    Dsh,
    Claude,
    #[serde(rename = "opencode")]
    #[value(name = "opencode")]
    OpenCode,
}

impl Harness {
    pub const ALL: [Harness; 4] = [Harness::Codex, Harness::Dsh, Harness::Claude, Harness::OpenCode];

    pub fn name(self) -> &'static str {
        match self {
            Harness::Codex => "codex",
            Harness::Dsh => "dsh",
            Harness::Claude => "claude",
            Harness::OpenCode => "opencode",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|h| h.name() == s)
    }

    /// 能不能从它读出对话（迁移出去、看对话记录）
    pub fn can_export(self) -> bool {
        matches!(self, Harness::Codex | Harness::Dsh | Harness::Claude)
    }

    /// 能不能把别的对话写进它（迁移进来）
    pub fn can_import(self) -> bool {
        matches!(self, Harness::Codex | Harness::Dsh | Harness::Claude)
    }

    /// 默认数据目录（跟着各工具自己的环境变量走）
    pub fn default_home(self) -> PathBuf {
        let env = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
        match self {
            Harness::Codex => codex::codex_home(),
            Harness::Dsh => dsh::dsh_home(),
            Harness::Claude => env("CLAUDE_CONFIG_DIR").unwrap_or_else(|| home().join(".claude")),
            // OpenCode 的对话在数据目录里（不是配置目录）
            Harness::OpenCode => env("XDG_DATA_HOME").unwrap_or_else(|| home().join(".local").join("share")).join("opencode"),
        }
    }
}

/// 一个对话来源 / 迁移目标：某个工具的一个数据目录。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Source {
    /// 自动找到的默认目录用工具名（`codex`），登记的用 `reg-…`
    pub id: String,
    pub kind: Harness,
    pub name: String,
    pub home: PathBuf,
    /// 程序位置（只有 Codex 写入时用到）；空着就在用户的 shell 里找
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bin: Option<String>,
}

impl Source {
    pub fn default_for(kind: Harness) -> Self {
        Source { id: kind.name().to_string(), kind, name: String::new(), home: kind.default_home(), bin: None }
    }
}

fn unsupported(kind: Harness, what: &str) -> anyhow::Error {
    anyhow::Error::new(Unsupported { kind, what: what.to_string() })
}

/// 这个工具还不支持这件事（读或写）；网关把它变成 `HARNESS_UNSUPPORTED`。
#[derive(Debug)]
pub struct Unsupported {
    pub kind: Harness,
    pub what: String,
}

impl std::fmt::Display for Unsupported {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "还不支持{} {} / {} does not support {} yet", self.kind.name(), self.what, self.kind.name(), self.what)
    }
}

impl std::error::Error for Unsupported {}

/// 中间表示里的一项，按对话顺序。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Item {
    User { text: String, time_ms: i64 },
    Assistant { text: String, time_ms: i64 },
    ToolCall { id: String, name: String, arguments: String, time_ms: i64 },
    ToolResult { call_id: String, output: String, is_error: bool, time_ms: i64 },
}

impl Item {
    pub fn time_ms(&self) -> i64 {
        match self {
            Item::User { time_ms, .. } | Item::Assistant { time_ms, .. } | Item::ToolCall { time_ms, .. } | Item::ToolResult { time_ms, .. } => *time_ms,
        }
    }
}

/// 迁移清单：哪些东西没有带过去、各有多少。不含任何对话内容。
#[derive(Debug, Default, Clone, Serialize)]
pub struct Manifest {
    pub dropped: BTreeMap<String, usize>,
    /// 为了让目标工具接受而补上的东西（例如原会话里没有结果的工具调用）
    pub synthesized: BTreeMap<String, usize>,
}

impl Manifest {
    pub fn drop(&mut self, what: &str) {
        *self.dropped.entry(what.to_string()).or_default() += 1;
    }
    fn synth(&mut self, what: &str) {
        *self.synthesized.entry(what.to_string()).or_default() += 1;
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Conversation {
    pub harness: Harness,
    pub id: String,
    pub title: Option<String>,
    pub cwd: Option<String>,
    pub created_ms: i64,
    /// 原会话用的模型（知道时），写进目标的助手消息来源
    pub model: Option<String>,
    pub items: Vec<Item>,
    pub manifest: Manifest,
}

/// 会话列表里的一行。
#[derive(Debug, Clone, Serialize)]
pub struct Summary {
    pub harness: Harness,
    pub id: String,
    pub title: Option<String>,
    pub cwd: Option<String>,
    pub updated_ms: i64,
}

/// 迁移结果：目标里的新会话，以及接着聊的命令。
#[derive(Debug, Clone, Serialize)]
pub struct Migrated {
    pub harness: Harness,
    pub id: String,
    pub cwd: Option<String>,
    pub resume: String,
    pub items: usize,
    pub manifest: Manifest,
}

/// 某个工具（默认目录）的会话，最近的在前。
pub fn list(h: Harness) -> Result<Vec<Summary>> {
    list_in(&Source::default_for(h))
}

/// 某个来源的会话，最近的在前。
pub fn list_in(src: &Source) -> Result<Vec<Summary>> {
    let mut v = match src.kind {
        Harness::Codex => codex::list(&src.home)?,
        Harness::Dsh => dsh::list(&src.home)?,
        Harness::Claude => claude::list(&src.home)?,
        k => return Err(unsupported(k, "read")),
    };
    v.sort_by(|a, b| b.updated_ms.cmp(&a.updated_ms));
    Ok(v)
}

pub fn read(h: Harness, id: &str) -> Result<Conversation> {
    read_in(&Source::default_for(h), id)
}

pub fn read_in(src: &Source, id: &str) -> Result<Conversation> {
    let mut c = match src.kind {
        Harness::Codex => codex::read(&src.home, id)?,
        Harness::Dsh => dsh::read(&src.home, id)?,
        Harness::Claude => claude::read(&src.home, id)?,
        k => return Err(unsupported(k, "read")),
    };
    normalize(&mut c);
    if c.items.is_empty() {
        bail!("这个会话里没有可以迁移的对话 / This session has no conversation to migrate");
    }
    Ok(c)
}

/// 把 `id` 迁移到 `to`。`cwd` 不填时沿用原会话的工作目录；`codex_bin`：在用户 shell 里找到的 codex 路径。
pub async fn migrate(from: Harness, id: &str, to: Harness, cwd: Option<String>, codex_bin: Option<&str>) -> Result<Migrated> {
    migrate_between(&Source::default_for(from), id, &Source::default_for(to), cwd, codex_bin).await
}

/// 从一个来源迁移到另一个来源（可以是同一种工具的两个数据目录）。
pub async fn migrate_between(from: &Source, id: &str, to: &Source, cwd: Option<String>, codex_bin: Option<&str>) -> Result<Migrated> {
    if !to.kind.can_import() {
        return Err(unsupported(to.kind, "import"));
    }
    let mut c = read_in(from, id)?;
    if let Some(d) = cwd {
        c.cwd = Some(d);
    }
    match to.kind {
        Harness::Codex => codex::write(&to.home, &c, to.bin.as_deref().or(codex_bin)).await,
        Harness::Dsh => dsh::write(&to.home, &c),
        Harness::Claude => claude::write(&to.home, &c),
        k => Err(unsupported(k, "import")),
    }
}

/// 整理成两边都接受的形状：去掉空文字；工具结果必须跟在自己的调用后面（找不到调用的丢弃）；
/// 没有结果的调用补一个“原会话里没有记录结果”的错误结果，放在下一条用户 / 助手文字之前。
/// 时间单调不减。
pub fn normalize(c: &mut Conversation) {
    let mut out: Vec<Item> = Vec::with_capacity(c.items.len());
    let mut open: Vec<String> = Vec::new();
    let mut seen_calls = std::collections::HashSet::new();
    let mut last = c.created_ms;
    // 补上的结果放在当前位置，时间也用当前位置的
    let close = |open: &mut Vec<String>, out: &mut Vec<Item>, m: &mut Manifest, t: i64| {
        for id in open.drain(..) {
            m.synth("missing_tool_result");
            out.push(Item::ToolResult { call_id: id, output: MISSING_RESULT.into(), is_error: true, time_ms: t });
        }
    };
    for mut it in std::mem::take(&mut c.items) {
        let t = it.time_ms().max(last);
        last = t;
        match &mut it {
            Item::User { text, time_ms } | Item::Assistant { text, time_ms } => {
                *time_ms = t;
                if text.trim().is_empty() {
                    continue;
                }
                close(&mut open, &mut out, &mut c.manifest, t);
            }
            Item::ToolCall { id, time_ms, .. } => {
                *time_ms = t;
                if !seen_calls.insert(id.clone()) {
                    c.manifest.drop("duplicate_tool_call");
                    continue;
                }
                open.push(id.clone());
            }
            Item::ToolResult { call_id, time_ms, .. } => {
                *time_ms = t;
                let Some(i) = open.iter().position(|id| id == call_id) else {
                    c.manifest.drop("orphan_tool_result");
                    continue;
                };
                open.remove(i);
            }
        }
        out.push(it);
    }
    close(&mut open, &mut out, &mut c.manifest, last);
    c.items = out;
}

pub const MISSING_RESULT: &str = "(The source session recorded no result for this tool call.)";

/// 会话标题：取第一条用户消息的第一行，最多 60 个字符。
pub fn title_from(items: &[Item]) -> Option<String> {
    items.iter().find_map(|i| match i {
        Item::User { text, .. } => {
            let line = text.lines().map(str::trim).find(|l| !l.is_empty())?;
            let mut s: String = line.chars().take(60).collect();
            if line.chars().count() > 60 {
                s.push('…');
            }
            Some(s)
        }
        _ => None,
    })
}

pub fn now_ms() -> i64 {
    (time::OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000) as i64
}

/// RFC 3339 时间转毫秒，解析不了时用 `fallback`。
pub fn parse_ms(s: Option<&str>, fallback: i64) -> i64 {
    s.and_then(|s| time::OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339).ok())
        .map(|t| (t.unix_timestamp_nanos() / 1_000_000) as i64)
        .unwrap_or(fallback)
}

fn home() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conv(items: Vec<Item>) -> Conversation {
        Conversation { harness: Harness::Codex, id: "x".into(), title: None, cwd: None, created_ms: 10, model: None, items, manifest: Manifest::default() }
    }

    #[test]
    fn normalize_pairs_tools_and_orders_time() {
        let mut c = conv(vec![
            Item::User { text: "hi".into(), time_ms: 20 },
            Item::ToolResult { call_id: "nope".into(), output: "x".into(), is_error: false, time_ms: 21 },
            Item::ToolCall { id: "a".into(), name: "exec".into(), arguments: "{}".into(), time_ms: 15 },
            Item::ToolCall { id: "b".into(), name: "exec".into(), arguments: "{}".into(), time_ms: 22 },
            Item::ToolResult { call_id: "b".into(), output: "ok".into(), is_error: false, time_ms: 23 },
            Item::Assistant { text: "  ".into(), time_ms: 24 },
            Item::Assistant { text: "done".into(), time_ms: 25 },
        ]);
        normalize(&mut c);
        let kinds: Vec<_> = c.items.iter().map(|i| match i {
            Item::User { .. } => "u".to_string(),
            Item::Assistant { .. } => "a".to_string(),
            Item::ToolCall { id, .. } => format!("c{id}"),
            Item::ToolResult { call_id, .. } => format!("r{call_id}"),
        }).collect();
        assert_eq!(kinds, ["u", "ca", "cb", "rb", "ra", "a"]);
        assert!(c.items.windows(2).all(|w| w[0].time_ms() <= w[1].time_ms()));
        assert_eq!(c.manifest.dropped["orphan_tool_result"], 1);
        assert_eq!(c.manifest.synthesized["missing_tool_result"], 1);
    }

    #[test]
    fn title_uses_first_user_line() {
        let t = title_from(&[Item::Assistant { text: "x".into(), time_ms: 0 }, Item::User { text: "\n  修复登录页的 bug\n细节".into(), time_ms: 0 }]);
        assert_eq!(t.as_deref(), Some("修复登录页的 bug"));
    }
}
