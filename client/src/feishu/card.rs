//! 飞书卡片（JSON 2.0，docs/TASK_BOARD.md §6.3、§6.4）。
//!
//! - 卡片都带 `update_multi: true`，之后能整张换掉（处理掉以后变灰、去掉按钮）；
//! - 会话里来的文字（命令、回复、问题）一律放 `plain_text` 或代码块，不当 markdown 解析，免得混进链接；
//! - 按钮回调的 `value` 见 [`Act`]，回到 `notify.rs` 处理。

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// 按钮上带的东西（回调原样带回来）
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Act {
    /// allow / deny / answer / dialog / say / test
    pub a: String,
    /// 会话所在的电脑
    #[serde(default)]
    pub dev: String,
    #[serde(default)]
    pub inst: String,
    /// 这张卡片对应的“等你处理”（`notify::attn_key`）
    #[serde(default)]
    pub p: String,
    /// 电脑发卡片时生成的随机数，处理时核对
    #[serde(default)]
    pub n: String,
    /// 选第几项
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub i: Option<usize>,
}

/// 卡片上共有的信息
pub struct Info<'a> {
    pub en: bool,
    /// Claude / Codex
    pub agent: &'a str,
    pub title: &'a str,
    pub project: &'a str,
    pub device: &'a str,
    /// “打开”的链接
    pub url: &'a str,
    /// 卡片里显示命令和回复内容（§6.5 内容开关）
    pub content: bool,
    /// 能在飞书里处理（关掉 = 只通知）
    pub actions: bool,
}

/// 中英文案
fn tr(en: bool, zh: &str, e: &str) -> String {
    if en { e.into() } else { zh.into() }
}

fn plain(s: &str) -> Value {
    json!({ "tag": "plain_text", "content": s })
}

fn div(s: &str) -> Value {
    json!({ "tag": "div", "text": plain(s) })
}

fn note(s: &str) -> Value {
    json!({ "tag": "div", "text": { "tag": "plain_text", "content": s, "text_size": "notation", "text_color": "grey" } })
}

/// 命令放代码块，最多 10 行
fn code(s: &str) -> Value {
    let lines: Vec<&str> = s.lines().take(10).collect();
    let mut body = lines.join("\n").replace("```", "'''");
    if s.lines().count() > 10 {
        body.push_str("\n…");
    }
    json!({ "tag": "markdown", "content": format!("```bash\n{body}\n```") })
}

fn button(text: &str, kind: &str, act: &Act, confirm: Option<(&str, &str)>) -> Value {
    let mut b = json!({ "tag": "button", "text": plain(text), "type": kind, "behaviors": [{ "type": "callback", "value": act }] });
    if let Some((title, body)) = confirm {
        b["confirm"] = json!({ "title": plain(title), "text": plain(body) });
    }
    b
}

fn link(text: &str, url: &str) -> Value {
    json!({ "tag": "button", "text": plain(text), "type": "default", "behaviors": [{ "type": "open_url", "default_url": url }] })
}

fn row(buttons: Vec<Value>) -> Value {
    let columns: Vec<Value> = buttons.into_iter().map(|b| json!({ "tag": "column", "width": "auto", "elements": [b] })).collect();
    json!({ "tag": "column_set", "flex_mode": "flow", "horizontal_spacing": "8px", "columns": columns })
}

/// 回复一句话的输入框（送进会话，§9 第 3 步）
fn say_form(i: &Info, act: &Act) -> Value {
    json!({
        "tag": "form",
        "name": "say",
        "elements": [
            { "tag": "input", "name": "text", "placeholder": plain(&tr(i.en, "回复一句话，发给 AI…", "Reply to the AI…")), "max_length": 1000, "width": "fill" },
            { "tag": "button", "name": "send", "text": plain(&tr(i.en, "发送", "Send")), "type": "primary", "form_action_type": "submit", "behaviors": [{ "type": "callback", "value": act }] },
        ],
    })
}

