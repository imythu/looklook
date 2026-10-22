//! 管理台经 WebRTC 端到端访问（服务端 docs/06）：授权票据、answer 签名、TURN 凭证与信令消息。
//!
//! - **授权票据** RtcGrant（主服务 → 客户端，§4.2）：`base64url(json).base64url(Ed25519(json))`，平台签名密钥签，
//!   绑定用户、子站会话、目标设备与浏览器的 DTLS 指纹（`browser_fp`），60 秒有效、一次性（`jti`）；
//! - **answer 签名**：设备用 Ed25519 私钥签 [`answer_canonical`]，主服务用该设备的公钥验签后才交给浏览器，
//!   中转改不了 answer 里电脑的 DTLS 指纹；
//! - **TURN 凭证**（§7.2）：TURN REST 格式，`username = "{exp}:{uid}"`，`password = base64(HMAC-SHA1(secret, username))`，
//!   coturn 的 `use-auth-secret` 按同样规则校验；
//! - **吊销推送**（§8.2）：中转经通道 `POST /_ll/rtc/revoke` 通知客户端关闭某些会话的连接，
//!   用（设备, 中转）的网关 MAC 密钥签（[`revoke_mac`]）。客户端只据此关闭连接，不当作授权依据。

use ed25519_dalek::{SigningKey, VerifyingKey};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::signing::{b64, b64_decode, sha256_hex, sign, verify};

/// 浏览器 → 主服务：签发票据、经回退路径转发 offer、校验 answer 签名。
pub const CONNECT_PATH: &str = "/_ll/rtc/connect";
/// 主服务 → 设备（经回退路径，带会话凭证与网关令牌）：`OfferRequest` → `OfferResponse`。
pub const OFFER_PATH: &str = "/_ll/rtc/offer";
/// 中转 → 设备：吊销推送。
pub const REVOKE_PATH: &str = "/_ll/rtc/revoke";
/// 启动页上报连接质量事件（用户站，docs/06 §15）。
pub const REPORT_PATH: &str = "/_ll/rtc/report";
/// 吊销推送的 MAC 请求头。
pub const REVOKE_MAC_HEADER: &str = "x-ll-revoke-mac";

pub const GRANT_VERSION: u8 = 1;
/// 票据有效期：只约束建连。
pub const GRANT_TTL_MS: i64 = 60_000;
/// 客户端校验 `exp` 时允许的误差。
pub const LEEWAY_MS: i64 = 30_000;
/// 通道上 `hello` 的协议版本（docs/06 §5 第 6 步）。
pub const PROTO: u32 = 1;

/// TURN 服务端口（UDP 与 TCP），各中转相同。
pub const TURN_PORT: u16 = 3478;
/// TURN 凭证有效期。
pub const TURN_TTL_SECS: i64 = 86_400;

