//! 管理台经 WebRTC 端到端访问（服务端 docs/06）。
//!
//! - **信令**（[`offer`]，`POST /_ll/rtc/offer`）：主服务经回退路径转来，先照常经远程入口校验会话凭证与网关令牌；
//!   这里再校验授权票据（平台签名、未过期、一次性、发给本机用户与本设备、同一子站会话、未被吊销），
//!   核对 offer 里浏览器的 DTLS 指纹等于票据里的 `browser_fp`，建立 PeerConnection，用设备私钥为 answer 签名；
//! - **连接**（[`run_peer`]）：浏览器开的每条 DataChannel 交给 [`bridge`]，请求带 `Access::Rtc` 进现有路由；
//! - **失效与吊销**（§8.2）：子站会话到期、30 秒内没连上、连接失败或断开超过 30 秒时关闭；心跳下发的吊销列表、
//!   退出登录、账户被停用时关闭（[`Rtc::watch`] 监视账户状态）；中转经通道推送的吊销（[`revoke`]）同样处理，
//!   它只用来更快地关闭，不作为放行的依据。连接断开后还保留着的终端会话（[`resume`]）同样关闭。

pub mod bridge;
pub mod frame;
pub mod net;
pub mod outbound;
pub mod peer;
pub mod probe;
pub mod resume;
pub mod signal;
pub mod turn_tcp;

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::{Extension, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use looklook_protocol::rtc::{self as proto, OfferRequest, OfferResponse, RtcGrant};
use tokio_util::sync::CancellationToken;

use crate::account::Account;
use crate::gateway::{remote::Remote, App};

/// 每台设备最多同时保持的连接数（超过时关掉最早的一条）。
const MAX_PEERS: usize = 16;
/// 等候选收集完的上限（中转上的 TURN 分配要一两个往返）。
const GATHER_LIMIT: Duration = Duration::from_secs(3);
/// 建立连接的期限。
const CONNECT_LIMIT: Duration = Duration::from_secs(30);
/// 断开后等它自己恢复的时间。
const DISCONNECT_GRACE: Duration = Duration::from_secs(30);
const MAX_JTIS: usize = 4096;

struct Entry {
    sid: String,
    cancel: CancellationToken,
}

#[derive(Default)]
pub struct Rtc {
    peers: Mutex<HashMap<u64, Entry>>,
    /// 已用的票据 jti → 到期时间（Unix 毫秒）。
    jtis: Mutex<HashMap<String, i64>>,
    next: AtomicU64,
    /// 可续接的终端会话（连接断了还保留一会儿，见 [`resume`]）。
    pub terms: Arc<resume::Terms>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Rtc {
    /// 关闭这些子站会话的全部连接。
    pub fn revoke(&self, sids: &[String]) {
        lock(&self.peers).values().filter(|e| sids.contains(&e.sid)).for_each(|e| e.cancel.cancel());
        self.terms.revoke(sids);
    }

    pub fn close_all(&self) {
        lock(&self.peers).values().for_each(|e| e.cancel.cancel());
        self.terms.close_all();
    }

    pub fn count(&self) -> usize {
        lock(&self.peers).len()
    }

    /// 账户状态变化（心跳的吊销列表、退出登录、停用、到期）时关闭对应的连接。
    pub async fn watch(self: Arc<Self>, account: Arc<Account>) {
        let mut rx = account.subscribe();
        while rx.changed().await.is_ok() {
            if account.session().is_none() || !account.gate().allowed() {
                self.close_all();
                continue;
            }
            let mut sids: Vec<String> = lock(&self.peers).values().map(|e| e.sid.clone()).collect();
            sids.extend(self.terms.sids());
            let revoked: Vec<String> = sids.into_iter().filter(|s| account.is_revoked(s)).collect();
            if !revoked.is_empty() {
                self.revoke(&revoked);
            }
        }
    }

    fn fresh_jti(&self, jti: &str, until: i64, now: i64) -> bool {
        let mut m = lock(&self.jtis);
        if m.get(jti).is_some_and(|&t| t >= now) {
            return false;
        }
        if m.len() >= MAX_JTIS {
            m.retain(|_, t| *t >= now);
            if m.len() >= MAX_JTIS {
                return false;
            }
        }
        m.insert(jti.to_string(), until);
        true
    }

    fn add(&self, sid: &str) -> (u64, CancellationToken) {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let cancel = CancellationToken::new();
        let mut m = lock(&self.peers);
        while m.len() >= MAX_PEERS {
            let Some(&oldest) = m.keys().min() else { break };
            if let Some(e) = m.remove(&oldest) {
                e.cancel.cancel();
            }
        }
        m.insert(id, Entry { sid: sid.to_string(), cancel: cancel.clone() });
        (id, cancel)
    }

    fn remove(&self, id: u64) {
        lock(&self.peers).remove(&id);
    }
}

fn reject(status: StatusCode, why: &'static str) -> Response {
    tracing::debug!(why, "WebRTC 信令被拒绝");
    (status, Json(serde_json::json!({ "error": why }))).into_response()
}

/// 校验授权票据（不含建立连接）：通过时返回票据。
fn check_grant(app: &App, sid: Option<&str>, req: &OfferRequest) -> Result<RtcGrant, (StatusCode, &'static str)> {
    let account = &app.account;
    let session = account.session().ok_or((StatusCode::UNAUTHORIZED, "not_logged_in"))?;
    if !account.gate().allowed() {
        return Err((StatusCode::FORBIDDEN, "locked"));
    }
    let now = account.clock.now_ms();
    let platform = account.platform();
    let g = proto::verify_grant(&req.grant, |kid| platform.key(kid), &session.user.id, &session.device_id, now)
        .map_err(|_| (StatusCode::FORBIDDEN, "grant"))?;
    if g.aud != session.relay.name {
        return Err((StatusCode::FORBIDDEN, "wrong_relay"));
    }
    // 票据与转发它的请求属于同一个子站会话（经信令长连接直接从主服务来的没有这一层凭证）
    if sid.is_some_and(|sid| g.sid != sid) {
        return Err((StatusCode::FORBIDDEN, "wrong_session"));
    }
    if account.is_revoked(&g.sid) {
        return Err((StatusCode::FORBIDDEN, "revoked"));
    }
    if g.abs_exp <= now {
        return Err((StatusCode::FORBIDDEN, "session_expired"));
    }
    if proto::sdp_fingerprint(&req.offer).as_deref() != Some(g.browser_fp.as_str()) {
        return Err((StatusCode::FORBIDDEN, "fingerprint"));
    }
    if !app.rtc.fresh_jti(&g.jti, g.exp + proto::LEEWAY_MS, now) {
        return Err((StatusCode::FORBIDDEN, "replay"));
    }
    Ok(g)
}

/// `POST /_ll/rtc/offer`：主服务经中转转来（旧路径），只接受经远程入口校验过的请求。
pub async fn offer(State(app): State<App>, remote: Option<Extension<Remote>>, _headers: HeaderMap, body: Bytes) -> Response {
    let Some(Extension(remote)) = remote else { return reject(StatusCode::NOT_FOUND, "not_remote") };
    let Ok(req) = serde_json::from_slice::<OfferRequest>(&body) else { return reject(StatusCode::BAD_REQUEST, "body") };
    match accept(&app, Some(&remote.sid), &req).await {
        Ok(r) => Json(r).into_response(),
        Err((s, why)) => reject(s, why),
    }
}

/// 校验 offer 的授权票据、建立 PeerConnection、签 answer。`sid`：经通道来的请求所属的子站会话（票据要属于它）；
/// 经信令长连接（[`signal`]）从主服务直接来的为 None。
pub(crate) async fn accept(app: &App, sid: Option<&str>, req: &OfferRequest) -> Result<OfferResponse, (StatusCode, &'static str)> {
    let g = check_grant(app, sid, req)?;
    let mut pc = peer::Pc::answer(&g.ice, &req.offer).map_err(|e| {
        tracing::warn!(error = %format!("{e:#}"), "WebRTC 建立连接失败");
        (StatusCode::BAD_REQUEST, "offer")
    })?;
    let answer = pc.local_answer(GATHER_LIMIT).await.map_err(|e| {
        tracing::warn!(error = %format!("{e:#}"), "WebRTC 生成 answer 失败");
        (StatusCode::INTERNAL_SERVER_ERROR, "answer")
    })?;
    let (key, sig) = app.account.sign_device(&proto::answer_canonical(&req.grant, &answer)).ok_or((StatusCode::UNAUTHORIZED, "not_logged_in"))?;
    let (id, cancel) = app.rtc.add(&g.sid);
    let ttl = Duration::from_millis((g.abs_exp - app.account.clock.now_ms()).max(0) as u64);
    let ctx = bridge::Ctx {
        router: crate::gateway::router(app.clone()),
        host: g.host.clone(),
        origin: g.origin.clone(),
        sid: g.sid.clone(),
        device: g.dev.clone(),
        terms: app.rtc.terms.clone(),
        expires: tokio::time::Instant::now() + ttl,
    };
    tracing::info!(sid = %g.sid, peers = app.rtc.count(), parked_terms = app.rtc.terms.count(), "WebRTC 连接建立中");
    tokio::spawn(run_peer(app.clone(), id, pc, ctx, cancel, ttl));
    Ok(OfferResponse { answer, key, sig })
}

/// 一条连接的生命周期：把浏览器开的通道交给 [`bridge`]，到期、吊销、失败时关闭。
async fn run_peer(app: App, id: u64, mut pc: peer::Pc, ctx: bridge::Ctx, cancel: CancellationToken, ttl: Duration) {
    let mut tasks = tokio::task::JoinSet::new();
    let expires = tokio::time::sleep(ttl);
    let deadline = tokio::time::sleep(CONNECT_LIMIT);
    tokio::pin!(expires, deadline);
    let mut connected = false;
    let why = loop {
        tokio::select! {
            _ = cancel.cancelled() => break "closed",
            _ = &mut expires => break "session_expired",
            _ = &mut deadline => break if connected { "disconnected" } else { "connect_timeout" },
            ch = pc.incoming.recv() => match ch {
                Some(ch) if ch.label == probe::LABEL => { tasks.spawn(probe::echo(ch)); }
                Some(ch) => { tasks.spawn(bridge::serve(ctx.clone(), ch)); }
                None => break "closed",
            },
            r = pc.state.changed() => {
                if r.is_err() {
                    break "closed";
                }
                let st = *pc.state.borrow_and_update();
                match st {
                    peer::PcState::Connected => {
                        if !connected {
                            let path = pc.path();
                            tracing::info!(sid = %ctx.sid, kind = ?path.as_ref().map(|p| p.kind), local = path.as_ref().map(|p| p.local.as_str()), remote = path.as_ref().map(|p| p.remote.as_str()), "WebRTC 已连接");
                        }
                        connected = true;
                        deadline.as_mut().reset(tokio::time::Instant::now() + Duration::from_secs(86_400 * 365));
                    }
                    peer::PcState::Disconnected => deadline.as_mut().reset(tokio::time::Instant::now() + DISCONNECT_GRACE),
                    peer::PcState::Failed | peer::PcState::Closed => break "failed",
                    _ => {}
                }
            },
            Some(_) = tasks.join_next(), if !tasks.is_empty() => {}
        }
    };
    tracing::info!(sid = %ctx.sid, why, "WebRTC 连接关闭");
    // 先关掉各条通道，再释放连接（rtcDeletePeerConnection 会等回调结束，放到阻塞线程上）。
    tasks.shutdown().await;
    pc.incoming.close();
    let _ = tokio::task::spawn_blocking(move || drop(pc)).await;
    app.rtc.remove(id);
}

/// `POST /_ll/rtc/revoke`：中转推送的吊销（用本设备在当前中转上的网关 MAC 密钥签）。只关闭连接。
pub async fn revoke(State(app): State<App>, headers: HeaderMap, body: Bytes) -> Response {
    let Some(session) = app.account.session() else { return StatusCode::UNAUTHORIZED.into_response() };
    let Some(key) = looklook_protocol::signing::b64_decode(&session.relay.gateway_mac_key).filter(|k| !k.is_empty()) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let mac = headers.get(proto::REVOKE_MAC_HEADER).and_then(|v| v.to_str().ok()).unwrap_or("");
    if !proto::verify_revoke_mac(&key, &body, mac) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let Ok(push) = serde_json::from_slice::<proto::RevokePush>(&body) else { return StatusCode::BAD_REQUEST.into_response() };
    app.rtc.revoke(&push.sids);
    StatusCode::NO_CONTENT.into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jti_once() {
        let r = Rtc::default();
        assert!(r.fresh_jti("a", 100, 0));
        assert!(!r.fresh_jti("a", 100, 50));
        assert!(r.fresh_jti("a", 300, 200), "expired entries can be reused");
    }

    #[test]
    fn peers_are_capped_and_revoked_by_session() {
        let r = Rtc::default();
        let first = r.add("s0").1;
        for i in 1..MAX_PEERS {
            r.add(&format!("s{i}"));
        }
        assert!(!first.is_cancelled());
        let (_, newest) = r.add("s1");
        assert!(first.is_cancelled(), "oldest closed when over the cap");
        assert_eq!(r.count(), MAX_PEERS);
        r.revoke(&["s1".into()]);
        assert!(newest.is_cancelled());
    }
}
