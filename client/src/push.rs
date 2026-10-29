//! 只通知的渠道（docs/PLAN_2026-10-28.md §1）：企业微信群机器人、钉钉群机器人、Server酱、PushPlus、Bark、ntfy。
//!
//! - 和飞书发同样的事（等你处理、做完、出错、卡住、汇总），什么时候发由 `notify.rs` 决定，这里只管把一条消息发出去；
//! - 不能在消息里处理，只带一个“打开”的链接；
//! - 配置在同步项 `notify/hooks`（一个数组），地址和密钥由你的电脑直接发给对应的服务，不经过看看的服务器；
//! - 会话里来的文字（标题、命令、回复）一律转义，不当 markdown 解析，免得混进链接。

use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use hmac::{Hmac, Mac};
use reqwest::Url;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::Sha256;

use crate::feishu::card::{duration, Attn, Info, Line};

/// 最多配几个
pub const MAX_HOOKS: usize = 10;
/// 汇总消息最多列几行
const SUMMARY_MAX: usize = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// 企业微信群机器人：Webhook 地址
    Wecom,
    /// 钉钉群机器人：Webhook 地址，可选加签密钥
    Dingtalk,
    /// Server酱（推到微信）：SendKey
    Serverchan,
    /// PushPlus（推到微信）：token
    Pushplus,
    /// Bark（iPhone）：设备地址，比如 https://api.day.app/xxxx
    Bark,
    /// ntfy：主题地址，比如 https://ntfy.sh/my-topic，可选访问令牌
    Ntfy,
}

/// 一个渠道（同步项 `notify/hooks` 里的一项）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Hook {
    pub id: String,
    pub kind: Kind,
    /// 地址或 key
    pub target: String,
    /// 钉钉加签的密钥、ntfy 的访问令牌
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub secret: String,
    #[serde(default)]
    pub name: String,
}

fn key_ok(s: &str) -> bool {
    (8..=128).contains(&s.len()) && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn http_url(s: &str) -> Option<Url> {
    Url::parse(s).ok().filter(|u| matches!(u.scheme(), "https" | "http") && u.host_str().is_some())
}

impl Hook {
    /// 地址 / key 的格式对不对（保存前检查）
    pub fn validate(&self) -> Result<(), &'static str> {
        let t = self.target.as_str();
        let ok = match self.kind {
            Kind::Wecom => t.starts_with("https://qyapi.weixin.qq.com/cgi-bin/webhook/send?key=") && http_url(t).is_some(),
            Kind::Dingtalk => t.starts_with("https://oapi.dingtalk.com/robot/send?access_token=") && http_url(t).is_some(),
            Kind::Serverchan | Kind::Pushplus => key_ok(t),
            // Bark、ntfy 可以自己部署，局域网里也可能是 http
            Kind::Bark => http_url(t).is_some_and(|u| u.path().len() > 1),
            Kind::Ntfy => http_url(t).is_some_and(|u| u.path_segments().and_then(|mut s| s.next_back()).is_some_and(|s| !s.is_empty())),
        };
        if !ok {
            return Err("target");
        }
        if self.secret.len() > 256 || self.name.chars().count() > 40 {
            return Err("secret");
        }
        Ok(())
    }

    /// 界面上显示的样子：不露出完整的地址和 key
    pub fn masked(&self) -> String {
        let t = &self.target;
        let tail: String = t.chars().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect();
        match self.kind {
            Kind::Serverchan | Kind::Pushplus => format!("{}…{tail}", t.chars().take(4).collect::<String>()),
            _ => match http_url(t) {
                Some(u) => format!("{}://{}/…{tail}", u.scheme(), u.host_str().unwrap_or_default()),
                None => format!("…{tail}"),
            },
        }
    }
}

/// 一条消息：标题 + 几段文字 + “打开”的链接。文字都是纯文本，发之前按渠道转义
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Msg {
    pub title: String,
    pub lines: Vec<String>,
    pub url: String,
    pub link: String,
}

