//! 网关令牌 v3（中转直连改造方案 §4.1）：转发给客户端的每个请求所附带的证明。
//!
//! `X-LL-Gateway-Token: base64url(payload_json) + "." + base64url(HMAC-SHA256(gateway_mac_key, payload_json))`
//!
//! MAC 密钥每（设备, 中转）一把，只有主服务、该中转与该设备知道（`RelayParams.gateway_mac_key`）。
//! 令牌证明请求来自中转或主服务（不是局域网里直接访问控制台的人），并用 `cap` 把请求绑到一张
//! 会话凭证（`edge_auth::SessionCap`）上；是否放行由凭证决定。

use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

use crate::signing::{b64, b64_decode};

pub const HEADER: &str = "x-ll-gateway-token";
pub const VERSION: u8 = 3;
pub const TTL_MS: i64 = 30_000;
/// 客户端校验 `exp` 时允许的误差。
pub const LEEWAY_MS: i64 = 30_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    UserHost,
    Tunnel,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Payload {
    pub v: u8,
    /// 同一请求所带会话凭证的哈希（`edge_auth::cap_hash`）。
    pub cap: String,
    /// 设备 public_id，客户端校验等于自己。
    pub dev: String,
    /// 浏览器实际访问的主机名（稳定地址或中转地址），客户端据此校验 Origin、改写跳转。
    pub host: String,
    pub iat: i64,
    pub exp: i64,
    pub jti: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum TokenError {
    Malformed,
    BadMac,
    Expired,
    WrongDevice,
    WrongVersion,
}

fn mac(key: &[u8], json: &str) -> Hmac<Sha256> {
    let mut m = <Hmac<Sha256> as Mac>::new_from_slice(key).expect("hmac key");
    m.update(json.as_bytes());
    m
}

pub fn encode(key: &[u8], payload: &Payload) -> String {
    let json = serde_json::to_string(payload).expect("payload serializes");
    format!("{}.{}", b64(json.as_bytes()), b64(&mac(key, &json).finalize().into_bytes()))
}

/// 客户端侧校验：MAC、版本、`dev` 等于本机、按估算平台时间未过期。`jti` 去重与 `cap` 比对由调用方完成。
pub fn verify_token(token: &str, key: &[u8], own_device: &str, now_ms: i64) -> Result<Payload, TokenError> {
    let (p, s) = token.split_once('.').ok_or(TokenError::Malformed)?;
    let json = String::from_utf8(b64_decode(p).ok_or(TokenError::Malformed)?).map_err(|_| TokenError::Malformed)?;
    let tag = b64_decode(s).ok_or(TokenError::Malformed)?;
    mac(key, &json).verify_slice(&tag).map_err(|_| TokenError::BadMac)?;
    let payload: Payload = serde_json::from_str(&json).map_err(|_| TokenError::Malformed)?;
    if payload.v != VERSION {
        return Err(TokenError::WrongVersion);
    }
    if payload.exp + LEEWAY_MS < now_ms {
        return Err(TokenError::Expired);
    }
    if payload.dev != own_device {
        return Err(TokenError::WrongDevice);
    }
    Ok(payload)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let key = [3u8; 32];
        let p = Payload {
            v: VERSION,
            cap: "h".into(),
            dev: "dev1".into(),
            host: "alice--k7m2q6xa.r2.looklook.example".into(),
            iat: 0,
            exp: TTL_MS,
            jti: "j".into(),
        };
        let t = encode(&key, &p);
        assert_eq!(verify_token(&t, &key, "dev1", 1000), Ok(p.clone()));
        assert_eq!(verify_token(&t, &key, "dev2", 1000), Err(TokenError::WrongDevice));
        assert_eq!(verify_token(&t, &key, "dev1", TTL_MS + LEEWAY_MS + 1), Err(TokenError::Expired));
        assert_eq!(verify_token(&t, &[4u8; 32], "dev1", 0), Err(TokenError::BadMac));
        let old = encode(&key, &Payload { v: 2, ..p });
        assert_eq!(verify_token(&old, &key, "dev1", 0), Err(TokenError::WrongVersion));
    }
}
