//! 以前的对话：读出这台电脑上 Claude Code、Codex、OpenCode 自己存的对话记录，会话页里列出来、接着聊。
//! 只读，不改它们的文件。
//!
//! - **Claude Code**：`<配置目录>/projects/<文件夹>/<会话号>.jsonl`。文件夹名是工作目录把非字母数字换成 `-`，
//!   会丢信息，工作目录以记录里的 `cwd` 为准。标题取 `custom-title`（/rename）> `ai-title` > `summary` > 第一句话。
//! - **Codex**：`$CODEX_HOME/sessions/年/月/日/rollout-*.jsonl`，第一行 `session_meta` 里有会话号和 `cwd`；
//!   标题在 `session_index.jsonl`（`thread_name`，同一个会话号以最后一行为准）。子代理、`codex exec` 的会话不列。
//! - **OpenCode**：数据目录（`$XDG_DATA_HOME/opencode`，默认 `~/.local/share/opencode`）里的 `opencode.db`
//!   （新版）或 `storage/session/*/*.json`（旧版），只取标题、目录、时间。
//!
//! 每个文件只读开头和结尾：开头拿工作目录、分支、第一句话，结尾拿最新的标题、最后一句话。
//! 按 路径 + 修改时间 + 大小 缓存，再次打开只需要 stat。

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 对话文件这么久之内还在变，就当作可能正开在别的终端里。
const LIVE_SECS: u64 = 120;
/// 列表只读每个文件开头和结尾这么多（第一次打开要扫几百个文件）；预览读结尾多一些
const HEAD_BYTES: u64 = 64 * 1024;
const TAIL_BYTES: u64 = 64 * 1024;
const PREVIEW_BYTES: u64 = 256 * 1024;
/// 列表里的文字最多多少个字符
const TEXT_MAX: usize = 160;
/// 预览里每条消息最多多少个字符
const PREVIEW_MAX: usize = 600;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Conv {
    pub agent: &'static str,
    pub id: String,
    pub title: String,
    pub workdir: String,
    pub branch: Option<String>,
    /// 最后一句你说的话
    pub last: Option<String>,
    pub updated_at: String,
    #[serde(skip)]
    pub updated_ms: i64,
    /// 最近两分钟还在写，可能正在别处运行
    pub live: bool,
    /// 正在某个看看会话里（见 gateway 里的 `list_history`）
    pub instance_id: Option<String>,
    /// 对话开始的时间（记录里的第一条）
    pub started_at: Option<String>,
    /// 搜对话全文时命中的那一段
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snippet: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Message {
    pub role: &'static str,
    pub text: String,
}

#[derive(Debug, Default, Deserialize)]
pub struct Query {
    pub workdir: Option<String>,
    pub agent: Option<String>,
    pub q: Option<String>,
    pub limit: Option<usize>,
    /// 搜对话全文（`q` 不只匹配标题、你说的话和文件夹，还在整份记录里找）
    pub deep: Option<bool>,
    /// 只要这个时间（毫秒）之前有消息的：往后翻页
    pub before: Option<i64>,
}

/// 能接着的工具
pub const AGENTS: [&str; 3] = ["claude", "codex", "opencode"];

/// 会话号只允许字母、数字、`-`、`_`（要拼进命令行）。
pub fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 128 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

#[derive(Clone)]
struct Cached {
    mtime: SystemTime,
    size: u64,
    conv: Option<Conv>,
}

static CACHE: Mutex<Option<HashMap<PathBuf, Cached>>> = Mutex::new(None);

fn env_dir(k: &str) -> Option<PathBuf> {
    std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from)
}

fn home() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))
}

fn claude_dir() -> PathBuf {
    env_dir("CLAUDE_CONFIG_DIR").unwrap_or_else(|| home().join(".claude")).join("projects")
}

fn codex_dir() -> PathBuf {
    env_dir("CODEX_HOME").unwrap_or_else(|| home().join(".codex"))
}

fn opencode_dir() -> PathBuf {
    env_dir("XDG_DATA_HOME").unwrap_or_else(|| home().join(".local").join("share")).join("opencode")
}

/// 列出对话：最近有消息的排前面。
pub async fn list(q: Query) -> Vec<Conv> {
    tokio::task::spawn_blocking(move || {
        let mut out = list_blocking(&q);
        out.truncate(q.limit.unwrap_or(50).clamp(1, 500));
        out
    })
    .await
    .unwrap_or_default()
}