fn tr(en: bool, zh: &str, e: &str) -> String {
    if en { e.into() } else { zh.into() }
}

/// 副标题：Claude · 项目 · 电脑
fn subtitle(i: &Info) -> String {
    let mut s = format!("{} · {}", i.title, i.project);
    if !i.device.is_empty() {
        s.push_str(&format!(" · {}", i.device));
    }
    s
}

fn first_lines(s: &str, n: usize) -> String {
    s.lines().take(n).collect::<Vec<_>>().join("\n").chars().take(600).collect()
}

/// 等你处理（飞书卡片 `card::attn` 的只通知版本）
pub fn attn(i: &Info, a: &Attn) -> Msg {
    let high = a.risk == Some("high") || a.guard.is_some();
    let mut title = match a.kind {
        "permission" => tr(i.en, &format!("{} 在等你批准", i.agent), &format!("{} needs your approval", i.agent)),
        "question" => tr(i.en, &format!("{} 有个问题", i.agent), &format!("{} has a question", i.agent)),
        "plan" => tr(i.en, &format!("{} 写好了计划", i.agent), &format!("{} finished a plan", i.agent)),
        "limit" => tr(i.en, "额度用完了", "Usage limit reached"),
        _ => tr(i.en, &format!("{} 在等你选择", i.agent), &format!("{} is waiting for a choice", i.agent)),
    };
    if high {
        title = tr(i.en, &format!("[高风险] {title}"), &format!("[High risk] {title}"));
    }
    let mut lines = vec![subtitle(i)];
    match a.kind {
        "permission" => {
            lines.push(tr(i.en, &format!("要用 {}", a.tool), &format!("Wants to use {}", a.tool)));
            if i.content && !a.detail.is_empty() {
                lines.push(first_lines(a.detail, 5));
            }
            if let Some(rule) = a.guard {
                lines.push(tr(i.en, &format!("命中了看看的命令规则：{rule}"), &format!("Matches a Looklook command rule: {rule}")));
            }
        }
        "limit" => lines.push(tr(i.en, "打开后可以等重置自动继续，或者换一个账号。", "Open it to wait for the reset or switch accounts.")),
        _ => {
            if i.content && !a.detail.is_empty() {
                lines.push(first_lines(a.detail, 5));
            }
            if i.content && !a.options.is_empty() {
                lines.push(a.options.iter().take(6).map(|o| format!("· {o}")).collect::<Vec<_>>().join("\n"));
            }
        }
    }
    Msg { title, lines, url: i.url.into(), link: tr(i.en, "打开处理", "Open to respond") }
}

/// 一轮结束
pub fn ended(i: &Info, failed: bool, text: &str, took_ms: i64) -> Msg {
    let title = if failed { tr(i.en, "出错了", "Something went wrong") } else { tr(i.en, "做完了", "Done") };
    let mut lines = vec![format!("{} · {}", i.agent, subtitle(i))];
    if i.content && !text.is_empty() {
        lines.push(if failed { first_lines(text, 1) } else { first_lines(text, 5) });
    }
    if !failed && took_ms > 0 {
        lines.push(tr(i.en, &format!("用时 {}", duration(took_ms, false)), &format!("Took {}", duration(took_ms, true))));
    }
    Msg { title, lines, url: i.url.into(), link: tr(i.en, "打开", "Open") }
}

/// 卡住提醒
pub fn stalled(i: &Info, doing: &str, idle_ms: i64) -> Msg {
    let mut lines = vec![
        format!("{} · {}", i.agent, subtitle(i)),
        tr(i.en, &format!("已经 {}没有新的进展。可能卡住了，也可能在跑很久的命令。", duration(idle_ms, false)), &format!("Nothing new for {}. It may be stuck, or running a long command.", duration(idle_ms, true))),
    ];
    if i.content && !doing.is_empty() {
        lines.push(tr(i.en, &format!("正在：{doing}"), &format!("Doing: {doing}")));
    }
    Msg { title: tr(i.en, "看起来没有动静了", "No progress for a while"), lines, url: i.url.into(), link: tr(i.en, "打开", "Open") }
}