fn card(header: Value, summary: &str, elements: Vec<Value>) -> Value {
    json!({
        "schema": "2.0",
        "config": { "update_multi": true, "summary": { "content": summary } },
        "header": header,
        "body": { "elements": elements },
    })
}

fn header(title: &str, sub: &str, template: &str, tag: Option<(&str, &str)>) -> Value {
    let mut h = json!({ "title": plain(title), "subtitle": plain(sub), "template": template });
    if let Some((text, color)) = tag {
        h["text_tag_list"] = json!([{ "tag": "text_tag", "text": plain(text), "color": color }]);
    }
    h
}

fn subtitle(i: &Info) -> String {
    let mut s = String::new();
    if !i.title.is_empty() {
        s.push_str(i.title);
        s.push_str(" · ");
    }
    s.push_str(i.project);
    if !i.device.is_empty() {
        s.push_str(" · ");
        s.push_str(i.device);
    }
    s
}

fn open(i: &Info) -> Value {
    link(&tr(i.en, "打开", "Open"), i.url)
}

/// “等你处理”的卡片
pub struct Attn<'a> {
    /// permission / question / plan / limit / dialog
    pub kind: &'a str,
    pub tool: &'a str,
    pub detail: &'a str,
    pub risk: Option<&'a str>,
    pub options: &'a [String],
    /// 额度菜单里“等重置后自动继续”是第几项
    pub limit_wait: Option<usize>,
}

pub fn attn(i: &Info, a: &Attn, base: &Act) -> Value {
    let act = |name: &str, idx: Option<usize>| Act { a: name.into(), i: idx, ..base.clone() };
    let high = a.risk == Some("high");
    let (title, template) = match a.kind {
        "permission" => (tr(i.en, &format!("{} 在等你批准", i.agent), &format!("{} needs your approval", i.agent)), if high { "red" } else { "orange" }),
        "question" => (tr(i.en, &format!("{} 有个问题", i.agent), &format!("{} has a question", i.agent)), "orange"),
        "plan" => (tr(i.en, &format!("{} 写好了计划", i.agent), &format!("{} finished a plan", i.agent)), "orange"),
        "limit" => (tr(i.en, "额度用完了", "Usage limit reached"), "orange"),
        _ => (tr(i.en, &format!("{} 在等你选择", i.agent), &format!("{} is waiting for a choice", i.agent)), "orange"),
    };
    let tag = high.then_some((if i.en { "High risk" } else { "高风险" }, "red"));
    let mut els = Vec::new();
    let mut buttons = Vec::new();
    match a.kind {
        "permission" => {
            els.push(div(&tr(i.en, &format!("要用 {}：", a.tool), &format!("Wants to use {}:", a.tool))));
            if i.content && !a.detail.is_empty() {
                els.push(if a.tool == "Bash" { code(a.detail) } else { div(a.detail) });
            }
            if i.actions {
                let confirm = high.then_some((if i.en { "Allow a high-risk action?" } else { "允许高风险操作？" }, if i.en { "This runs on your computer right away." } else { "允许后会马上在电脑上执行。" }));
                buttons.push(button(&tr(i.en, "允许", "Allow"), if high { "danger" } else { "primary" }, &act("allow", None), confirm));
                buttons.push(button(&tr(i.en, "拒绝", "Deny"), "default", &act("deny", None), None));
            }
        }
        "question" => {
            if i.content && !a.detail.is_empty() {
                els.push(div(a.detail));
            }
            if i.actions {
                for (n, o) in a.options.iter().enumerate().take(4) {
                    buttons.push(button(o, "default", &act("answer", Some(n)), None));
                }
            }
        }
        "plan" => {
            if i.content && !a.detail.is_empty() {
                els.push(div(a.detail));
            }
            els.push(note(&tr(i.en, "计划要打开看完再批准。", "Open it to read the whole plan before approving.")));
        }
        "limit" => {
            els.push(div(&tr(i.en, "可以等额度重置后自动继续，或者打开换一个账号。", "Wait for the reset and continue automatically, or open it to switch accounts.")));
            if let (true, Some(n)) = (i.actions, a.limit_wait) {
                buttons.push(button(&tr(i.en, "等重置后继续", "Wait and continue"), "primary", &act("dialog", Some(n)), None));
            }
        }
        _ => {
            if !a.detail.is_empty() {
                els.push(div(a.detail));
            }
            if i.actions && a.options.len() <= 4 {
                for (n, o) in a.options.iter().enumerate() {
                    buttons.push(button(o, "default", &act("dialog", Some(n)), None));
                }
            }
        }
    }
    buttons.push(open(i));
    els.push(row(buttons));
    if i.actions && a.kind == "question" && !a.options.is_empty() {
        els.push(say_form(i, &act("say", None)));
    }
    card(header(&title, &subtitle(i), template, tag), &title, els)
}

