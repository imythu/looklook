//! 看看服务端客户端接口（详细设计 §4.4、§7.3）。
//!
//! - 每个 `/api/client/v1` 请求用设备私钥签名（`LL1-REQ`），时间戳取可信时钟推算的平台时间；
//! - 每个响应必须带平台签名（`LL1-RESP`，绑定本次请求的 nonce），验签失败按网络错误处理，
//!   因此响应里的 `server_time`、到期时间可以放心记录；
//! - `TIMESTAMP_SKEW` 时用错误参数里的平台时间校准时钟后重试一次，本机时间不准也能用。

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use ed25519_dalek::{SigningKey, VerifyingKey};
use looklook_protocol::dto::WellKnown;
use looklook_protocol::error_code::ErrorBody;
use looklook_protocol::signing::{self, b64, request_canonical, RequestParts};
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::{Map, Value};

use crate::clock::TrustedClock;
use crate::util::{parse_time_ms, random_bytes};

pub const CLIENT_API: &str = "/api/client/v1";

#[derive(Debug, Clone, thiserror::Error)]
pub enum PlatformError {
    /// 连不上、超时、响应未通过平台签名校验
    #[error("网络错误：{0}")]
    Network(String),
    /// 平台返回的业务错误
    #[error("{code}")]
    Api { status: u16, code: String, params: Map<String, Value> },
}

impl PlatformError {
    pub fn code(&self) -> &str {
        match self {
            PlatformError::Network(_) => "NETWORK",
            PlatformError::Api { code, .. } => code,
        }
    }
    pub fn is(&self, code: &str) -> bool {
        self.code() == code
    }
}

pub type PResult<T> = Result<T, PlatformError>;

/// 签名身份：登录时为 `-`/`-` + 新生成的密钥（持有证明）。
#[derive(Clone, Copy)]
pub struct Signer<'a> {
    pub device: &'a str,
    pub key_id: &'a str,
    pub key: &'a SigningKey,
}

pub struct Platform {
    http: reqwest::Client,
    /// 下载安装包用：没有总超时（几十 MB 在慢网络上要好几分钟），只限制连接与两次读之间的间隔
    dl: reqwest::Client,
    /// 主站地址，如 `https://looklook.example`（无结尾斜杠）
    pub base: String,
    keys: RwLock<HashMap<String, VerifyingKey>>,
    pub clock: Arc<TrustedClock>,
}

/// `*.localhost` 按 RFC 6761 一律解析到本机，开发环境不依赖系统 DNS。
pub fn localhost_addr(host: &str) -> Option<IpAddr> {
    let h = host.trim_end_matches('.').to_ascii_lowercase();
    (h == "localhost" || h.ends_with(".localhost")).then_some(IpAddr::V4(Ipv4Addr::LOCALHOST))
}

/// 开发与测试：`LOOKLOOK_RESOLVE=host=ip[,host=ip]` 指定主机名解析（同 curl --resolve）。
pub fn resolve_override(host: &str) -> Option<IpAddr> {
    std::env::var("LOOKLOOK_RESOLVE")
        .ok()?
        .split(',')
        .filter_map(|kv| kv.trim().split_once('='))
        .find(|(h, _)| h.eq_ignore_ascii_case(host))
        .and_then(|(_, ip)| ip.trim().parse().ok())
}

impl Platform {
    pub fn new(base: &str, keys: HashMap<String, VerifyingKey>, clock: Arc<TrustedClock>) -> anyhow::Result<Self> {
        let base = base.trim().trim_end_matches('/').to_string();
        let url = reqwest::Url::parse(&base).map_err(|_| anyhow::anyhow!("服务端地址无效：{base}"))?;
        let builder = || {
            let mut b = reqwest::Client::builder().user_agent(concat!("looklook-client/", env!("CARGO_PKG_VERSION"))).connect_timeout(Duration::from_secs(10));
            if let Some(host) = url.host_str() {
                let port = url.port_or_known_default().unwrap_or(80);
                if let Some(ip) = localhost_addr(host).or_else(|| resolve_override(host)) {
                    b = b.resolve(host, SocketAddr::new(ip, port));
                }
            }
            b
        };
        let http = builder().timeout(Duration::from_secs(30)).build()?;
        let dl = builder().read_timeout(Duration::from_secs(60)).build()?;
        Ok(Self { http, dl, base, keys: RwLock::new(keys), clock })
    }

