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
        /// 正在处理：状态行里有“esc to interrupt”，或输入框上面有转圈的状态行（窄屏时没有“esc to interrupt”）
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
/// 光标（反色的一个字）压在推荐提示的第一个字上时（2.1.310：`❯ ␛[7mm␛[0;2make median…`），这个字跟着后面算暗淡。
pub(super) fn strip(line: &str) -> (String, String) {
    let mut plain = String::new();
    let mut bright = String::new();
    let (mut dim, mut reverse) = (false, false);
    // 反色的字先放着，看后面紧跟的字是不是暗淡的再决定
    let mut held: Option<char> = None;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            if chars.peek() == Some(&'[') {
                chars.next();
                let mut params = String::new();
                for p in chars.by_ref() {
                    if p.is_ascii_alphabetic() {
                        if p == 'm' {
                            let codes: Vec<&str> = params.split(';').collect();
                            let mut i = 0;
                            while i < codes.len() {
                                match codes[i] {
                                    "2" => dim = true,
                                    "7" => reverse = true,
                                    "27" => reverse = false,
                                    "0" | "" => (dim, reverse) = (false, false),
                                    "22" => dim = false,
                                    // 颜色：`38;5;n`、`38;2;r;g;b` 后面的数字不是属性（不然 `38;2;…` 里的 2 会被当成暗淡）
                                    "38" | "48" | "58" => i += if codes.get(i + 1) == Some(&"2") { 4 } else { 2 },
                                    _ => {}
                                }
                                i += 1;
                            }
                        }
                        break;
                    }
                    params.push(p);
                }
            }
            continue;
        }
        if let Some(h) = held.take() {
            plain.push(h);
            if !dim {
                bright.push(h);
            }
        }
        if reverse && !dim {
            held = Some(c);
            continue;
        }
        plain.push(c);
        if !dim {
            bright.push(c);
        }
    }
    if let Some(h) = held {
        plain.push(h);
        bright.push(h);
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
    let busy = lines[bottom + 1..].iter().any(|(p, _)| p.contains("esc to interrupt")) || live(capture).activity.is_some();
    Screen::Input { text: text.trim().to_string(), busy }
}

/// 输入框空着时灰色的占位文字不是推荐回复：`Try "…"` 是 Claude Code 的示例提示；
/// 推荐回复生成不出来时它也会把这句话灰着显示出来（testdata/bg-shell.txt），不能当成建议给用户点。
const NOT_SUGGESTIONS: &[&str] = &["Try \"", "I have no obvious next step"];

/// Claude Code 跑完一轮后在空输入框里灰着显示的推荐回复（终端里按 Tab / → 就能用）。
/// 只认空闲、用户什么都没打时 ❯ 后面整段暗淡的文字（折行的续行也是暗淡的）；示例提示不算。
pub fn suggestion(capture: &str) -> Option<String> {
    if classify(capture) != (Screen::Input { text: String::new(), busy: false }) {
        return None;
    }
    let lines: Vec<(String, String)> = capture.lines().map(strip).collect();
    let bottom = lines.iter().rposition(|(p, _)| is_rule(p))?;
    let top = lines[..bottom].iter().rposition(|(p, _)| is_rule(p))?;
    let mut text = String::new();
    for (n, (plain, bright)) in lines[top + 1..bottom].iter().enumerate() {
        let plain = if n == 0 { plain.trim_start().strip_prefix('❯').or_else(|| plain.trim_start().strip_prefix('>')).unwrap_or(plain) } else { plain };
        let bright = bright.trim().trim_start_matches(['❯', '>']).trim();
        // 有不暗淡的字：不是推荐回复
        if !bright.is_empty() {
            return None;
        }
        let t = plain.trim();
        if !t.is_empty() {
            if !text.is_empty() {
                text.push(' ');
            }
            text.push_str(t);
        }
    }
    let text = text.trim().to_string();
    (!text.is_empty() && !NOT_SUGGESTIONS.iter().any(|p| text.starts_with(p))).then_some(text)
}

/// 输入框上下的状态信息：权限模式、思考强度、后台任务个数（`⏸ manual mode on · 1 shell · ↓ to manage`）。
/// hooks 只在下一个事件时才带权限模式，后台任务个数更是只有屏幕上有。
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct Footer {
    /// 和 hook 的 `permission_mode` 同样的写法
    pub mode: Option<String>,
    pub effort: Option<String>,
    /// 还在运行的后台任务：`[(个数, 种类)]`，种类是单数（shell、subagent、monitor……）
    pub background: Vec<(u32, String)>,
}

