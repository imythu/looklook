//! 客户端请求签名与平台响应签名（详细设计 §4.4.2、§4.4.3、§4.7）。

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use sha2::{Digest, Sha256};

pub const HDR_DEVICE: &str = "x-ll-device";
pub const HDR_KEY: &str = "x-ll-key";
pub const HDR_TIMESTAMP: &str = "x-ll-timestamp";
pub const HDR_NONCE: &str = "x-ll-nonce";
pub const HDR_SIGNATURE: &str = "x-ll-signature";
pub const HDR_PLATFORM_KEY: &str = "x-ll-platform-key";
pub const HDR_PLATFORM_SIGNATURE: &str = "x-ll-platform-signature";

/// 登录请求的 `X-LL-Device` / `X-LL-Key` 取值。
pub const LOGIN_PLACEHOLDER: &str = "-";

/// 允许的时间偏差（秒）。
pub const MAX_SKEW_SECONDS: i64 = 300;

pub fn b64(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

pub fn b64_decode(s: &str) -> Option<Vec<u8>> {
    URL_SAFE_NO_PAD.decode(s.trim_end_matches('=')).ok()
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

pub struct RequestParts<'a> {
    pub method: &'a str,
    /// 路径，不含域名，含查询串原样。
    pub path_and_query: &'a str,
    pub device: &'a str,
    pub key: &'a str,
    pub timestamp_ms: i64,
    pub nonce: &'a str,
    pub body: &'a [u8],
}

/// 请求规范串 `LL1-REQ`。
pub fn request_canonical(p: &RequestParts<'_>) -> String {
    format!(
        "LL1-REQ\n{}\n{}\n{}\n{}\n{}\n{}\n{}",
        p.method.to_ascii_uppercase(),
        p.path_and_query,
        p.device,
        p.key,
        p.timestamp_ms,
        p.nonce,
        sha256_hex(p.body)
    )
}

/// 响应规范串 `LL1-RESP`。
pub fn response_canonical(request_nonce: &str, status: u16, body: &[u8]) -> String {
    format!("LL1-RESP\n{request_nonce}\n{status}\n{}", sha256_hex(body))
}

/// 密钥轮换持有证明规范串 `LL1-ROTATE`。
pub fn rotate_canonical(device_id: &str, new_public_key_b64: &str, nonce: &str) -> String {
    format!("LL1-ROTATE\n{device_id}\n{new_public_key_b64}\n{nonce}")
}

pub fn sign(key: &SigningKey, message: &str) -> String {
    b64(&key.sign(message.as_bytes()).to_bytes())
}

pub fn parse_public_key(b64_key: &str) -> Option<VerifyingKey> {
    let bytes: [u8; 32] = b64_decode(b64_key)?.try_into().ok()?;
    VerifyingKey::from_bytes(&bytes).ok()
}

pub fn verify(key: &VerifyingKey, message: &str, signature_b64: &str) -> bool {
    let Some(bytes) = b64_decode(signature_b64) else { return false };
    let Ok(bytes): Result<[u8; 64], _> = bytes.try_into() else { return false };
    key.verify(message.as_bytes(), &Signature::from_bytes(&bytes)).is_ok()
}

/// 公钥指纹（登录请求的防重放键）：`fp_` + SHA-256 前 16 字节 hex。
pub fn public_key_fingerprint(key: &VerifyingKey) -> String {
    format!("fp_{}", &sha256_hex(key.as_bytes())[..32])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> SigningKey {
        SigningKey::from_bytes(&[7u8; 32])
    }

    #[test]
    fn request_roundtrip_and_tamper() {
        let k = key();
        let parts = RequestParts {
            method: "post",
            path_and_query: "/api/client/v1/heartbeat",
            device: "dev",
            key: "k_1",
            timestamp_ms: 1,
            nonce: "n",
            body: b"{}",
        };
        let sig = sign(&k, &request_canonical(&parts));
        assert!(verify(&k.verifying_key(), &request_canonical(&parts), &sig));
        let tampered = RequestParts { body: b"{ }", ..parts };
        assert!(!verify(&k.verifying_key(), &request_canonical(&tampered), &sig));
        assert!(request_canonical(&tampered).starts_with("LL1-REQ\nPOST\n"));
    }

    #[test]
    fn empty_body_hash() {
        assert_eq!(sha256_hex(b""), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
    }

    #[test]
    fn public_key_codec() {
        let vk = key().verifying_key();
        assert_eq!(parse_public_key(&b64(vk.as_bytes())), Some(vk));
        assert!(parse_public_key("short").is_none());
    }
}