/// 一个文件夹里的对话：各工具多少个、最近的几个。
#[derive(Debug, Clone, Serialize)]
pub struct Folder {
    pub workdir: String,
    /// 工具 → 对话数
    pub counts: HashMap<&'static str, usize>,
    pub total: usize,
    pub updated_at: String,
    /// 最近的 `per` 个（从新到旧）
    pub items: Vec<Conv>,
}

/// 所有对话按文件夹分组（新到旧），`keep` 决定哪些对话算进来（比如去掉隐藏的），每组留最近 `per` 个。
pub async fn folders(per: usize, keep: impl Fn(&Conv) -> bool + Send + 'static) -> Vec<Folder> {
    tokio::task::spawn_blocking(move || group_folders(list_blocking(&Query::default()).into_iter().filter(keep), per)).await.unwrap_or_default()
}

/// 已经按时间从新到旧排好的对话按文件夹分组；组的顺序是各组最新那个的先后。
fn group_folders(convs: impl Iterator<Item = Conv>, per: usize) -> Vec<Folder> {
    let mut out: Vec<Folder> = vec![];
    let mut at: HashMap<String, usize> = HashMap::new();
    for c in convs {
        let key = dir_key(&c.workdir);
        if key.is_empty() {
            continue;
        }
        let i = *at.entry(key).or_insert_with(|| {
            out.push(Folder { workdir: c.workdir.clone(), counts: HashMap::new(), total: 0, updated_at: c.updated_at.clone(), items: vec![] });
            out.len() - 1
        });
        let f = &mut out[i];
        *f.counts.entry(c.agent).or_default() += 1;
        f.total += 1;
        if f.items.len() < per {
            f.items.push(c);
        }
    }
    out
}

/// 全部符合条件的对话，从新到旧（不截断）。
fn list_blocking(q: &Query) -> Vec<Conv> {
    let want = |a: &str| q.agent.as_deref().is_none_or(|w| w == a);
    let workdir = q.workdir.as_deref().map(str::trim).filter(|w| !w.is_empty());
    let mut files: Vec<(&'static str, PathBuf, std::fs::Metadata)> = vec![];
    if want("claude") {
        files.extend(claude_files(workdir).into_iter().map(|(p, m)| ("claude", p, m)));
    }
    if want("codex") {
        files.extend(codex_files().into_iter().map(|(p, m)| ("codex", p, m)));
    }
    let titles = if want("codex") { codex_titles() } else { HashMap::new() };
    let now = SystemTime::now();
    let mut out = vec![];
    let text = q.q.as_deref().map(|s| s.trim().to_lowercase()).filter(|s| !s.is_empty());
    let deep = text.as_deref().filter(|_| q.deep == Some(true));
    {
        let mut guard = CACHE.lock().unwrap_or_else(|e| e.into_inner());
        let cache = guard.get_or_insert_with(HashMap::new);
        for (agent, path, meta) in files {
            let mtime = meta.modified().unwrap_or(UNIX_EPOCH);
            let size = meta.len();
            let hit = cache.get(&path).filter(|c| c.mtime == mtime && c.size == size).cloned();
            let mut conv = match hit {
                Some(c) => c.conv,
                None => {
                    let conv = match agent {
                        "claude" => parse_claude(&path),
                        _ => parse_codex(&path),
                    };
                    cache.insert(path.clone(), Cached { mtime, size, conv: conv.clone() });
                    conv
                }
            };
            if let Some(c) = conv.as_mut() {
                c.live = now.duration_since(mtime).map_or(true, |d| d < Duration::from_secs(LIVE_SECS));
                if let Some(t) = titles.get(&c.id) {
                    c.title = t.clone();
                }
                let mut c = c.clone();
                if let Some(w) = deep {
                    match find_in_file(&path, w) {
                        Some(snip) => c.snippet = Some(snip),
                        None if !matches_text(&c, w) => continue,
                        None => {}
                    }
                }
                out.push(c);
            }
        }
        // 文件没了的记录不再留着
        if cache.len() > 4096 {
            cache.retain(|p, _| p.exists());
        }
    }
    if want("opencode") {
        out.extend(opencode_list());
    }
    if let Some(w) = workdir {
        out.retain(|c| same_dir(&c.workdir, w));
    }
    if let Some(text) = &text {
        out.retain(|c| c.snippet.is_some() || matches_text(c, text));
    }
    if let Some(b) = q.before {
        out.retain(|c| c.updated_ms < b);
    }
    out.sort_by_key(|c| std::cmp::Reverse(c.updated_ms));
    out
}

/// 标题、你最后说的话、文件夹、分支里有没有这段文字（已转成小写）。
fn matches_text(c: &Conv, text: &str) -> bool {
    [Some(c.title.as_str()), c.last.as_deref(), Some(c.workdir.as_str()), c.branch.as_deref()].into_iter().flatten().any(|s| s.to_lowercase().contains(text))
}

/// 搜对话全文：整份记录（最多读 32 MB）里找这段文字，返回命中处前后的一段（去掉 JSON 的转义）。
fn find_in_file(path: &Path, text: &str) -> Option<String> {
    const MAX: u64 = 32 << 20;
    let meta = std::fs::metadata(path).ok()?;
    if meta.len() > MAX {
        return None;
    }
    let raw = std::fs::read_to_string(path).ok()?;
    let lower = raw.to_lowercase();
    // 转成小写后长度可能变（少数字符），按字符位置对回原文
    let at = lower.find(text)?;
    let start_c = lower[..at].chars().count();
    let len_c = text.chars().count();
    let chars: Vec<char> = raw.chars().collect();
    let from = start_c.saturating_sub(50);
    let to = (start_c + len_c + 70).min(chars.len());
    let mut s: String = chars[from.min(chars.len())..to].iter().collect();
    s = s.replace("\\n", " ").replace("\\\"", "\"").replace("\\t", " ");
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    Some(format!("{}{}{}", if from > 0 { "…" } else { "" }, s, if to < chars.len() { "…" } else { "" }))
}

/// 两个目录是不是同一个：去掉末尾的分隔符；Windows 上不分大小写、`/` 和 `\` 一样。
pub fn same_dir(a: &str, b: &str) -> bool {
    let (a, b) = (dir_key(a), dir_key(b));
    !a.is_empty() && a == b
}

/// 比较目录用的形式（见 [`same_dir`]）。
pub fn dir_key(s: &str) -> String {
    let s = if cfg!(windows) { s.replace('/', "\\").to_lowercase() } else { s.to_string() };
    s.trim_end_matches(['/', '\\']).to_string()
}

/// Claude Code 存对话的文件夹名：工作目录里不是字母数字的都换成 `-`。
fn claude_slug(workdir: &str) -> String {
    workdir.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect()
}

fn jsonl_in(dir: &Path) -> Vec<(PathBuf, std::fs::Metadata)> {
    let Ok(rd) = std::fs::read_dir(dir) else { return vec![] };
    rd.flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "jsonl"))
        .filter_map(|e| Some((e.path(), e.metadata().ok().filter(|m| m.is_file())?)))
        .collect()
}

