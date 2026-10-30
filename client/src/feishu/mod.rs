//! 飞书（docs/TASK_BOARD.md §6）：用户自己建的企业自建应用，经长连接收回调、经开放接口发卡片。
//!
//! - [`Api`]：换 `tenant_access_token`（有效期 2 小时，提前 5 分钟换新的）、发消息、更新卡片；
//! - [`ws`]：长连接；[`card`]：卡片模板。
//!
//! 什么时候发、发给谁、按钮点了怎么办在 `notify.rs`。

pub mod card;
#[cfg(test)]
pub(crate) mod fake;
pub mod register;
pub mod ws;

use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use serde_json::{json, Value};

/// 消息里的一张图片或一个文件（下载时要带上所在消息的 id）
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Resource {
    /// image / file
    pub kind: String,
    pub key: String,
    #[serde(default)]
    pub name: String,
    /// 在哪条消息里（合并转发里的子消息、连发合并的前几条）；空 = 下载时给的那条
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub msg: String,
}

/// 测试里换成本机的假飞书（[`fake`]）
#[cfg(test)]
pub(crate) static TEST_BASE: std::sync::OnceLock<&'static str> = std::sync::OnceLock::new();

/// 飞书（中国）或 Lark（海外）
pub fn base(domain: &str) -> &'static str {
    #[cfg(test)]
    if let Some(b) = TEST_BASE.get() {
        return b;
    }
    if domain == "lark" {
        "https://open.larksuite.com"
    } else {
        "https://open.feishu.cn"
    }
}

/// 手拼 multipart/form-data（只有上传图片和文件用，不为它引入 reqwest 的 multipart）
fn multipart(fields: &[(&str, &str)], file_field: &str, name: &str, bytes: Vec<u8>) -> (String, Vec<u8>) {
    use rand::RngCore;
    let mut r = [0u8; 12];
    rand::thread_rng().fill_bytes(&mut r);
    let boundary = format!("looklook{}", r.iter().map(|b| format!("{b:02x}")).collect::<String>());
    // 文件名里的引号、换行会破坏头部
    let name: String = name.chars().map(|c| if matches!(c, '"' | '\r' | '\n' | '\\') { '_' } else { c }).collect();
    let mut body = Vec::with_capacity(bytes.len() + 512);
    for (k, v) in fields {
        let v: String = v.chars().map(|c| if matches!(c, '\r' | '\n') { ' ' } else { c }).collect();
        body.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{k}\"\r\n\r\n{v}\r\n").as_bytes());
    }
    body.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{file_field}\"; filename=\"{name}\"\r\nContent-Type: application/octet-stream\r\n\r\n").as_bytes());
    body.extend_from_slice(&bytes);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    (format!("multipart/form-data; boundary={boundary}"), body)
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

/// 流式已经关了（10 分钟自动关闭）：用 `card_settings` 重新打开再推
pub fn is_stream_closed(code: i64) -> bool {
    matches!(code, 200850 | 300309)
}

