//! 终端里正在跑的 AI 编程助手 ↔ 它的会话记录，给会话页的“对话”看法用：Codex（rollout 文件）、
//! Claude Code（`projects/<项目>/<id>.jsonl`）、DSH 终端版（会话日志）。
//!
//! - 接着某个会话聊的终端（`codex resume` / `claude --resume` / `dsh tui --resume`）：直接按 id 找文件。
//! - 新开的终端：找工作目录相同、在终端启动之后才开始的会话；有好几个时取最近还在写的（在里面开了新会话
//!   之后会换成新的）。找到后由调用方记下来，下次不用再找。
//!
//! 读文件有缓存（按文件大小和修改时间），对话看法每两秒问一次也不会反复解析几 MB 的文件。

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;

use anyhow::Result;
use serde_json::{json, Value};

use super::{claude, codex, dsh, Conversation, Harness, Item};
use crate::explain;

/// 发给网页的最多条数（从最后往前数）。
const ENTRIES_MAX: usize = 400;
/// 单条文字最多字符数（长回复照样完整，工具输出不发）。
const TEXT_MAX: usize = 20_000;

type Cached = (u64, SystemTime, Arc<Conversation>);

fn cache() -> &'static Mutex<HashMap<PathBuf, Cached>> {
    static C: OnceLock<Mutex<HashMap<PathBuf, Cached>>> = OnceLock::new();
    C.get_or_init(Default::default)
}

/// 读会话记录；文件没变时用上次的结果。
pub fn conversation(kind: Harness, home: &Path, path: &Path) -> Result<Arc<Conversation>> {
    let meta = std::fs::metadata(path)?;
    let (len, modified) = (meta.len(), meta.modified().unwrap_or(SystemTime::UNIX_EPOCH));
    if let Some((l, m, c)) = cache().lock().unwrap().get(path) {
        if *l == len && *m == modified {
            return Ok(c.clone());
        }
    }
    let c = Arc::new(match kind {
        Harness::Claude => claude::read_file(path)?,
        Harness::Dsh => dsh::read_log(path)?,
        _ => codex::read_file(home, path)?,
    });
    let mut map = cache().lock().unwrap();
    // 只留最近用到的一些
    if map.len() > 32 {
        map.clear();
    }
    map.insert(path.to_path_buf(), (len, modified, c.clone()));
    Ok(c)
}

fn same_dir(a: &str, b: &str) -> bool {
    let n = |s: &str| {
        let s = s.trim_end_matches(['/', '\\']).replace('\\', "/");
        if cfg!(windows) { s.to_lowercase() } else { s }
    };
    n(a) == n(b)
}

/// `sessions/YYYY/MM/DD` 这一层目录：只看终端启动那天（前后各放一天）以后的。
fn recent_rollouts(root: &Path, since_day: (i32, u32, u32)) -> Vec<PathBuf> {
    let num = |p: &Path| p.file_name().and_then(|n| n.to_str()).and_then(|n| n.parse::<u32>().ok());
    let dirs = |p: &Path| -> Vec<PathBuf> { std::fs::read_dir(p).map(|rd| rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect()).unwrap_or_default() };
    let mut out = Vec::new();
    for y in dirs(root) {
        let Some(yy) = num(&y) else { continue };
        if (yy as i32) < since_day.0 {
            continue;
        }
        for m in dirs(&y) {
            let Some(mm) = num(&m) else { continue };
            if (yy as i32, mm) < (since_day.0, since_day.1) {
                continue;
            }
            for d in dirs(&m) {
                let Some(dd) = num(&d) else { continue };
                if (yy as i32, mm, dd) < since_day {
                    continue;
                }
                codex::rollouts(&d, &mut out);
            }
        }
    }
    out
}

/// 接着聊的会话 id → 会话文件。
pub fn find(kind: Harness, home: &Path, id: &str) -> Option<PathBuf> {
    match kind {
        Harness::Claude => claude::find(home, id).ok(),
        Harness::Dsh => dsh::find_log(home, id).ok(),
        _ => codex::find(home, id).ok(),
    }
}

/// 新开的终端对应的会话文件：工作目录相同、在 `started_ms` 之后开始的会话里最近还在写的一个。
pub fn discover(kind: Harness, home: &Path, cwd: &str, started_ms: i64) -> Option<PathBuf> {
    match kind {
        Harness::Claude => claude::discover(home, cwd, started_ms),
        Harness::Dsh => dsh::discover(home, cwd, started_ms),
        _ => discover_codex(home, cwd, started_ms),
    }
}