/// 指定了工作目录时只看对应的文件夹（名字太长被 Claude Code 截断后加了后缀的，按前 200 个字符找）。
fn claude_files(workdir: Option<&str>) -> Vec<(PathBuf, std::fs::Metadata)> {
    let root = claude_dir();
    let Ok(rd) = std::fs::read_dir(&root) else { return vec![] };
    let slug = workdir.map(claude_slug);
    rd.flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter(|e| {
            let Some(slug) = &slug else { return true };
            let name = e.file_name().to_string_lossy().to_lowercase();
            let slug = slug.to_lowercase();
            name == slug || (name.len() > 200 && name.get(..200).is_some_and(|n| slug.get(..200) == Some(n)))
        })
        .flat_map(|e| jsonl_in(&e.path()))
        .collect()
}

/// Codex：年 / 月 / 日 三层目录。
fn codex_files() -> Vec<(PathBuf, std::fs::Metadata)> {
    let mut out = vec![];
    let mut stack = vec![(codex_dir().join("sessions"), 0)];
    while let Some((dir, depth)) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if depth < 3 && e.file_type().is_ok_and(|t| t.is_dir()) {
                stack.push((p, depth + 1));
            } else if p.extension().is_some_and(|x| x == "jsonl") && p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("rollout-")) {
                if let Ok(m) = e.metadata() {
                    out.push((p, m));
                }
            }
        }
    }
    out
}

/// Codex 对话 `thread` 的记录文件
pub fn codex_rollout(thread: &str) -> Option<PathBuf> {
    if thread.is_empty() {
        return None;
    }
    let tail = format!("-{thread}.jsonl");
    codex_files().into_iter().map(|(p, _)| p).find(|p| p.file_name().is_some_and(|n| n.to_string_lossy().ends_with(&tail)))
}