/// 后台任务的种类（`1 shell`、`2 subagents`）：单数形式
const TASK_KINDS: &[&str] = &["shell", "subagent", "agent", "monitor", "workflow", "teammate", "task", "background task", "mcp task", "cloud session"];
pub const EFFORTS: &[&str] = &["low", "medium", "high", "xhigh", "max"];

/// 普通输入框时的状态信息；不是普通输入框时为 None。
pub fn footer(capture: &str) -> Option<Footer> {
    if !matches!(classify(capture), Screen::Input { .. }) {
        return None;
    }
    let lines: Vec<String> = capture.lines().map(|l| strip(l).0.trim_end().to_string()).collect();
    let bottom = lines.iter().rposition(|l| is_rule(l))?;
    let top = lines[..bottom].iter().rposition(|l| is_rule(l))?;
    let mut out = Footer::default();
    for seg in lines[bottom + 1..].iter().flat_map(|l| l.split(" · ")) {
        let s = seg.trim().trim_start_matches(|c: char| !c.is_ascii_alphanumeric()).trim();
        let low = s.to_ascii_lowercase();
        if out.mode.is_none() {
            out.mode = mode_label(&low).map(String::from);
        }
        if let Some((n, kind)) = low.split_once(' ') {
            if let Ok(n) = n.parse::<u32>() {
                let one = if n > 1 { kind.strip_suffix('s').unwrap_or(kind) } else { kind };
                if TASK_KINDS.contains(&one) {
                    out.background.push((n, one.to_string()));
                }
            }
        }
    }
    // 默认模式：新版本显示“manual mode on”，老版本什么也不显示
    out.mode.get_or_insert_with(|| "default".into());
    // 输入框上沿上面一行右边：`◐ medium · /effort`
    if let Some(l) = top.checked_sub(1).map(|i| &lines[i]).filter(|l| l.contains("/effort")) {
        let head = l.split(" · /effort").next().unwrap_or("");
        out.effort = head.split_whitespace().last().filter(|w| EFFORTS.contains(w)).map(String::from);
    }
    Some(out)
}

/// 状态栏上的权限模式（`⏵⏵ accept edits on (shift+tab to cycle)`）
fn mode_label(s: &str) -> Option<&'static str> {
    let s = s.split(" (").next().unwrap_or(s);
    if !s.ends_with(" on") {
        return None;
    }
    Some(match s.trim_end_matches(" on").trim() {
        "manual mode" | "default mode" => "default",
        "accept edits" | "auto-accept edits" => "acceptEdits",
        "plan mode" => "plan",
        "auto mode" => "auto",
        "bypass permissions" | "bypass permissions mode" => "bypassPermissions",
        "don't ask mode" | "dont ask mode" => "dontAsk",
        _ => return None,
    })
}

/// 后台任务列表（输入框里按 ↓ 再按回车打开）：每行一个任务，`❯` 是选中的那个。
/// 返回 (各行的说明, 选中的序号)；不是这个列表时为 None。
pub fn task_list(capture: &str) -> Option<(Vec<String>, usize)> {
    let lines: Vec<String> = capture.lines().map(|l| strip(l).0.trim_end().to_string()).collect();
    if !lines.iter().any(|l| l.contains("x to stop") && l.contains("to select")) {
        return None;
    }
    let mut rows = vec![];
    let mut selected = None;
    for l in &lines {
        let t = l.trim_start();
        let (sel, rest) = match t.strip_prefix('❯') {
            Some(r) => (true, r.trim_start()),
            None => (false, t),
        };
        let Some(body) = rest.strip_prefix("● ").or_else(|| rest.strip_prefix("◯ ")).or_else(|| rest.strip_prefix("○ ")) else { continue };
        if sel {
            selected = Some(rows.len());
        }
        rows.push(body.trim().to_string());
    }
    Some((rows, selected?))
}

