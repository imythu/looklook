//! 远程入口的请求校验（中转直连改造方案 §4.4）。
//!
//! 每个转发请求带两样东西：
//! - **会话凭证**（`X-LL-Session-Cap`）：平台签名，绑定中转、稳定地址、用户、会话与隧道端口。
//!   v2（多设备）起不绑定设备：同一子站会话对该用户的所有设备有效，校验 `uid` 等于本机登录的用户；
//!   请求确实是发给本设备的，由网关令牌的 `dev` 与（设备, 中转）MAC 密钥保证。
//!   按哈希缓存验签结果，同一张凭证只验一次签。
//! - **网关令牌 v3**（`X-LL-Gateway-Token`）：中转或平台用（设备, 中转）的 MAC 密钥签，30 秒有效，
//!   `cap` 引用同一请求的凭证；`jti` 去重防重放。
//!
//! 放行与否由凭证决定：`aud` 必须是当前中转、`sid` 不在心跳下发的吊销列表里、浏览器主机名属于凭证。

use std::collections::HashMap;
use std::sync::Mutex;

use looklook_protocol::edge_auth::{self, SessionCap};
use looklook_protocol::gateway_token::{self, Kind};
use looklook_protocol::signing::b64_decode;

use crate::account::Account;

/// 校验通过的远程请求。
#[derive(Debug, Clone)]
pub struct Remote {
    pub kind: Kind,
    /// 浏览器实际访问的主机名（稳定地址或中转地址）。
    pub host: String,
    /// 隧道目标端口（来自凭证）。
    pub port: Option<u16>,
}

/// 凭证缓存上限：超过时先清掉过期的，仍超过就整体清空（只是多验几次签）。
const MAX_CAPS: usize = 4096;
/// 已用 jti 上限：令牌 30 秒有效（另加 30 秒误差），开发服务器一次加载几百个模块也远不到这个数。
const MAX_JTIS: usize = 65_536;

#[derive(Default)]
pub struct Verifier {
    caps: Mutex<HashMap<String, SessionCap>>,
    jtis: Mutex<HashMap<String, i64>>,
}