fn codex_titles() -> HashMap<String, String> {
    let mut m = HashMap::new();
    let Ok(text) = std::fs::read_to_string(codex_dir().join("session_index.jsonl")) else { return m };
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        if let (Some(id), Some(name)) = (v["id"].as_str(), v["thread_name"].as_str()) {
            let name = clip(name, TEXT_MAX);
            if !name.is_empty() {
                m.insert(id.to_string(), name);
            }
        }
    }
    m
}

// ---------------- 读文件的开头和结尾 ----------------

/// 只解析含有这些片段之一的行：助手回答、请求记录这些大行用不上，跳过能省下大部分时间。
type Want<'a> = &'a [&'a str];

fn wanted(line: &[u8], want: Want) -> bool {
    want.iter().any(|w| memchr::memmem::find(line, w.as_bytes()).is_some())
}

/// 文件开头若干字节里的完整行。
fn head_lines(path: &Path, max: u64, want: Want) -> Vec<Value> {
    let Ok(mut f) = File::open(path) else { return vec![] };
    let mut buf = vec![];
    let _ = (&mut f).take(max).read_to_end(&mut buf);
    let complete = buf.len() as u64 == max;
    let mut lines: Vec<&[u8]> = buf.split(|b| *b == b'\n').collect();
    if complete {
        lines.pop();
    }
    lines.into_iter().filter(|l| wanted(l, want)).filter_map(|l| serde_json::from_slice(l).ok()).collect()
}

/// 文件结尾若干字节里的完整行（丢掉第一段不完整的）。
fn tail_lines(path: &Path, max: u64, want: Want) -> Vec<Value> {
    let Ok(mut f) = File::open(path) else { return vec![] };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let start = len.saturating_sub(max);
    if f.seek(SeekFrom::Start(start)).is_err() {
        return vec![];
    }
    let mut buf = vec![];
    let _ = f.read_to_end(&mut buf);
    let mut lines: Vec<&[u8]> = buf.split(|b| *b == b'\n').collect();
    if start > 0 && !lines.is_empty() {
        lines.remove(0);
    }
    lines.into_iter().filter(|l| wanted(l, want)).filter_map(|l| serde_json::from_slice(l).ok()).collect()
}

/// 一行文字：合并空白，太长截断。
fn clip(s: &str, max: usize) -> String {
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.chars().count() <= max {
        return s;
    }
    let mut t: String = s.chars().take(max).collect();
    t.push('…');
    t
}

/// 保留换行的截断（预览用）。
fn clip_block(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut t: String = s.chars().take(max).collect();
    t.push('…');
    t
}

/// 工具自己塞进来的“用户消息”（命令、提醒、环境说明），不算你说的话。
fn is_injected(text: &str) -> bool {
    let t = text.trim_start();
    t.is_empty() || t.starts_with('<') || t.starts_with("# AGENTS.md instructions") || t.starts_with("Caveat:")
}

/// Claude Code 列表要用的行：你说的话（带 cwd、分支）和标题
const CLAUDE_LIST: Want = &["\"type\":\"user\"", "-title\"", "\"type\":\"summary\"", "\"type\":\"last-prompt\""];
const CLAUDE_MSGS: Want = &["\"type\":\"user\"", "\"type\":\"assistant\""];
const CODEX_LIST: Want = &["\"type\":\"session_meta\"", "\"role\":\"user\""];
const CODEX_MSGS: Want = &["\"role\":\"user\"", "\"role\":\"assistant\""];

// ---------------- Claude Code ----------------

/// 一条记录里的消息：你说的话或 Claude 的回答（只要文字，工具调用、工具结果不算）。
fn claude_msg(v: &Value) -> Option<(&'static str, String)> {
    let role = match v["type"].as_str()? {
        "user" => "user",
        "assistant" => "assistant",
        _ => return None,
    };
    if v["isMeta"].as_bool() == Some(true) || v["isSidechain"].as_bool() == Some(true) || v["isCompactSummary"].as_bool() == Some(true) {
        return None;
    }
    let content = &v["message"]["content"];
    let text = match content {
        Value::String(s) => s.clone(),
        Value::Array(items) => items.iter().filter(|i| i["type"] == "text").filter_map(|i| i["text"].as_str()).collect::<Vec<_>>().join("\n"),
        _ => return None,
    };
    if text.trim().is_empty() || (role == "user" && is_injected(&text)) {
        return None;
    }
    Some((role, text))
}

fn parse_claude(path: &Path) -> Option<Conv> {
    let id = path.file_stem()?.to_str()?.to_string();
    if !valid_id(&id) {
        return None;
    }
    let head = head_lines(path, HEAD_BYTES, CLAUDE_LIST);
    let tail = tail_lines(path, TAIL_BYTES, CLAUDE_LIST);
    // 程序调用（`claude -p`、SDK）的对话不是你聊的，不列
    if head.iter().chain(tail.iter()).find_map(|v| v["entrypoint"].as_str()).is_some_and(|e| e.starts_with("sdk")) {
        return None;
    }
    let all = || head.iter().chain(tail.iter());
    let workdir = all().find_map(|v| v["cwd"].as_str())?.to_string();
    let first = head.iter().filter_map(claude_msg).find(|(r, _)| *r == "user").map(|(_, t)| t);
    let last = tail.iter().rev().filter_map(claude_msg).find(|(r, _)| *r == "user").map(|(_, t)| t);
    let last = last.or_else(|| tail.iter().rev().find_map(|v| (v["type"] == "last-prompt").then(|| v["lastPrompt"].as_str()).flatten().map(String::from)));
    let title_of = |kind: &str, key: &str| tail.iter().rev().chain(head.iter().rev()).find_map(|v| (v["type"] == kind).then(|| v[key].as_str()).flatten().map(String::from));
    let title = title_of("custom-title", "customTitle").or_else(|| title_of("ai-title", "aiTitle")).or_else(|| title_of("summary", "summary")).or_else(|| first.clone()).or_else(|| last.clone())?;
    let branch = tail.iter().rev().chain(head.iter()).find_map(|v| v["gitBranch"].as_str()).filter(|b| !b.is_empty() && *b != "HEAD").map(String::from);
    let updated_ms = mtime_ms(path)?;
    Some(Conv {
        agent: "claude",
        id,
        title: clip(&title, TEXT_MAX),
        workdir,
        branch,
        last: last.map(|t| clip(&t, TEXT_MAX)),
        updated_at: crate::util::fmt_ms(updated_ms),
        updated_ms,
        live: false,
        instance_id: None,
        started_at: head.iter().find_map(|v| v["timestamp"].as_str()).map(String::from),
        snippet: None,
    })
}

// ---------------- Codex ----------------

fn codex_msg(v: &Value) -> Option<(&'static str, String)> {
    let p = &v["payload"];
    match (v["type"].as_str()?, p["type"].as_str()?) {
        ("response_item", "message") => {
            let role = match p["role"].as_str()? {
                "user" => "user",
                "assistant" => "assistant",
                _ => return None,
            };
            let text = p["content"].as_array()?.iter().filter(|c| matches!(c["type"].as_str(), Some("input_text" | "output_text"))).filter_map(|c| c["text"].as_str()).collect::<Vec<_>>().join("\n");
            (!text.trim().is_empty() && !(role == "user" && is_injected(&text))).then_some((role, text))
        }
        _ => None,
    }
}

fn parse_codex(path: &Path) -> Option<Conv> {
    // 第一行 session_meta 带着整份系统提示，可能有几十 KB；不完整时多读一些
    let mut head = head_lines(path, HEAD_BYTES * 2, CODEX_LIST);
    if head.first().is_none_or(|v| v["type"] != "session_meta") {
        head = head_lines(path, HEAD_BYTES * 8, CODEX_LIST);
    }
    let meta = head.first().filter(|v| v["type"] == "session_meta")?;
    let p = &meta["payload"];
    // 子代理（source 是对象）、`codex exec`、SDK 调用的会话不列
    let source = p["source"].as_str()?;
    if source == "exec" || p["thread_source"].as_str().is_some_and(|s| s != "user") {
        return None;
    }
    let id = p["id"].as_str().or_else(|| p["session_id"].as_str())?.to_string();
    if !valid_id(&id) {
        return None;
    }
    let workdir = p["cwd"].as_str()?.to_string();
    let tail = tail_lines(path, TAIL_BYTES, CODEX_LIST);
    let first = head.iter().filter_map(codex_msg).find(|(r, _)| *r == "user").map(|(_, t)| t);
    let last = tail.iter().rev().filter_map(codex_msg).find(|(r, _)| *r == "user").map(|(_, t)| t).or_else(|| first.clone());
    let title = first.clone().or_else(|| last.clone())?;
    let branch = p["git"]["branch"].as_str().filter(|b| !b.is_empty()).map(String::from);
    let updated_ms = mtime_ms(path)?;
    Some(Conv {
        agent: "codex",
        id,
        title: clip(&title, TEXT_MAX),
        workdir,
        branch,
        last: last.map(|t| clip(&t, TEXT_MAX)),
        updated_at: crate::util::fmt_ms(updated_ms),
        updated_ms,
        live: false,
        instance_id: None,
        started_at: p["timestamp"].as_str().or_else(|| meta["timestamp"].as_str()).map(String::from),
        snippet: None,
    })
}

fn mtime_ms(path: &Path) -> Option<i64> {
    let t = std::fs::metadata(path).ok()?.modified().ok()?;
    Some(t.duration_since(UNIX_EPOCH).ok()?.as_millis() as i64)
}

// ---------------- OpenCode ----------------

fn opencode_conv(id: &str, title: &str, dir: &str, updated_ms: i64) -> Option<Conv> {
    if !valid_id(id) || dir.is_empty() {
        return None;
    }
    let now = crate::util::system_ms();
    Some(Conv {
        agent: "opencode",
        id: id.to_string(),
        title: clip(if title.trim().is_empty() { id } else { title }, TEXT_MAX),
        workdir: dir.to_string(),
        branch: None,
        last: None,
        updated_at: crate::util::fmt_ms(updated_ms),
        updated_ms,
        live: now - updated_ms < (LIVE_SECS * 1000) as i64,
        instance_id: None,
        started_at: None,
        snippet: None,
    })
}

fn opencode_list() -> Vec<Conv> {
    let dir = opencode_dir();
    let db = dir.join("opencode.db");
    if db.is_file() {
        if let Some(v) = opencode_db(&db) {
            return v;
        }
    }
    // 旧版：storage/session/<项目>/<会话>.json
    let mut out = vec![];
    let Ok(projects) = std::fs::read_dir(dir.join("storage").join("session")) else { return out };
    for p in projects.flatten() {
        let Ok(files) = std::fs::read_dir(p.path()) else { continue };
        for f in files.flatten() {
            let path = f.path();
            if path.extension().is_none_or(|x| x != "json") {
                continue;
            }
            let Some(v) = std::fs::read(&path).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()) else { continue };
            // 子会话（子代理）不列
            if v["parentID"].is_string() {
                continue;
            }
            let updated = v["time"]["updated"].as_i64().or_else(|| v["time"]["created"].as_i64()).unwrap_or(0);
            if let Some(c) = opencode_conv(v["id"].as_str().unwrap_or(""), v["title"].as_str().unwrap_or(""), v["directory"].as_str().unwrap_or(""), updated) {
                out.push(c);
            }
        }
    }
    out
}

