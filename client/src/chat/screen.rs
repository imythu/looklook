//! 抓屏判断 Claude Code TUI 当前是不是“普通输入框”（docs/CLAUDE_UI.md §4.3）。
//!
//! hooks 覆盖不到信任对话框、`/model`、`/resume` 这类选择器、Esc 中断，所以要看屏幕。
//! 这里只描述“正常时长什么样”，不去枚举有哪些对话框：
//!
//! ```text
//! ────────────────────────────────   ← 横线
//! ❯ 输入框里的文字                     ← 紧跟着 ❯ 开头的一行（多行输入时下面几行是缩进的续行）
//! ────────────────────────────────   ← 横线
//!   ⏵⏵ auto mode on (shift+tab …)    ← 最多几行状态、提示
//! ```
//!
//! 权限确认框、计划确认框、AskUserQuestion、选择器、信任对话框都不满足“横线下一行以 ❯ 开头、再往下有横线、
//! 横线下面只剩几行”，一律当作 TUI 交互界面。

/// 抓到的屏幕是什么状态
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Screen {
    /// 普通输入框
    Input {
        /// 输入框里用户真正输入的文字（灰色的推荐提示不算）
        text: String,
        /// 状态行里有“esc to interrupt”：正在处理
        busy: bool,
    },
    /// 别的界面（对话框、选择器、确认框）
    Modal,
}

/// 横线：大部分是 `─`，至少 20 个
fn is_rule(line: &str) -> bool {
    let n = line.chars().filter(|c| *c == '─').count();
    n >= 20 && n * 10 >= line.trim().chars().count() * 9
}

/// `capture-pane -p -e` 的输出（带 SGR 转义）：去掉转义，同时记下哪些字符是“暗淡”（推荐提示）的。
fn strip(line: &str) -> (String, String) {
    let mut plain = String::new();
    let mut bright = String::new();
    let mut dim = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            if chars.peek() == Some(&'[') {
                chars.next();
                let mut params = String::new();
                for p in chars.by_ref() {
                    if p.is_ascii_alphabetic() {
                        if p == 'm' {
                            for code in params.split(';') {
                                match code {
                                    "2" => dim = true,
                                    "0" | "" | "22" => dim = false,
                                    _ => {}
                                }
                            }
                        }
                        break;
                    }
                    params.push(p);
                }
            }
            continue;
        }
        plain.push(c);
        if !dim {
            bright.push(c);
        }
    }
    (plain, bright)
}

pub fn classify(capture: &str) -> Screen {
    let lines: Vec<(String, String)> = capture.lines().map(strip).collect();
    let mut end = lines.len();
    while end > 0 && lines[end - 1].0.trim().is_empty() {
        end -= 1;
    }
    let lines = &lines[..end];
    // 最下面的横线：下面最多 4 行（状态、提示）
    let Some(bottom) = lines.iter().rposition(|(p, _)| is_rule(p)) else { return Screen::Modal };
    if lines.len() - bottom - 1 > 4 {
        return Screen::Modal;
    }
    // 往上找输入框上沿的横线（输入框最多 20 行）
    let Some(top) = lines[..bottom].iter().rposition(|(p, _)| is_rule(p)) else { return Screen::Modal };
    if bottom - top < 2 || bottom - top > 21 {
        return Screen::Modal;
    }
    let first = &lines[top + 1];
    let Some(rest) = first.0.trim_start().strip_prefix('❯').or_else(|| first.0.trim_start().strip_prefix('>')) else { return Screen::Modal };
    // 续行必须是缩进的（选项列表 `  2. xxx` 也是缩进的，但它们上面不会紧跟 ❯ 行再接横线）
    if lines[top + 2..bottom].iter().any(|(p, _)| !p.is_empty() && !p.starts_with(' ')) {
        return Screen::Modal;
    }
    // ❯ 后面是选项编号（`❯ 1. Yes`）：选择框，不是输入框
    let rest_trim = rest.trim_start();
    if rest_trim.split_once(". ").is_some_and(|(n, _)| !n.is_empty() && n.len() <= 2 && n.chars().all(|c| c.is_ascii_digit())) {
        return Screen::Modal;
    }
    let bright: String = {
        let b = first.1.trim_start();
        b.strip_prefix('❯').or_else(|| b.strip_prefix('>')).unwrap_or(b).trim().to_string()
    };
    let mut text = bright;
    for (_, b) in &lines[top + 2..bottom] {
        let t = b.trim();
        if !t.is_empty() {
            text.push('\n');
            text.push_str(t);
        }
    }
    let busy = lines[bottom + 1..].iter().any(|(p, _)| p.contains("esc to interrupt"));
    Screen::Input { text: text.trim().to_string(), busy }
}