/// 一轮结束：完成或出错
pub fn ended(i: &Info, failed: bool, text: &str, took_ms: i64, base: &Act) -> Value {
    let title = if failed { tr(i.en, "出错了", "Something went wrong") } else { tr(i.en, "做完了", "Done") };
    let mut els = Vec::new();
    if i.content && !text.is_empty() {
        let shown: String = if failed { text.lines().next().unwrap_or_default().into() } else { text.lines().take(5).collect::<Vec<_>>().join("\n") };
        els.push(div(&shown));
    }
    if !failed && took_ms > 0 {
        els.push(note(&tr(i.en, &format!("用时 {}", duration(took_ms, false)), &format!("Took {}", duration(took_ms, true)))));
    }
    els.push(row(vec![open(i)]));
    if i.actions {
        els.push(say_form(i, &Act { a: "say".into(), ..base.clone() }));
    }
    card(header(&title, &subtitle(i), if failed { "red" } else { "green" }, None), &format!("{title} · {}", i.title), els)
}

/// 处理掉了：灰色，去掉按钮，写明结果
pub fn resolved(i: &Info, result: &str) -> Value {
    card(header(result, &subtitle(i), "grey", None), result, vec![row(vec![open(i)])])
}

/// 回复已经送进会话
pub fn said(i: &Info, text: &str) -> Value {
    let title = tr(i.en, "已发给 AI", "Sent to the AI");
    let mut els = Vec::new();
    if i.content {
        els.push(div(text));
    }
    els.push(row(vec![open(i)]));
    card(header(&title, &subtitle(i), "grey", None), &title, els)
}

/// 设置页“发一条测试”：点按钮能收到回调才算配好（漏了 `card.action.trigger` 时会报 200340）
pub fn test(en: bool, act: &Act, ok: bool) -> Value {
    if ok {
        let title = tr(en, "看看：按钮回调已接通", "LookLook: button callbacks work");
        return card(header(&title, "", "green", None), &title, vec![div(&tr(en, "配置完成，之后的通知会发到这里。", "All set. Notifications will arrive here."))]);
    }
    let title = tr(en, "看看：测试通知", "LookLook: test notification");
    card(
        header(&title, "", "blue", None),
        &title,
        vec![
            div(&tr(en, "能看到这条，说明发消息没问题。再点一下下面的按钮，检查按钮回调。", "Sending works. Tap the button below to check button callbacks.")),
            note(&tr(en, "点了报错 200340：到开发者后台“事件与回调 → 回调配置”里加上“卡片回传交互”，再发布一个版本。", "Error 200340 when tapping: add “card.action.trigger” under Events & Callbacks → Callback configuration, then publish a new version.")),
            row(vec![button(&tr(en, "点一下", "Tap me"), "primary", act, None)]),
        ],
    )
}

