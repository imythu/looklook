//! 对话界面里和 AI 工具有关的部分（docs/CODEX_UI.md §3）。
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
    /// 界面上哪些操作可用（随状态推给界面，界面只按它显示按钮，不自己判断工具和版本）
    fn caps(&self) -> Value;
    /// 已经绑定了会话时，哪些事件带来的会话和记录文件可以换掉当前的（`/clear`、`/resume` 之后）
    fn rebinds_on(&self, _event: &str) -> bool {
        true
    }
    /// SessionStart 要等到第一条消息才来（Codex）：没收到 hook 之前也当作已接上，不显示“没有接入对话界面”
    fn session_starts_on_prompt(&self) -> bool {
        false
    }
    /// 面板前台报成 shell 时，用来确认工具还在运行的进程名（小写、不带 `.exe`）；空的就只看前台命令
    fn process_names(&self) -> &'static [&'static str] {
        &[]
    }
    /// 这个进程（命令行参数）是不是带着看看的 hooks 启动的；看不出来时 None
    fn launched_with_hooks(&self, _args: &[String]) -> Option<bool> {
        None
    }
    /// TUI 上的选择框（信任文件夹、确认框、选择器……）：界面显示成弹窗，选了哪项再替用户按键
    fn dialog(&self, capture: &str) -> Option<screen::Dialog> {
        screen::dialog(capture)
    }
    /// 审批 hook 还在等答复时，TUI 是不是同时也弹出了自己的确认框（Claude Code 是，两边谁先答算谁的；
    /// Codex 不是：要等 hook 不做决定才弹）。是的话，确认框从屏幕上消失就说明已经在终端里答过了。
    fn tui_asks_while_hook_waits(&self) -> bool {
        true
    }
    /// 送出后等 `UserPromptSubmit` 多久才算没送到
    fn delivery_wait(&self) -> std::time::Duration {
        std::time::Duration::from_secs(3)
    }
}

/// 按终端的启动类型选适配器；没有对话界面的类型为 None。
pub fn for_launch(launch: &str) -> Option<&'static dyn Agent> {
    match launch {
        "claude" => Some(&Claude),
        "codex" => Some(&Codex),
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
        super::valid_transcript_path(p)
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

    fn caps(&self) -> Value {
        json!({ "allow_session": true, "allow_dirs": true, "images": true, "plan": true, "questions": true, "models": true, "usage": true, "mode": true })
    }
}

// ---------------- Codex ----------------

pub struct Codex;

impl Agent for Codex {
    fn launch(&self) -> &'static str {
        "codex"
    }

    /// rollout 只能是 `…/sessions/YYYY/MM/DD/rollout-*.jsonl`
    fn valid_transcript(&self, p: &Path) -> bool {
        p.is_absolute()
            && p.extension().is_some_and(|e| e == "jsonl")
            && p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("rollout-"))
            && p.ancestors().nth(4).and_then(|d| d.file_name()).is_some_and(|n| n == "sessions")
            && !p.components().any(|c| matches!(c, std::path::Component::ParentDir))
    }

    fn open_transcript(&self, path: PathBuf) -> Box<dyn TranscriptReader> {
        Box::new(super::codex_rollout::Rollout::new(path))
    }

    fn classify(&self, capture: &str) -> Screen {
        super::codex_screen::classify(capture)
    }

    fn live(&self, capture: &str) -> Live {
        super::codex_screen::live(capture)
    }

    fn pending_kind(&self, tool: &str) -> &'static str {
        if tool == "AskUserQuestion" {
            "question"
        } else {
            "permission"
        }
    }

    /// app-server 请求的答复（见 `codex_chat::decision`）
    fn decision(&self, p: &PendingView, kind: &str, req: &PermissionAnswer) -> Result<Value, LocalError> {
        super::codex_chat::decision(p.tool, kind, p.input, req)
    }

    fn plan_check(&self, _choice: u8) -> Option<(&'static [&'static str], &'static str)> {
        None
    }

    /// 自定义命令以后再加（`$CODEX_HOME/prompts`、skills，docs/CODEX_UI.md §9）；内置命令在界面里
    fn commands(&self, _workdir: &Path) -> Value {
        json!([])
    }

    /// 走 app-server：能选模型、看账号用量、“本会话都允许”、提问、发图片；没有权限模式和计划确认框
    fn caps(&self) -> Value {
        json!({ "allow_session": true, "allow_dirs": false, "images": true, "plan": false, "questions": true, "models": true, "usage": true, "mode": false })
    }

    /// 子代理的 hook 可能带着它自己的 rollout（S11 未验证），只认主线的开始和用户发消息
    fn rebinds_on(&self, event: &str) -> bool {
        matches!(event, "SessionStart" | "UserPromptSubmit")
    }

    fn session_starts_on_prompt(&self) -> bool {
        true
    }

    fn process_names(&self) -> &'static [&'static str] {
        &["codex"]
    }


    fn dialog(&self, capture: &str) -> Option<screen::Dialog> {
        super::codex_screen::dialog(capture)
    }

    /// 实测：hook 答复之前 TUI 一直是“Working”的输入框，hook 不做决定才弹确认框
    fn tui_asks_while_hook_waits(&self) -> bool {
        false
    }

    /// Windows 上每个 hook 都要先起一个 PowerShell（一两秒），第一条消息前面还有 SessionStart：3 秒常常不够（实测）
    fn delivery_wait(&self) -> std::time::Duration {
        std::time::Duration::from_secs(10)
    }

}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_adapter_by_launch() {
        assert_eq!(for_launch("claude").map(|a| a.launch()), Some("claude"));
        assert_eq!(for_launch("codex").map(|a| a.launch()), Some("codex"));
        assert!(for_launch("shell").is_none());
    }

    #[test]
    fn codex_rollout_paths_and_decisions() {
        let c = Codex;
        let ok = if cfg!(windows) { r"C:\Users\a\.codex\sessions\2026\10\18\rollout-2026-10-18T17-35-38-01a1.jsonl" } else { "/home/a/.codex/sessions/2026/10/18/rollout-2026-10-18T17-35-38-01a1.jsonl" };
        assert!(c.valid_transcript(Path::new(ok)));
        assert!(!c.valid_transcript(Path::new("/home/a/.codex/sessions/2026/10/18/other.jsonl")));
        assert!(!c.valid_transcript(Path::new("/home/a/.claude/projects/x/s.jsonl")), "Claude 的路径不认");
        assert!(!c.valid_transcript(Path::new("/home/a/.codex/sessions/2026/10/../../../../etc/rollout-x.jsonl")));
        let p = PendingView { tool: "Bash", input: &Value::Null, suggestions: &Value::Null };
        let ans = |d: &str| PermissionAnswer { id: 1, decision: d.into(), message: None, answers: None };
        assert_eq!(c.decision(&p, "permission", &ans("allow")).unwrap(), json!({ "decision": "accept" }));
        assert_eq!(c.decision(&p, "permission", &ans("deny")).unwrap(), json!({ "decision": "decline" }));
        assert!(c.plan_check(1).is_none());
        assert_eq!(c.pending_kind("AskUserQuestion"), "question");
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
