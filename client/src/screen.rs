//! 认出终端屏幕上 AI 编程助手（目前是 Codex）在干什么：在干活、在等你同意、在等你选一项、空闲。
//!
//! Claude Code 的确认框画在方框里，选中的一项前面是 `❯`，没有“enter …”提示行；DSH 终端版的菜单同样处理。
//!
//! 只看 `capture-pane` 拿到的最后一屏文字，按界面里固定的几句话来认（样本见
//! `tests/fixtures/screen/`）。认不出就是 [`State::Unknown`]，界面只说“去终端里看看”，
//! 不会显示能直接点的同意按钮——宁可少认，也不能点错。
//!
//! Codex 的选择菜单长这样（当前选中的一行前面有 `›`，括号里是快捷键）：
//!
//! ```text
//!   Would you like to run the following command?
//!   Reason: …
//!   $ npm test
//! › 1. Yes, proceed (y)
//!   2. Yes, and don't ask again for commands that start with `npm test` (p)
//!   3. No, and tell Codex what to do differently (esc)
//!   Press enter to confirm or esc to cancel
//! ```

use std::hash::{Hash, Hasher};

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    /// 在干活（屏幕上有“esc to interrupt”）
    Working,
    /// 在等你同意一件事（运行命令、改文件、给权限）
    Approval,
    /// 在等你从几项里选一项（例如“信任这个文件夹吗”、更新提示）
    Choice,
    /// 等你说下一句
    Idle,
    /// 认不出（可能 Codex 已经退出，回到了 shell）
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Opt {
    pub n: u32,
    pub label: String,
    /// 括号里的快捷键（`y`、`p`、`esc`…），没有就是 None
    pub key: Option<String>,
    /// 当前选中的一项（前面有 `›`）
    pub selected: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Prompt {
    /// `exec` 运行命令、`patch` 改文件、`permissions` 要权限、`other` 其他选择
    pub kind: &'static str,
    pub question: String,
    pub command: Option<String>,
    pub reason: Option<String>,
    pub options: Vec<Opt>,
    /// 问题 + 命令 + 选项的摘要；同意前用它确认屏幕上还是同一个提示
    pub fingerprint: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Screen {
    pub state: State,
    pub prompt: Option<Prompt>,
}

/// 去掉方框的边线（`│ … │`），保留里面的缩进。
fn unbox(line: &str) -> String {
    let t = line.trim_end();
    let t = t.strip_suffix('│').unwrap_or(t).trim_end();
    let lead = t.len() - t.trim_start().len();
    match t[lead..].strip_prefix('│') {
        Some(rest) => rest.to_string(),
        None => t.to_string(),
    }
}

/// 选中标记：Codex 是 `›`，Claude Code 是 `❯`。
const MARKS: [char; 2] = ['›', '❯'];

/// 选项行：`› 1. Yes, proceed (y)` / `❯ 1. Yes` / `  2. Skip`
fn option_line(line: &str) -> Option<Opt> {
    let selected = line.trim_start().starts_with(MARKS);
    let t = line.trim_start().trim_start_matches(MARKS).trim_start();
    let dot = t.find(". ")?;
    let n: u32 = t[..dot].parse().ok()?;
    let mut label = t[dot + 2..].trim().to_string();
    let mut key = None;
    if label.ends_with(')') {
        if let Some(open) = label.rfind(" (") {
            let k = &label[open + 2..label.len() - 1];
            if !k.is_empty() && k.len() <= 5 && k.chars().all(|c| c.is_ascii_alphanumeric()) {
                key = Some(k.to_ascii_lowercase());
                label.truncate(open);
            }
        }
    }
    Some(Opt { n, label, key, selected })
}

fn fingerprint(question: &str, command: Option<&str>, options: &[Opt]) -> String {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    question.hash(&mut h);
    command.hash(&mut h);
    for o in options {
        o.label.hash(&mut h);
    }
    format!("{:016x}", h.finish())
}

pub fn parse(screen: &str) -> Screen {
    let lines: Vec<String> = screen.lines().map(unbox).filter(|l| !l.trim().is_empty()).collect();
    let tail = &lines[lines.len().saturating_sub(40)..];
    if let Some(prompt) = menu(tail) {
        let state = if prompt.kind == "other" { State::Choice } else { State::Approval };
        return Screen { state, prompt: Some(prompt) };
    }
    let last = &tail[tail.len().saturating_sub(8)..];
    let state = if last.iter().any(|l| l.contains("esc to interrupt")) {
        State::Working
    } else if last.iter().any(|l| l.contains("for shortcuts") || l.starts_with("› ") || l.trim_start().starts_with("> ")) {
        // 输入框（Codex 行首是 `›`，Claude Code 是 `>`）：空着时有“? for shortcuts”提示，打了字之后只剩输入框
        State::Idle
    } else {
        State::Unknown
    };
    Screen { state, prompt: None }
}

/// 只由边线字符组成的行（方框的上下边）。
fn border(t: &str) -> bool {
    !t.is_empty() && t.chars().all(|c| matches!(c, '─' | '━' | '═' | '╌' | '╭' | '╮' | '╰' | '╯' | '┌' | '┐' | '└' | '┘' | '-' | ' '))
}

/// 菜单下面的操作提示：“Press enter to confirm or esc to cancel”“enter continue · esc skip”“Esc to cancel”。
fn footer(t: &str) -> bool {
    let l = t.to_ascii_lowercase();
    t.len() < 120 && (l.contains("enter") || l.contains("esc"))
}

/// 确认框上方的标题 → 种类：Claude Code 的“Bash command”“Edit file”……
fn header_kind(t: &str) -> Option<&'static str> {
    let l = t.trim().to_ascii_lowercase();
    if l == "bash command" || l == "powershell command" || l == "run command" {
        Some("exec")
    } else if ["edit file", "create file", "write file", "update file", "edit notebook", "overwrite file"].contains(&l.as_str()) {
        Some("patch")
    } else {
        None
    }
}

/// 屏幕底部的选择菜单：一串编号选项（其中一项带选中标记），下面最多几行边线或操作提示。
fn menu(tail: &[String]) -> Option<Prompt> {
    // 从最底下往上跳过边线、操作提示，直到选项；碰到别的内容就不是菜单
    let mut end = tail.len();
    let mut skipped = 0;
    while end > 0 {
        let l = &tail[end - 1];
        if option_line(l).is_some() {
            break;
        }
        let t = l.trim();
        if skipped < 4 && (border(t) || footer(t)) {
            end -= 1;
            skipped += 1;
            continue;
        }
        return None;
    }
    // 往上收选项；太长的选项会折到下一行（缩进更深），接回它所在的选项
    let mut first = end;
    let mut options = Vec::new();
    let mut wrapped: Vec<&str> = Vec::new();
    let mut i = end;
    while i > 0 {
        let l = tail[i - 1].as_str();
        if let Some(o) = option_line(l) {
            let o = if wrapped.is_empty() {
                o
            } else {
                let joined = std::iter::once(l.trim_end()).chain(wrapped.iter().rev().map(|w| w.trim())).collect::<Vec<_>>().join(" ");
                wrapped.clear();
                option_line(&joined)?
            };
            options.push(o);
            first = i - 1;
        } else if wrapped.len() < 3 && l.len() - l.trim_start().len() >= 4 {
            wrapped.push(l);
        } else {
            break;
        }
        i -= 1;
    }
    options.reverse();
    if options.len() < 2 || options[0].n != 1 || options.windows(2).any(|w| w[1].n != w[0].n + 1) || !options.iter().any(|o| o.selected) {
        return None;
    }
    // 往上找问题：Codex 是 “Would you like to …?”，Claude Code 是 “Do you want to …?”（下面的 Reason 里也可能
    // 有问号，先找这两句）；再不行找最近一句带 ? 的话；都没有时（例如更新提示）用选项上面最多三行当说明
    let from = first.saturating_sub(15);
    let ask = (from..first).rev().find(|&i| {
        let t = tail[i].trim_start();
        t.starts_with("Would you like to") || t.starts_with("Do you want to")
    });
    let (qi, question) = match ask.or_else(|| (from..first).rev().find(|&i| tail[i].contains('?'))) {
        Some(i) => {
            let t = tail[i].trim();
            (i, t[..t.find('?').map_or(t.len(), |q| q + 1)].trim().to_string())
        }
        None => {
            let i = first.saturating_sub(3).max(from);
            (first.saturating_sub(1), tail[i..first].iter().map(|l| l.trim()).collect::<Vec<_>>().join(" "))
        }
    };
    if question.is_empty() {
        return None;
    }
    let between = &tail[(qi + 1).min(first)..first];
    // Claude Code：问题上面是“Bash command”这类标题，标题下一行就是命令
    let head = (qi.saturating_sub(12)..qi).rev().find_map(|i| header_kind(&tail[i]).map(|k| (i, k)));
    let mut command = between.iter().find_map(|l| l.trim().strip_prefix("$ ")).map(|s| s.trim().to_string());
    if command.is_none() {
        if let Some((hi, "exec")) = head {
            command = tail[hi + 1..qi].iter().map(|l| l.trim()).find(|l| !l.is_empty()).map(str::to_string);
        }
    }
    let reason = between.iter().find_map(|l| l.trim().strip_prefix("Reason:")).map(|s| s.trim().to_string());
    let q = question.to_ascii_lowercase();
    let kind = if q.starts_with("would you like to run") || head.is_some_and(|(_, k)| k == "exec") {
        "exec"
    } else if q.starts_with("would you like to make") || q.starts_with("do you want to make") || q.starts_with("do you want to create") || head.is_some_and(|(_, k)| k == "patch") {
        "patch"
    } else if q.contains("grant") {
        "permissions"
    } else {
        "other"
    };
    let fingerprint = fingerprint(&question, command.as_deref(), &options);
    Some(Prompt { kind, question, command, reason, options, fingerprint })
}

/// 你在界面上点的选择 → 要发给终端的按键（tmux `send-keys` 的参数）。
/// `once`：第 1 项；`session`：第 2 项（必须也是 “Yes…”）；`deny`：以 “No” 开头的一项；`n`：第 n 项。
pub fn keys_for(p: &Prompt, choice: &str, n: Option<u32>) -> Option<Vec<String>> {
    let pick = match choice {
        "once" => p.options.first().filter(|o| o.label.starts_with("Yes")),
        "session" => p.options.get(1).filter(|o| o.label.starts_with("Yes")),
        "deny" => p.options.iter().find(|o| o.label.starts_with("No")),
        "option" => n.and_then(|n| p.options.iter().find(|o| o.n == n)),
        _ => None,
    }?;
    Some(match pick.key.as_deref() {
        Some("esc") => vec!["Escape".to_string()],
        Some(k) if k.chars().count() == 1 => vec![k.to_string()],
        // 没有快捷键：从当前选中的一项用方向键移过去，再回车
        _ => {
            let from = p.options.iter().find(|o| o.selected)?.n;
            let step = if pick.n > from { "Down" } else { "Up" };
            let mut keys = vec![step.to_string(); pick.n.abs_diff(from) as usize];
            keys.push("Enter".to_string());
            keys
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXEC: &str = include_str!("../tests/fixtures/screen/codex-exec-approval.txt");
    const AFTER: &str = include_str!("../tests/fixtures/screen/codex-after-deny.txt");
    const WRAPPED: &str = include_str!("../tests/fixtures/screen/codex-exec-approval-wrapped.txt");

    #[test]
    fn wrapped_option() {
        let s = parse(WRAPPED);
        assert_eq!(s.state, State::Approval);
        let p = s.prompt.unwrap();
        assert_eq!(p.options.len(), 3);
        assert_eq!(p.options[1].label, "Yes, and don't ask again for commands that start with `New-Item -ItemType File approved.txt`");
        assert_eq!(p.options[1].key.as_deref(), Some("p"));
        assert_eq!(p.command.as_deref(), Some("New-Item -ItemType File approved.txt"));
    }

    #[test]
    fn typed_but_not_sent_is_idle() {
        let s = parse("• Created x.txt.\n  Worked for 8s • 23:06\n› Run exactly this one PowerShell command\n  GPT-6-Astra medium · ~\\work · Run PowerSh…\n   ⚠ 2 warnings · f2 to view\n");
        assert_eq!(s.state, State::Idle);
    }

    #[test]
    fn exec_approval() {
        let s = parse(EXEC);
        assert_eq!(s.state, State::Approval);
        let p = s.prompt.unwrap();
        assert_eq!(p.kind, "exec");
        assert_eq!(p.question, "Would you like to run the following command?");
        assert_eq!(p.command.as_deref(), Some("New-Item -ItemType File b.txt"));
        assert!(p.reason.as_deref().unwrap().starts_with("The read-only sandbox"));
        assert_eq!(p.options.len(), 3);
        assert_eq!(p.options[0].label, "Yes, proceed");
        assert_eq!(p.options[0].key.as_deref(), Some("y"));
        assert_eq!(p.options[2].key.as_deref(), Some("esc"));
        assert_eq!(keys_for(&p, "once", None), Some(vec!["y".to_string()]));
        assert_eq!(keys_for(&p, "session", None), Some(vec!["p".to_string()]));
        assert_eq!(keys_for(&p, "deny", None), Some(vec!["Escape".to_string()]));
    }

    #[test]
    fn idle_after_deny() {
        let s = parse(AFTER);
        assert_eq!(s.state, State::Idle);
        assert!(s.prompt.is_none());
    }

    #[test]
    fn trust_folder_is_a_choice() {
        let screen = "  Folder access\n  C:\\work\n  Trust this folder? Codex can read, edit, and run files here, subject to your permission settings. Folder settings\n  can run code automatically. Your trust\n  decision will be saved.\n› 1. Trust and continue\n  2. Quit\n  enter continue · esc quit\n";
        let s = parse(screen);
        assert_eq!(s.state, State::Choice);
        assert_eq!(s.prompt.unwrap().question, "Trust this folder?");
        let screen = "  Update available · 0.159.3 → 0.162.1\n  Release notes: https://github.com/openai/codex/releases/latest\n› 1. Update now (runs `npm install -g @openai/codex`)\n  2. Skip\n  3. Skip until next version\n  enter continue · esc skip\n";
        let s = parse(screen);
        assert_eq!(s.state, State::Choice);
        let p = s.prompt.unwrap();
        assert_eq!(p.kind, "other");
        assert!(p.question.starts_with("Update available"));
        assert_eq!(p.options[0].key, None);
        assert_eq!(keys_for(&p, "option", Some(2)), Some(vec!["Down".to_string(), "Enter".to_string()]));
        assert_eq!(keys_for(&p, "once", None), None);
    }

    #[test]
    fn claude_code_bash_prompt() {
        let screen = "╭──────────────────────────────────────────────╮\n│ Bash command                                 │\n│                                              │\n│   rm -rf build                               │\n│   Remove the build directory                 │\n│                                              │\n│ Do you want to proceed?                      │\n│ ❯ 1. Yes                                     │\n│   2. Yes, and don't ask again for rm commands in F:\\work │\n│   3. No, and tell Claude what to do differently (esc) │\n╰──────────────────────────────────────────────╯\n";
        let s = parse(screen);
        assert_eq!(s.state, State::Approval);
        let p = s.prompt.unwrap();
        assert_eq!(p.kind, "exec");
        assert_eq!(p.question, "Do you want to proceed?");
        assert_eq!(p.command.as_deref(), Some("rm -rf build"));
        assert_eq!(p.options.len(), 3);
        assert!(p.options[0].selected);
        assert_eq!(keys_for(&p, "once", None), Some(vec!["Enter".to_string()]));
        assert_eq!(keys_for(&p, "session", None), Some(vec!["Down".to_string(), "Enter".to_string()]));
        assert_eq!(keys_for(&p, "deny", None), Some(vec!["Escape".to_string()]));
    }

    #[test]
    fn claude_code_edit_prompt() {
        let screen = " Edit file\n src/app.rs\n  12 - let a = 1;\n  12 + let a = 2;\n Do you want to make this edit to app.rs?\n ❯ 1. Yes\n   2. Yes, allow all edits during this session (shift+tab)\n   3. No, and tell Claude what to do differently (esc)\n";
        let p = parse(screen).prompt.unwrap();
        assert_eq!(p.kind, "patch");
        assert_eq!(p.options[1].key, None);
        assert_eq!(p.options[1].label, "Yes, allow all edits during this session (shift+tab)");
    }

    #[test]
    fn menu_without_footer_is_a_choice() {
        let screen = "  These settings persist and can disable functionality for other clients.\n  1. Run without daemon this time\n  2. Restart with these settings\n› 3. Cancel\n";
        let s = parse(screen);
        assert_eq!(s.state, State::Choice);
        let p = s.prompt.unwrap();
        assert_eq!(keys_for(&p, "option", Some(1)), Some(vec!["Up".to_string(), "Up".to_string(), "Enter".to_string()]));
    }

    #[test]
    fn numbered_list_in_a_reply_is_not_a_menu() {
        let screen = "• 下一步可以：\n  1. 跑测试\n  2. 提交\n› Ask Codex to do anything\n  ? for shortcuts\n";
        let s = parse(screen);
        assert_eq!(s.state, State::Idle);
        assert!(s.prompt.is_none());
    }

    #[test]
    fn working() {
        let s = parse("› fix it\n• Working (12s • esc to interrupt)\n\n› Ask Codex to do anything\n");
        assert_eq!(s.state, State::Working);
    }

    #[test]
    fn shell_is_unknown() {
        assert_eq!(parse("PS C:\\Users\\me> ").state, State::Unknown);
    }
}
