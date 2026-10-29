//! 用你本人的身份调飞书（`user_access_token`）：给 AI 助手的飞书工具用（`feishu_tools.rs`）。
//!
//! - 授权走飞书的 OAuth 设备流：`begin` 拿到一个链接，绑定的人在飞书里点开同意，`poll` 拿到令牌；
//!   一定带 `offline_access`，才有 `refresh_token`（7 天，每次换新时一起换新）；
//! - `access_token` 2 小时，提前 5 分钟用 `refresh` 换新。`refresh_token` 只能用一次，换新要排队（调用方加锁）；
//! - 令牌只存在这台电脑（store `feishu_user`），不同步：多台电脑同时换新会互相作废。
//!
//! 协议照飞书官方 `openclaw-lark` 的 `core/device-flow.js` 写。

use std::time::Duration;

use anyhow::{anyhow, Result};
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::ApiError;

/// AI 工具要用的用户权限（扫码建应用 / 补权限时一并开通，`register::addons` 的 `scopes.user`）
pub const SCOPES: &[&str] = &[
    // 会话列表、群信息、群成员
    "im:chat:read",
    "im:chat.members:read",
    // 读群聊、单聊的消息（含话题里的回复），下载里面的图片和文件
    "im:message:readonly",
    "im:message.group_msg:get_as_user",
    "im:message.p2p_msg:get_as_user",
    // 以你的身份发消息
    "im:message",
    "im:message.send_as_user",
    // 找人、看名字
    "contact:user:search",
    "contact:contact.base:readonly",
    "contact:user.base:readonly",
    // 搜、读云文档和知识库
    "search:docs:read",
    "docx:document:readonly",
    "wiki:node:read",
];

const KEY: &str = "feishu_user";

/// 存在这台电脑上的用户令牌
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Token {
    /// 授权时的应用：换了应用这个令牌就不能用了
    pub app_id: String,
    pub open_id: String,
    pub name: String,
    pub access: String,
    /// 毫秒
    pub access_exp: i64,
    pub refresh: String,
    pub refresh_exp: i64,
    /// 用户同意了的权限（空格分开）
    pub scope: String,
    pub at: i64,
}

impl Token {
    pub fn load(store: &crate::store::Store) -> Option<Self> {
        store.get(KEY).ok().flatten()
    }

    pub fn save(&self, store: &crate::store::Store) -> anyhow::Result<()> {
        store.set(KEY, self)
    }

    pub fn clear(store: &crate::store::Store) {
        if let Err(e) = store.remove(KEY) {
            tracing::warn!(error = %e, "飞书：用户令牌没删掉");
        }
    }

    /// 还没开通的权限（应用没开或者用户没同意）
    pub fn missing(&self) -> Vec<&'static str> {
        let has: Vec<&str> = self.scope.split_whitespace().collect();
        SCOPES.iter().copied().filter(|s| !has.contains(s)).collect()
    }

    fn from_reply(v: &Value, now: i64) -> Option<Self> {
        let access = v["access_token"].as_str().filter(|s| !s.is_empty())?;
        let secs = |k: &str, d: i64| v[k].as_i64().unwrap_or(d) * 1000;
        Some(Self {
            access: access.into(),
            access_exp: now + secs("expires_in", 7200),
            refresh: v["refresh_token"].as_str().unwrap_or_default().into(),
            refresh_exp: now + secs("refresh_token_expires_in", 604_800),
            scope: v["scope"].as_str().unwrap_or_default().into(),
            at: now,
            ..Default::default()
        })
    }
}

fn accounts(domain: &str) -> &'static str {
    #[cfg(test)]
    if let Some(b) = super::TEST_BASE.get() {
        return b;
    }
    if domain == "lark" {
        "https://accounts.larksuite.com"
    } else {
        "https://accounts.feishu.cn"
    }
}

fn mcp_base(domain: &str) -> &'static str {
    #[cfg(test)]
    if let Some(b) = super::TEST_BASE.get() {
        return b;
    }
    if domain == "lark" {
        "https://mcp.larksuite.com"
    } else {
        "https://mcp.feishu.cn"
    }
}

pub struct Begun {
    pub device_code: String,
    /// 给用户点的链接
    pub url: String,
    pub expires_in: u64,
    pub interval: u64,
}

