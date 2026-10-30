//! 扫码一键建应用（飞书官方的 OAuth 设备流，SDK 里的 `registerApp`，OpenClaw 也用这个）：
//!
//! 1. `begin` 拿到 `device_code` 和一个链接，链接上带着要开的权限、事件、回调（`addons`）；
//! 2. 用户用飞书扫码（或在飞书里打开链接），确认创建（或给已有的应用补权限）；
//! 3. 轮询 `poll`，拿到 App ID、App Secret 和扫码人的 open_id —— 不用再发绑定码。
//!
//! 订阅方式（长连接）和发布版本不归 `addons` 管：建出来的“个人智能体”应用默认就是长连接、扫码人可用。
//! 协议照 `@larksuiteoapi/node-sdk` 1.76 的 `registerApp` 写（文档：open.feishu.cn 搜“一键创建应用”）。

use std::io::Write;

use anyhow::{anyhow, Result};
use base64::Engine;
use serde_json::{json, Value};

const FEISHU: &str = "https://accounts.feishu.cn";
const LARK: &str = "https://accounts.larksuite.com";
const ENDPOINT: &str = "/oauth/v1/app/registration";

/// 看看要用的权限、事件、回调（设置页手动配置的列表跟这里一致）
pub const SCOPES: &[&str] = &[
    "im:message:send_as_bot",
    "im:message.p2p_msg:readonly",
    "cardkit:card:write",
    "im:resource",
    "im:message.group_at_msg:readonly",
    // 收到就加“正在输入”表情
    "im:message.reactions:write_only",
    // 读合并转发、引用的消息
    "im:message:readonly",
    // 项目群
    "im:chat:create",
];
/// 项目群里不 @ 也能收到消息：敏感权限，用到时（建项目群）再要，不放进扫码建应用里，免得企业管理员要审核
pub const GROUP_MSG: &str = "im:message.group_msg";
pub const EVENTS: &[&str] = &["im.message.receive_v1", "application.bot.menu_v6"];
pub const CALLBACKS: &[&str] = &["card.action.trigger"];

/// `addons` 参数：JSON → gzip → base64（URL 安全、去掉 `=`）。`extra`：再多要的权限。
/// 不要用户权限：以你本人的身份读、发飞书交给飞书官方的 lark-cli（AI 助手自己调），不用给应用申请
fn addons(extra: &[&str]) -> String {
    let scopes: Vec<&str> = SCOPES.iter().chain(extra).copied().collect();
    let v = json!({
        "scopes": { "tenant": scopes },
        "events": { "items": { "tenant": EVENTS } },
        "callbacks": { "items": CALLBACKS },
    });
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    gz.write_all(v.to_string().as_bytes()).expect("写内存不会失败");
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(gz.finish().expect("写内存不会失败"))
}

pub struct Begun {
    pub device_code: String,
    /// 给用户扫的链接
    pub url: String,
    pub expires_in: u64,
    pub interval: u64,
}

pub enum Poll {
    Pending,
    /// 飞书让慢一点
    SlowDown,
    Done { app_id: String, app_secret: String, open_id: Option<String>, lark: bool },
    /// `access_denied`（用户取消）、`expired_token`（过期）或别的
    Failed(String),
}

async fn post(http: &reqwest::Client, base: &str, form: &[(&str, &str)]) -> Result<Value> {
    // 测试里指到假飞书（它不认这个接口，begin 会失败，跟真飞书不可用时一样）
    #[cfg(test)]
    let base = super::TEST_BASE.get().copied().unwrap_or(base);
    // 等待中、慢一点这类结果是 HTTP 400 带 JSON（RFC 8628），一样按 JSON 读
    let r = http.post(format!("{base}{ENDPOINT}")).form(form).send().await?;
    let status = r.status();
    r.json::<Value>().await.map_err(|e| anyhow!("飞书注册接口 {status}：{e}"))
}

/// 开始：`app_id` 有值 = 给这个已有的应用补权限，否则建新的。`extra`：除了 [`SCOPES`] 再要的权限
pub async fn begin(http: &reqwest::Client, app_id: Option<&str>, extra: &[&str], en: bool) -> Result<Begun> {
    let v = post(http, FEISHU, &[("action", "begin"), ("archetype", "PersonalAgent"), ("auth_method", "client_secret"), ("request_user_info", "open_id")]).await?;
    let (Some(code), Some(uri)) = (v["device_code"].as_str(), v["verification_uri_complete"].as_str()) else {
        return Err(anyhow!("飞书注册接口返回不对：{}", v["error_description"].as_str().or(v["error"].as_str()).unwrap_or("")));
    };
    let mut url = reqwest::Url::parse(uri)?;
    {
        let mut q = url.query_pairs_mut();
        q.append_pair("from", "sdk").append_pair("source", "looklook").append_pair("tp", "sdk");
        q.append_pair("name", if en { "LookLook" } else { "看看" });
        q.append_pair("desc", if en { "LookLook notifications and AI chat" } else { "看看的通知和 AI 对话" });
        q.append_pair("addons", &addons(extra));
        if let Some(id) = app_id {
            q.append_pair("clientID", id);
        }
    }
    Ok(Begun { device_code: code.into(), url: url.into(), expires_in: v["expires_in"].as_u64().unwrap_or(600), interval: v["interval"].as_u64().unwrap_or(5).max(1) })
}

/// 问一次结果。`lark`：已经换到 Lark 的域名上问（扫码的是 Lark 租户时换一次）
pub async fn poll(http: &reqwest::Client, device_code: &str, lark: &mut bool) -> Result<Poll> {
    loop {
        let v = post(http, if *lark { LARK } else { FEISHU }, &[("action", "poll"), ("device_code", device_code)]).await?;
        if v["user_info"]["tenant_brand"] == "lark" && !*lark {
            *lark = true;
            continue;
        }
        if let (Some(id), Some(secret)) = (v["client_id"].as_str(), v["client_secret"].as_str()) {
            let open_id = v["user_info"]["open_id"].as_str().map(String::from);
            return Ok(Poll::Done { app_id: id.into(), app_secret: secret.into(), open_id, lark: *lark || v["user_info"]["tenant_brand"] == "lark" });
        }
        return Ok(match v["error"].as_str() {
            Some("authorization_pending") | None => Poll::Pending,
            Some("slow_down") => Poll::SlowDown,
            Some(e) => Poll::Failed(e.into()),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn addons_round_trip() {
        let s = addons(&[GROUP_MSG]);
        assert!(s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
        let gz = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(&s).unwrap();
        let mut out = String::new();
        flate2::read::GzDecoder::new(&gz[..]).read_to_string(&mut out).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["callbacks"]["items"][0], "card.action.trigger");
        assert_eq!(v["events"]["items"]["tenant"][0], "im.message.receive_v1");
        assert_eq!(v["scopes"]["tenant"].as_array().unwrap().len(), SCOPES.len() + 1);
        assert!(v["scopes"]["user"].is_null());
    }

    /// 真飞书：拿一个链接、问一次（没人扫，应该是等待中）。`cargo test register_live -- --ignored --nocapture`
    #[tokio::test]
    #[ignore]
    async fn register_live() {
        let http = reqwest::Client::new();
        let b = begin(&http, None, &[], false).await.unwrap();
        println!("{}", b.url);
        assert!(b.url.contains("addons=") && b.expires_in > 0);
        assert!(matches!(poll(&http, &b.device_code, &mut false).await.unwrap(), Poll::Pending));
    }
}