    pub fn has_keys(&self) -> bool {
        !self.keys.read().unwrap_or_else(|e| e.into_inner()).is_empty()
    }

    pub fn keys_b64(&self) -> HashMap<String, String> {
        self.keys.read().unwrap_or_else(|e| e.into_inner()).iter().map(|(k, v)| (k.clone(), b64(v.as_bytes()))).collect()
    }

    pub fn key(&self, kid: &str) -> Option<VerifyingKey> {
        self.keys.read().unwrap_or_else(|e| e.into_inner()).get(kid).copied()
    }

    /// 首次使用时从 `/.well-known/looklook.json` 取平台公钥并固定下来（没有内置公钥的构建）。
    pub async fn pin_keys_from_well_known(&self) -> PResult<HashMap<String, String>> {
        let wk: WellKnown = self.get_json_url(&format!("{}/.well-known/looklook.json", self.base)).await?;
        let mut map = HashMap::new();
        for k in &wk.platform_keys {
            if let Some(vk) = signing::parse_public_key(&k.public_key) {
                map.insert(k.kid.clone(), vk);
            }
        }
        if map.is_empty() {
            return Err(PlatformError::Network("平台没有公布公钥".into()));
        }
        *self.keys.write().unwrap_or_else(|e| e.into_inner()) = map;
        if let Some(t) = parse_time_ms(&wk.server_time) {
            if !self.clock.has_anchor() {
                self.clock.set(t);
            }
        }
        Ok(self.keys_b64())
    }

    async fn get_json_url<T: DeserializeOwned>(&self, url: &str) -> PResult<T> {
        let r = self.http.get(url).send().await.map_err(net)?;
        let status = r.status().as_u16();
        let body = r.bytes().await.map_err(net)?;
        if status >= 400 {
            return Err(api_error(status, &body));
        }
        serde_json::from_slice(&body).map_err(|e| PlatformError::Network(format!("响应格式错误：{e}")))
    }

    /// 公共接口（无签名）：检查更新、推广位等。
    pub async fn public<T: DeserializeOwned>(&self, path_and_query: &str) -> PResult<T> {
        self.get_json_url(&format!("{}/api/public/v1{path_and_query}", self.base)).await
    }

    /// 下载文件到 `dest`，边下边算 SHA-256（小写十六进制）。`progress(已下载, 总大小)`；超过 `max` 字节中止。
    pub async fn download(&self, url: &str, dest: &std::path::Path, max: u64, progress: impl Fn(u64, Option<u64>)) -> PResult<String> {
        use sha2::{Digest, Sha256};
        use tokio::io::AsyncWriteExt;
        let mut r = self.dl.get(url).send().await.map_err(net)?;
        if !r.status().is_success() {
            return Err(PlatformError::Network(format!("HTTP {}", r.status().as_u16())));
        }
        let total = r.content_length();
        if total.is_some_and(|t| t > max) {
            return Err(PlatformError::Network("文件太大".into()));
        }
        let io = |e: std::io::Error| PlatformError::Network(format!("写入失败：{e}"));
        let mut f = tokio::fs::File::create(dest).await.map_err(io)?;
        let mut h = Sha256::new();
        let mut done = 0u64;
        while let Some(chunk) = r.chunk().await.map_err(net)? {
            done += chunk.len() as u64;
            if done > max {
                return Err(PlatformError::Network("文件太大".into()));
            }
            h.update(&chunk);
            f.write_all(&chunk).await.map_err(io)?;
            progress(done, total);
        }
        f.flush().await.map_err(io)?;
        Ok(data_encoding::HEXLOWER.encode(&h.finalize()))
    }