/// 勿扰时段结束、或合并的时间到了
pub fn summary(en: bool, quiet: bool, ls: &[Line], content: bool, url: &str) -> Msg {
    let title = match (quiet, en) {
        (true, false) => format!("勿扰期间结束了 {} 个会话", ls.len()),
        (true, true) => format!("{} sessions finished during quiet hours", ls.len()),
        (false, false) => format!("{} 个会话做完了", ls.len()),
        (false, true) => format!("{} sessions finished", ls.len()),
    };
    let mut lines = Vec::new();
    for l in ls.iter().take(SUMMARY_MAX) {
        let mark = if l.failed { "✗" } else { "✓" };
        let mut s = format!("{mark} {} · {} · {}", l.agent, if l.title.is_empty() { &l.project } else { &l.title }, l.project);
        if !l.device.is_empty() {
            s.push_str(&format!(" · {}", l.device));
        }
        if content && !l.text.is_empty() {
            s.push_str(&format!("：{}", l.text.lines().next().unwrap_or_default().chars().take(80).collect::<String>()));
        }
        lines.push(s);
    }
    if ls.len() > SUMMARY_MAX {
        let more = ls.len() - SUMMARY_MAX;
        lines.push(tr(en, &format!("还有 {more} 个"), &format!("and {more} more")));
    }
    Msg { title, lines, url: url.into(), link: tr(en, "打开任务", "Open Tasks") }
}

/// 超出每分钟上限时合并成一条
pub fn digest(en: bool, n: usize, url: &str) -> Msg {
    Msg {
        title: tr(en, &format!("还有 {n} 条通知没发"), &format!("{n} more notifications")),
        lines: vec![tr(en, "一分钟里通知太多了，打开任务页看全部。", "Too many notifications in a minute. Open the Tasks page to see them all.")],
        url: url.into(),
        link: tr(en, "打开任务", "Open Tasks"),
    }
}

/// 设置页“发一条测试”
pub fn test(en: bool, url: &str) -> Msg {
    Msg {
        title: tr(en, "看看：测试通知", "Looklook: test notification"),
        lines: vec![tr(en, "能看到这条，说明配好了。AI 等你处理、做完、出错时会发到这里。", "It works. You'll get a message here when an AI agent needs you, finishes or fails.")],
        url: url.into(),
        link: tr(en, "打开看看", "Open Looklook"),
    }
}

/// 没登录的电脑：这台电脑的地址在手机上打不开，链接换成开通远程访问
pub fn remote_hint(mut m: Msg, en: bool, url: &str) -> Msg {
    m.lines.push(tr(
        en,
        "这台电脑还没开通远程访问，离开它就打不开看看。开通后出门在外用手机一点就能接着处理（Beta 期间注册送一年）。",
        "Remote access isn't on for this computer, so Looklook only opens on it. Turn it on to pick things up from your phone anywhere, free for a year if you sign up during the beta.",
    ));
    m.url = url.into();
    m.link = tr(en, "开通远程访问", "Turn on remote access");
    m
}

/// markdown 转义：会话里来的文字不能变成链接、标题、加粗
fn md_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '\\' | '`' | '*' | '_' | '[' | ']' | '(' | ')' | '<' | '>' | '#' | '!' | '|' | '~') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// 企业微信的 markdown 不认反斜杠转义：把能变成链接、标签的字符换成全角
fn wecom_escape(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '[' => '［',
            ']' => '］',
            '<' => '＜',
            '>' => '＞',
            '`' => '｀',
            '*' => '＊',
            c => c,
        })
        .collect()
}