/// 单个后台任务的详情（只有一个任务时直接打开这个）：`Command:` / `Description:` 那一行的内容。
/// 有输出时详情比屏幕高，最下面的按键提示（`x to stop`）看不到，所以按标题（`Shell details`）和 `Status:` 认。
pub fn task_details(capture: &str) -> Option<String> {
    let lines: Vec<String> = capture.lines().map(|l| strip(l).0.trim().to_string()).collect();
    let title = lines.iter().position(|l| l.ends_with(" details") && l.split_whitespace().count() <= 3)?;
    if !lines[title..].iter().any(|l| l.starts_with("Status:")) {
        return None;
    }
    lines.iter().find_map(|l| l.strip_prefix("Command:").or_else(|| l.strip_prefix("Description:")).or_else(|| l.strip_prefix("Prompt:")).map(|s| s.trim().to_string()))
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
    // 状态行下面可能挂着 `  ⎿  Tip: …` 提示（窄屏时折成好几行缩进的续行）：跳过它们找到状态行
    let mut j = top;
    while j > 0 && top - j < 12 && (lines[j - 1].trim().is_empty() || lines[j - 1].starts_with("  ")) {
        j -= 1;
    }
    if j > 0 && j < top {
        let t = lines[j - 1].as_str();
        // `✻ Manifesting… (12s)`：第一个词以 … 结尾；`⏺ Bash(ls …)` 下面的 `⎿` 是工具输出，不算
        let verb = t.trim_start_matches(SPINNERS).split_whitespace().next().is_some_and(|w| w.ends_with('…'));
        if !t.starts_with('⏺') && t.starts_with(SPINNERS) && verb && lines[j..top].iter().any(|l| l.trim_start().starts_with('⎿')) {
            out.activity = Some(t.trim_start_matches(SPINNERS).trim().chars().take(200).collect());
            i = j - 1;
        }
    }
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

// ---------------- 选择框 ----------------

/// TUI 上的选择框（信任文件夹、切换模型的确认、没被我们接住的权限确认……），读出来在界面上显示成原生的弹窗。
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct Dialog {
    pub title: String,
    pub body: Vec<String>,
    pub options: Vec<DialogOption>,
    /// 选项光标（`❯`）现在在第几项
    pub selected: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DialogOption {
    pub label: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub hint: String,
    /// 带着 ✔：当前的设置
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub current: bool,
}

/// 额度用完时 Claude Code 的菜单（`/rate-limit-options`，标题 “What do you want to do?”，2.1.310 实测）。
/// 关掉了 `autoContinueAtUsageLimit` 时才有“等重置后自动继续”这一项：`… at 3pm`、`… shortly`、`… when the limit resets`；
/// 开着时（默认）不弹菜单，直接等，屏幕上是 [`limit_wait`] 那一行
pub const LIMIT_AUTO: &str = "Wait here, then continue automatically";
const LIMIT_STOP: &str = "Stop and wait for limit to reset";

impl Dialog {
    /// 是额度用完时的菜单
    pub fn is_limit(&self) -> bool {
        self.options.iter().any(|o| o.label.starts_with(LIMIT_AUTO) || o.label == LIMIT_STOP)
    }

    /// 额度用完的菜单里“等重置后自动继续”在第几项
    pub fn limit_auto(&self) -> Option<usize> {
        self.options.iter().position(|o| o.label.starts_with(LIMIT_AUTO))
    }
}

/// 已经在等额度重置、到点自动继续：屏幕上的 `Usage limit reached · continuing automatically at 3pm · esc to cancel`
/// （再次用完时是 `Usage limit reached again · …`，等待中是 `Continuing automatically …`），返回中间的 `at 3pm`
pub fn limit_wait(capture: &str) -> Option<String> {
    const KEY: &str = "ontinuing automatically ";
    capture.lines().rev().map(|l| strip(l).0).find_map(|t| {
        let i = t.find(KEY)?;
        if !t.contains("esc to cancel") {
            return None;
        }
        let when = t[i + KEY.len()..].split('·').next()?.trim();
        (!when.is_empty()).then(|| when.to_string())
    })
}

/// 选项前面的编号（`1. `、`12.  `）：返回编号后面的文字
fn numbered(t: &str) -> Option<&str> {
    let (n, rest) = t.split_once('.')?;
    (!n.is_empty() && n.len() <= 2 && n.chars().all(|c| c.is_ascii_digit()) && rest.starts_with(' ')).then(|| rest.trim_start())
}

/// 行首的光标、滚动箭头去掉
fn unmark(t: &str, cursor: char) -> (bool, &str) {
    let t = t.trim_start();
    if let Some(r) = t.strip_prefix(cursor) {
        return (true, r.trim_start());
    }
    (false, t.trim_start_matches(['↑', '↓', ' ']))
}

/// 两个以上空格分开的是选项名和说明
fn split_hint(t: &str) -> (String, String) {
    match t.find("  ") {
        Some(i) => (t[..i].trim().to_string(), t[i..].trim().trim_start_matches('·').trim().to_string()),
        None => (t.trim().to_string(), String::new()),
    }
}

fn is_frame(t: &str) -> bool {
    let frame = |c: char| matches!(c, '▔' | '▁' | '─' | '━' | '═' | '╭' | '╮' | '╰' | '╯' | '│');
    let n = t.chars().filter(|c| frame(*c)).count();
    let all = t.chars().filter(|c| !c.is_whitespace()).count();
    all > 0 && (n == all || (n >= 20 && n * 10 >= all * 8))
}

/// 读屏幕上的选择框；没有 `❯` 光标（读不出选项）时为 None，界面只给“取消”。
pub fn dialog(capture: &str) -> Option<Dialog> {
    dialog_with(capture, '❯')
}

/// 同 [`dialog`]，选项光标是 `cursor`（Codex 用 `›`）
pub fn dialog_with(capture: &str, cursor: char) -> Option<Dialog> {
    let lines: Vec<String> = capture.lines().map(|l| strip(l).0.trim_end().to_string()).collect();
    let sel = lines.iter().rposition(|l| l.trim_start().starts_with(cursor))?;
    let (_, sel_text) = unmark(&lines[sel], cursor);
    let is_num = numbered(sel_text).is_some();
    // 输入框（光标行上面紧挨着横线、下面几行内又是横线）不是选择框：重绘到一半被认成“别的界面”时，
    // 输入框里的推荐回复会被读成只有一项的选择框，弹出来又马上消失
    let in_box = sel > 0 && is_rule(&lines[sel - 1]) && lines[sel + 1..].iter().take(21).find(|l| !l.trim().is_empty() && !l.starts_with(' ')).is_some_and(|l| is_rule(l));
    if in_box && !is_num {
        return None;
    }
    // 选项：编号的就找所有带编号的行；不带编号的找和光标那一行文字对齐（缩进相同）、连在一起的行
    let text_col = lines[sel].chars().count() - sel_text.chars().count();
    let is_opt = |l: &str| -> bool {
        let (on, t) = unmark(l, cursor);
        if t.is_empty() {
            return false;
        }
        if is_num {
            numbered(t).is_some()
        } else {
            on || l.chars().take_while(|c| *c == ' ').count() == text_col
        }
    };
    let mut first = sel;
    while first > 0 && (is_opt(&lines[first - 1]) || (is_num && lines[first - 1].trim().is_empty() && first >= 2 && is_opt(&lines[first - 2]))) {
        first -= 1;
    }
    let mut last = sel;
    while last + 1 < lines.len() && (is_opt(&lines[last + 1]) || (is_num && lines[last + 1].trim().is_empty() && last + 2 < lines.len() && is_opt(&lines[last + 2]))) {
        last += 1;
    }
    let mut out = Dialog::default();
    for (i, l) in lines.iter().enumerate().take(last + 1).skip(first) {
        if !is_opt(l) {
            continue;
        }
        if i == sel {
            out.selected = out.options.len();
        }
        let (_, t) = unmark(l, cursor);
        let t = if is_num { numbered(t).unwrap_or(t) } else { t };
        let (mut label, hint) = split_hint(t);
        let current = label.ends_with('✔');
        if current {
            label = label.trim_end_matches('✔').trim_end().to_string();
        }
        out.options.push(DialogOption { label, hint, current });
    }
    // 只有一项的不是选择框（真的选择框至少有两项可选）
    if out.options.len() < 2 || out.options.len() > 30 {
        return None;
    }
    // 选项上面到最近的横线（最多 16 行）：第一行是标题，其余是说明
    let top = lines[..first].iter().rposition(|l| is_frame(l)).map_or(first.saturating_sub(16), |t| t + 1).max(first.saturating_sub(16));
    let mut text: Vec<String> = lines[top..first].iter().map(|l| l.trim().to_string()).collect();
    while text.first().is_some_and(String::is_empty) {
        text.remove(0);
    }
    while text.last().is_some_and(String::is_empty) {
        text.pop();
    }
    let mut text = text.into_iter();
    out.title = text.next().unwrap_or_default().trim_start_matches(['☐', '☒', '✔', ' ']).to_string();
    out.body = text.collect();
    // 连续的空行只留一个
    out.body.dedup_by(|a, b| a.is_empty() && b.is_empty());
    Some(out)
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

    #[test]
    fn suggestions() {
        let s = format!("● 改好了\n\n{RULE}\n❯ \x1b[2mcommit this\x1b[0m\n{RULE}\n  ? for shortcuts\n");
        assert_eq!(suggestion(&s).as_deref(), Some("commit this"));
        // 折行的推荐回复接成一行
        let s = format!("{RULE}\n❯ \x1b[2mrun the tests and fix\x1b[0m\n  \x1b[2many failures\x1b[0m\n{RULE}\n  ? for shortcuts\n");
        assert_eq!(suggestion(&s).as_deref(), Some("run the tests and fix any failures"));
        // 用户打了字、在运行、示例提示、空着：都没有
        let s = format!("{RULE}\n❯ 你好\n{RULE}\n  ? for shortcuts\n");
        assert_eq!(suggestion(&s), None);
        let s = format!("{RULE}\n❯ \x1b[2mcommit this\x1b[0m\n{RULE}\n  ⏵⏵ auto mode on · esc to interrupt\n");
        assert_eq!(suggestion(&s), None);
        assert_eq!(suggestion(include_str!("testdata/idle.txt")), None);
        assert_eq!(suggestion(include_str!("testdata/bg-shell.txt")), None);
        assert_eq!(suggestion(include_str!("testdata/stream12.txt")), None);
        assert_eq!(suggestion(include_str!("testdata/model.txt")), None);
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
        // Claude Code 2.1.3xx 手机上（42 列）：状态行下面挂着折行的 Tip，状态栏里没有“esc to interrupt”
        let s = include_str!("testdata/busy-tip.txt");
        assert_eq!(live(s).activity.as_deref(), Some("Manifesting… (thought for 1s)"));
        assert!(matches!(classify(s), Screen::Input { busy: true, .. }));
        let s = format!("⏺ Bash(sleep 1 …)\n  ⎿  done\n\n{RULE}\n❯ \n{RULE}\n  ? for shortcuts\n");
        assert!(matches!(classify(&s), Screen::Input { busy: false, .. }));
    }

    #[test]
    fn live_preview_skips_tool_calls() {
        let s = format!("● Bash(ls)\n  ⎿  calc.py\n\n✶ Running… (3s)\n{RULE}\n❯ \n{RULE}\n  ⏸ manual mode on · esc to interrupt\n");
        assert_eq!(live(&s), Live { activity: Some("Running… (3s)".into()), text: None });
        let s = format!("● 第一段很长的回复\n  第二行\n\n{RULE}\n❯ \n{RULE}\n  ? for shortcuts\n");
        assert_eq!(live(&s).text.as_deref(), Some("第一段很长的回复\n第二行"));
    }

    #[test]
    fn footer_from_real_frames() {
        // Claude Code 2.1.303：后台有一个 shell
        let f = footer(include_str!("testdata/bg-shell.txt")).unwrap();
        assert_eq!(f.mode.as_deref(), Some("default"));
        assert_eq!(f.background, vec![(1, "shell".to_string())]);
        let f = footer(include_str!("testdata/idle.txt")).unwrap();
        assert_eq!((f.mode.as_deref(), f.effort.as_deref()), (Some("auto"), Some("medium")));
        assert!(f.background.is_empty());
        assert_eq!(footer(include_str!("testdata/model.txt")), None);
        assert_eq!(footer(include_str!("testdata/bg-details.txt")), None);
    }

    #[test]
    fn footer_variants() {
        let s = format!("{RULE}\n❯ \n{RULE}\n  ⏵⏵ accept edits on (shift+tab to cycle) · 2 shells · 1 subagent · ↓ to manage\n");
        let f = footer(&s).unwrap();
        assert_eq!(f.mode.as_deref(), Some("acceptEdits"));
        assert_eq!(f.background, vec![(2, "shell".into()), (1, "subagent".into())]);
        let s = format!("  ○ low · /effort\n{RULE}\n❯ \n{RULE}\n  ? for shortcuts\n");
        let f = footer(&s).unwrap();
        assert_eq!((f.mode.as_deref(), f.effort.as_deref()), (Some("default"), Some("low")));
        let s = format!("{RULE}\n❯ \n{RULE}\n  ⏸ plan mode on · esc to interrupt\n");
        assert_eq!(footer(&s).unwrap().mode.as_deref(), Some("plan"));
    }

    #[test]
    fn background_task_dialogs() {
        let list = format!("{RULE}\n  Background\n  2 active shells\n\n    ● sleep 501   running\n  ❯ ● sleep 500   running\n\n  ↑/↓ to select · Enter to view · x to stop · Esc to close\n");
        assert_eq!(task_list(&list), Some((vec!["sleep 501   running".into(), "sleep 500   running".into()], 1)));
        assert_eq!(classify(&list), Screen::Modal);
        assert_eq!(task_details(include_str!("testdata/bg-details.txt")).as_deref(), Some("sleep 501"));
        assert_eq!(task_list(include_str!("testdata/bg-details.txt")), None);
        assert_eq!(task_details(include_str!("testdata/idle.txt")), None);
        // 有输出时按键提示在屏幕外
        let tall = format!("{RULE}\n  Shell details\n\n  Status:   running\n  Runtime:  15s\n  Command:  for i in 1 2 3; do echo tick$i; done\n\n  Output:\n  ╭──╮\n  │ tick1 │\n");
        assert_eq!(task_details(&tall).as_deref(), Some("for i in 1 2 3; do echo tick$i; done"));
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

    #[test]
    fn resume_dialog_parsed() {
        // Claude Code 2.1.306 接着很久没动的长对话时
        let cap = "
 Resume this conversation?

 This conversation has been inactive for 13h 54m and is 433.3k tokens long. Resuming it will use about 8% of your
 5-hour usage limit.

 ❯ 1. Resume
   2. Start a new conversation  Claude can look this one up if you refer to it later

 Enter to confirm · Esc to cancel
";
        let d = dialog(cap).expect("dialog");
        assert_eq!(d.title, "Resume this conversation?");
        assert_eq!(d.options.len(), 2);
        assert_eq!(d.options[0].label, "Resume");
        assert_eq!(d.selected, 0);
    }

    /// 额度用完的菜单（2.1.310 实测，没用完时用 /rate-limit-options 打开的；用完时才多出“等重置后自动继续”）
    #[test]
    fn limit_menu() {
        let d = dialog(include_str!("testdata/limit-menu.txt")).unwrap();
        assert_eq!(d.title, "What do you want to do?");
        assert_eq!(d.options.iter().map(|o| o.label.as_str()).collect::<Vec<_>>(), ["Stop and wait for limit to reset", "Switch to usage credits", "Upgrade your plan"]);
        assert!(d.is_limit());
        assert_eq!(d.limit_auto(), None);
        let cap = format!("{RULE}\n  What do you want to do?\n  ❯ 1. Stop and wait for limit to reset\n    2. Wait here, then continue automatically at 3pm (Asia/Shanghai)\n    3. Upgrade your plan\n  Enter to confirm · Esc to cancel\n");
        let d = dialog(&cap).unwrap();
        assert_eq!(d.limit_auto(), Some(1));
        assert!(!dialog(&format!("{RULE}\n Bash command\n\n Do you want to proceed?\n ❯ 1. Yes\n   2. No\n")).unwrap().is_limit());
    }

    #[test]
    fn limit_wait_line() {
        let cap = format!("● 我先跑一下测试\n\n  Usage limit reached · continuing automatically at 3pm (Asia/Shanghai) · esc to cancel\n{RULE}\n❯ \n{RULE}\n");
        assert_eq!(limit_wait(&cap).as_deref(), Some("at 3pm (Asia/Shanghai)"));
        assert_eq!(limit_wait("  Continuing automatically when your usage limit resets · esc to cancel\n").as_deref(), Some("when your usage limit resets"));
        // 对话内容里提到这句话不算（没有 esc to cancel）
        assert_eq!(limit_wait("● Claude Code shows continuing automatically at 3pm in the footer\n"), None);
    }

    /// 2.1.310：光标压在推荐回复的第一个字上（反色、不暗淡），整段仍是推荐回复；输入框不能被读成选择框
    #[test]
    fn suggestion_under_cursor() {
        let c = include_str!("testdata/suggestion-cursor.txt");
        assert_eq!(classify(c), Screen::Input { text: String::new(), busy: false });
        assert_eq!(suggestion(c).as_deref(), Some("make median raise ValueError on empty like mean"));
        assert_eq!(dialog(c), None);
        // 光标在用户自己打的字中间：反色的那个字是用户的
        let typed = format!("{RULE}\n❯ ab\x1b[7mc\x1b[0mde\n{RULE}\n");
        assert_eq!(classify(&typed), Screen::Input { text: "abcde".into(), busy: false });
        // 24 位色里的 2 不是“暗淡”
        assert_eq!(strip("\x1b[38;2;10;20;30mhi\x1b[0m").1, "hi");
    }

    #[test]
    fn dialogs_parsed() {
        let permission = format!("● Creating empty file\n  ⎿  $ touch x\n{RULE}\n Bash command\n\n   touch x\n   Create file\n\n Do you want to proceed?\n ❯ 1. Yes\n   2. Yes, and don't ask again for touch commands in /tmp\n   3. No\n\n Esc to cancel · Tab to amend\n");
        let d = dialog(&permission).unwrap();
        assert_eq!(d.title, "Bash command");
        assert_eq!(d.body, ["", "touch x", "Create file", "", "Do you want to proceed?"]);
        assert_eq!(d.options.iter().map(|o| o.label.as_str()).collect::<Vec<_>>(), ["Yes", "Yes, and don't ask again for touch commands in /tmp", "No"]);
        assert_eq!(d.selected, 0);

        let trust = format!("{RULE}\n Accessing workspace:\n\n /tmp/proj\n\n Quick safety check: Is this a project you created or one you trust?\n\n ❯ 1. Yes, I trust this folder\n   2. No, exit\n\n Enter to confirm · Esc to cancel\n");
        let d = dialog(&trust).unwrap();
        assert_eq!(d.title, "Accessing workspace:");
        assert_eq!(d.options.len(), 2);

        let unnumbered = format!("{RULE}\n Accessing workspace:\n\n /tmp/proj\n\n ❯ No, exit\n   Yes, I trust this folder\n\n Enter to confirm · Esc to cancel\n");
        let d = dialog(&unnumbered).unwrap();
        assert_eq!(d.options.iter().map(|o| o.label.as_str()).collect::<Vec<_>>(), ["No, exit", "Yes, I trust this folder"]);

        let switch = "   Switch model?\n   This conversation is cached for Opus 5.5. Switching will re-read it.\n\n   ❯ 1. Yes, switch to Sonnet 5.5\n     2. No, keep Opus 5.5\n";
        let d = dialog(switch).unwrap();
        assert_eq!(d.title, "Switch model?");
        assert_eq!(d.selected, 0);
    }

    #[test]
    fn model_picker_parsed() {
        let d = dialog(include_str!("testdata/model.txt")).unwrap();
        assert_eq!(d.title, "Select model");
        assert_eq!(d.options[0].label, "Default (recommended)");
        assert_eq!(d.options[0].hint, "Opus 5.5 · Best for everyday, complex tasks");
        assert_eq!(d.options[4].label, "Haiku 5.5");
        assert!(d.options[4].current);
        assert_eq!(d.selected, 4);
        assert_eq!(d.options.len(), 9);
    }


    #[test]
    fn real_dialogs_parsed() {
        let d = dialog(include_str!("testdata/trust.txt")).unwrap();
        assert_eq!(d.title, "Accessing workspace:");
        assert_eq!(d.options.iter().map(|o| o.label.as_str()).collect::<Vec<_>>(), ["No, exit", "Yes, I trust this folder"]);
        assert_eq!(d.selected, 0);
        assert_eq!(classify(include_str!("testdata/trust.txt")), Screen::Modal);
        let d = dialog(include_str!("testdata/plan.txt")).unwrap();
        assert_eq!(d.title, "Claude has written up a plan and is ready to execute. Would you like to proceed?");
        assert_eq!(d.options.iter().map(|o| o.label.as_str()).collect::<Vec<_>>(), ["Yes, and use auto mode", "Yes, manually approve edits", "Tell Claude what to change"]);
    }

}

