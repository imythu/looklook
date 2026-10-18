//! 对话界面里和 AI 工具有关的部分（CODEX_UI.md §3）。
//!
//! 核心（`mod.rs`：hook 状态机、审批挂起、送达确认、事件推送）两边共用；每个工具实现 [`Agent`]：
//! 记录文件怎么读、屏幕怎么认、审批怎么答复、计划确认框长什么样、`/` 命令从哪里找。
//! 目前只有 Claude Code（调用 `transcript.rs`、`screen.rs`、`commands.rs`），Codex 之后加在这里。

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use super::screen::{self, Live, Screen};
use super::transcript::{Change, Item, Stats, Transcript};
use super::{commands, PermissionAnswer};
use crate::error::LocalError;

/// 跟读一个会话的记录文件，整理成对话条目。
pub trait TranscriptReader: Send + Sync {
    fn path(&self) -> &Path;
    /// 读新写入的部分
    fn poll(&mut self) -> std::io::Result<Change>;
    fn len(&self) -> usize;
    /// 第 `from` 到 `to` 条（不含）
    fn items(&self, from: usize, to: usize) -> Vec<Item>;
    /// 按 key 取不截断的原始内容
    fn full_item(&self, key: &str) -> Option<Value>;
    /// 某次工具调用的参数
    fn tool_input(&self, tool_use_id: &str) -> Option<&Value>;
    /// 记录里看到的工具版本
    fn version(&self) -> Option<String>;
    fn stats(&self) -> Stats;
}

impl TranscriptReader for Transcript {
    fn path(&self) -> &Path {
        Transcript::path(self)
    }
    fn poll(&mut self) -> std::io::Result<Change> {
        Transcript::poll(self)
    }
    fn len(&self) -> usize {
        Transcript::len(self)
    }
    fn items(&self, from: usize, to: usize) -> Vec<Item> {
        Transcript::items(self, from, to)
    }
    fn full_item(&self, key: &str) -> Option<Value> {
        Transcript::full_item(self, key)
    }
    fn tool_input(&self, tool_use_id: &str) -> Option<&Value> {
        Transcript::tool_input(self, tool_use_id)
    }
    fn version(&self) -> Option<String> {
        self.version.clone()
    }
    fn stats(&self) -> Stats {
        self.stats
    }
}

/// 挂起的审批里，答复时要用到的内容
pub struct PendingView<'a> {
    pub tool: &'a str,
    pub input: &'a Value,
    pub suggestions: &'a Value,
}

/// 一种 AI 工具的对话界面适配。
pub trait Agent: Send + Sync {
    /// 启动类型（`instances.rs` 的 `launch`），也是推给界面的 `agent`
    fn launch(&self) -> &'static str;
    /// hook 带来的记录文件路径是不是这个工具的（防止借伪造的 hook 读任意文件）
    fn valid_transcript(&self, path: &Path) -> bool;
    fn open_transcript(&self, path: PathBuf) -> Box<dyn TranscriptReader>;
    /// TUI 当前是普通输入框，还是对话框、选择器
    fn classify(&self, capture: &str) -> Screen;
    /// 运行中屏幕上的状态行和正在输出的内容
    fn live(&self, capture: &str) -> Live;
    /// 审批的种类：`permission` / `question` / `plan`
    fn pending_kind(&self, tool: &str) -> &'static str;
    /// 界面的回答 → PermissionRequest hook 的 `decision`。`terminal` 由核心处理，不会传进来。
    fn decision(&self, pending: &PendingView, kind: &str, req: &PermissionAnswer) -> Result<Value, LocalError>;
    /// 计划确认框：屏幕上要有这些字之一，并且有 `choice` 对应的选项文字。None：不支持这个选项
    fn plan_check(&self, choice: u8) -> Option<(&'static [&'static str], &'static str)>;
    /// 输入 / 时提示的自定义命令和技能
    fn commands(&self, workdir: &Path) -> Value;
}

/// 按终端的启动类型选适配器；没有对话界面的类型为 None。
pub fn for_launch(launch: &str) -> Option<&'static dyn Agent> {
    match launch {
        "claude" => Some(&Claude),
        _ => None,
    }
}

// ---------------- Claude Code ----------------

pub struct Claude;