pub enum Poll {
    Pending,
    SlowDown,
    Done(Token),
    /// `access_denied`（用户拒绝）、`expired_token`（过期）或别的
    Failed(String),
}

/// 开始授权：`scopes` 加上 `offline_access`
pub async fn begin(http: &reqwest::Client, domain: &str, app_id: &str, secret: &str, scopes: &[&str]) -> Result<Begun> {
    let basic = base64::engine::general_purpose::STANDARD.encode(format!("{app_id}:{secret}"));
    let scope = scopes.iter().copied().chain(["offline_access"]).collect::<Vec<_>>().join(" ");
    let r = http
        .post(format!("{}/oauth/v1/device_authorization", accounts(domain)))
        .header(reqwest::header::AUTHORIZATION, format!("Basic {basic}"))
        .form(&[("client_id", app_id), ("scope", &scope)])
        .timeout(Duration::from_secs(15))
        .send()
        .await?;
    let status = r.status();
    let v: Value = r.json().await.map_err(|e| anyhow!("飞书授权接口 {status}：{e}"))?;
    let (Some(code), Some(url)) = (v["device_code"].as_str(), v["verification_uri_complete"].as_str().or(v["verification_uri"].as_str())) else {
        let why = v["error_description"].as_str().or(v["error"].as_str()).or(v["msg"].as_str()).unwrap_or("");
        return Err(ApiError { code: v["code"].as_i64().unwrap_or(-1), msg: format!("飞书授权接口返回不对：{why}") }.into());
    };
    Ok(Begun { device_code: code.into(), url: url.into(), expires_in: v["expires_in"].as_u64().unwrap_or(240), interval: v["interval"].as_u64().unwrap_or(5).max(1) })
}

async fn token_call(http: &reqwest::Client, domain: &str, form: &[(&str, &str)]) -> Result<Value> {
    let r = http.post(format!("{}/open-apis/authen/v2/oauth/token", super::base(domain))).form(form).timeout(Duration::from_secs(15)).send().await?;
    let status = r.status();
    r.json::<Value>().await.map_err(|e| anyhow!("飞书令牌接口 {status}：{e}"))
}

/// 问一次授权结果
pub async fn poll(http: &reqwest::Client, domain: &str, app_id: &str, secret: &str, device_code: &str) -> Result<Poll> {
    let form = [("grant_type", "urn:ietf:params:oauth:grant-type:device_code"), ("device_code", device_code), ("client_id", app_id), ("client_secret", secret)];
    let v = token_call(http, domain, &form).await?;
    if let Some(t) = Token::from_reply(&v, crate::util::system_ms()) {
        return Ok(Poll::Done(Token { app_id: app_id.into(), ..t }));
    }
    Ok(match v["error"].as_str() {
        Some("authorization_pending") => Poll::Pending,
        Some("slow_down") => Poll::SlowDown,
        Some(e) => Poll::Failed(e.into()),
        None => Poll::Failed(v["msg"].as_str().unwrap_or("unknown").into()),
    })
}

/// 用 `refresh_token` 换一对新的（旧的随即作废）
pub async fn refresh(http: &reqwest::Client, domain: &str, secret: &str, old: &Token) -> Result<Token> {
    let form = [("grant_type", "refresh_token"), ("refresh_token", old.refresh.as_str()), ("client_id", old.app_id.as_str()), ("client_secret", secret)];
    let v = token_call(http, domain, &form).await?;
    let t = Token::from_reply(&v, crate::util::system_ms()).ok_or_else(|| ApiError { code: v["code"].as_i64().unwrap_or(-1), msg: v["error"].as_str().or(v["msg"].as_str()).unwrap_or("refresh").into() })?;
    // 有的回应不带 scope：沿用原来的
    let scope = if t.scope.is_empty() { old.scope.clone() } else { t.scope };
    Ok(Token { scope, app_id: old.app_id.clone(), open_id: old.open_id.clone(), name: old.name.clone(), ..t })
}