    /// 签名请求。`body` 由 nonce 生成（密钥轮换的持有证明要绑定本次请求的 nonce）。
    pub async fn call_raw(
        &self,
        method: &str,
        path: &str,
        signer: Signer<'_>,
        body: &(dyn Fn(&str) -> Option<Vec<u8>> + Sync),
    ) -> PResult<(u16, bytes::Bytes)> {
        let full = format!("{CLIENT_API}{path}");
        let mut retried = false;
        loop {
            let nonce = b64(&random_bytes::<16>());
            let bytes = body(&nonce);
            let ts = self.clock.now_ms();
            let raw = bytes.clone().unwrap_or_default();
            let canonical = request_canonical(&RequestParts {
                method,
                path_and_query: &full,
                device: signer.device,
                key: signer.key_id,
                timestamp_ms: ts,
                nonce: &nonce,
                body: &raw,
            });
            let m: reqwest::Method = method.parse().map_err(|_| PlatformError::Network("bad method".into()))?;
            let mut req = self
                .http
                .request(m, format!("{}{full}", self.base))
                .header(signing::HDR_DEVICE, signer.device)
                .header(signing::HDR_KEY, signer.key_id)
                .header(signing::HDR_TIMESTAMP, ts.to_string())
                .header(signing::HDR_NONCE, &nonce)
                .header(signing::HDR_SIGNATURE, signing::sign(signer.key, &canonical));
            if let Some(b) = bytes {
                req = req.header("content-type", "application/json").body(b);
            }
            let resp = req.send().await.map_err(net)?;
            let status = resp.status().as_u16();
            let kid = header(&resp, signing::HDR_PLATFORM_KEY);
            let sig = header(&resp, signing::HDR_PLATFORM_SIGNATURE);
            let body_bytes = resp.bytes().await.map_err(net)?;
            let verified = match (kid.and_then(|k| self.key(&k)), sig) {
                (Some(vk), Some(s)) => signing::verify(&vk, &signing::response_canonical(&nonce, status, &body_bytes), &s),
                _ => false,
            };
            if !verified {
                return Err(PlatformError::Network("平台响应签名校验失败".into()));
            }
            if status >= 400 {
                let err = api_error(status, &body_bytes);
                if let PlatformError::Api { code, params, .. } = &err {
                    if code == "TIMESTAMP_SKEW" && !retried {
                        if let Some(t) = params.get("server_time").and_then(Value::as_str).and_then(parse_time_ms) {
                            self.clock.set(t);
                            retried = true;
                            continue;
                        }
                    }
                }
                return Err(err);
            }
            return Ok((status, body_bytes));
        }
    }

    /// 无需签名的请求（浏览器授权登录：此时设备还没有密钥身份）。仍要求响应带有效的平台签名。
    pub async fn call_unsigned<T: DeserializeOwned>(&self, path: &str, body: &(impl Serialize + Sync)) -> PResult<T> {
        let nonce = b64(&random_bytes::<16>());
        let resp = self
            .http
            .post(format!("{}{CLIENT_API}{path}", self.base))
            .header(signing::HDR_NONCE, &nonce)
            .json(body)
            .send()
            .await
            .map_err(net)?;
        let status = resp.status().as_u16();
        let kid = header(&resp, signing::HDR_PLATFORM_KEY);
        let sig = header(&resp, signing::HDR_PLATFORM_SIGNATURE);
        let bytes = resp.bytes().await.map_err(net)?;
        let verified = match (kid.and_then(|k| self.key(&k)), sig) {
            (Some(vk), Some(s)) => signing::verify(&vk, &signing::response_canonical(&nonce, status, &bytes), &s),
            _ => false,
        };
        if !verified {
            return Err(PlatformError::Network("平台响应签名校验失败".into()));
        }
        if status >= 400 {
            return Err(api_error(status, &bytes));
        }
        serde_json::from_slice(&bytes).map_err(|e| PlatformError::Network(format!("响应格式错误：{e}")))
    }