impl Verifier {
    pub fn verify(&self, account: &Account, cap: &str, token: &str) -> Result<Remote, &'static str> {
        let session = account.session().ok_or("not_logged_in")?;
        let now = account.clock.now_ms();
        let key = b64_decode(&session.relay.gateway_mac_key).filter(|k| !k.is_empty()).ok_or("no_mac_key")?;
        let tok = gateway_token::verify_token(token, &key, &session.device_id, now).map_err(|_| "token")?;
        if tok.cap != edge_auth::cap_hash(cap) {
            return Err("cap_mismatch");
        }
        let cap = self.cap(account, cap, &tok.cap, &session.user.id, now)?;
        if cap.aud != session.relay.name {
            return Err("wrong_relay");
        }
        if account.is_revoked(&cap.sid) {
            return Err("revoked");
        }
        if !edge_auth::host_matches(&cap, &tok.host) {
            return Err("wrong_host");
        }
        self.fresh_jti(&tok.jti, tok.exp + gateway_token::LEEWAY_MS, now)?;
        Ok(Remote { kind: cap.kind, host: tok.host, port: cap.port })
    }

    fn cap(&self, account: &Account, cap: &str, hash: &str, user: &str, now: i64) -> Result<SessionCap, &'static str> {
        let alive = |c: &SessionCap| c.exp + edge_auth::LEEWAY_MS >= now && c.uid == user;
        if let Some(c) = lock(&self.caps).get(hash).filter(|c| alive(c)) {
            return Ok(c.clone());
        }
        let platform = account.platform();
        let c = edge_auth::verify_cap(cap, |kid| platform.key(kid), user, now).map_err(|_| "cap")?;
        let mut caps = lock(&self.caps);
        if caps.len() >= MAX_CAPS {
            caps.retain(|_, c| alive(c));
            if caps.len() >= MAX_CAPS {
                caps.clear();
            }
        }
        caps.insert(hash.to_string(), c.clone());
        Ok(c)
    }

    fn fresh_jti(&self, jti: &str, until: i64, now: i64) -> Result<(), &'static str> {
        let mut jtis = lock(&self.jtis);
        if jtis.get(jti).is_some_and(|&t| t >= now) {
            return Err("replay");
        }
        if jtis.len() >= MAX_JTIS {
            // 过期的 jti 不需要再记；仍然太多说明有人在刷，拒绝而不是清空（清空会让重放重新可用）。
            jtis.retain(|_, t| *t >= now);
            if jtis.len() >= MAX_JTIS {
                return Err("busy");
            }
        }
        jtis.insert(jti.to_string(), until);
        Ok(())
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use ed25519_dalek::SigningKey;
    use looklook_protocol::dto::RelayParams;
    use looklook_protocol::edge_auth::{encode_cap, CAP_VERSION};
    use looklook_protocol::gateway_token::{encode, Payload, TTL_MS, VERSION};
    use looklook_protocol::signing::b64;

    use super::*;
    use crate::account::{Session, UserInfo};

    const NOW: i64 = 1_800_000_000_000;
    const MAC: [u8; 32] = [9u8; 32];

    fn platform_key() -> SigningKey {
        SigningKey::from_bytes(&[5u8; 32])
    }

    fn account(dir: &std::path::Path, revoked: &[&str]) -> Arc<Account> {
        let store = Arc::new(crate::store::Store::memory().unwrap());
        let paths = crate::paths::Paths { home: dir.to_path_buf(), ttyd: None, mux: None, fonts: None, trzsz: None };
        let keys = format!("p1={}", b64(platform_key().verifying_key().as_bytes()));
        let a = Account::new(store, paths, "https://looklook.test", &keys).unwrap();
        a.clock.set(NOW);
        a.set_for_test(
            Session {
                server: "https://looklook.test".into(),
                device_id: "dev1".into(),
                key_id: "k1".into(),
                user: UserInfo { id: "1".into(), username: "alice".into(), nickname: "a".into(), email: None },
                user_host_url: "https://alice.looklook.test/".into(),
                expires_ms: NOW * 2,
                heartbeat_interval: 600,
                offline_grace: 600,
                last_ok_ms: NOW,
                allow_run: true,
                reason: None,
                relay: RelayParams {
                    name: "r2".into(),
                    server_addr: "r2.looklook.test:2333".into(),
                    transport: "noise".into(),
                    noise_remote_public_key: "x".into(),
                    service_name: "u1".into(),
                    token: "t".into(),
                    gateway_mac_key: b64(&MAC),
                },
                update: None,
                rotation: None,
            },
            revoked,
        );
        a
    }

    fn cap(aud: &str, sid: &str) -> String {
        cap_for(aud, sid, "1")
    }

    fn cap_for(aud: &str, sid: &str, uid: &str) -> String {
        encode_cap(
            &platform_key(),
            &SessionCap {
                v: CAP_VERSION,
                kid: "p1".into(),
                aud: aud.into(),
                kind: Kind::Tunnel,
                host: "alice--k7m2q6xa.looklook.test".into(),
                uid: uid.into(),
                sid: sid.into(),
                tun: Some("k7m2q6xa".into()),
                port: Some(5173),
                iat: NOW,
                exp: NOW + edge_auth::CAP_TTL_MS,
            },
        )
    }

    fn token(cap: &str, host: &str, jti: &str, key: &[u8]) -> String {
        encode(
            key,
            &Payload {
                v: VERSION,
                cap: edge_auth::cap_hash(cap),
                dev: "dev1".into(),
                host: host.into(),
                iat: NOW,
                exp: NOW + TTL_MS,
                jti: jti.into(),
            },
        )
    }

    #[test]
    fn accepts_and_binds_port_and_host() {
        let dir = tempfile::tempdir().unwrap();
        let a = account(dir.path(), &[]);
        let v = Verifier::default();
        let c = cap("r2", "s1");
        let r = v.verify(&a, &c, &token(&c, "alice--k7m2q6xa.r2.looklook.test", "j1", &MAC)).unwrap();
        assert_eq!(r.kind, Kind::Tunnel);
        assert_eq!(r.port, Some(5173));
        assert_eq!(r.host, "alice--k7m2q6xa.r2.looklook.test");
        // 稳定地址（经主服务回退转发）同样放行；第二次命中凭证缓存。
        assert!(v.verify(&a, &c, &token(&c, "alice--k7m2q6xa.looklook.test", "j2", &MAC)).is_ok());
    }

    #[test]
    fn rejects() {
        let dir = tempfile::tempdir().unwrap();
        let a = account(dir.path(), &["s9"]);
        let v = Verifier::default();
        let c = cap("r2", "s1");
        let host = "alice--k7m2q6xa.r2.looklook.test";
        let t = token(&c, host, "j1", &MAC);
        assert!(v.verify(&a, &c, &t).is_ok());
        assert_eq!(v.verify(&a, &c, &t).unwrap_err(), "replay");
        assert_eq!(v.verify(&a, &c, &token(&c, host, "j2", &[1u8; 32])).unwrap_err(), "token");
        let other = cap("r2", "s2");
        assert_eq!(v.verify(&a, &other, &token(&c, host, "j3", &MAC)).unwrap_err(), "cap_mismatch");
        let wrong_relay = cap("r3", "s1");
        assert_eq!(v.verify(&a, &wrong_relay, &token(&wrong_relay, host, "j4", &MAC)).unwrap_err(), "wrong_relay");
        let revoked = cap("r2", "s9");
        assert_eq!(v.verify(&a, &revoked, &token(&revoked, host, "j5", &MAC)).unwrap_err(), "revoked");
        assert_eq!(v.verify(&a, &c, &token(&c, "bob--k7m2q6xa.r2.looklook.test", "j6", &MAC)).unwrap_err(), "wrong_host");
        let forged = encode_cap(&SigningKey::from_bytes(&[6u8; 32]), &serde_json::from_slice::<SessionCap>(&looklook_protocol::signing::b64_decode(c.split('.').next().unwrap()).unwrap()).unwrap());
        assert_eq!(v.verify(&a, &forged, &token(&forged, host, "j7", &MAC)).unwrap_err(), "cap");
        // 别的用户的会话凭证（v2 不绑定设备，按 uid 校验）
        let other_user = cap_for("r2", "s1", "2");
        assert_eq!(v.verify(&a, &other_user, &token(&other_user, host, "j8", &MAC)).unwrap_err(), "cap");
        // v1 凭证（带 dev）签名有效也被拒绝
        let mut p: serde_json::Value = serde_json::from_slice(&looklook_protocol::signing::b64_decode(c.split('.').next().unwrap()).unwrap()).unwrap();
        p["v"] = 1.into();
        p["dev"] = "dev1".into();
        let json = p.to_string();
        let v1 = format!("{}.{}", b64(json.as_bytes()), looklook_protocol::signing::sign(&platform_key(), &json));
        assert_eq!(v.verify(&a, &v1, &token(&v1, host, "j9", &MAC)).unwrap_err(), "cap");
    }
}
