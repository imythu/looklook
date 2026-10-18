//! 抓屏判断 Codex TUI 当前是不是“普通输入框”（CODEX_UI.md §7）。样本：`testdata/codex-<版本>-screen-*.txt`。
//!
//! ```text
//! • Working (6s • esc to interrupt)        ← 运行中的活动行
//!
//! › 输入框里的文字                          ← 最后一个 `› ` 开头的行（空着时是暗淡的 “Ask Codex to do anything”）
//!   续行缩进两格
//!                                           ← 空行
//!   GPT-6-Astra medium · ~\path            ← 状态行
//!   ? for shortcuts                         ← 提示行
//! ```
//!
//! 对话框（信任文件夹、审批、更新提示、`/model` 之类的选择器）底部是 `› 1. …` 这样的编号选项。
//! 注意：刚启动时输入框先画出来，信任文件夹的对话框要过几秒才弹出（实测），所以不能看到一次输入框就当作可以输入。

use super::screen::{strip, Live, Screen};

/// 只看最下面这么多行（再往上是对话历史，里面也可能有 `›`）
const BOTTOM: usize = 24;

/// `› 1. Trust and continue` 这样的选中项
fn is_option(plain: &str) -> bool {
    let rest = plain.trim_start().strip_prefix('›').map(str::trim_start).unwrap_or("");
    let digits = rest.chars().take_while(char::is_ascii_digit).count();
    digits > 0 && rest[digits..].starts_with('.')
}

pub fn classify(capture: &str) -> Screen {
    let lines: Vec<(String, String)> = capture.lines().map(strip).collect();
    let mut end = lines.len();
    while end > 0 && lines[end - 1].0.trim().is_empty() {
        end -= 1;
    }
    let start = end.saturating_sub(BOTTOM);
    let bottom = &lines[start..end];
    if bottom.iter().any(|(p, _)| is_option(p)) {
        return Screen::Modal;
    }
    // 最后一个 `› ` 开头的行是输入框（`/` 命令的候选也是 `›` 开头，但在输入框上面）
    let Some(i) = bottom.iter().rposition(|(p, _)| p.starts_with('›')) else { return Screen::Modal };
    let mut text = bottom[i].1.strip_prefix('›').unwrap_or("").trim_start().trim_end().to_string();
    for (plain, bright) in &bottom[i + 1..] {
        if plain.trim().is_empty() || !plain.starts_with("  ") {
            break;
        }
        let cont = bright.trim();
        if !cont.is_empty() {
            text.push('\n');
            text.push_str(cont);
        }
    }
    let busy = bottom.iter().any(|(p, _)| p.contains("esc to interrupt"));
    Screen::Input { text, busy }
}

/// 对话界面能直接回答的对话框。现在只有“信任这个文件夹”（每个新文件夹第一次启动时）：
/// `{ "kind": "trust_folder", "path": "<文件夹>" }`。其余对话框还是到终端里操作。
pub fn dialog(capture: &str) -> Option<serde_json::Value> {
    let plain: Vec<String> = capture.lines().map(|l| strip(l).0).collect();
    let q = plain.iter().rposition(|l| l.contains(TRUST_QUESTION))?;
    if !plain[q..].iter().any(|l| l.trim_start().trim_start_matches('›').trim_start().starts_with(TRUST_OPTION)) {
        return None;
    }
    // 文件夹在 “Folder access” 和问题之间，长路径会折行
    let start = plain[..q].iter().rposition(|l| l.trim() == "Folder access").map_or(q, |i| i + 1);
    let path: String = plain[start..q].iter().map(|l| l.trim()).collect();
    Some(serde_json::json!({ "kind": "trust_folder", "path": path }))
}

/// 回答对话框要按的键。先核对屏幕上还是这个对话框、选中的是预期的选项，对不上返回 None。
pub fn dialog_keys(capture: &str, kind: &str) -> Option<&'static [&'static str]> {
    match kind {
        // 选中的是 “› 1. Trust and continue” 时按回车
        "trust_folder" if dialog(capture).is_some() && capture.lines().map(|l| strip(l).0).any(|l| l.trim_start().strip_prefix('›').is_some_and(|r| r.trim_start().starts_with(TRUST_OPTION))) => Some(&["Enter"]),
        _ => None,
    }
}

const TRUST_QUESTION: &str = "Trust this folder?";
const TRUST_OPTION: &str = "1. Trust and continue";

/// 运行中的活动行，例如 `Working (6s • esc to interrupt)`。正在写的回复第一版不取（Codex 的历史区里分不清写完没写完）。
pub fn live(capture: &str) -> Live {
    let activity = capture
        .lines()
        .rev()
        .take(BOTTOM)
        .map(|l| strip(l).0)
        .find(|l| l.contains("esc to interrupt"))
        .map(|l| l.trim().trim_start_matches('•').trim().to_string());
    Live { activity, text: None }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(name: &str) -> String {
        std::fs::read_to_string(format!("{}/src/chat/testdata/codex-0.159.3-screen-{name}.txt", env!("CARGO_MANIFEST_DIR"))).unwrap()
    }

    #[test]
    fn real_captures() {
        assert_eq!(classify(&sample("idle")), Screen::Input { text: String::new(), busy: false }, "暗淡的占位提示不算输入");
        assert_eq!(
            classify(&sample("typed")),
            Screen::Input { text: "Run one shell command that creates s1.txt containing hi, requesting escalated permissions outside the read-only\nsandbox. Then reply OK.".into(), busy: false }
        );
        assert_eq!(classify(&sample("busy")), Screen::Input { text: String::new(), busy: true });
        assert_eq!(classify(&sample("busy2")), Screen::Input { text: String::new(), busy: true });
        assert_eq!(classify(&sample("done")), Screen::Input { text: String::new(), busy: false });
        assert_eq!(classify(&sample("slash")), Screen::Input { text: "/model".into(), busy: false }, "候选在上面，输入框是最后一个 ›");
        for modal in ["trust", "approval"] {
            assert_eq!(classify(&sample(modal)), Screen::Modal, "{modal}");
        }
    }

    #[test]
    fn trust_dialog() {
        let d = dialog(&sample("trust")).unwrap();
        assert_eq!(d["kind"], "trust_folder");
        assert_eq!(d["path"], r"C:\Users\alice\AppData\Local\Temp\claude\proj\work\work-1792318910612", "折行的路径接起来");
        assert_eq!(dialog_keys(&sample("trust"), "trust_folder"), Some(&["Enter"][..]));
        for other in ["approval", "idle", "busy"] {
            assert!(dialog(&sample(other)).is_none(), "{other}");
            assert!(dialog_keys(&sample(other), "trust_folder").is_none(), "{other}");
        }
        // 光标移到了 “2. Quit” 上：不按
        let moved = sample("trust").replace("› 1. Trust", "  1. Trust").replace("  2. Quit", "› 2. Quit");
        assert!(dialog_keys(&moved, "trust_folder").is_none());
    }

    #[test]
    fn activity_line() {
        assert_eq!(live(&sample("busy2")).activity.as_deref(), Some("Working (6s • esc to interrupt)"));
        assert_eq!(live(&sample("done")), Live::default());
    }

    #[test]
    fn options_and_shells() {
        assert!(is_option("› 1. Yes, proceed (y)"));
        assert!(is_option("  ›  12. Something"));
        assert!(!is_option("› 1st thing"));
        assert!(!is_option("› Ask Codex to do anything"));
        assert_eq!(classify("PS C:\\> "), Screen::Modal, "不是 Codex 的界面");
    }
}