    pub async fn call<T: DeserializeOwned>(&self, method: &str, path: &str, signer: Signer<'_>, body: Option<&(impl Serialize + Sync)>) -> PResult<T> {
        let json = body.map(|b| serde_json::to_vec(b).unwrap_or_default());
        let (_, bytes) = self.call_raw(method, path, signer, &|_| json.clone()).await?;
        serde_json::from_slice(&bytes).map_err(|e| PlatformError::Network(format!("响应格式错误：{e}")))
    }

    /// 期望 204 的签名请求。
    pub async fn call_empty(&self, method: &str, path: &str, signer: Signer<'_>, body: Option<&(impl Serialize + Sync)>) -> PResult<()> {
        let json = body.map(|b| serde_json::to_vec(b).unwrap_or_default());
        self.call_raw(method, path, signer, &|_| json.clone()).await.map(|_| ())
    }
}

fn header(r: &reqwest::Response, name: &str) -> Option<String> {
    r.headers().get(name).and_then(|v| v.to_str().ok()).map(str::to_owned)
}

fn net(e: reqwest::Error) -> PlatformError {
    PlatformError::Network(e.without_url().to_string())
}

fn api_error(status: u16, body: &[u8]) -> PlatformError {
    match serde_json::from_slice::<ErrorBody>(body) {
        Ok(b) => PlatformError::Api { status, code: b.error.code.as_str(), params: b.error.params },
        // 协议里没有的错误码也按原样传给界面
        Err(_) => match serde_json::from_slice::<Value>(body).ok().and_then(|v| v["error"]["code"].as_str().map(str::to_owned)) {
            Some(code) => PlatformError::Api { status, code, params: Map::new() },
            None => PlatformError::Network(format!("HTTP {status}")),
        },
    }
}

/// 解析内置/配置的平台公钥：`p1=base64url,p2=base64url`。
pub fn parse_key_list(s: &str) -> HashMap<String, VerifyingKey> {
    s.split(',')
        .filter_map(|kv| kv.trim().split_once('='))
        .filter_map(|(k, v)| signing::parse_public_key(v.trim()).map(|vk| (k.trim().to_string(), vk)))
        .collect()
}

/// 当前进程是否以 root 身份运行（仅 Unix；Windows 恒为 false）。
pub fn is_root() -> bool {
    #[cfg(unix)]
    {
        unsafe { libc::geteuid() == 0 }
    }
    #[cfg(not(unix))]
    {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn localhost_names() {
        assert!(localhost_addr("looklook.localhost").is_some());
        assert!(localhost_addr("relay.looklook.localhost.").is_some());
        assert!(localhost_addr("localhost").is_some());
        assert!(localhost_addr("looklook.example").is_none());
        assert!(localhost_addr("notlocalhost").is_none());
    }

    #[test]
    fn key_list() {
        let k = SigningKey::from_bytes(&[9u8; 32]).verifying_key();
        let m = parse_key_list(&format!("p1={}, bad=xx", b64(k.as_bytes())));
        assert_eq!(m.len(), 1);
        assert_eq!(m["p1"], k);
    }

    #[test]
    fn error_bodies() {
        let e = api_error(409, br#"{"error":{"code":"DEVICE_ALREADY_LOGGED_IN","params":{"device_name":"x"},"request_id":"r"}}"#);
        assert!(e.is("DEVICE_ALREADY_LOGGED_IN"));
        let e = api_error(400, br#"{"error":{"code":"SOMETHING_NEW"}}"#);
        assert!(e.is("SOMETHING_NEW"));
        assert!(api_error(502, b"<html>").is("NETWORK"));
    }
}
