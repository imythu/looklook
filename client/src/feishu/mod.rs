//! 飞书（docs/TASK_BOARD.md §6）：用户自己建的企业自建应用，经长连接收回调、经开放接口发卡片。
//!
//! - [`Api`]：换 `tenant_access_token`（有效期 2 小时，提前 5 分钟换新的）、发消息、更新卡片；
//! - [`ws`]：长连接；[`card`]：卡片模板。
//!
//! 什么时候发、发给谁、按钮点了怎么办在 `notify.rs`。

pub mod card;
pub mod ws;

use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use serde_json::{json, Value};

/// 飞书（中国）或 Lark（海外）
pub fn base(domain: &str) -> &'static str {
    if domain == "lark" {
        "https://open.larksuite.com"
    } else {
        "https://open.feishu.cn"
    }
}

/// 飞书接口返回的错误（`code` 不为 0）
#[derive(Debug, Clone, thiserror::Error)]
#[error("飞书 {code}：{msg}")]
pub struct ApiError {
    pub code: i64,
    pub msg: String,
}

/// 从错误里取飞书的错误码（不是飞书接口的错误时为 None）
pub fn code_of(e: &anyhow::Error) -> Option<i64> {
    e.downcast_ref::<ApiError>().map(|e| e.code)
}

/// 应用凭证有问题（改了 Secret、应用被停用）：设置页标红，不重试
pub fn is_auth_error(code: i64) -> bool {
    matches!(code, 10003 | 10014 | 99991663 | 99991664 | 99991661 | 99991668 | 403 | 514)
        // 长连接地址接口：1 是繁忙、1000040343 是内部错误，可以重试；其余（如 1000040346 App ID 不对、1000040350 连接数满）重试也没用
        || (code >= 1_000_040_000 && code != 1_000_040_343)
}

/// 限流：退避后重试
fn is_rate_limited(code: i64) -> bool {
    matches!(code, 11232 | 99991400 | 230020)
}

pub struct Api {
    http: reqwest::Client,
    base: &'static str,
    app_id: String,
    app_secret: String,
    token: Mutex<Option<(String, Instant)>>,
}

impl Api {
    pub fn new(http: reqwest::Client, domain: &str, app_id: &str, app_secret: &str) -> Self {
        Self { http, base: base(domain), app_id: app_id.into(), app_secret: app_secret.into(), token: Mutex::new(None) }
    }

    async fn token(&self) -> Result<String> {
        if let Some((t, until)) = self.token.lock().unwrap_or_else(|e| e.into_inner()).clone() {
            if Instant::now() < until {
                return Ok(t);
            }
        }
        let v: Value = self
            .http
            .post(format!("{}/open-apis/auth/v3/tenant_access_token/internal", self.base))
            .json(&json!({ "app_id": self.app_id, "app_secret": self.app_secret }))
            .send()
            .await?
            .json()
            .await?;
        let code = v["code"].as_i64().unwrap_or(-1);
        if code != 0 {
            return Err(ApiError { code, msg: v["msg"].as_str().unwrap_or_default().into() }.into());
        }
        let t = v["tenant_access_token"].as_str().ok_or_else(|| anyhow!("没有 tenant_access_token"))?.to_string();
        let ttl = v["expire"].as_u64().unwrap_or(7200).saturating_sub(300).max(60);
        *self.token.lock().unwrap_or_else(|e| e.into_inner()) = Some((t.clone(), Instant::now() + Duration::from_secs(ttl)));
        Ok(t)
    }

    /// 调一次开放接口；限流时退避重试 2 次；令牌失效时换一次令牌再试
    async fn call(&self, method: reqwest::Method, path: &str, body: &Value) -> Result<Value> {
        let mut tries = 0;
        loop {
            tries += 1;
            let token = self.token().await?;
            let resp = self.http.request(method.clone(), format!("{}{path}", self.base)).bearer_auth(&token).json(body).timeout(Duration::from_secs(10)).send().await?;
            let status = resp.status();
            let v: Value = resp.json().await.unwrap_or_else(|_| json!({ "code": status.as_u16(), "msg": status.to_string() }));
            let code = v["code"].as_i64().unwrap_or(-1);
            if code == 0 {
                return Ok(v["data"].clone());
            }
            let err = ApiError { code, msg: v["msg"].as_str().unwrap_or_default().into() };
            if tries <= 2 && (is_rate_limited(code) || status.as_u16() == 429) {
                tokio::time::sleep(Duration::from_millis(600 * tries)).await;
                continue;
            }
            // 令牌被提前作废（比如重置了 Secret 又改回来）：清掉缓存再试一次
            if tries == 1 && matches!(code, 99991663 | 99991661 | 99991668) {
                *self.token.lock().unwrap_or_else(|e| e.into_inner()) = None;
                continue;
            }
            return Err(err.into());
        }
    }

    /// 发一张卡片给某人（私聊），返回消息 id
    pub async fn send_card(&self, open_id: &str, card: &Value) -> Result<String> {
        let body = json!({ "receive_id": open_id, "msg_type": "interactive", "content": card.to_string() });
        let d = self.call(reqwest::Method::POST, "/open-apis/im/v1/messages?receive_id_type=open_id", &body).await?;
        d["message_id"].as_str().map(String::from).ok_or_else(|| anyhow!("没有 message_id"))
    }

    /// 发一句文字给某人（绑定成功的回复）
    pub async fn send_text(&self, open_id: &str, text: &str) -> Result<()> {
        let body = json!({ "receive_id": open_id, "msg_type": "text", "content": json!({ "text": text }).to_string() });
        self.call(reqwest::Method::POST, "/open-apis/im/v1/messages?receive_id_type=open_id", &body).await.map(drop)
    }

    /// 更新已发的卡片（14 天内，卡片要带 `update_multi: true`）
    pub async fn patch_card(&self, message_id: &str, card: &Value) -> Result<()> {
        let body = json!({ "content": card.to_string() });
        self.call(reqwest::Method::PATCH, &format!("/open-apis/im/v1/messages/{message_id}"), &body).await.map(drop)
    }

    /// 检查凭证：能换到令牌就算对
    pub async fn check(&self) -> Result<()> {
        *self.token.lock().unwrap_or_else(|e| e.into_inner()) = None;
        self.token().await.map(drop)
    }
}