/// 浏览器 `RTCIceServer` 的格式；客户端也用它。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IceServer {
    pub urls: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RtcGrant {
    pub v: u8,
    /// 平台签名密钥 id。
    pub kid: String,
    /// 设备通道所在中转名：客户端校验等于自己当前的中转（TURN 也在这个中转上）。
    pub aud: String,
    /// users.id（字符串），客户端校验等于本机登录的用户。
    pub uid: String,
    /// 子站会话短 id：吊销列表按它下发，连接按它关闭。
    pub sid: String,
    /// 目标设备 public_id，客户端校验等于自己。
    pub dev: String,
    /// 稳定地址的主机名（`alice.looklook.example`）：通道上的请求以它为 Host。
    pub host: String,
    /// 稳定地址的源（`https://alice.looklook.example`）：通道上的请求视为来自这个源（本机直连登记的嵌入来源）。
    pub origin: String,
    /// offer 里浏览器的 DTLS 指纹（[`sdp_fingerprint`] 的格式）。
    pub browser_fp: String,
    /// 给客户端用的 ICE 服务器（TURN 凭证，§7.2）。
    #[serde(default)]
    pub ice: Vec<IceServer>,
    /// 子站会话的绝对到期时间（Unix 毫秒）：到期时客户端关闭连接（§8.2）。
    pub abs_exp: i64,
    pub iat: i64,
    pub exp: i64,
    pub jti: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrantError {
    Malformed,
    UnknownKey,
    BadSignature,
    WrongVersion,
    Expired,
    WrongUser,
    WrongDevice,
}

pub fn encode_grant(key: &SigningKey, g: &RtcGrant) -> String {
    let json = serde_json::to_string(g).expect("grant serializes");
    format!("{}.{}", b64(json.as_bytes()), sign(key, &json))
}

/// 客户端侧校验：签名、版本、`exp`、`uid`、`dev`。`aud`、`jti` 去重、吊销与指纹比对由调用方完成。
pub fn verify_grant(
    token: &str,
    key_for: impl Fn(&str) -> Option<VerifyingKey>,
    own_user: &str,
    own_device: &str,
    now_ms: i64,
) -> Result<RtcGrant, GrantError> {
    let (p, s) = token.split_once('.').ok_or(GrantError::Malformed)?;
    let json = String::from_utf8(b64_decode(p).ok_or(GrantError::Malformed)?).map_err(|_| GrantError::Malformed)?;
    let g: RtcGrant = serde_json::from_str(&json).map_err(|_| GrantError::Malformed)?;
    let key = key_for(&g.kid).ok_or(GrantError::UnknownKey)?;
    if !verify(&key, &json, s) {
        return Err(GrantError::BadSignature);
    }
    if g.v != GRANT_VERSION {
        return Err(GrantError::WrongVersion);
    }
    if g.exp + LEEWAY_MS < now_ms {
        return Err(GrantError::Expired);
    }
    if g.uid != own_user {
        return Err(GrantError::WrongUser);
    }
    if g.dev != own_device {
        return Err(GrantError::WrongDevice);
    }
    Ok(g)
}

/// answer 签名规范串：`LL1-RTC-ANSWER\n{票据原文的 SHA-256}\n{answer SDP 的 SHA-256}`（hex）。
pub fn answer_canonical(grant: &str, answer_sdp: &str) -> String {
    format!("LL1-RTC-ANSWER\n{}\n{}", sha256_hex(grant.as_bytes()), sha256_hex(answer_sdp.as_bytes()))
}

/// SDP 里的 DTLS 指纹，规范成 `sha-256 AB:CD:…`（大写）。
///
/// 只接受 sha-256；会话级和各媒体段的 `a=fingerprint` 必须一致（出现两个不同的指纹时无法确定对端会用哪一个，拒绝）。
pub fn sdp_fingerprint(sdp: &str) -> Option<String> {
    let mut found: Option<String> = None;
    for line in sdp.lines() {
        let Some(v) = line.trim().strip_prefix("a=fingerprint:") else { continue };
        let (alg, fp) = v.trim().split_once(' ')?;
        if !alg.eq_ignore_ascii_case("sha-256") {
            return None;
        }
        let fp = fp.trim().to_ascii_uppercase();
        let ok = fp.len() == 32 * 3 - 1 && fp.split(':').all(|b| b.len() == 2 && b.chars().all(|c| c.is_ascii_hexdigit()));
        if !ok {
            return None;
        }
        let norm = format!("sha-256 {fp}");
        match &found {
            Some(f) if *f != norm => return None,
            _ => found = Some(norm),
        }
    }
    found
}

/// TURN REST 凭证：`(username, password)`，`exp` 为 Unix 秒。
pub fn turn_credential(secret: &[u8], uid: &str, exp_secs: i64) -> (String, String) {
    use base64::Engine;
    let username = format!("{exp_secs}:{uid}");
    let mut m = <Hmac<sha1::Sha1> as Mac>::new_from_slice(secret).expect("hmac key");
    m.update(username.as_bytes());
    let password = base64::engine::general_purpose::STANDARD.encode(m.finalize().into_bytes());
    (username, password)
}

/// 用户所在中转上的 ICE 服务器：STUN（打洞时查自己的公网地址）与 TURN（UDP、TCP）。
pub fn ice_servers(relay_host: &str, secret: &[u8], uid: &str, exp_secs: i64) -> Vec<IceServer> {
    let (username, credential) = turn_credential(secret, uid, exp_secs);
    vec![
        IceServer { urls: vec![format!("stun:{relay_host}:{TURN_PORT}")], username: None, credential: None },
        IceServer {
            urls: vec![
                format!("turn:{relay_host}:{TURN_PORT}?transport=udp"),
                format!("turn:{relay_host}:{TURN_PORT}?transport=tcp"),
            ],
            username: Some(username),
            credential: Some(credential),
        },
    ]
}

/// 浏览器 → 主服务。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectRequest {
    /// 目标设备 public_id。
    pub dev: String,
    /// 收集完候选的 offer SDP（不用 trickle）。
    pub offer: String,
}