/// 新版的 SQLite：表结构可能会变，按列名找，找不到就当没有。
fn opencode_db(db: &Path) -> Option<Vec<Conv>> {
    use rusqlite::{Connection, OpenFlags};
    let conn = Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX).ok()?;
    let _ = conn.busy_timeout(Duration::from_millis(500));
    let cols: Vec<String> = conn.prepare("PRAGMA table_info(session)").ok()?.query_map([], |r| r.get::<_, String>(1)).ok()?.flatten().collect();
    let pick = |names: &[&str]| names.iter().find(|n| cols.iter().any(|c| c == *n)).map(|s| s.to_string());
    let dir = pick(&["directory"])?;
    let updated = pick(&["time_updated", "updated_at", "time_created"])?;
    let parent = pick(&["parent_id", "parentID"]);
    let filter = parent.map(|p| format!(" WHERE {p} IS NULL")).unwrap_or_default();
    let sql = format!("SELECT id, COALESCE(title, ''), {dir}, {updated} FROM session{filter} ORDER BY {updated} DESC LIMIT 500");
    let mut stmt = conn.prepare(&sql).ok()?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, i64>(3)?))).ok()?;
    Some(rows.flatten().filter_map(|(id, title, dir, t)| opencode_conv(&id, &title, &dir, t)).collect())
}

