//! Claude Code 的输入历史（配置目录下的 `history.jsonl`，终端里按上键翻的就是它）：对话界面的上下键、Ctrl+R 用同一份。
//!
//! 每行一条（2.1.310）：`{"display", "pastedContents", "timestamp", "project", "sessionId"}`。
//! 粘贴的长文字在 `display` 里是 `[Pasted text #1 +5 lines]`，内容在 `pastedContents` 里（`content`），
//! 或者只给 `contentHash`、内容在配置目录的 `paste-cache/<hash>.txt`（被清理掉了就照原样留着占位）。
//! 每个账号的配置目录各有一份（claude_profiles.rs），都读、按时间合起来。

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;

/// 每个文件最多读最后这么多（够几千条）
const TAIL: u64 = 4 << 20;
/// 粘贴内容最多放进来这么多字节
const PASTE_MAX: usize = 64 << 10;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Prompt {
    pub text: String,
    /// 毫秒
    pub at: i64,
    pub session: Option<String>,
}

/// `dirs` 里各个配置目录的 `history.jsonl` 中，项目是 `project` 的那些，最新的在前，同样的话只留最新一条，最多 `limit` 条。
pub fn claude_prompts(dirs: &[PathBuf], project: &Path, limit: usize) -> Vec<Prompt> {
    let want = norm(&project.to_string_lossy());
    let mut all: Vec<Prompt> = vec![];
    for dir in dirs {
        let Some(text) = read_tail(&dir.join("history.jsonl")) else { continue };
        for line in text.lines() {
            let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
            if v["project"].as_str().map(norm).as_deref() != Some(want.as_str()) {
                continue;
            }
            let Some(display) = v["display"].as_str() else { continue };
            let text = expand(display, &v["pastedContents"], dir);
            if text.trim().is_empty() {
                continue;
            }
            all.push(Prompt { text, at: v["timestamp"].as_i64().unwrap_or(0), session: v["sessionId"].as_str().map(String::from) });
        }
    }
    all.sort_by(|a, b| b.at.cmp(&a.at));
    let mut seen = std::collections::HashSet::new();
    all.retain(|p| seen.insert(p.text.trim().to_string()));
    all.truncate(limit);
    all
}

/// 比较项目路径：去掉末尾的斜杠；Windows 上不分大小写、两种斜杠一样
fn norm(p: &str) -> String {
    let p = p.trim_end_matches(['/', '\\']);
    if cfg!(windows) {
        p.replace('\\', "/").to_lowercase()
    } else {
        p.to_string()
    }
}

fn read_tail(path: &Path) -> Option<String> {
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    let skip = len.saturating_sub(TAIL);
    f.seek(SeekFrom::Start(skip)).ok()?;
    let mut buf = vec![];
    f.read_to_end(&mut buf).ok()?;
    let mut s = String::from_utf8_lossy(&buf).into_owned();
    // 从中间开始读的：第一行不完整，丢掉
    if skip > 0 {
        s = s.split_once('\n').map(|(_, rest)| rest.to_string()).unwrap_or_default();
    }
    Some(s)
}

/// 把 `[Pasted text #N …]` 换回粘贴的内容；找不到内容的照原样留着
fn expand(display: &str, pasted: &Value, dir: &Path) -> String {
    let Some(map) = pasted.as_object().filter(|m| !m.is_empty()) else { return display.to_string() };
    let mut out = display.to_string();
    for (n, p) in map {
        if p["type"].as_str() != Some("text") {
            continue;
        }
        let content = p["content"].as_str().map(String::from).or_else(|| {
            let hash = p["contentHash"].as_str().filter(|h| !h.is_empty() && h.chars().all(|c| c.is_ascii_alphanumeric()))?;
            std::fs::read_to_string(dir.join("paste-cache").join(format!("{hash}.txt"))).ok()
        });
        let Some(content) = content.filter(|c| c.len() <= PASTE_MAX) else { continue };
        let head = format!("[Pasted text #{n}");
        if let Some(start) = out.find(&head) {
            if let Some(end) = out[start..].find(']') {
                out.replace_range(start..start + end + 1, &content);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_project_newest_first_with_pastes() {
        let tmp = tempfile::tempdir().unwrap();
        let (a, b) = (tmp.path().join("a"), tmp.path().join("b"));
        std::fs::create_dir_all(a.join("paste-cache")).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(a.join("paste-cache").join("abc123.txt"), "cached\npaste").unwrap();
        let lines = [
            r#"{"display":"old","pastedContents":{},"timestamp":1,"project":"/w/p","sessionId":"s1"}"#,
            r#"{"display":"other project","pastedContents":{},"timestamp":5,"project":"/w/q","sessionId":"s1"}"#,
            r#"{"display":"look [Pasted text #1 +1 lines] ok","pastedContents":{"1":{"id":1,"type":"text","content":"inline\ntext"}},"timestamp":3,"project":"/w/p/","sessionId":"s2"}"#,
            r#"{"display":"[Pasted text #2 +1 lines]","pastedContents":{"2":{"id":2,"type":"text","contentHash":"abc123"}},"timestamp":4,"project":"/w/p","sessionId":"s2"}"#,
            "not json",
        ];
        std::fs::write(a.join("history.jsonl"), lines.join("\n")).unwrap();
        std::fs::write(b.join("history.jsonl"), r#"{"display":"old","pastedContents":{},"timestamp":9,"project":"/w/p","sessionId":"s3"}"#).unwrap();
        let got = claude_prompts(&[a, b, tmp.path().join("missing")], Path::new("/w/p"), 10);
        let texts: Vec<_> = got.iter().map(|p| p.text.as_str()).collect();
        assert_eq!(texts, ["old", "cached\npaste", "look inline\ntext ok"]);
        assert_eq!(got[0].at, 9);
        assert_eq!(got[0].session.as_deref(), Some("s3"));
    }
}