/// 用用户令牌调一次开放接口，返回 `data`
pub async fn call(http: &reqwest::Client, domain: &str, token: &str, method: reqwest::Method, path: &str, body: &Value) -> Result<Value> {
    let mut req = http.request(method, format!("{}{path}", super::base(domain))).bearer_auth(token).timeout(Duration::from_secs(20));
    if !body.is_null() {
        req = req.json(body);
    }
    let resp = req.send().await?;
    let status = resp.status();
    let mut v: Value = resp.json().await.unwrap_or_else(|_| json!({ "code": status.as_u16(), "msg": status.to_string() }));
    let code = v["code"].as_i64().unwrap_or(-1);
    if code != 0 {
        return Err(ApiError { code, msg: v["msg"].as_str().unwrap_or_default().into() }.into());
    }
    Ok(v["data"].take())
}

/// 授权的是谁
pub async fn me(http: &reqwest::Client, domain: &str, token: &str) -> Result<(String, String)> {
    let d = call(http, domain, token, reqwest::Method::GET, "/open-apis/authen/v1/user_info", &Value::Null).await?;
    let id = d["open_id"].as_str().ok_or_else(|| anyhow!("没有 open_id"))?;
    Ok((id.into(), d["name"].as_str().unwrap_or_default().into()))
}

/// 用用户令牌下载消息里的图片或文件（`kind`：image / file），最多 `max` 字节
pub async fn download(http: &reqwest::Client, domain: &str, token: &str, message_id: &str, key: &str, kind: &str, max: usize) -> Result<Vec<u8>> {
    let url = format!("{}/open-apis/im/v1/messages/{message_id}/resources/{key}?type={kind}", super::base(domain));
    let mut resp = http.get(url).bearer_auth(token).timeout(Duration::from_secs(120)).send().await?;
    let json = resp.headers().get(reqwest::header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).is_some_and(|v| v.starts_with("application/json"));
    if json || !resp.status().is_success() {
        let status = resp.status();
        let v: Value = resp.json().await.unwrap_or_else(|_| json!({ "code": status.as_u16(), "msg": status.to_string() }));
        return Err(ApiError { code: v["code"].as_i64().unwrap_or(-1), msg: v["msg"].as_str().unwrap_or_default().into() }.into());
    }
    let mut out = Vec::new();
    while let Some(chunk) = resp.chunk().await? {
        out.extend_from_slice(&chunk);
        if out.len() > max {
            return Err(anyhow!("文件太大"));
        }
    }
    Ok(out)
}

/// 飞书托管的 MCP（读云文档 `fetch-doc` 只有这里有）：JSON-RPC `tools/call`，用户令牌放在 `X-Lark-MCP-UAT`
pub async fn remote_mcp(http: &reqwest::Client, domain: &str, token: &str, tool: &str, args: &Value) -> Result<Value> {
    let base = mcp_base(domain);
    let body = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": { "name": tool, "arguments": args } });
    let r = http
        .post(format!("{base}/mcp"))
        .header("X-Lark-MCP-UAT", token)
        .header("X-Lark-MCP-Allowed-Tools", tool)
        .json(&body)
        .timeout(Duration::from_secs(60))
        .send()
        .await?;
    let status = r.status();
    let text = r.text().await?;
    if !status.is_success() {
        return Err(anyhow!("飞书 MCP {status}：{}", text.chars().take(500).collect::<String>()));
    }
    let mut v: Value = serde_json::from_str(&text).map_err(|_| anyhow!("飞书 MCP 返回的不是 JSON"))?;
    if let Some(e) = v.get("error").filter(|e| !e.is_null()) {
        return Err(anyhow!("飞书 MCP {}：{}", e["code"], e["message"].as_str().unwrap_or_default()));
    }
    Ok(v["result"].take())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_reply_and_missing() {
        let v = json!({ "code": 0, "access_token": "u-1", "refresh_token": "r-1", "expires_in": 7200, "refresh_token_expires_in": 604800, "scope": "im:chat:read offline_access" });
        let t = Token::from_reply(&v, 1000).unwrap();
        assert_eq!((t.access_exp, t.refresh_exp), (1000 + 7_200_000, 1000 + 604_800_000));
        assert!(!t.missing().contains(&"im:chat:read") && t.missing().contains(&"im:message.send_as_user"));
        assert!(Token::from_reply(&json!({ "error": "authorization_pending" }), 0).is_none());
    }
}