// ---------------- 预览 ----------------

/// 对话最后几条消息（展开预览用）。找不到返回 None。
/// 历史页右边的完整预览：读最后多少字节、最多几条、每条最多多少个字符
const FULL_BYTES: u64 = 4 << 20;
const FULL_COUNT: usize = 200;
const FULL_MAX: usize = 4000;

/// `full`：历史页右边看完整的对话（最近 200 条）；否则是展开一行时看的最后几句。
pub async fn preview(agent: String, id: String, full: bool) -> Option<Vec<Message>> {
    tokio::task::spawn_blocking(move || preview_blocking(&agent, &id, full)).await.ok().flatten()
}

fn preview_blocking(agent: &str, id: &str, full: bool) -> Option<Vec<Message>> {
    if !valid_id(id) {
        return None;
    }
    type Parse = fn(&Value) -> Option<(&'static str, String)>;
    let (path, parse, want): (PathBuf, Parse, Want) = match agent {
        "claude" => {
            let name = format!("{id}.jsonl");
            let root = claude_dir();
            let p = std::fs::read_dir(&root).ok()?.flatten().map(|e| e.path().join(&name)).find(|p| p.is_file())?;
            (p, claude_msg, CLAUDE_MSGS)
        }
        "codex" => {
            let p = codex_files().into_iter().map(|(p, _)| p).find(|p| p.file_stem().is_some_and(|s| s.to_string_lossy().ends_with(id)))?;
            (p, codex_msg, CODEX_MSGS)
        }
        "opencode" => return Some(vec![]),
        _ => return None,
    };
    let (bytes, count, max) = if full { (FULL_BYTES, FULL_COUNT, FULL_MAX) } else { (PREVIEW_BYTES, 6, PREVIEW_MAX) };
    let msgs: Vec<Message> = tail_lines(&path, bytes, want).iter().filter_map(parse).map(|(role, t)| Message { role, text: clip_block(&t, max) }).collect();
    let skip = msgs.len().saturating_sub(count);
    Some(msgs.into_iter().skip(skip).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, lines: &[Value]) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, lines.iter().map(|v| v.to_string() + "\n").collect::<String>()).unwrap();
        p
    }

    #[test]
    fn folders_grouped_newest_first() {
        let conv = |agent: &'static str, id: &str, workdir: &str, ms: i64| Conv {
            agent,
            id: id.into(),
            title: id.into(),
            workdir: workdir.into(),
            branch: None,
            last: None,
            updated_at: ms.to_string(),
            updated_ms: ms,
            live: false,
            instance_id: None,
            started_at: None,
            snippet: None,
        };
        let all = vec![conv("claude", "a", "/w/app", 5), conv("codex", "b", "/w/lib/", 4), conv("codex", "c", "/w/app/", 3), conv("claude", "d", "/w/app", 2), conv("claude", "e", "", 1)];
        let f = group_folders(all.into_iter(), 2);
        assert_eq!(f.len(), 2, "没有目录的不算");
        assert_eq!((f[0].workdir.as_str(), f[0].total, f[0].updated_at.as_str()), ("/w/app", 3, "5"));
        assert_eq!((f[0].counts["claude"], f[0].counts["codex"]), (2, 1));
        assert_eq!(f[0].items.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(), ["a", "c"], "末尾斜杠不同也是同一个文件夹，只留最近两个");
        assert_eq!(f[1].workdir, "/w/lib/");
    }

    #[test]
    fn claude_title_workdir_and_last() {
        let dir = tempfile::tempdir().unwrap();
        let p = write(
            dir.path(),
            "96e9b84c-16d9-4bc7-ab76-f92861e5f141.jsonl",
            &[
                serde_json::json!({"type":"user","cwd":"/w/app","gitBranch":"main","entrypoint":"cli","timestamp":"2026-10-19T01:00:00Z","message":{"role":"user","content":"<command-name>/clear</command-name>"}}),
                serde_json::json!({"type":"user","cwd":"/w/app","gitBranch":"main","timestamp":"2026-10-19T01:00:01Z","message":{"role":"user","content":"修一下  登录\n页面"}}),
                serde_json::json!({"type":"assistant","cwd":"/w/app","timestamp":"2026-10-19T01:00:02Z","message":{"role":"assistant","content":[{"type":"text","text":"好的"}]}}),
                serde_json::json!({"type":"ai-title","aiTitle":"修登录页"}),
                serde_json::json!({"type":"user","cwd":"/w/app","gitBranch":"fix/login","timestamp":"2026-10-19T02:00:00Z","message":{"role":"user","content":[{"type":"tool_result","content":"x"}]}}),
                serde_json::json!({"type":"user","cwd":"/w/app","gitBranch":"fix/login","timestamp":"2026-10-19T02:00:01Z","message":{"role":"user","content":[{"type":"text","text":"再跑一次测试"}]}}),
            ],
        );
        let c = parse_claude(&p).unwrap();
        assert_eq!(c.id, "96e9b84c-16d9-4bc7-ab76-f92861e5f141");
        assert_eq!(c.title, "修登录页");
        assert_eq!(c.workdir, "/w/app");
        assert_eq!(c.branch.as_deref(), Some("fix/login"));
        assert_eq!(c.last.as_deref(), Some("再跑一次测试"));
        let msgs = tail_lines(&p, TAIL_BYTES, CLAUDE_MSGS).iter().filter_map(claude_msg).map(|(r, _)| r).collect::<Vec<_>>();
        assert_eq!(msgs, ["user", "assistant", "user"]);
    }

    #[test]
    fn claude_sdk_and_empty_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let sdk = write(dir.path(), "a.jsonl", &[serde_json::json!({"type":"user","cwd":"/w","entrypoint":"sdk-cli","message":{"content":"hi"}})]);
        assert!(parse_claude(&sdk).is_none());
        let empty = write(dir.path(), "b.jsonl", &[serde_json::json!({"type":"mode","mode":"normal"})]);
        assert!(parse_claude(&empty).is_none());
    }

    #[test]
    fn codex_meta_and_filters() {
        let dir = tempfile::tempdir().unwrap();
        let meta = |source: Value, ts: &str| serde_json::json!({"timestamp":"2026-10-08T03:21:02Z","type":"session_meta","payload":{"id":"01a11987-078c-7c91-9ef9-c10914660ea2","cwd":"/w/x","source":source,"thread_source":ts,"git":{"branch":"master"}}});
        let msg = |role: &str, kind: &str, text: &str, t: &str| serde_json::json!({"timestamp":t,"type":"response_item","payload":{"type":"message","role":role,"content":[{"type":kind,"text":text}]}});
        let p = write(
            dir.path(),
            "rollout-2026-10-07T23-20-43-01a11987-078c-7c91-9ef9-c10914660ea2.jsonl",
            &[
                meta(Value::from("cli"), "user"),
                msg("user", "input_text", "<environment_context>…</environment_context>", "2026-10-08T03:21:03Z"),
                msg("user", "input_text", "连接 afs-analyzer 仓库", "2026-10-08T03:21:04Z"),
                msg("assistant", "output_text", "已连接", "2026-10-08T03:21:05Z"),
                msg("user", "input_text", "不需要推送", "2026-10-08T03:22:00Z"),
            ],
        );
        let c = parse_codex(&p).unwrap();
        assert_eq!((c.id.as_str(), c.workdir.as_str(), c.title.as_str()), ("01a11987-078c-7c91-9ef9-c10914660ea2", "/w/x", "连接 afs-analyzer 仓库"));
        assert_eq!(c.last.as_deref(), Some("不需要推送"));
        assert_eq!(c.branch.as_deref(), Some("master"));
        let sub = write(dir.path(), "rollout-sub.jsonl", &[meta(serde_json::json!({"subagent":{}}), "subagent"), msg("user", "input_text", "hi", "2026-10-08T03:21:04Z")]);
        assert!(parse_codex(&sub).is_none());
        let exec = write(dir.path(), "rollout-exec.jsonl", &[meta(Value::from("exec"), "user"), msg("user", "input_text", "hi", "2026-10-08T03:21:04Z")]);
        assert!(parse_codex(&exec).is_none());
    }

    #[test]
    fn dirs_and_ids() {
        assert!(same_dir("/a/b/", "/a/b"));
        assert!(!same_dir("/a/b", "/a/bc"));
        assert!(!same_dir("", ""));
        assert_eq!(claude_slug("/workspace/ttyd/looklook-server"), "-workspace-ttyd-looklook-server");
        assert_eq!(claude_slug("/tmp/a.b_c"), "-tmp-a-b-c");
        assert!(valid_id("ses_01ABcd-9"));
        assert!(!valid_id("a b") && !valid_id("a;rm") && !valid_id(""));
    }

    #[test]
    fn tail_drops_partial_first_line() {
        let dir = tempfile::tempdir().unwrap();
        let p = write(dir.path(), "t.jsonl", &[serde_json::json!({"a": "x".repeat(100)}), serde_json::json!({"b": 1})]);
        let v = tail_lines(&p, 20, &["\""]);
        assert_eq!(v, vec![serde_json::json!({"b": 1})]);
    }
}