/// 应用缺权限（没开 `cardkit:card:write` 或开了没发布版本）
pub fn is_scope_error(code: i64) -> bool {
    matches!(code, 99991672 | 99991679)
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

    /// 调一次开放接口，返回 `data`
    async fn call(&self, method: reqwest::Method, path: &str, body: &Value) -> Result<Value> {
        self.call_full(method, path, body).await.map(|mut v| v["data"].take())
    }

    /// 调一次开放接口，返回整个回应（有的接口不放在 `data` 里）；限流时退避重试 2 次；令牌失效时换一次令牌再试
    async fn call_full(&self, method: reqwest::Method, path: &str, body: &Value) -> Result<Value> {
        let mut tries = 0;
        loop {
            tries += 1;
            let token = self.token().await?;
            let mut req = self.http.request(method.clone(), format!("{}{path}", self.base)).bearer_auth(&token).timeout(Duration::from_secs(10));
            if !body.is_null() {
                req = req.json(body);
            }
            let resp = req.send().await?;
            let status = resp.status();
            let v: Value = resp.json().await.unwrap_or_else(|_| json!({ "code": status.as_u16(), "msg": status.to_string() }));
            let code = v["code"].as_i64().unwrap_or(-1);
            if code == 0 {
                return Ok(v);
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

    /// 回复一条消息，返回新消息 id。`thread`：回复到话题里；所在的会话不支持话题时退回普通回复
    pub async fn reply(&self, to: &str, msg_type: &str, content: &Value, thread: bool) -> Result<String> {
        let path = format!("/open-apis/im/v1/messages/{to}/reply");
        let body = |t: bool| json!({ "msg_type": msg_type, "content": content.to_string(), "reply_in_thread": t });
        let d = match self.call(reqwest::Method::POST, &path, &body(thread)).await {
            // 230071：不支持话题；230072：合并转发的消息不能开话题
            Err(e) if thread && matches!(code_of(&e), Some(230071 | 230072)) => self.call(reqwest::Method::POST, &path, &body(false)).await?,
            r => r?,
        };
        d["message_id"].as_str().map(String::from).ok_or_else(|| anyhow!("没有 message_id"))
    }

    /// 回复一句文字（普通引用回复；回复的是话题里的消息时飞书会留在话题里）
    pub async fn reply_text(&self, to: &str, text: &str) -> Result<String> {
        self.reply(to, "text", &json!({ "text": text }), false).await
    }

    /// 给消息加一个表情（当“正在输入”用），返回 reaction_id
    pub async fn add_reaction(&self, message_id: &str, emoji: &str) -> Result<String> {
        let d = self.call(reqwest::Method::POST, &format!("/open-apis/im/v1/messages/{message_id}/reactions"), &json!({ "reaction_type": { "emoji_type": emoji } })).await?;
        d["reaction_id"].as_str().map(String::from).ok_or_else(|| anyhow!("没有 reaction_id"))
    }

    pub async fn remove_reaction(&self, message_id: &str, reaction_id: &str) -> Result<()> {
        self.call(reqwest::Method::DELETE, &format!("/open-apis/im/v1/messages/{message_id}/reactions/{reaction_id}"), &Value::Null).await.map(drop)
    }

    /// 取一条消息；合并转发的消息连同子消息一起返回（子消息带 `upper_message_id`）
    pub async fn get_message(&self, message_id: &str) -> Result<Vec<Value>> {
        let mut d = self.call(reqwest::Method::GET, &format!("/open-apis/im/v1/messages/{message_id}"), &Value::Null).await?;
        Ok(d["items"].as_array_mut().map(std::mem::take).unwrap_or_default())
    }

    /// 建一个话题模式的群，群主是 `owner`（机器人当管理员），返回 chat_id
    pub async fn create_group(&self, name: &str, desc: &str, owner: &str) -> Result<String> {
        let body = json!({ "name": name, "description": desc, "owner_id": owner, "user_id_list": [owner], "group_message_type": "thread", "chat_type": "private" });
        let d = self.call(reqwest::Method::POST, "/open-apis/im/v1/chats?user_id_type=open_id&set_bot_manager=true", &body).await?;
        d["chat_id"].as_str().map(String::from).ok_or_else(|| anyhow!("没有 chat_id"))
    }

    /// 发一条消息到一个会话（私聊或群），返回消息 id
    pub async fn send_to_chat(&self, chat_id: &str, msg_type: &str, content: &Value) -> Result<String> {
        let body = json!({ "receive_id": chat_id, "msg_type": msg_type, "content": content.to_string() });
        let d = self.call(reqwest::Method::POST, "/open-apis/im/v1/messages?receive_id_type=chat_id", &body).await?;
        d["message_id"].as_str().map(String::from).ok_or_else(|| anyhow!("没有 message_id"))
    }

    /// 机器人自己的 open_id（群里认“@ 的是不是我”）
    pub async fn bot_open_id(&self) -> Result<String> {
        let v = self.call_full(reqwest::Method::GET, "/open-apis/bot/v3/info", &Value::Null).await?;
        v["bot"]["open_id"].as_str().map(String::from).ok_or_else(|| anyhow!("没有机器人的 open_id"))
    }

    /// 下载消息里的图片或文件（`kind`：image / file），最多 `max` 字节
    pub async fn download(&self, message_id: &str, key: &str, kind: &str, max: usize) -> Result<Vec<u8>> {
        let token = self.token().await?;
        let url = format!("{}/open-apis/im/v1/messages/{message_id}/resources/{key}?type={kind}", self.base);
        let mut resp = self.http.get(url).bearer_auth(&token).timeout(Duration::from_secs(120)).send().await?;
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

    /// 上传一个文件（multipart），返回 `data`
    async fn upload(&self, path: &str, fields: &[(&str, &str)], file_field: &str, name: &str, bytes: Vec<u8>) -> Result<Value> {
        let token = self.token().await?;
        let (ctype, body) = multipart(fields, file_field, name, bytes);
        let resp = self.http.post(format!("{}{path}", self.base)).bearer_auth(&token).header(reqwest::header::CONTENT_TYPE, ctype).body(body).timeout(Duration::from_secs(120)).send().await?;
        let status = resp.status();
        let v: Value = resp.json().await.unwrap_or_else(|_| json!({ "code": status.as_u16(), "msg": status.to_string() }));
        let code = v["code"].as_i64().unwrap_or(-1);
        if code != 0 {
            return Err(ApiError { code, msg: v["msg"].as_str().unwrap_or_default().into() }.into());
        }
        Ok(v["data"].clone())
    }

    /// 上传图片（发消息用，不超过 10 MB），返回 image_key
    pub async fn upload_image(&self, name: &str, bytes: Vec<u8>) -> Result<String> {
        let d = self.upload("/open-apis/im/v1/images", &[("image_type", "message")], "image", name, bytes).await?;
        d["image_key"].as_str().map(String::from).ok_or_else(|| anyhow!("没有 image_key"))
    }

    /// 上传文件（不超过 30 MB），返回 file_key
    pub async fn upload_file(&self, name: &str, bytes: Vec<u8>) -> Result<String> {
        let d = self.upload("/open-apis/im/v1/files", &[("file_type", "stream"), ("file_name", name)], "file", name, bytes).await?;
        d["file_key"].as_str().map(String::from).ok_or_else(|| anyhow!("没有 file_key"))
    }

    // ---------------- 卡片实体（CardKit，流式更新） ----------------

    /// 建一个卡片实体，返回 card_id（14 天内有效，只能发一次）
    pub async fn card_create(&self, card: &Value) -> Result<String> {
        let d = self.call(reqwest::Method::POST, "/open-apis/cardkit/v1/cards", &json!({ "type": "card_json", "data": card.to_string() })).await?;
        d["card_id"].as_str().map(String::from).ok_or_else(|| anyhow!("没有 card_id"))
    }

    /// 把卡片实体回复到一条消息下面。`thread`：开成话题（项目群里）
    pub async fn reply_card_entity(&self, to: &str, card_id: &str, thread: bool) -> Result<String> {
        self.reply(to, "interactive", &json!({ "type": "card", "data": { "card_id": card_id } }), thread).await
    }

    /// 推一个文本组件的全量内容（旧内容是前缀时飞书以打字机效果续写）。`seq` 对同一张卡片严格递增
    pub async fn card_text(&self, card_id: &str, element: &str, content: &str, seq: u32) -> Result<()> {
        let body = json!({ "content": content, "sequence": seq });
        self.call(reqwest::Method::PUT, &format!("/open-apis/cardkit/v1/cards/{card_id}/elements/{element}/content"), &body).await.map(drop)
    }

    /// 改卡片的配置（开关流式）
    pub async fn card_settings(&self, card_id: &str, config: &Value, seq: u32) -> Result<()> {
        let body = json!({ "settings": json!({ "config": config }).to_string(), "sequence": seq });
        self.call(reqwest::Method::PATCH, &format!("/open-apis/cardkit/v1/cards/{card_id}/settings"), &body).await.map(drop)
    }

    /// 整张换掉
    pub async fn card_update(&self, card_id: &str, card: &Value, seq: u32) -> Result<()> {
        let body = json!({ "card": { "type": "card_json", "data": card.to_string() }, "sequence": seq });
        self.call(reqwest::Method::PUT, &format!("/open-apis/cardkit/v1/cards/{card_id}"), &body).await.map(drop)
    }

    /// 检查凭证：能换到令牌就算对
    pub async fn check(&self) -> Result<()> {
        *self.token.lock().unwrap_or_else(|e| e.into_inner()) = None;
        self.token().await.map(drop)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multipart_body() {
        let (ctype, body) = multipart(&[("file_type", "stream"), ("file_name", "a\nb.txt")], "file", "报\"告.txt", b"hi".to_vec());
        let boundary = ctype.strip_prefix("multipart/form-data; boundary=").unwrap();
        let s = String::from_utf8(body).unwrap();
        assert!(s.starts_with(&format!("--{boundary}\r\n")));
        assert!(s.contains("name=\"file_name\"\r\n\r\na b.txt\r\n"));
        assert!(s.contains("name=\"file\"; filename=\"报_告.txt\"\r\nContent-Type: application/octet-stream\r\n\r\nhi\r\n"));
        assert!(s.ends_with(&format!("--{boundary}--\r\n")));
    }
}
