//! 任务看板（docs/TASK_BOARD.md §3、§4）：每个会话的这一轮、看过没有、一句话摘要。
//!
//! 摘要按规则从 hook、记录文件、屏幕拼出来，不调模型（花用户额度，也不能替用户调 Anthropic 的接口）。

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// 一轮怎么结束的
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Done,
    Failed,
    /// 人按了中断，或者会话结束了：不算“刚结束”
    Interrupted,
}

/// 一轮：切到运行中开始，回到空闲结束。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Turn {
    pub started_at: i64,
    #[serde(default)]
    pub ended_at: Option<i64>,
    #[serde(default)]
    pub outcome: Option<Outcome>,
    #[serde(default)]
    pub error: Option<String>,
    /// 最后一条回复的第一段
    #[serde(default)]
    pub summary: Option<String>,
}

impl Turn {
    pub fn active(&self) -> bool {
        self.ended_at.is_none()
    }
}

/// 存在本机数据库里（`board.{id}`）：客户端重启后看板上的“刚结束”和标题还在。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Meta {
    #[serde(default)]
    pub turn: Option<Turn>,
    /// 最后一次有人在看这个会话（毫秒）
    #[serde(default)]
    pub seen_at: i64,
    #[serde(default)]
    pub title: Option<String>,
    /// 没有标题时用：第一条用户消息
    #[serde(default)]
    pub first_prompt: Option<String>,
}

pub fn key(id: &str) -> String {
    format!("board.{id}")
}

/// 截到 `n` 个字，换行和连续空白合成一个空格
pub fn clip(s: &str, n: usize) -> String {
    let flat = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= n {
        flat
    } else {
        flat.chars().take(n).collect::<String>() + "…"
    }
}

/// 回复的第一段（去掉标题、列表的记号），200 字以内
pub fn first_para(s: &str) -> Option<String> {
    let para = s.split("\n\n").map(str::trim).find(|p| !p.is_empty())?;
    let lines: Vec<&str> = para.lines().map(|l| l.trim().trim_start_matches(['#', '>', '-', '*']).trim()).collect();
    let out = clip(&lines.join(" "), 200);
    (!out.is_empty()).then_some(out)
}

fn base_name(p: &str) -> String {
    p.rsplit(['/', '\\']).find(|s| !s.is_empty()).unwrap_or(p).to_string()
}

/// 正在做的事：工具和对象（界面按工具换成“读 / 改 / 跑命令”和颜色）
pub fn doing(tool: &str, input: &Value) -> Value {
    let s = |k: &str| input[k].as_str().filter(|v| !v.trim().is_empty());
    let detail = match tool {
        "Bash" => s("description").or(s("command")).map(|v| clip(v, 120)),
        "Read" | "Edit" | "Write" | "MultiEdit" | "NotebookEdit" => s("file_path").or(s("notebook_path")).map(base_name),
        "Grep" | "Glob" => s("pattern").map(|v| clip(v, 80)),
        "WebFetch" => s("url").map(|v| clip(v, 80)),
        "WebSearch" => s("query").map(|v| clip(v, 80)),
        "Task" | "Agent" => s("description").map(|v| clip(v, 80)),
        _ => None,
    };
    json!({ "tool": tool, "detail": detail })
}

/// “等你处理”的一句话：命令、文件、问题、计划第一行；提问只有一个单选问题时带上选项（看板上直接点）。
pub fn attn(tool: &str, kind: &str, input: &Value) -> (Option<String>, Vec<String>) {
    match kind {
        "question" => {
            let qs = input["questions"].as_array().cloned().unwrap_or_default();
            let q = qs.first();
            let text = q.and_then(|q| q["question"].as_str()).map(|v| clip(v, 160));
            let single = qs.len() == 1 && q.is_some_and(|q| q["multiSelect"] != true && q["secret"] != true);
            let options = if single {
                q.and_then(|q| q["options"].as_array()).into_iter().flatten().filter_map(|o| o["label"].as_str().or_else(|| o.as_str())).map(String::from).collect()
            } else {
                vec![]
            };
            (text, options)
        }
        "plan" => (input["plan"].as_str().and_then(|p| p.lines().map(|l| l.trim().trim_start_matches('#').trim()).find(|l| !l.is_empty())).map(|l| clip(l, 160)), vec![]),
        _ => {
            let detail = match tool {
                "Bash" => input["command"].as_str().map(|c| clip(c, 300)),
                // Codex 改文件：改了哪些文件
                "apply_patch" => changed_files(&input["changes"]),
                _ => doing(tool, input)["detail"].as_str().map(String::from),
            };
            (detail, vec![])
        }
    }
}