/// 主服务 → 浏览器。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectResponse {
    pub answer: String,
    /// offer 走的路：`signal`（设备的信令长连接）或 `relay`（经中转转发，旧客户端）。上报连接事件用。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub via: String,
}

/// 本机客户端作为发起方连到同一账户的其他电脑（docs/06 §14）：客户端接口（`/api/client/v1`）下的相对路径，
/// 设备签名。`devices` 应答 [`crate::dto::RemoteDevices`]（`default` 是发起的这台），`connect` 收 [`ConnectRequest`]。
pub const CLIENT_DEVICES_PATH: &str = "/rtc/devices";
pub const CLIENT_CONNECT_PATH: &str = "/rtc/connect";

/// 主服务 → 发起的客户端：`host`、`origin` 是授权票据里的用户站地址，经通道发的请求以它们为 Host 与 Origin。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientConnectResponse {
    pub answer: String,
    pub host: String,
    pub origin: String,
}

/// 主服务 → 设备。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OfferRequest {
    pub grant: String,
    pub offer: String,
}

/// 设备 → 主服务：`sig` 是设备私钥对 [`answer_canonical`] 的签名，`key` 是签名用的设备密钥 id。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OfferResponse {
    pub answer: String,
    pub key: String,
    pub sig: String,
}

/// 设备 → 主服务的信令长连接（客户端接口下的相对路径）：设备签名的 `GET` 升级成 WebSocket，之后主服务把 offer
/// 直接推给设备，不再经中转转发；连着就说明设备在线。消息是 [`SignalMsg`] 的 JSON 文本帧。
pub const SIGNAL_PATH: &str = "/signal";
/// 主服务每隔这么久发一个 WebSocket ping（也让 Cloudflare 等代理不因空闲断开）；设备 3 倍时间收不到任何帧就重连。
pub const SIGNAL_PING_SECS: u64 = 20;

/// 信令长连接上的消息。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum SignalMsg {
    /// 主服务 → 设备：同 [`OfferRequest`]，`id` 对应回复。
    Offer { id: u64, grant: String, offer: String },
    /// 设备 → 主服务：同 [`OfferResponse`]。
    Answer { id: u64, answer: String, key: String, sig: String },
    /// 设备 → 主服务：不接受这个 offer，`status`、`why` 同 HTTP 路径的拒绝。
    Reject { id: u64, status: u16, why: String },
}

/// 主服务 →（经通道）设备：关闭这些子站会话的连接。`ts` 为 Unix 毫秒（主服务时钟）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevokePush {
    pub sids: Vec<String>,
    pub ts: i64,
}

/// 吊销推送的 MAC：`HMAC-SHA256(gateway_mac_key, "LL1-RTC-REVOKE\n" + body)`，base64url。
pub fn revoke_mac(key: &[u8], body: &[u8]) -> String {
    let mut m = <Hmac<Sha256> as Mac>::new_from_slice(key).expect("hmac key");
    m.update(b"LL1-RTC-REVOKE\n");
    m.update(body);
    b64(&m.finalize().into_bytes())
}

pub fn verify_revoke_mac(key: &[u8], body: &[u8], mac: &str) -> bool {
    let Some(tag) = b64_decode(mac) else { return false };
    let mut m = <Hmac<Sha256> as Mac>::new_from_slice(key).expect("hmac key");
    m.update(b"LL1-RTC-REVOKE\n");
    m.update(body);
    m.verify_slice(&tag).is_ok()
}