/// 超出每分钟上限时合并成一条
pub fn digest(en: bool, n: usize, url: &str) -> Value {
    let title = tr(en, &format!("还有 {n} 条通知没发"), &format!("{n} more notifications"));
    card(
        header(&title, "", "orange", None),
        &title,
        vec![div(&tr(en, "一分钟里通知太多了，打开任务页看全部。", "Too many notifications in a minute. Open the Tasks page to see them all.")), row(vec![link(&tr(en, "打开任务", "Open Tasks"), url)])],
    )
}

/// 用时：12 秒、3 分、1 小时 5 分
pub fn duration(ms: i64, en: bool) -> String {
    let s = ms / 1000;
    match (s, en) {
        (s, false) if s < 60 => format!("{s} 秒"),
        (s, true) if s < 60 => format!("{s}s"),
        (s, false) if s < 3600 => format!("{} 分", s / 60),
        (s, true) if s < 3600 => format!("{}m", s / 60),
        (s, false) => format!("{} 小时 {} 分", s / 3600, s % 3600 / 60),
        (s, true) => format!("{}h {}m", s / 3600, s % 3600 / 60),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(actions: bool) -> Info<'static> {
        Info { en: false, agent: "Claude", title: "发布看板", project: "looklook", device: "工作站", url: "https://a.example/s/x", content: true, actions }
    }

    fn buttons(c: &Value) -> Vec<String> {
        let mut out = Vec::new();
        fn walk(v: &Value, out: &mut Vec<String>) {
            if v["tag"] == "button" {
                out.push(v["text"]["content"].as_str().unwrap_or_default().to_string());
            }
            match v {
                Value::Array(a) => a.iter().for_each(|x| walk(x, out)),
                Value::Object(m) => m.values().for_each(|x| walk(x, out)),
                _ => {}
            }
        }
        walk(c, &mut out);
        out
    }

    #[test]
    fn high_risk_permission() {
        let base = Act { dev: "d".into(), inst: "i".into(), p: "7".into(), n: "abc".into(), ..Default::default() };
        let a = Attn { kind: "permission", tool: "Bash", detail: "git push --force", risk: Some("high"), options: &[], limit_wait: None };
        let c = attn(&info(true), &a, &base);
        assert_eq!(c["schema"], "2.0");
        assert_eq!(c["config"]["update_multi"], true);
        assert_eq!(c["header"]["template"], "red");
        assert_eq!(c["header"]["text_tag_list"][0]["text"]["content"], "高风险");
        assert_eq!(buttons(&c), ["允许", "拒绝", "打开"]);
        let allow = &c["body"]["elements"][2]["columns"][0]["elements"][0];
        assert_eq!(allow["behaviors"][0]["value"]["a"], "allow");
        assert_eq!(allow["behaviors"][0]["value"]["n"], "abc");
        assert!(allow["confirm"].is_object());
        assert!(c["body"]["elements"][1]["content"].as_str().unwrap().contains("```bash\ngit push --force\n```"));
        // 只通知：只剩“打开”
        assert_eq!(buttons(&attn(&info(false), &a, &base)), ["打开"]);
    }

    #[test]
    fn question_and_content_switch() {
        let opts = vec!["SQLite".to_string(), "Postgres".to_string()];
        let a = Attn { kind: "question", tool: "AskUserQuestion", detail: "用哪个？", risk: None, options: &opts, limit_wait: None };
        let c = attn(&info(true), &a, &Act::default());
        assert_eq!(buttons(&c), ["SQLite", "Postgres", "打开", "发送"]);
        let hidden = Info { content: false, ..info(true) };
        assert!(!attn(&hidden, &a, &Act::default()).to_string().contains("用哪个"));
    }

    #[test]
    fn ended_and_duration() {
        let c = ended(&info(true), false, "已修好。\n第二行", 754_000, &Act::default());
        assert_eq!(c["header"]["template"], "green");
        assert!(c.to_string().contains("用时 12 分"));
        assert_eq!(duration(3_900_000, true), "1h 5m");
        assert_eq!(resolved(&info(true), "已允许 · 在飞书上")["header"]["template"], "grey");
    }
}