/// Codex 改了哪些文件（文件名）：`changes` 是按路径的对象，或者 `[{path, …}]`
pub fn changed_files(changes: &Value) -> Option<String> {
    let paths: Vec<String> = match changes {
        Value::Object(m) => m.keys().map(|k| base_name(k)).collect(),
        Value::Array(a) => a.iter().filter_map(|c| c["path"].as_str()).map(base_name).collect(),
        _ => vec![],
    };
    (!paths.is_empty()).then(|| clip(&paths.join(", "), 120))
}

/// Claude Code 的 transcript 里最后一条（主对话的）回复的第一段：只读文件最后 64 KB。
pub fn last_reply(path: &Path) -> Option<String> {
    const TAIL: u64 = 64 * 1024;
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    f.seek(SeekFrom::Start(len.saturating_sub(TAIL))).ok()?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf);
    text.lines().rev().filter_map(|l| serde_json::from_str::<Value>(l).ok()).find_map(|v| {
        if v["type"] != "assistant" || v["isSidechain"] == true {
            return None;
        }
        let joined: String = v["message"]["content"].as_array()?.iter().filter(|b| b["type"] == "text").filter_map(|b| b["text"].as_str()).collect::<Vec<_>>().join("\n\n");
        first_para(&joined)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_para_strips_marks_and_clips() {
        assert_eq!(first_para("## 改好了\n\n细节").as_deref(), Some("改好了"));
        assert_eq!(first_para("- 第一行\n- 第二行\n\n后面").as_deref(), Some("第一行 第二行"));
        assert_eq!(first_para("  \n\n  ").as_deref(), None);
        assert_eq!(first_para(&"字".repeat(300)).unwrap().chars().count(), 201);
    }

    #[test]
    fn attn_details() {
        let (d, o) = attn("Bash", "permission", &json!({ "command": "git push --force" }));
        assert_eq!((d.as_deref(), o.len()), (Some("git push --force"), 0));
        let q = json!({ "questions": [{ "question": "用哪个？", "options": [{ "label": "A" }, { "label": "B" }] }] });
        assert_eq!(attn("AskUserQuestion", "question", &q), (Some("用哪个？".into()), vec!["A".into(), "B".into()]));
        let multi = json!({ "questions": [{ "question": "一", "options": [] }, { "question": "二", "options": [] }] });
        assert!(attn("AskUserQuestion", "question", &multi).1.is_empty());
        assert_eq!(attn("ExitPlanMode", "plan", &json!({ "plan": "\n# 计划：加看板\n1. …" })).0.as_deref(), Some("计划：加看板"));
        let patch = json!({ "changes": { "/a/b/Board.jsx": {}, "/a/c.rs": {} } });
        assert_eq!(attn("apply_patch", "permission", &patch).0.as_deref(), Some("Board.jsx, c.rs"));
        assert_eq!(attn("Edit", "permission", &json!({ "file_path": "/x/y/z.rs" })).0.as_deref(), Some("z.rs"));
    }

    #[test]
    fn doing_prefers_description() {
        assert_eq!(doing("Bash", &json!({ "command": "npm test", "description": "跑测试" }))["detail"], "跑测试");
        assert_eq!(doing("Read", &json!({ "file_path": "C:\\src\\main.rs" }))["detail"], "main.rs");
        assert!(doing("Foo", &json!({}))["detail"].is_null());
    }

    #[test]
    fn last_reply_reads_tail() {
        let dir = std::env::temp_dir().join(format!("ll-board-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("t.jsonl");
        let lines = [
            json!({ "type": "assistant", "message": { "content": [{ "type": "text", "text": "早先的" }] } }),
            json!({ "type": "assistant", "message": { "content": [{ "type": "text", "text": "已修好。\n\n细节" }, { "type": "tool_use" }] } }),
            json!({ "type": "assistant", "isSidechain": true, "message": { "content": [{ "type": "text", "text": "子代理" }] } }),
            json!({ "type": "user", "message": { "content": "x" } }),
        ];
        std::fs::write(&p, lines.iter().map(|l| l.to_string() + "\n").collect::<String>()).unwrap();
        assert_eq!(last_reply(&p).as_deref(), Some("已修好。"));
        let _ = std::fs::remove_file(&p);
    }
}