impl Agent for Claude {
    fn launch(&self) -> &'static str {
        "claude"
    }

    /// transcript 只能是 `…/projects/<项目>/<session>.jsonl`
    fn valid_transcript(&self, p: &Path) -> bool {
        p.is_absolute()
            && p.extension().is_some_and(|e| e == "jsonl")
            && p.parent().and_then(|d| d.parent()).and_then(|d| d.file_name()).is_some_and(|n| n == "projects")
            && !p.components().any(|c| matches!(c, std::path::Component::ParentDir))
    }

    fn open_transcript(&self, path: PathBuf) -> Box<dyn TranscriptReader> {
        Box::new(Transcript::new(path))
    }

    fn classify(&self, capture: &str) -> Screen {
        screen::classify(capture)
    }

    fn live(&self, capture: &str) -> Live {
        screen::live(capture)
    }

    fn pending_kind(&self, tool: &str) -> &'static str {
        match tool {
            "AskUserQuestion" => "question",
            "ExitPlanMode" => "plan",
            _ => "permission",
        }
    }

    fn decision(&self, p: &PendingView, kind: &str, req: &PermissionAnswer) -> Result<Value, LocalError> {
        Ok(match req.decision.as_str() {
            "allow" => json!({ "behavior": "allow" }),
            "allow_session" => {
                let mut rule = json!({ "toolName": p.tool });
                if let Some(cmd) = p.input["command"].as_str() {
                    rule["ruleContent"] = json!(cmd);
                }
                json!({ "behavior": "allow", "updatedPermissions": [{ "type": "addRules", "rules": [rule], "behavior": "allow", "destination": "session" }] })
            }
            // 只回传“允许访问这个目录”那一条建议：建议里可能还有 setMode，会切换权限模式
            "allow_dirs" => {
                let dirs: Vec<Value> = p.suggestions.as_array().map(|a| a.iter().filter(|s| s["type"] == "addDirectories").cloned().collect()).unwrap_or_default();
                if dirs.is_empty() {
                    return Err(LocalError::invalid("decision", "invalid"));
                }
                json!({ "behavior": "allow", "updatedPermissions": dirs })
            }
            "deny" => {
                let msg = req.message.as_deref().map(str::trim).filter(|m| !m.is_empty()).unwrap_or("The user denied this in the Looklook chat view.");
                json!({ "behavior": "deny", "message": msg.chars().take(2000).collect::<String>() })
            }
            "answer" if kind == "question" => {
                let answers = req.answers.as_ref().filter(|a| a.is_object()).ok_or_else(|| LocalError::invalid("answers", "required"))?;
                let mut input = p.input.clone();
                input["answers"] = answers.clone();
                json!({ "behavior": "allow", "updatedInput": input })
            }
            _ => return Err(LocalError::invalid("decision", "invalid")),
        })
    }

    /// hook 批准不了计划（实测），只能在 TUI 的确认框里按数字键
    fn plan_check(&self, choice: u8) -> Option<(&'static [&'static str], &'static str)> {
        const MARKERS: &[&str] = &["ready to execute", "Would you like to proceed"];
        match choice {
            1 => Some((MARKERS, "1. Yes")),
            2 => Some((MARKERS, "2. Yes")),
            _ => None,
        }
    }

    fn commands(&self, workdir: &Path) -> Value {
        json!(commands::scan(workdir))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_adapter_by_launch() {
        assert_eq!(for_launch("claude").map(|a| a.launch()), Some("claude"));
        assert!(for_launch("codex").is_none(), "Codex 的适配器还没加");
        assert!(for_launch("shell").is_none());
    }

    #[test]
    fn claude_transcript_paths() {
        let c = Claude;
        assert!(c.valid_transcript(Path::new(if cfg!(windows) { r"C:\Users\a\.claude\projects\-tmp-proj\96e9b84c.jsonl" } else { "/root/.claude/projects/-tmp-proj/96e9b84c.jsonl" })));
        assert!(!c.valid_transcript(Path::new("/etc/passwd")));
        assert!(!c.valid_transcript(Path::new("/root/.claude/projects/x/../../../etc/a.jsonl")));
        assert!(!c.valid_transcript(Path::new("relative/projects/x/a.jsonl")));
    }

    #[test]
    fn claude_decisions() {
        let c = Claude;
        let input = json!({ "command": "npm test" });
        let sugg = json!([{ "type": "addDirectories", "directories": ["/x"] }, { "type": "setMode", "mode": "acceptEdits" }]);
        let p = PendingView { tool: "Bash", input: &input, suggestions: &sugg };
        let ans = |d: &str| PermissionAnswer { id: 1, decision: d.into(), message: None, answers: None };
        assert_eq!(c.decision(&p, "permission", &ans("allow")).unwrap(), json!({ "behavior": "allow" }));
        let s = c.decision(&p, "permission", &ans("allow_session")).unwrap();
        assert_eq!(s["updatedPermissions"][0]["rules"][0], json!({ "toolName": "Bash", "ruleContent": "npm test" }));
        let d = c.decision(&p, "permission", &ans("allow_dirs")).unwrap();
        assert_eq!(d["updatedPermissions"].as_array().unwrap().len(), 1, "setMode 不能带上");
        assert_eq!(c.decision(&p, "permission", &ans("deny")).unwrap()["behavior"], "deny");
        assert!(c.decision(&p, "permission", &ans("answer")).is_err(), "不是提问就不能 answer");
        assert!(c.decision(&p, "permission", &ans("whatever")).is_err());
        assert_eq!(c.pending_kind("AskUserQuestion"), "question");
        assert_eq!(c.pending_kind("ExitPlanMode"), "plan");
        assert_eq!(c.pending_kind("Bash"), "permission");
    }
}