/// 每段一行，段内换行保留
fn markdown(m: &Msg, esc: fn(&str) -> String, heading: bool) -> String {
    let mut s = String::new();
    if heading {
        s.push_str(&format!("**{}**\n\n", esc(&m.title)));
    }
    for l in &m.lines {
        // markdown 里单个换行会被合并：段内换行用两个空格 + 换行
        s.push_str(&esc(l).replace('\n', "  \n"));
        s.push_str("\n\n");
    }
    if !m.url.is_empty() {
        s.push_str(&format!("[{}]({})", esc(&m.link), m.url));
    }
    s
}

fn plain(m: &Msg) -> String {
    m.lines.join("\n")
}

/// 一个渠道的请求：地址、JSON、额外的请求头
type Request = (Url, Value, Vec<(&'static str, String)>);

fn request(h: &Hook, m: &Msg, now_ms: i64) -> Result<Request> {
    let bad = || anyhow!("地址不对");
    Ok(match h.kind {
        Kind::Wecom => {
            // 企业微信 markdown 最多 4096 字节
            let mut text = markdown(m, wecom_escape, true);
            truncate_bytes(&mut text, 4000);
            (http_url(&h.target).ok_or_else(bad)?, json!({ "msgtype": "markdown", "markdown": { "content": text } }), vec![])
        }
        Kind::Dingtalk => {
            let mut u = http_url(&h.target).ok_or_else(bad)?;
            if !h.secret.is_empty() {
                // 加签：HmacSHA256(secret, "timestamp\nsecret")，Base64 后放进地址
                let mut mac = Hmac::<Sha256>::new_from_slice(h.secret.as_bytes()).expect("HMAC 接受任意长度的密钥");
                mac.update(format!("{now_ms}\n{}", h.secret).as_bytes());
                let sign = STANDARD.encode(mac.finalize().into_bytes());
                u.query_pairs_mut().append_pair("timestamp", &now_ms.to_string()).append_pair("sign", &sign);
            }
            (u, json!({ "msgtype": "markdown", "markdown": { "title": m.title, "text": format!("#### {}\n\n{}", md_escape(&m.title), markdown(m, md_escape, false)) } }), vec![])
        }
        Kind::Serverchan => {
            let t = &h.target;
            // Server酱³ 的 key 是 sctp{uid}t…，地址不一样
            let url = match t.strip_prefix("sctp").and_then(|r| r.split_once('t')).filter(|(uid, _)| !uid.is_empty() && uid.chars().all(|c| c.is_ascii_digit())) {
                Some((uid, _)) => format!("https://{uid}.push.ft07.com/send/{t}.send"),
                None => format!("https://sctapi.ftqq.com/{t}.send"),
            };
            (Url::parse(&url)?, json!({ "title": m.title.chars().take(32).collect::<String>(), "desp": markdown(m, md_escape, false) }), vec![])
        }
        Kind::Pushplus => (Url::parse("https://www.pushplus.plus/send")?, json!({ "token": h.target, "title": m.title, "content": markdown(m, md_escape, false), "template": "markdown" }), vec![]),
        Kind::Bark => {
            let mut j = json!({ "title": m.title, "body": plain(m), "group": "Looklook" });
            if !m.url.is_empty() {
                j["url"] = json!(m.url);
            }
            (http_url(&h.target).ok_or_else(bad)?, j, vec![])
        }
        Kind::Ntfy => {
            // 发 JSON 到服务器根地址：标题能用中文（放在 Title 头里不行）
            let u = http_url(&h.target).ok_or_else(bad)?;
            let topic = u.path_segments().and_then(|mut s| s.next_back()).filter(|s| !s.is_empty()).ok_or_else(bad)?.to_string();
            let mut root = u.clone();
            root.set_query(None);
            root.path_segments_mut().map_err(|_| bad())?.pop();
            let mut j = json!({ "topic": topic, "title": m.title, "message": plain(m), "tags": ["robot"] });
            if !m.url.is_empty() {
                j["click"] = json!(m.url);
            }
            let auth = if h.secret.is_empty() { vec![] } else { vec![("Authorization", format!("Bearer {}", h.secret))] };
            (root, j, auth)
        }
    })
}

fn truncate_bytes(s: &mut String, max: usize) {
    if s.len() > max {
        let mut cut = max;
        while !s.is_char_boundary(cut) {
            cut -= 1;
        }
        s.truncate(cut);
    }
}

/// 服务返回的是不是成功（各家的格式不一样）
fn check_reply(kind: Kind, status: reqwest::StatusCode, body: &str) -> Result<()> {
    if !status.is_success() {
        bail!("HTTP {}：{}", status.as_u16(), body.chars().take(200).collect::<String>());
    }
    let v: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let (field, ok) = match kind {
        Kind::Wecom | Kind::Dingtalk => ("errcode", 0),
        Kind::Serverchan => ("code", 0),
        Kind::Pushplus | Kind::Bark => ("code", 200),
        Kind::Ntfy => return Ok(()),
    };
    match v[field].as_i64() {
        Some(c) if c == ok => Ok(()),
        Some(c) => {
            let msg = ["errmsg", "message", "msg"].iter().find_map(|k| v[*k].as_str()).unwrap_or_default();
            bail!("{c}：{msg}")
        }
        None => bail!("{}", body.chars().take(200).collect::<String>()),
    }
}

/// 发一条
pub async fn send(http: &reqwest::Client, h: &Hook, m: &Msg) -> Result<()> {
    let (url, body, headers) = request(h, m, crate::util::system_ms())?;
    let mut req = http.post(url).json(&body).timeout(Duration::from_secs(15));
    for (k, v) in headers {
        req = req.header(k, v);
    }
    let r = req.send().await?;
    let status = r.status();
    let text = r.text().await.unwrap_or_default();
    check_reply(h.kind, status, &text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hook(kind: Kind, target: &str) -> Hook {
        Hook { id: "h".into(), kind, target: target.into(), secret: String::new(), name: String::new() }
    }

    fn info() -> Info<'static> {
        Info { en: false, agent: "Claude", title: "修 [登录](http://evil)", project: "looklook", device: "工作站", url: "https://a.example/s/x", content: true, actions: false }
    }

    #[test]
    fn validate_targets() {
        assert!(hook(Kind::Wecom, "https://qyapi.weixin.qq.com/cgi-bin/webhook/send?key=abc").validate().is_ok());
        assert!(hook(Kind::Wecom, "https://evil.example/cgi-bin/webhook/send?key=abc").validate().is_err());
        assert!(hook(Kind::Dingtalk, "https://oapi.dingtalk.com/robot/send?access_token=x").validate().is_ok());
        assert!(hook(Kind::Serverchan, "SCT12345abcdef").validate().is_ok());
        assert!(hook(Kind::Serverchan, "SCT 123").validate().is_err());
        assert!(hook(Kind::Bark, "https://api.day.app/KEY").validate().is_ok());
        assert!(hook(Kind::Bark, "https://api.day.app/").validate().is_err());
        assert!(hook(Kind::Ntfy, "http://192.168.1.2:8080/agents").validate().is_ok());
        assert!(hook(Kind::Ntfy, "https://ntfy.sh/").validate().is_err());
        assert!(hook(Kind::Ntfy, "ftp://ntfy.sh/x").validate().is_err());
    }

    #[test]
    fn masked_hides_keys() {
        assert_eq!(hook(Kind::Wecom, "https://qyapi.weixin.qq.com/cgi-bin/webhook/send?key=abcdef1234").masked(), "https://qyapi.weixin.qq.com/…1234");
        assert_eq!(hook(Kind::Serverchan, "SCT12345abcdef").masked(), "SCT1…cdef");
    }

    #[test]
    fn session_text_is_escaped() {
        let opts = vec![];
        let a = Attn { kind: "permission", tool: "Bash", detail: "rm -rf build/*", risk: Some("high"), guard: None, options: &opts, limit_wait: None };
        let m = attn(&info(), &a);
        assert_eq!(m.title, "[高风险] Claude 在等你批准");
        let md = markdown(&m, md_escape, true);
        assert!(md.contains(r"修 \[登录\]\(http://evil\)"));
        assert!(md.contains(r"rm -rf build/\*"));
        assert!(md.ends_with("[打开处理](https://a.example/s/x)"));
        let wc = markdown(&m, wecom_escape, true);
        assert!(wc.contains("修 ［登录］(http://evil)"));
    }

    #[test]
    fn requests_per_kind() {
        let m = test(false, "https://a.example");
        let (u, b, _) = request(&hook(Kind::Serverchan, "SCT1abcdefgh"), &m, 0).unwrap();
        assert_eq!(u.as_str(), "https://sctapi.ftqq.com/SCT1abcdefgh.send");
        assert!(b["desp"].as_str().unwrap().contains("[打开看看](https://a.example)"));
        let (u, _, _) = request(&hook(Kind::Serverchan, "sctp123tabcdef"), &m, 0).unwrap();
        assert_eq!(u.as_str(), "https://123.push.ft07.com/send/sctp123tabcdef.send");
        let mut d = hook(Kind::Dingtalk, "https://oapi.dingtalk.com/robot/send?access_token=x");
        d.secret = "SEC1".into();
        let (u, b, _) = request(&d, &m, 1_700_000_000_000).unwrap();
        assert!(u.as_str().contains("access_token=x&timestamp=1700000000000&sign="));
        assert_eq!(b["msgtype"], "markdown");
        let mut n = hook(Kind::Ntfy, "https://ntfy.example/base/agents");
        n.secret = "tk".into();
        let (u, b, h) = request(&n, &m, 0).unwrap();
        assert_eq!(u.as_str(), "https://ntfy.example/base");
        assert_eq!(b["topic"], "agents");
        assert_eq!(b["click"], "https://a.example");
        assert_eq!(h, vec![("Authorization", "Bearer tk".to_string())]);
        let (_, b, _) = request(&hook(Kind::Bark, "https://api.day.app/K"), &m, 0).unwrap();
        assert_eq!(b["group"], "Looklook");
    }

    #[test]
    fn replies() {
        use reqwest::StatusCode as S;
        assert!(check_reply(Kind::Wecom, S::OK, r#"{"errcode":0,"errmsg":"ok"}"#).is_ok());
        assert_eq!(check_reply(Kind::Wecom, S::OK, r#"{"errcode":93000,"errmsg":"invalid webhook url"}"#).unwrap_err().to_string(), "93000：invalid webhook url");
        assert!(check_reply(Kind::Pushplus, S::OK, r#"{"code":200}"#).is_ok());
        assert!(check_reply(Kind::Serverchan, S::OK, r#"{"code":40001,"message":"bad key"}"#).is_err());
        assert!(check_reply(Kind::Ntfy, S::FORBIDDEN, "").is_err());
    }

    #[test]
    fn wecom_is_truncated() {
        let m = Msg { title: "t".into(), lines: vec!["很长".repeat(2000)], url: String::new(), link: String::new() };
        let (_, b, _) = request(&hook(Kind::Wecom, "https://qyapi.weixin.qq.com/cgi-bin/webhook/send?key=a"), &m, 0).unwrap();
        assert!(b["markdown"]["content"].as_str().unwrap().len() <= 4000);
    }

    #[test]
    fn guest_link_goes_to_register() {
        let m = remote_hint(test(false, "http://127.0.0.1:1234/tasks"), false, "https://looklook.test/register");
        assert_eq!((m.url.as_str(), m.link.as_str()), ("https://looklook.test/register", "开通远程访问"));
        assert!(m.lines.last().unwrap().contains("注册送一年"));
    }
}