fn discover_codex(home: &Path, cwd: &str, started_ms: i64) -> Option<PathBuf> {
    let day = time::OffsetDateTime::from_unix_timestamp(started_ms / 1000).ok()? - time::Duration::days(1);
    let since = (day.year(), u32::from(u8::from(day.month())), u32::from(day.day()));
    let mut best: Option<(SystemTime, PathBuf)> = None;
    for p in recent_rollouts(&home.join("sessions"), since) {
        let Ok(meta) = std::fs::metadata(&p) else { continue };
        let modified = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        if best.as_ref().is_some_and(|(m, _)| *m >= modified) {
            continue;
        }
        let Ok(f) = std::fs::File::open(&p) else { continue };
        let mut first = String::new();
        if BufReader::new(f).read_line(&mut first).is_err() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(&first) else { continue };
        let m = &v["payload"];
        if v["type"] != "session_meta" || codex::is_subagent(m) {
            continue;
        }
        let begun = super::parse_ms(m["timestamp"].as_str(), 0);
        // 终端启动时间只精确到秒，Codex 也要先启动一会儿：放宽 10 秒
        if begun + 10_000 < started_ms || !m["cwd"].as_str().is_some_and(|c| same_dir(c, cwd)) {
            continue;
        }
        best = Some((modified, p));
    }
    best.map(|(_, p)| p)
}

fn clip(s: &str) -> String {
    match s.char_indices().nth(TEXT_MAX) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_string(),
    }
}

/// 对话看法的条目：用户的话、AI 的回复、AI 做的每一步（工具调用拆成一步步，带说明和风险）。
/// 最多最后 [`ENTRIES_MAX`] 条；返回（条目，总条数）。
pub fn entries(c: &Conversation) -> (Vec<Value>, usize) {
    let mut out: Vec<Value> = Vec::new();
    let mut steps_of: HashMap<String, Vec<usize>> = HashMap::new();
    for it in &c.items {
        match it {
            Item::User { text, time_ms } => out.push(json!({"kind": "user", "text": clip(text), "time_ms": time_ms})),
            Item::Assistant { text, time_ms } => out.push(json!({"kind": "assistant", "text": clip(text), "time_ms": time_ms})),
            Item::ToolCall { id, name, arguments, time_ms } => {
                for s in explain::tool_call(name, arguments) {
                    steps_of.entry(id.clone()).or_default().push(out.len());
                    let mut v = serde_json::to_value(&s).unwrap_or(Value::Null);
                    v["kind"] = json!("step");
                    v["call_id"] = json!(id);
                    v["status"] = json!("running");
                    v["time_ms"] = json!(time_ms);
                    out.push(v);
                }
            }
            Item::ToolResult { call_id, output, is_error, .. } => {
                // 你点了“不同意”或按了 Esc：Codex 记成 “aborted by user after …”
                let o = output.trim_start().to_ascii_lowercase();
                let status = if o.starts_with("aborted by user") || o.starts_with("rejected by user") || o.contains("user rejected") {
                    "stopped"
                } else if *is_error {
                    "error"
                } else {
                    "done"
                };
                for &i in steps_of.get(call_id).map(Vec::as_slice).unwrap_or_default() {
                    out[i]["status"] = json!(status);
                }
            }
        }
    }
    let total = out.len();
    let skip = total.saturating_sub(ENTRIES_MAX);
    (out.into_iter().skip(skip).collect(), total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::convo::{Harness, Manifest};

    #[test]
    fn steps_follow_results() {
        let c = Conversation {
            harness: Harness::Codex,
            id: "x".into(),
            title: None,
            cwd: None,
            created_ms: 0,
            model: None,
            items: vec![
                Item::User { text: "列一下文件".into(), time_ms: 1 },
                Item::ToolCall { id: "a".into(), name: "exec".into(), arguments: "await tools.exec_command({cmd:\"ls\"})".into(), time_ms: 2 },
                Item::ToolResult { call_id: "a".into(), output: "x".into(), is_error: false, time_ms: 3 },
                Item::ToolCall { id: "b".into(), name: "exec".into(), arguments: "await tools.exec_command({cmd:\"rm x\"})".into(), time_ms: 4 },
                Item::Assistant { text: "好了".into(), time_ms: 5 },
            ],
            manifest: Manifest::default(),
        };
        let (e, total) = entries(&c);
        assert_eq!(total, 4);
        assert_eq!(e[1]["action"], "read");
        assert_eq!(e[1]["status"], "done");
        assert_eq!(e[2]["action"], "delete");
        assert_eq!(e[2]["risk"], "high");
        assert_eq!(e[2]["status"], "running");
        assert_eq!(e[3]["kind"], "assistant");
    }
}
