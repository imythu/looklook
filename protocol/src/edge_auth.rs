//! 会话凭证（中转直连改造方案 §4.1）：主服务签发、客户端校验，证明“这个子站会话是平台放行的”。
//!
//! `X-LL-Session-Cap: base64url(payload_json) + "." + base64url(Ed25519(payload_json))`
//!
//! 凭证绑定中转（`aud`）、稳定地址（`host`）、用户、会话与隧道端口，每个转发请求都带上；
//! v2（多设备，docs/05 §8）起不再绑定设备：一个子站会话对该用户的所有设备有效，
//! 请求确实送到了该去的设备由网关令牌 v3（带 `dev`、按（设备, 中转）的 MAC 密钥签）保证。
//! 同一请求的网关令牌（`gateway_token`）用 `cap` 字段引用它的哈希。负载不含随机数，
//! 同一会话在同一小时内签出的凭证逐字节相同，客户端按哈希缓存验签结果。

use ed25519_dalek::{SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::gateway_token::Kind;
use crate::signing::{b64, b64_decode, sign, verify};

pub const CAP_HEADER: &str = "x-ll-session-cap";
pub const CAP_VERSION: u8 = 2;
/// 凭证最长有效期：1 天（且不超过会话的绝对到期时间）。
pub const CAP_TTL_MS: i64 = 86_400_000;
/// 签发时间按小时取整，让同一会话的凭证在一小时内保持不变。
pub const CAP_IAT_STEP_MS: i64 = 3_600_000;
/// 客户端校验 `exp` 时允许的误差。
pub const LEEWAY_MS: i64 = 30_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionCap {
    pub v: u8,
    /// 平台签名密钥 id。
    pub kid: String,
    /// 中转名（`r1`、`r2`…），客户端校验等于自己当前的中转。
    pub aud: String,
    pub kind: Kind,
    /// 稳定地址的主机名：`alice.looklook.example` / `alice--k7m2q6xa.looklook.example`。
    pub host: String,
    /// users.id（字符串），客户端校验等于本机登录的用户。
    pub uid: String,
    /// 子站会话的短 id；吊销列表按它下发。
    pub sid: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tun: Option<String>,
    /// 隧道目标端口：客户端只把隧道请求转到这里。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    pub iat: i64,
    pub exp: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapError {
    Malformed,
    UnknownKey,
    BadSignature,
    Expired,
    WrongUser,
    WrongVersion,
}

pub fn encode_cap(key: &SigningKey, cap: &SessionCap) -> String {
    let json = serde_json::to_string(cap).expect("cap serializes");
    format!("{}.{}", b64(json.as_bytes()), sign(key, &json))
}

/// 签发时间：`now` 按小时取整。
pub fn cap_iat(now_ms: i64) -> i64 {
    now_ms - now_ms.rem_euclid(CAP_IAT_STEP_MS)
}

/// 网关令牌里引用凭证用的哈希：SHA-256(凭证原文)，base64url。
pub fn cap_hash(token: &str) -> String {
    b64(&Sha256::digest(token.as_bytes()))
}

/// 客户端侧校验：按 `kid` 找平台公钥验签、版本、`uid` 等于本机登录的用户、按估算平台时间未过期。
pub fn verify_cap(
    token: &str,
    key_for: impl Fn(&str) -> Option<VerifyingKey>,
    own_user: &str,
    now_ms: i64,
) -> Result<SessionCap, CapError> {
    let (p, s) = token.split_once('.').ok_or(CapError::Malformed)?;
    let json = String::from_utf8(b64_decode(p).ok_or(CapError::Malformed)?).map_err(|_| CapError::Malformed)?;
    let cap: SessionCap = serde_json::from_str(&json).map_err(|_| CapError::Malformed)?;
    let key = key_for(&cap.kid).ok_or(CapError::UnknownKey)?;
    if !verify(&key, &json, s) {
        return Err(CapError::BadSignature);
    }
    if cap.v != CAP_VERSION {
        return Err(CapError::WrongVersion);
    }
    if cap.exp + LEEWAY_MS < now_ms {
        return Err(CapError::Expired);
    }
    if cap.uid != own_user {
        return Err(CapError::WrongUser);
    }
    Ok(cap)
}

/// 浏览器实际访问的主机名是否属于这张凭证：稳定地址本身，或中转地址 `{label}.{aud}.{rest}`
/// （稳定地址为 `{label}.{rest}`）。
pub fn host_matches(cap: &SessionCap, host: &str) -> bool {
    if host.eq_ignore_ascii_case(&cap.host) {
        return true;
    }
    let Some((label, rest)) = cap.host.split_once('.') else { return false };
    host.eq_ignore_ascii_case(&format!("{label}.{}.{rest}", cap.aud))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cap() -> SessionCap {
        SessionCap {
            v: CAP_VERSION,
            kid: "p1".into(),
            aud: "r2".into(),
            kind: Kind::Tunnel,
            host: "alice--k7m2q6xa.looklook.example".into(),
            uid: "1".into(),
            sid: "abc".into(),
            tun: Some("k7m2q6xa".into()),
            port: Some(5173),
            iat: 0,
            exp: CAP_TTL_MS,
        }
    }

    #[test]
    fn roundtrip() {
        let k = SigningKey::from_bytes(&[3u8; 32]);
        let vk = k.verifying_key();
        let keys = |kid: &str| (kid == "p1").then_some(vk);
        let t = encode_cap(&k, &cap());
        assert_eq!(verify_cap(&t, keys, "1", 1000), Ok(cap()));
        assert_eq!(verify_cap(&t, keys, "2", 1000), Err(CapError::WrongUser));
        assert_eq!(verify_cap(&t, keys, "1", CAP_TTL_MS + LEEWAY_MS + 1), Err(CapError::Expired));
        assert_eq!(verify_cap(&t, |_| None, "1", 0), Err(CapError::UnknownKey));
        let other = SigningKey::from_bytes(&[4u8; 32]).verifying_key();
        assert_eq!(verify_cap(&t, |_| Some(other), "1", 0), Err(CapError::BadSignature));
        // Ed25519 签名确定：同一负载签出同一凭证，哈希可作缓存键。
        assert_eq!(cap_hash(&t), cap_hash(&encode_cap(&k, &cap())));
    }

    /// v1 凭证（带 `dev`、`v=1`）签名有效也被拒绝。
    #[test]
    fn rejects_v1() {
        let k = SigningKey::from_bytes(&[3u8; 32]);
        let vk = k.verifying_key();
        let mut v = serde_json::to_value(cap()).unwrap();
        v["v"] = 1.into();
        v["dev"] = "dev1".into();
        let json = v.to_string();
        let t = format!("{}.{}", b64(json.as_bytes()), sign(&k, &json));
        assert_eq!(verify_cap(&t, |_| Some(vk), "1", 1000), Err(CapError::WrongVersion));
    }

    #[test]
    fn hosts() {
        let c = cap();
        assert!(host_matches(&c, "alice--k7m2q6xa.looklook.example"));
        assert!(host_matches(&c, "alice--k7m2q6xa.r2.looklook.example"));
        assert!(!host_matches(&c, "alice--k7m2q6xa.r3.looklook.example"));
        assert!(!host_matches(&c, "alice.looklook.example"));
        assert!(!host_matches(&c, "bob--k7m2q6xa.r2.looklook.example"));
    }

    #[test]
    fn iat_rounds_down_to_hour() {
        assert_eq!(cap_iat(CAP_IAT_STEP_MS * 5 + 1234), CAP_IAT_STEP_MS * 5);
        assert_eq!(cap_iat(CAP_IAT_STEP_MS * 5), CAP_IAT_STEP_MS * 5);
    }
}