/// 运行中屏幕上看得到、但 transcript 里还没有的东西（docs/CLAUDE_UI.md §4.5“正在输出的预览”）
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Live {
    /// 状态行，例如 `Symbioting… (6s · ↓ 274 tokens · thinking)`
    pub activity: Option<String>,
    /// 正在输出的那段回复（`● ` 开头、后面缩进两格的部分，折行已接回）
    pub text: Option<String>,
}

const SPINNERS: &[char] = &['·', '✢', '✳', '✶', '✻', '✽', '*', '∗', '⏺', '◆'];
const PREVIEW_LIMIT: usize = 4000;

/// 显示宽度（中日韩字符、全角符号算两格）
fn width(s: &str) -> usize {
    s.chars().map(|c| if (c as u32) >= 0x1100 && !matches!(c as u32, 0x2500..=0x257f) { 2 } else { 1 }).sum()
}

pub fn live(capture: &str) -> Live {
    let lines: Vec<String> = capture.lines().map(|l| strip(l).0.trim_end().to_string()).collect();
    let Some(bottom) = lines.iter().rposition(|l| is_rule(l)) else { return Live::default() };
    let Some(top) = lines[..bottom].iter().rposition(|l| is_rule(l)) else { return Live::default() };
    let full = width(&lines[top]);
    let mut out = Live::default();
    let mut i = top;
    // 输入框上面：空行、右对齐的提示（缩进很多）、状态行
    while i > 0 {
        let l = &lines[i - 1];
        let t = l.trim_start();
        if t.is_empty() || l.len() - t.len() >= 6 {
            i -= 1;
            continue;
        }
        if out.activity.is_none() && t.starts_with(SPINNERS) && t.contains('…') {
            out.activity = Some(t.trim_start_matches(SPINNERS).trim().chars().take(200).collect());
            i -= 1;
            continue;
        }
        break;
    }
    // 往上收集缩进两格的续行，直到 `● ` 开头的那一行
    let mut block: Vec<&str> = vec![];
    while i > 0 {
        let l = lines[i - 1].as_str();
        if let Some(head) = l.strip_prefix("● ").or_else(|| l.strip_prefix("⏺ ")) {
            // `● Bash(ls)` 这类是工具调用，不是回复
            let tool = head.split_once('(').is_some_and(|(n, _)| !n.is_empty() && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == ':'));
            if tool {
                return Live { text: None, ..out };
            }
            block.push(head);
            break;
        }
        match l.strip_prefix("  ") {
            Some(rest) if !rest.trim_start().starts_with('⎿') => block.push(rest),
            None if l.is_empty() => block.push(""),
            _ => return Live { text: None, ..out },
        }
        i -= 1;
    }
    block.reverse();
    while block.last().is_some_and(|l| l.is_empty()) {
        block.pop();
    }
    while block.first().is_some_and(|l| l.is_empty()) {
        block.remove(0);
    }
    // 占满一整行的是自动折行：接回去，不加换行
    let mut text = String::new();
    for (n, l) in block.iter().enumerate() {
        text.push_str(l);
        if n + 1 < block.len() && width(l) + 2 + 3 < full {
            text.push('\n');
        }
    }
    if !text.trim().is_empty() {
        let skip = text.chars().count().saturating_sub(PREVIEW_LIMIT);
        out.text = Some(text.chars().skip(skip).collect());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const RULE: &str = "────────────────────────────────────────────────────────────";

    #[test]
    fn normal_input_box() {
        let s = format!(" ▐▛███▛█   Claude Code v2.1.302\n\n● 好的\n\n{RULE}\n❯ 你好\n{RULE}\n  ⏵⏵ auto mode on (shift+tab to cycle)\n\n\n");
        assert_eq!(classify(&s), Screen::Input { text: "你好".into(), busy: false });
        let s = format!("✽ Baking… (6s)\n{RULE}\n❯ Press up to edit queued messages\n{RULE}\n  ⏵⏵ auto mode on (shift+tab to cycle) · esc to interrupt\n");
        assert!(matches!(classify(&s), Screen::Input { busy: true, .. }));
        let s = format!("{RULE}\n❯ [Pasted text #1 +1 lines]\n{RULE}\n  paste again to expand\n");
        assert!(matches!(classify(&s), Screen::Input { .. }));
        // 多行输入
        let s = format!("{RULE}\n❯ 第一行\n  第二行\n{RULE}\n  ⏸ manual mode on\n");
        assert_eq!(classify(&s), Screen::Input { text: "第一行\n第二行".into(), busy: false });
    }

    #[test]
    fn dim_suggestion_is_not_input() {
        let s = format!("{RULE}\n❯ \x1b[2mTry \"write a test for <filepath>\"\x1b[0m\n{RULE}\n  ? for shortcuts\n");
        assert_eq!(classify(&s), Screen::Input { text: String::new(), busy: false });
    }

    /// 实际抓到的屏幕（`capture-pane -p -e`，Claude Code 2.1.302）
    #[test]
    fn real_captures() {
        assert_eq!(classify(include_str!("testdata/idle.txt")), Screen::Input { text: String::new(), busy: false });
        assert_eq!(classify(include_str!("testdata/model.txt")), Screen::Modal);
        // 刚启动、还没画出来
        assert_eq!(classify("\n\n\n"), Screen::Modal);
    }

    #[test]
    fn live_preview_from_real_frames() {
        // 思考中：只有状态行
        let l = live(include_str!("testdata/stream6.txt"));
        assert_eq!(l.activity.as_deref(), Some("Symbioting… (6s · ↓ 274 tokens · thinking)"));
        // 正在输出：从 `● ` 开始的整段，折行接回
        let l = live(include_str!("testdata/stream9.txt"));
        let text = l.text.unwrap();
        assert!(text.starts_with("ls 的输出："), "{text}");
        assert!(text.contains("Python 生成器简介") && text.contains("yield"));
        assert!(!text.contains("tmux detected"));
        // 结束了：没有状态行
        assert_eq!(live(include_str!("testdata/stream12.txt")).activity, None);
        assert_eq!(live(include_str!("testdata/idle.txt")), Live::default());
    }

    #[test]
    fn live_preview_skips_tool_calls() {
        let s = format!("● Bash(ls)\n  ⎿  calc.py\n\n✶ Running… (3s)\n{RULE}\n❯ \n{RULE}\n  ⏸ manual mode on · esc to interrupt\n");
        assert_eq!(live(&s), Live { activity: Some("Running… (3s)".into()), text: None });
        let s = format!("● 第一段很长的回复\n  第二行\n\n{RULE}\n❯ \n{RULE}\n  ? for shortcuts\n");
        assert_eq!(live(&s).text.as_deref(), Some("第一段很长的回复\n第二行"));
    }

    #[test]
    fn dialogs_are_modal() {
        let permission = format!("● Creating empty file\n  ⎿  $ touch x\n{RULE}\n Bash command\n\n touch x\n\n Do you want to proceed?\n ❯ 1. Yes\n   2. Yes, and always allow\n   3. No\n\n Esc to cancel · Tab to amend\n");
        assert_eq!(classify(&permission), Screen::Modal);
        let ask = format!("{RULE}\n ☐ 颜色\n你喜欢哪种颜色？\n❯ 1. 红色\n  2. 蓝色\n  4. Type something.\n{RULE}\n  5. Chat about this\nEnter to select · ↑/↓ to navigate · Esc to cancel\n");
        assert_eq!(classify(&ask), Screen::Modal);
        let plan = format!("{RULE}\n   Claude has written up a plan and is ready to execute. Would you like to proceed?\n   ❯ 1. Yes, and use auto mode\n     2. Yes, manually approve edits\n     3. Tell Claude what to change\n");
        assert_eq!(classify(&plan), Screen::Modal);
        let model = "▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔\n   Select model\n     1.  Default (recommended)\n   ❯ 5.  Haiku 5.5 ✔\n\n   Enter to set as default · s to use this session only · Esc to cancel\n";
        assert_eq!(classify(model), Screen::Modal);
        let trust = format!("{RULE}\n Accessing workspace:\n\n /tmp/proj\n\n ❯ No, exit\n   Yes, I trust this folder\n\n Enter to confirm · Esc to cancel\n");
        assert_eq!(classify(&trust), Screen::Modal);
        let resume = "   Resume session\n   ╭────────────────────────────────╮\n   │ ⌕ Search…                      │\n   ╰────────────────────────────────╯\n   ❯ Bash ls 目录文件数量\n     54 seconds ago · HEAD · 704.2KB\n\n     Ctrl+A to show all projects\n";
        assert_eq!(classify(resume), Screen::Modal);
        assert_eq!(classify("root@host:/tmp/proj# "), Screen::Modal);
    }
}