/// 票据原文的哈希（hex），日志与去重用。
pub fn grant_hash(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const FP: &str = "AB:CD:EF:01:23:45:67:89:AB:CD:EF:01:23:45:67:89:AB:CD:EF:01:23:45:67:89:AB:CD:EF:01:23:45:67:89";

    fn grant() -> RtcGrant {
        RtcGrant {
            v: GRANT_VERSION,
            kid: "p1".into(),
            aud: "r2".into(),
            uid: "1".into(),
            sid: "s1".into(),
            dev: "dev1".into(),
            host: "alice.looklook.example".into(),
            origin: "https://alice.looklook.example".into(),
            browser_fp: format!("sha-256 {FP}"),
            ice: ice_servers("r2.looklook.example", b"secret", "1", 100),
            abs_exp: 10 * GRANT_TTL_MS,
            iat: 0,
            exp: GRANT_TTL_MS,
            jti: "j".into(),
        }
    }

    #[test]
    fn grant_roundtrip() {
        let k = SigningKey::from_bytes(&[3u8; 32]);
        let vk = k.verifying_key();
        let keys = |kid: &str| (kid == "p1").then_some(vk);
        let t = encode_grant(&k, &grant());
        assert_eq!(verify_grant(&t, keys, "1", "dev1", 1000), Ok(grant()));
        assert_eq!(verify_grant(&t, keys, "2", "dev1", 1000), Err(GrantError::WrongUser));
        assert_eq!(verify_grant(&t, keys, "1", "dev2", 1000), Err(GrantError::WrongDevice));
        assert_eq!(verify_grant(&t, keys, "1", "dev1", GRANT_TTL_MS + LEEWAY_MS + 1), Err(GrantError::Expired));
        assert_eq!(verify_grant(&t, |_| None, "1", "dev1", 0), Err(GrantError::UnknownKey));
        let other = SigningKey::from_bytes(&[4u8; 32]).verifying_key();
        assert_eq!(verify_grant(&t, |_| Some(other), "1", "dev1", 0), Err(GrantError::BadSignature));
        let old = encode_grant(&k, &RtcGrant { v: 0, ..grant() });
        assert_eq!(verify_grant(&old, keys, "1", "dev1", 0), Err(GrantError::WrongVersion));
        assert_eq!(verify_grant("x", keys, "1", "dev1", 0), Err(GrantError::Malformed));
    }

    #[test]
    fn answer_signature_binds_grant_and_sdp() {
        let k = SigningKey::from_bytes(&[7u8; 32]);
        let sig = sign(&k, &answer_canonical("g1", "v=0\r\n"));
        assert!(verify(&k.verifying_key(), &answer_canonical("g1", "v=0\r\n"), &sig));
        assert!(!verify(&k.verifying_key(), &answer_canonical("g2", "v=0\r\n"), &sig));
        assert!(!verify(&k.verifying_key(), &answer_canonical("g1", "v=1\r\n"), &sig));
    }

    #[test]
    fn fingerprints() {
        let lower = FP.to_ascii_lowercase();
        let sdp = format!("v=0\r\na=fingerprint:sha-256 {lower}\r\nm=application 9\r\na=fingerprint:SHA-256 {FP}\r\n");
        assert_eq!(sdp_fingerprint(&sdp), Some(format!("sha-256 {FP}")));
        // 两个不同的指纹
        let other = FP.replacen("AB", "00", 1);
        assert_eq!(sdp_fingerprint(&format!("a=fingerprint:sha-256 {FP}\na=fingerprint:sha-256 {other}\n")), None);
        // 只接受 sha-256、格式完整
        assert_eq!(sdp_fingerprint(&format!("a=fingerprint:sha-1 {FP}\n")), None);
        assert_eq!(sdp_fingerprint("a=fingerprint:sha-256 AB:CD\n"), None);
        assert_eq!(sdp_fingerprint("v=0\r\n"), None);
    }

    #[test]
    fn turn_rest_credential() {
        // coturn 文档里的算法：base64(HMAC-SHA1(secret, "exp:user"))
        let (u, p) = turn_credential(b"north", "1", 1_700_000_000);
        assert_eq!(u, "1700000000:1");
        let mut m = <Hmac<sha1::Sha1> as Mac>::new_from_slice(b"north").unwrap();
        m.update(u.as_bytes());
        use base64::Engine;
        assert_eq!(p, base64::engine::general_purpose::STANDARD.encode(m.finalize().into_bytes()));
        let s = ice_servers("r2.x", b"north", "1", 1_700_000_000);
        assert_eq!(s[0].urls, vec!["stun:r2.x:3478"]);
        assert_eq!(s[1].urls, vec!["turn:r2.x:3478?transport=udp", "turn:r2.x:3478?transport=tcp"]);
        assert_eq!(s[1].credential.as_deref(), Some(p.as_str()));
    }

    #[test]
    fn revoke_mac_roundtrip() {
        let body = br#"{"sids":["s1"],"ts":1}"#;
        let m = revoke_mac(b"k", body);
        assert!(verify_revoke_mac(b"k", body, &m));
        assert!(!verify_revoke_mac(b"k2", body, &m));
        assert!(!verify_revoke_mac(b"k", b"{}", &m));
        assert!(!verify_revoke_mac(b"k", body, "!!"));
    }
}
