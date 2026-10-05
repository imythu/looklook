//! 账户与设备：登录（单设备）、退出、10 分钟心跳、离线宽限、密钥轮换（详细设计 §4.3–§4.7）。
//!
//! “能不能用”（[`Gate`]）只由平台签名下发的数据和可信时钟决定：
//! 已登录 ∧ 平台允许运行 ∧ 推算平台时间 < 会员到期 ∧ 距上次成功心跳 < 离线宽限。
//! 不能用时拒绝新建/启动/打开终端和本机网页，但不强行结束已经在运行的任务。

use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use ed25519_dalek::SigningKey;
use looklook_protocol::dto::{
    AccountSummary, AuthorizePollRequest, AuthorizeStartRequest, AuthorizeStartResponse, CreateTunnel, DeviceInfo, HeartbeatRequest,
    HeartbeatResponse, LoginResponse, RelayChoices, RelayParams, RenameDevice,
    RotateConfirmRequest, RotateRequest, RotateResponse, SetRelayPreference, TerminalReport, Tunnel, TunnelList, UpdateHint, UpdateTunnel,
};
use looklook_protocol::signing::{self, b64, rotate_canonical};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use tokio::sync::{watch, Notify};

use crate::clock::{Snapshot, TrustedClock};
use crate::paths::{self, Paths};
use crate::platform::{PResult, Platform, PlatformError, Signer};
use crate::settings::Settings;
use crate::store::Store;
use crate::util::{fmt_ms, parse_time_ms, random_bytes, write_private};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// 上报给平台的版本。开发构建可用 `LOOKLOOK_DEV_VERSION` 覆盖，便于对接已发布过更高版本的开发平台。
pub fn client_version() -> String {
    if cfg!(debug_assertions) {
        if let Ok(v) = std::env::var("LOOKLOOK_DEV_VERSION") {
            return v;
        }
    }
    VERSION.to_string()
}
/// 超过离线宽限仍联系不上平台（客户端自己的原因码，不来自平台）。
pub const OFFLINE_TOO_LONG: &str = "OFFLINE_TOO_LONG";

const KV_SESSION: &str = "session";
const KV_SERVER: &str = "server";
const KV_CLOCK: &str = "clock";
const KV_INSTALL_ID: &str = "install_id";
/// 这些错误说明本设备的登录已经失效，只能重新登录。
const ENDED: &[&str] = &["DEVICE_REVOKED", "DEVICE_LOGGED_OUT", "KEY_REVOKED"];
/// 终端列表上报的去抖时间（服务端 docs/05 §5）：连续新建/删除只报最后一次。
pub const REPORT_DEBOUNCE: Duration = Duration::from_millis(300);
/// 平台接受的终端 id 个数上限（超出返回 400）。
const MAX_REPORTED_TERMINALS: usize = 200;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserInfo {
    pub id: String,
    pub username: String,
    pub nickname: String,
    #[serde(default)]
    pub email: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingRotation {
    pub rotation_id: String,
    pub new_key_id: String,
    pub relay_token: String,
}

/// 登录后的本机状态（私钥另存 `device.key`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub server: String,
    pub device_id: String,
    pub key_id: String,
    pub user: UserInfo,
    pub user_host_url: String,
    pub expires_ms: i64,
    pub heartbeat_interval: i64,
    pub offline_grace: i64,
    /// 最近一次成功心跳（或登录）时的平台时间
    pub last_ok_ms: i64,
    pub allow_run: bool,
    pub reason: Option<String>,
    pub relay: RelayParams,
    pub update: Option<UpdateHint>,
    pub rotation: Option<PendingRotation>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ServerConfig {
    pub url: String,
    /// 首次连接时固定下来的平台公钥 `kid → base64url`
    #[serde(default)]
    pub pinned_keys: std::collections::HashMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Gate {
    LoggedOut,
    Allowed,
    Locked(String),
}

impl Gate {
    pub fn allowed(&self) -> bool {
        *self == Gate::Allowed
    }
    pub fn reason(&self) -> Option<&str> {
        match self {
            Gate::LoggedOut => Some("NOT_LOGGED_IN"),
            Gate::Allowed => None,
            Gate::Locked(r) => Some(r),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Notice {
    pub code: String,
    pub params: Map<String, Value>,
}

/// `start_login` 的返回：给界面展示的授权码与授权页地址。
#[derive(Debug, Clone, Serialize)]
pub struct LoginStart {
    pub user_code: String,
    pub verification_url: String,
    pub expires_in: i64,
}

enum FlowState {
    Pending,
    Approved,
    Failed(PlatformError),
}

struct LoginFlow {
    id: u64,
    start: LoginStart,
    deadline: Instant,
    state: FlowState,
    abort: Option<tokio::task::AbortHandle>,
}

pub struct Account {
    store: Arc<Store>,
    paths: Paths,
    official: String,
    builtin_keys: String,
    platform: RwLock<Arc<Platform>>,
    pub clock: Arc<TrustedClock>,
    session: RwLock<Option<Session>>,
    key: RwLock<Option<SigningKey>>,
    notice: RwLock<Option<Notice>>,
    last_error: RwLock<Option<String>>,
    changed: watch::Sender<u64>,
    flow: RwLock<Option<LoginFlow>>,
    wake: Notify,
    busy: tokio::sync::Mutex<()>,
    started: Instant,
    /// 心跳下发的已吊销子站会话（会话凭证的 `sid`），远程入口据此拒绝请求。
    revoked: RwLock<std::collections::HashSet<String>>,
    /// 需要重新上报终端列表（登录、换中转）时加一，见 [`Account::report_terminals_loop`]。
    report: watch::Sender<u64>,
}

fn lock<T>(l: &RwLock<T>) -> std::sync::RwLockReadGuard<'_, T> {
    l.read().unwrap_or_else(|e| e.into_inner())
}
fn lock_mut<T>(l: &RwLock<T>) -> std::sync::RwLockWriteGuard<'_, T> {
    l.write().unwrap_or_else(|e| e.into_inner())
}

impl Account {
    /// `official`：默认服务端地址；`builtin_keys`：该服务端的内置平台公钥（`p1=...`）。
    pub fn new(store: Arc<Store>, paths: Paths, official: &str, builtin_keys: &str) -> anyhow::Result<Arc<Self>> {
        let clock = Arc::new(TrustedClock::restore(store.get::<Snapshot>(KV_CLOCK)?));
        let server = store.get::<ServerConfig>(KV_SERVER)?.unwrap_or(ServerConfig { url: official.to_string(), ..Default::default() });
        let session: Option<Session> = store.get(KV_SESSION)?;
        let key = std::fs::read(paths.device_key()).ok().and_then(|b| <[u8; 32]>::try_from(b.as_slice()).ok()).map(|b| SigningKey::from_bytes(&b));
        // 状态与私钥缺一不可；不一致时按未登录处理（平台上的旧登录需要去网页端下线）。
        let (session, key) = match (session, key) {
            (Some(s), Some(k)) => (Some(s), Some(k)),
            (Some(_), None) => {
                tracing::warn!("本机登录状态缺少私钥，按未登录处理");
                store.remove(KV_SESSION)?;
                (None, None)
            }
            _ => (None, None),
        };
        let platform = Arc::new(Platform::new(&server.url, keys_for(&server, official, builtin_keys), clock.clone())?);
        Ok(Arc::new(Self {
            store,
            paths,
            official: official.to_string(),
            builtin_keys: builtin_keys.to_string(),
            platform: RwLock::new(platform),
            clock,
            session: RwLock::new(session),
            key: RwLock::new(key),
            notice: RwLock::new(None),
            last_error: RwLock::new(None),
            changed: watch::channel(0).0,
            flow: RwLock::new(None),
            wake: Notify::new(),
            busy: tokio::sync::Mutex::new(()),
            started: Instant::now(),
            revoked: RwLock::default(),
            report: watch::channel(0).0,
        }))
    }

    pub fn platform(&self) -> Arc<Platform> {
        lock(&self.platform).clone()
    }

    /// 测试用：直接放入登录状态与吊销列表。
    #[cfg(test)]
    pub(crate) fn set_for_test(&self, s: Session, revoked: &[&str]) {
        self.save_session(Some(s)).unwrap();
        *lock_mut(&self.revoked) = revoked.iter().map(|r| r.to_string()).collect();
    }

    /// 测试用：放入设备私钥（需要真正发签名请求的测试）。
    #[cfg(test)]
    pub(crate) fn set_key_for_test(&self, key: SigningKey) {
        *lock_mut(&self.key) = Some(key);
    }

    /// 子站会话是否已被平台吊销（心跳下发的列表）。
    pub fn is_revoked(&self, sid: &str) -> bool {
        lock(&self.revoked).contains(sid)
    }

    pub fn session(&self) -> Option<Session> {
        lock(&self.session).clone()
    }

    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.changed.subscribe()
    }

    fn bump(&self) {
        self.changed.send_modify(|v| *v += 1);
    }

    pub fn gate(&self) -> Gate {
        let Some(s) = lock(&self.session).clone() else { return Gate::LoggedOut };
        let now = self.clock.now_ms();
        if !s.allow_run {
            return Gate::Locked(s.reason.unwrap_or_else(|| "ACCOUNT_DISABLED".into()));
        }
        if now >= s.expires_ms {
            return Gate::Locked("MEMBERSHIP_EXPIRED".into());
        }
        if now - s.last_ok_ms > s.offline_grace * 1000 {
            return Gate::Locked(OFFLINE_TOO_LONG.into());
        }
        Gate::Allowed
    }

    /// 给界面的状态摘要。
    pub fn status(&self) -> Value {
        let s = self.session();
        let gate = self.gate();
        let now = self.clock.now_ms();
        json!({
            "server": self.platform().base,
            "logged_in": s.is_some(),
            "allowed": gate.allowed(),
            "reason": gate.reason(),
            "notice": *lock(&self.notice),
            "server_time": fmt_ms(now),
            "last_error": *lock(&self.last_error),
            "login": self.login_state(),
            "session": s.map(|s| json!({
                "user": s.user,
                "device_id": s.device_id,
                "user_host_url": s.user_host_url,
                "membership_expires_at": fmt_ms(s.expires_ms),
                "last_heartbeat_at": fmt_ms(s.last_ok_ms),
                "offline_deadline": fmt_ms(s.last_ok_ms + s.offline_grace * 1000),
                "update": s.update,
            })),
        })
    }

    fn creds(&self) -> Option<(Session, SigningKey)> {
        Some((lock(&self.session).clone()?, lock(&self.key).clone()?))
    }

    fn save_session(&self, s: Option<Session>) -> anyhow::Result<()> {
        match &s {
            Some(s) => self.store.set(KV_SESSION, s)?,
            None => self.store.remove(KV_SESSION)?,
        }
        *lock_mut(&self.session) = s;
        self.bump();
        Ok(())
    }

    fn update_session(&self, f: impl FnOnce(&mut Session)) -> anyhow::Result<()> {
        let mut s = match self.session() {
            Some(s) => s,
            None => return Ok(()),
        };
        f(&mut s);
        self.save_session(Some(s))
    }

    pub fn persist_clock(&self) {
        if let Some(snap) = self.clock.snapshot() {
            if let Err(e) = self.store.set(KV_CLOCK, &snap) {
                tracing::warn!(error = %e, "保存可信时间失败");
            }
        }
    }

    fn install_id(&self) -> String {
        if let Ok(Some(id)) = self.store.get::<String>(KV_INSTALL_ID) {
            return id;
        }
        let id = hex_id();
        let _ = self.store.set(KV_INSTALL_ID, &id);
        id
    }

    // ---------------- 服务端地址 ----------------

    /// 切换服务端（仅未登录时）；公钥在下次登录时重新固定。
    pub fn set_server(&self, url: &str) -> anyhow::Result<()> {
        anyhow::ensure!(self.session().is_none(), "请先退出登录");
        self.cancel_login();
        let url = url.trim().trim_end_matches('/');
        let parsed = reqwest::Url::parse(url).map_err(|_| anyhow::anyhow!("地址格式不对"))?;
        anyhow::ensure!(matches!(parsed.scheme(), "https" | "http"), "地址需要以 https:// 开头");
        let cfg = ServerConfig { url: url.to_string(), pinned_keys: Default::default() };
        self.store.set(KV_SERVER, &cfg)?;
        *lock_mut(&self.platform) = Arc::new(Platform::new(url, keys_for(&cfg, &self.official, &self.builtin_keys), self.clock.clone())?);
        self.clock.clear();
        self.store.remove(KV_CLOCK)?;
        self.bump();
        Ok(())
    }

    // ---------------- 登录 / 退出 ----------------

    /// 首次连接该服务端时固定平台公钥。
    async fn ensure_keys(&self) -> PResult<()> {
        let platform = self.platform();
        if !platform.has_keys() {
            let pinned = platform.pin_keys_from_well_known().await?;
            tracing::warn!(server = %platform.base, "首次连接该服务端，已固定平台公钥：{:?}", pinned.keys().collect::<Vec<_>>());
            let cfg = ServerConfig { url: platform.base.clone(), pinned_keys: pinned };
            self.store.set(KV_SERVER, &cfg).map_err(internal)?;
        }
        Ok(())
    }

    /// 发起浏览器授权登录：生成密钥、向平台申请授权码，然后在后台轮询直到批准 / 拒绝 / 过期。
    pub async fn start_login(self: &Arc<Self>) -> PResult<LoginStart> {
        if self.session().is_some() {
            return Err(api("ALREADY_LOGGED_IN"));
        }
        self.cancel_login();
        self.ensure_keys().await?;
        let platform = self.platform();
        let key = SigningKey::from_bytes(&random_bytes::<32>());
        let settings = Settings::load(&self.store);
        let req = AuthorizeStartRequest {
            public_key: b64(key.verifying_key().as_bytes()),
            device: DeviceInfo {
                install_id: self.install_id(),
                name: settings.device_name(),
                os: paths::os_name().into(),
                arch: std::env::consts::ARCH.into(),
                client_version: client_version(),
                target: paths::target().into(),
                locale: Some("zh-CN".into()),
            },
        };
        let r: AuthorizeStartResponse = platform.call_unsigned("/device/authorize/start", &req).await?;
        let start = LoginStart { user_code: r.user_code, verification_url: r.verification_url, expires_in: r.expires_in };
        let deadline = Instant::now() + Duration::from_secs(r.expires_in.max(30) as u64 + 5);
        let id = {
            let mut flow = lock_mut(&self.flow);
            let id = flow.as_ref().map_or(0, |f| f.id) + 1;
            *flow = Some(LoginFlow { id, start: start.clone(), deadline, state: FlowState::Pending, abort: None });
            id
        };
        let me = self.clone();
        let interval = Duration::from_secs(r.interval.clamp(1, 30) as u64);
        let task = tokio::spawn(async move { me.run_login_flow(id, key, r.poll_token, interval, deadline).await });
        if let Some(f) = lock_mut(&self.flow).as_mut().filter(|f| f.id == id) {
            f.abort = Some(task.abort_handle());
        }
        self.bump();
        Ok(start)
    }

    /// 取消进行中的授权（换服务端、重试、用户点取消时）。
    pub fn cancel_login(&self) {
        if let Some(f) = lock_mut(&self.flow).take() {
            if let Some(a) = f.abort {
                a.abort();
            }
            self.bump();
        }
    }

    fn set_flow_state(&self, id: u64, state: FlowState) {
        if let Some(f) = lock_mut(&self.flow).as_mut().filter(|f| f.id == id) {
            f.state = state;
        }
        self.bump();
    }

    async fn run_login_flow(self: Arc<Self>, id: u64, key: SigningKey, poll_token: String, interval: Duration, deadline: Instant) {
        let req = AuthorizePollRequest { poll_token };
        loop {
            tokio::time::sleep(interval).await;
            if Instant::now() > deadline {
                self.set_flow_state(id, FlowState::Failed(api("AUTHORIZATION_EXPIRED")));
                return;
            }
            let platform = self.platform();
            match platform.call_unsigned::<LoginResponse>("/device/authorize/poll", &req).await {
                Ok(r) => {
                    let _g = self.busy.lock().await;
                    let state = match self.complete_login(&platform, key, r) {
                        Ok(()) => FlowState::Approved,
                        Err(e) => FlowState::Failed(e),
                    };
                    self.set_flow_state(id, state);
                    return;
                }
                // 还没批准，或暂时连不上：继续等。
                Err(e) if e.is("AUTHORIZATION_PENDING") || e.is("NETWORK") => {}
                Err(e) => {
                    self.set_flow_state(id, FlowState::Failed(e));
                    return;
                }
            }
        }
    }

    /// 授权登录进度，给界面用。
    pub fn login_state(&self) -> Value {
        let flow = lock(&self.flow);
        let Some(f) = flow.as_ref() else { return json!({ "state": "idle" }) };
        let (state, error) = match &f.state {
            FlowState::Pending if Instant::now() > f.deadline => ("failed", Some(api("AUTHORIZATION_EXPIRED"))),
            FlowState::Pending => ("pending", None),
            FlowState::Approved => ("approved", None),
            FlowState::Failed(e) => ("failed", Some(e.clone())),
        };
        json!({
            "state": state,
            "user_code": f.start.user_code,
            "verification_url": f.start.verification_url,
            "expires_in": f.deadline.saturating_duration_since(Instant::now()).as_secs(),
            "error": error.map(|e| match e {
                PlatformError::Api { code, params, .. } => json!({ "code": code, "params": params }),
                PlatformError::Network(_) => json!({ "code": "NETWORK", "params": {} }),
            }),
        })
    }

    /// 批准后落地本机登录状态（调用方持有 `busy` 锁）。
    fn complete_login(&self, platform: &Arc<Platform>, key: SigningKey, r: LoginResponse) -> PResult<()> {
        let server_ms = parse_time_ms(&r.server_time).ok_or_else(|| PlatformError::Network("平台时间格式错误".into()))?;
        platform.clock.set(server_ms);
        write_private(&self.paths.device_key(), &key.to_bytes()).map_err(internal)?;
        let _ = std::fs::remove_file(self.paths.pending_key());
        *lock_mut(&self.key) = Some(key);
        *lock_mut(&self.notice) = None;
        *lock_mut(&self.last_error) = None;
        self.save_session(Some(Session {
            server: platform.base.clone(),
            device_id: r.device_id,
            key_id: r.key_id,
            user: UserInfo { id: r.user.id, username: r.user.username, nickname: r.user.nickname, email: None },
            user_host_url: r.user_host_url,
            expires_ms: parse_time_ms(&r.membership_expires_at).unwrap_or(server_ms),
            heartbeat_interval: r.heartbeat_interval_seconds.max(60),
            offline_grace: r.offline_grace_seconds.max(60),
            last_ok_ms: server_ms,
            allow_run: true,
            reason: None,
            relay: r.relay,
            update: None,
            rotation: None,
        }))
        .map_err(internal)?;
        self.persist_clock();
        self.request_terminal_report();
        tracing::info!("已登录看看服务端");
        Ok(())
    }

    /// 退出登录。`force`：连不上平台时也清除本机登录（平台上的登录需要到网页端下线）。
    pub async fn logout(&self, force: bool) -> PResult<()> {
        let _g = self.busy.lock().await;
        let Some((s, key)) = self.creds() else { return Ok(()) };
        let r = self
            .platform()
            .call_empty("POST", "/auth/logout", Signer { device: &s.device_id, key_id: &s.key_id, key: &key }, None::<&()>)
            .await;
        match r {
            Ok(()) => {}
            Err(e) if ENDED.iter().any(|c| e.is(c)) => {}
            Err(e) if !force => return Err(e),
            Err(e) => tracing::warn!(error = %e, "平台退出失败，只清除本机登录"),
        }
        self.clear_local(None);
        Ok(())
    }

    fn clear_local(&self, notice: Option<Notice>) {
        let _ = std::fs::remove_file(self.paths.device_key());
        let _ = std::fs::remove_file(self.paths.pending_key());
        *lock_mut(&self.key) = None;
        *lock_mut(&self.flow) = None;
        *lock_mut(&self.notice) = notice;
        if let Err(e) = self.save_session(None) {
            tracing::error!(error = %e, "清除登录状态失败");
        }
    }

    // ---------------- 心跳 ----------------

    /// 后台任务：每个心跳间隔发送一次；失败时退避重试，直到离线宽限到期。
    pub async fn heartbeat_loop(self: Arc<Self>) {
        let mut changed = self.subscribe();
        let mut failures = 0u32;
        // 启动时若有未确认的密钥轮换，先接着完成。
        if self.session().and_then(|s| s.rotation).is_some() {
            let _g = self.busy.lock().await;
            if let Err(e) = self.confirm_rotation().await {
                tracing::warn!(error = %e, "继续密钥轮换失败");
            }
        }
        loop {
            if self.session().is_none() {
                failures = 0;
                if changed.changed().await.is_err() {
                    return;
                }
                continue;
            }
            let wait = match self.heartbeat_once().await {
                Ok(next) => {
                    failures = 0;
                    Duration::from_secs(next.clamp(30, 3600))
                }
                Err(PlatformError::Api { code, params, .. }) if code == "RATE_LIMITED" => {
                    Duration::from_secs(params.get("retry_after_seconds").and_then(Value::as_u64).unwrap_or(30).clamp(5, 600))
                }
                Err(e) => {
                    failures += 1;
                    tracing::warn!(error = %e, failures, "心跳失败");
                    let interval = self.session().map(|s| s.heartbeat_interval as u64).unwrap_or(600);
                    Duration::from_secs((15u64 << failures.min(6)).min(interval))
                }
            };
            // 心跳本身也会更新会话并触发 changed；只有登录的设备变了（退出、重新登录）才提前醒来。
            let deadline = tokio::time::Instant::now() + wait;
            let device = self.session().map(|s| s.device_id);
            loop {
                tokio::select! {
                    _ = tokio::time::sleep_until(deadline) => break,
                    _ = self.wake.notified() => break,
                    r = changed.changed() => {
                        if r.is_err() {
                            return;
                        }
                        if self.session().map(|s| s.device_id) != device {
                            break;
                        }
                    }
                }
            }
        }
    }

    /// 立即发送一次心跳（例如界面上点“重新连接”）。
    pub fn wake(&self) {
        self.wake.notify_one();
    }

    async fn heartbeat_once(&self) -> PResult<u64> {
        let _g = self.busy.lock().await;
        let Some((s, key)) = self.creds() else { return Ok(60) };
        let req = self.heartbeat_request();
        let platform = self.platform();
        let signer = Signer { device: &s.device_id, key_id: &s.key_id, key: &key };
        let r: HeartbeatResponse = match platform.call("POST", "/heartbeat", signer, Some(&req)).await {
            Ok(r) => r,
            Err(e) => {
                self.on_error(&e);
                return Err(e);
            }
        };
        let server_ms = parse_time_ms(&r.server_time).ok_or_else(|| PlatformError::Network("平台时间格式错误".into()))?;
        platform.clock.set(server_ms);
        tracing::debug!(allow_run = r.allow_run, server_time = %r.server_time, "心跳成功");
        *lock_mut(&self.last_error) = None;
        self.update_session(|s| {
            s.last_ok_ms = server_ms;
            s.expires_ms = parse_time_ms(&r.membership_expires_at).unwrap_or(s.expires_ms);
            s.allow_run = r.allow_run;
            s.reason = r.reason.clone();
            s.heartbeat_interval = r.next_heartbeat_seconds.max(60);
            s.offline_grace = r.offline_grace_seconds.max(60);
            s.update = r.update.clone();
        })
        .map_err(internal)?;
        self.persist_clock();
        // 升级前保存的通道参数没有中转名与网关 MAC 密钥，远程入口无法校验，重新获取一次。
        if r.relay_changed || s.relay.gateway_mac_key.is_empty() {
            self.refresh_relay(&platform, signer).await?;
        }
        if r.rotate_key {
            if let Err(e) = self.rotate_key().await {
                tracing::warn!(error = %e, "密钥轮换失败，下次心跳重试");
            }
        }
        *lock_mut(&self.revoked) = r.revoked_sids.into_iter().collect();
        // 在看看网页上改了电脑名称：同步到本机设置（与心跳同在 busy 锁里，本机同时改名时以本机为准）。
        if let Some(name) = r.device_name.filter(|n| !n.is_empty()) {
            let mut settings = Settings::load(&self.store);
            if name != settings.device_name() {
                tracing::info!(%name, "电脑名称已在网页上修改，同步到本机");
                settings.device_name = if name == crate::util::host_name() { String::new() } else { name };
                settings.save(&self.store).map_err(internal)?;
            }
        }
        Ok(r.next_heartbeat_seconds.max(60) as u64)
    }

    /// 重新获取通道参数；会话一变，`relay::run` 就用新参数重连。
    async fn refresh_relay(&self, platform: &Platform, signer: Signer<'_>) -> PResult<()> {
        match platform.call::<RelayParams>("GET", "/relay", signer, None::<&()>).await {
            Ok(relay) => {
                self.update_session(|s| s.relay = relay).map_err(internal)?;
                // 换了中转：新中转上的路由表要尽快有本机的终端
                self.request_terminal_report();
            }
            Err(e) => tracing::warn!(error = %e, "获取新的远程访问参数失败"),
        }
        Ok(())
    }

    fn heartbeat_request(&self) -> HeartbeatRequest {
        let settings = Settings::load(&self.store);
        HeartbeatRequest {
            client_version: client_version(),
            target: paths::target().into(),
            channel: "stable".into(),
            device_name: Some(settings.device_name()),
            uptime_seconds: Some(self.started.elapsed().as_secs() as i64),
            // 每次都带全量终端列表，作为单独上报失败时的兜底；读不到本机列表时不更新（None）
            terminals: self.terminal_ids().ok(),
        }
    }

    // ---------------- 终端列表上报（多设备路由表，服务端 docs/05 §5） ----------------

    /// 本机全部终端 id（平台只接受 `[a-z0-9]{1,32}`，最多 200 个）。
    pub fn terminal_ids(&self) -> anyhow::Result<Vec<String>> {
        let ids: Vec<String> = self.store.instances()?.into_iter().map(|r| r.id).filter(|id| valid_terminal_id(id)).collect();
        if ids.len() > MAX_REPORTED_TERMINALS {
            tracing::warn!(count = ids.len(), "终端太多，只上报前 {MAX_REPORTED_TERMINALS} 个");
        }
        Ok(ids.into_iter().take(MAX_REPORTED_TERMINALS).collect())
    }

    /// 登录、换中转等之后要求重新上报一次（去抖后发送）。
    pub fn request_terminal_report(&self) {
        self.report.send_modify(|v| *v += 1);
    }

    /// `PUT /terminals`：上报本机全部终端 id。未登录时什么也不做。
    pub async fn report_terminals(&self) -> PResult<()> {
        let _g = self.busy.lock().await;
        let Some((s, key)) = self.creds() else { return Ok(()) };
        let ids = self.terminal_ids().map_err(internal)?;
        let r = self
            .platform()
            .call_empty("PUT", "/terminals", Signer { device: &s.device_id, key_id: &s.key_id, key: &key }, Some(&TerminalReport { ids }))
            .await;
        // 只处理“登录已失效”；其他失败不影响界面上的连接状态（心跳会兜底）。
        if let Err(e) = &r {
            if ENDED.iter().any(|c| e.is(c)) {
                self.on_error(e);
            }
        }
        r
    }

    /// 后台任务：终端新建/删除（`instances` 订阅）或登录、换中转后，去抖 300 ms 上报终端列表；启动时也报一次。
    /// 失败只记日志，网络类错误退避重试；心跳每次也带全量列表。
    pub async fn report_terminals_loop(self: Arc<Self>, instances: watch::Receiver<u64>) {
        let triggers = Triggers { a: self.report.subscribe(), b: instances };
        report_loop(triggers, REPORT_DEBOUNCE, || {
            let me = self.clone();
            async move { me.report_terminals().await }
        })
        .await
    }

    fn on_error(&self, e: &PlatformError) {
        if ENDED.iter().any(|c| e.is(c)) {
            let params = match e {
                PlatformError::Api { params, .. } => params.clone(),
                _ => Map::new(),
            };
            tracing::warn!(code = e.code(), "本机登录已在平台失效");
            self.clear_local(Some(Notice { code: e.code().to_string(), params }));
        } else {
            *lock_mut(&self.last_error) = Some(e.code().to_string());
        }
    }

    // ---------------- 密钥轮换（§4.7） ----------------

    async fn rotate_key(&self) -> PResult<()> {
        let Some((s, key)) = self.creds() else { return Ok(()) };
        let new_key = SigningKey::from_bytes(&random_bytes::<32>());
        let new_pub = b64(new_key.verifying_key().as_bytes());
        let device = s.device_id.clone();
        let body = |nonce: &str| {
            let proof = signing::sign(&new_key, &rotate_canonical(&device, &new_pub, nonce));
            Some(serde_json::to_vec(&RotateRequest { new_public_key: new_pub.clone(), proof }).unwrap_or_default())
        };
        let signer = Signer { device: &s.device_id, key_id: &s.key_id, key: &key };
        let (_, bytes) = self.platform().call_raw("POST", "/keys/rotate", signer, &body).await?;
        let r: RotateResponse = serde_json::from_slice(&bytes).map_err(|e| PlatformError::Network(e.to_string()))?;
        // 新私钥与新通道令牌先落盘，旧的暂不删除：任何一步失败都还能用旧密钥。
        write_private(&self.paths.pending_key(), &new_key.to_bytes()).map_err(internal)?;
        self.update_session(|s| {
            s.rotation = Some(PendingRotation { rotation_id: r.rotation_id.clone(), new_key_id: r.new_key_id.clone(), relay_token: r.relay_token.clone() })
        })
        .map_err(internal)?;
        self.confirm_rotation().await
    }

    async fn confirm_rotation(&self) -> PResult<()> {
        let Some(s) = self.session() else { return Ok(()) };
        let Some(rot) = s.rotation.clone() else { return Ok(()) };
        let new_key = match std::fs::read(self.paths.pending_key()).ok().and_then(|b| <[u8; 32]>::try_from(b.as_slice()).ok()) {
            Some(b) => SigningKey::from_bytes(&b),
            None => {
                self.update_session(|s| s.rotation = None).map_err(internal)?;
                return Ok(());
            }
        };
        let signer = Signer { device: &s.device_id, key_id: &rot.new_key_id, key: &new_key };
        let r = self
            .platform()
            .call_empty("POST", "/keys/rotate/confirm", signer, Some(&RotateConfirmRequest { rotation_id: rot.rotation_id.clone() }))
            .await;
        match r {
            Ok(()) => {
                std::fs::rename(self.paths.pending_key(), self.paths.device_key()).map_err(internal)?;
                *lock_mut(&self.key) = Some(new_key);
                self.update_session(|s| {
                    s.key_id = rot.new_key_id.clone();
                    s.relay.token = rot.relay_token.clone();
                    s.rotation = None;
                })
                .map_err(internal)?;
                tracing::info!("设备密钥已轮换");
                Ok(())
            }
            Err(e) if e.is("KEY_ROTATION_NOT_FOUND") || e.is("KEY_REVOKED") => {
                // 待生效密钥已过期或被替代：丢弃，旧密钥继续有效。
                let _ = std::fs::remove_file(self.paths.pending_key());
                self.update_session(|s| s.rotation = None).map_err(internal)?;
                Err(e)
            }
            Err(e) => Err(e),
        }
    }

    // ---------------- 其他签名接口 ----------------

    async fn signed<T: serde::de::DeserializeOwned>(&self, method: &str, path: &str, body: Option<&(impl Serialize + Sync)>) -> PResult<T> {
        let (s, key) = self.creds().ok_or_else(|| api("NOT_LOGGED_IN"))?;
        let r = self.platform().call(method, path, Signer { device: &s.device_id, key_id: &s.key_id, key: &key }, body).await;
        if let Err(e) = &r {
            self.on_error(e);
        }
        r
    }

    pub async fn account(&self) -> PResult<AccountSummary> {
        let a: AccountSummary = self.signed("GET", "/account", None::<&()>).await?;
        self.update_session(|s| {
            s.user.nickname = a.nickname.clone();
            s.user.email = Some(a.email.clone());
            s.user_host_url = a.user_host_url.clone();
            if let Some(t) = parse_time_ms(&a.membership_expires_at) {
                s.expires_ms = t;
            }
        })
        .map_err(internal)?;
        Ok(a)
    }

    /// 保存本机设置；电脑名称变了时立即告诉平台（远程入口的电脑列表马上显示新名称）。
    /// 在 busy 锁里保存：进行中的心跳带回的旧名称不会覆盖刚改的名称。通知失败不要紧，下一次心跳会带上新名称。
    pub async fn save_settings(&self, settings: &Settings) -> anyhow::Result<()> {
        let renamed = {
            let _g = self.busy.lock().await;
            let old = Settings::load(&self.store).device_name();
            settings.save(&self.store)?;
            old != settings.device_name()
        };
        if renamed && self.creds().is_some() {
            let body = RenameDevice { name: settings.device_name() };
            if let Err(e) = self.signed::<Value>("PUT", "/device/name", Some(&body)).await {
                tracing::warn!(error = %e, "电脑名称同步到平台失败，下次心跳再同步");
            }
        }
        Ok(())
    }

    /// 可选的线路（中转）与当前所在、用户自选的线路。
    pub async fn relays(&self) -> PResult<RelayChoices> {
        self.signed("GET", "/relays", None::<&()>).await
    }

    /// 记住用户选的线路（None = 自动）。平台立即改派，这里马上换用新参数，不等下一次心跳。
    /// `auto`：后台测速自动选的（平台不会用它覆盖用户手动选的线路）。
    pub async fn set_relay(&self, relay: Option<String>, auto: bool) -> PResult<RelayChoices> {
        let r: RelayChoices = self.signed("PUT", "/relays/preference", Some(&SetRelayPreference { relay, auto })).await?;
        let _g = self.busy.lock().await;
        if let Some((s, key)) = self.creds() {
            if r.current.as_ref().is_some_and(|c| *c != s.relay.name) {
                let platform = self.platform();
                self.refresh_relay(&platform, Signer { device: &s.device_id, key_id: &s.key_id, key: &key }).await?;
            }
        }
        Ok(r)
    }

    pub async fn tunnels(&self) -> PResult<TunnelList> {
        self.signed("GET", "/tunnels", None::<&()>).await
    }

    /// 问题报告（设置 → 问题与日志）。
    pub async fn report(&self, body: &Value) -> PResult<Value> {
        self.signed("POST", "/reports", Some(body)).await
    }

    pub async fn create_tunnel(&self, req: &CreateTunnel) -> PResult<Tunnel> {
        self.signed("POST", "/tunnels", Some(req)).await
    }

    pub async fn update_tunnel(&self, id: &str, req: &UpdateTunnel) -> PResult<Tunnel> {
        self.signed("PATCH", &format!("/tunnels/{}", path_seg(id)), Some(req)).await
    }

    pub async fn delete_tunnel(&self, id: &str) -> PResult<()> {
        let (s, key) = self.creds().ok_or_else(|| api("NOT_LOGGED_IN"))?;
        let r = self
            .platform()
            .call_empty("DELETE", &format!("/tunnels/{}", path_seg(id)), Signer { device: &s.device_id, key_id: &s.key_id, key: &key }, None::<&()>)
            .await;
        if let Err(e) = &r {
            self.on_error(e);
        }
        r
    }
}

fn keys_for(server: &ServerConfig, official: &str, builtin: &str) -> std::collections::HashMap<String, ed25519_dalek::VerifyingKey> {
    let pinned: std::collections::HashMap<_, _> = server
        .pinned_keys
        .iter()
        .filter_map(|(k, v)| signing::parse_public_key(v).map(|vk| (k.clone(), vk)))
        .collect();
    if !pinned.is_empty() {
        return pinned;
    }
    if server.url.trim_end_matches('/') == official.trim_end_matches('/') {
        return crate::platform::parse_key_list(builtin);
    }
    Default::default()
}

fn valid_terminal_id(id: &str) -> bool {
    (1..=32).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
}

/// 两个“有变化”的来源（本账户的请求、终端列表）。任一来源关闭即返回 false。
pub(crate) struct Triggers {
    pub a: watch::Receiver<u64>,
    pub b: watch::Receiver<u64>,
}

impl Triggers {
    async fn changed(&mut self) -> bool {
        tokio::select! {
            r = self.a.changed() => r.is_ok(),
            r = self.b.changed() => r.is_ok(),
        }
    }

    /// 等到连续 `quiet` 时间内没有新的变化。
    async fn settle(&mut self, quiet: Duration) -> bool {
        loop {
            match tokio::time::timeout(quiet, self.changed()).await {
                Err(_) => return true,
                Ok(true) => continue,
                Ok(false) => return false,
            }
        }
    }
}

/// 连不上、平台临时出错、限流：值得稍后重试。其他错误（如参数不对）等下一次变化再说。
fn retryable(e: &PlatformError) -> bool {
    match e {
        PlatformError::Network(_) => true,
        PlatformError::Api { status, code, .. } => *status >= 500 || code == "RATE_LIMITED",
    }
}

/// 去抖上报循环：启动时先报一次，之后每次变化后等 `quiet` 无新变化再报；
/// 可重试的失败按 30 秒起、最长 10 分钟退避重试，期间有新变化就立即（去抖后）重报。
pub(crate) async fn report_loop<F, Fut>(mut triggers: Triggers, quiet: Duration, mut send: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = PResult<()>>,
{
    let mut pending = true;
    let mut failures = 0u32;
    loop {
        if !pending && !triggers.changed().await {
            return;
        }
        if !triggers.settle(quiet).await {
            return;
        }
        pending = false;
        match send().await {
            Ok(()) => failures = 0,
            Err(e) if retryable(&e) => {
                failures += 1;
                let wait = Duration::from_secs((30u64 << (failures - 1).min(5)).min(600));
                tracing::warn!(error = %e, failures, "上报终端列表失败，{} 秒后重试", wait.as_secs());
                tokio::select! {
                    _ = tokio::time::sleep(wait) => {}
                    ok = triggers.changed() => if !ok { return },
                }
                pending = true;
            }
            Err(e) => {
                failures = 0;
                tracing::warn!(error = %e, "上报终端列表失败");
            }
        }
    }
}

fn path_seg(s: &str) -> String {
    s.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').collect()
}

fn hex_id() -> String {
    random_bytes::<16>().iter().map(|b| format!("{b:02x}")).collect()
}

pub fn api(code: &str) -> PlatformError {
    PlatformError::Api { status: 400, code: code.into(), params: Map::new() }
}

fn internal(e: impl std::fmt::Display) -> PlatformError {
    tracing::error!(error = %e, "本机状态保存失败");
    PlatformError::Api { status: 500, code: "INTERNAL".into(), params: Map::new() }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Mutex;

    use axum::body::Bytes;
    use axum::http::{HeaderMap, Method, StatusCode, Uri};
    use axum::response::IntoResponse;
    use looklook_protocol::signing::{response_canonical, HDR_NONCE, HDR_PLATFORM_KEY, HDR_PLATFORM_SIGNATURE};

    use super::*;
    use crate::store::InstanceRow;

    const NOW: i64 = 1_800_000_000_000;

    fn platform_key() -> SigningKey {
        SigningKey::from_bytes(&[5u8; 32])
    }

    fn row(id: &str, port: u16) -> InstanceRow {
        InstanceRow {
            id: id.into(),
            name: id.into(),
            workdir: "/tmp".into(),
            launch: "shell".into(),
            command: String::new(),
            shell: String::new(),
            auto_start: false,
            want_running: false,
            port,
            created_at: format!("2026-01-01T00:00:0{}Z", port % 10),
            updated_at: String::new(),
        }
    }

    /// 已登录（带私钥）的账户，服务端指向 `server`。
    fn account(dir: &std::path::Path, server: &str) -> Arc<Account> {
        let store = Arc::new(Store::memory().unwrap());
        store.set(KV_SERVER, &ServerConfig { url: server.into(), pinned_keys: [("p1".to_string(), b64(platform_key().verifying_key().as_bytes()))].into() }).unwrap();
        let paths = Paths { home: dir.to_path_buf(), ttyd: None, mux: None, fonts: None, trzsz: None };
        let a = Account::new(store, paths, "https://looklook.test", "").unwrap();
        a.clock.set(NOW);
        a.set_for_test(
            Session {
                server: server.into(),
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
                    name: "r1".into(),
                    server_addr: "r1.looklook.test:2333".into(),
                    transport: "noise".into(),
                    noise_remote_public_key: "x".into(),
                    service_name: "d1".into(),
                    token: "t".into(),
                    gateway_mac_key: "x".into(),
                },
                update: None,
                rotation: None,
            },
            &[],
        );
        a.set_key_for_test(SigningKey::from_bytes(&[7u8; 32]));
        a
    }

    type Seen = Arc<Mutex<Vec<(String, String, Value)>>>;

    /// 假平台：记下每个请求（方法、路径、JSON），按 LL1-RESP 签名应答。
    async fn fake_platform() -> (String, Seen) {
        let seen: Seen = Arc::default();
        let log = seen.clone();
        let app = axum::Router::new().fallback(move |method: Method, uri: Uri, headers: HeaderMap, body: Bytes| {
            let log = log.clone();
            async move {
                let path = uri.path().to_string();
                log.lock().unwrap().push((method.to_string(), path.clone(), serde_json::from_slice(&body).unwrap_or(Value::Null)));
                let (status, out) = if path.ends_with("/heartbeat") {
                    let r = HeartbeatResponse {
                        allow_run: true,
                        reason: None,
                        server_time: fmt_ms(NOW),
                        membership_expires_at: fmt_ms(NOW * 2),
                        next_heartbeat_seconds: 600,
                        offline_grace_seconds: 600,
                        rotate_key: false,
                        update: None,
                        relay_changed: false,
                        revoked_sids: vec![],
                        device_name: None,
                    };
                    (StatusCode::OK, serde_json::to_vec(&r).unwrap())
                } else {
                    (StatusCode::NO_CONTENT, vec![])
                };
                let nonce = headers.get(HDR_NONCE).and_then(|v| v.to_str().ok()).unwrap_or_default().to_string();
                let sig = signing::sign(&platform_key(), &response_canonical(&nonce, status.as_u16(), &out));
                (status, [(HDR_PLATFORM_KEY, "p1".to_string()), (HDR_PLATFORM_SIGNATURE, sig), ("content-type", "application/json".to_string())], out).into_response()
            }
        });
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", l.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
        (url, seen)
    }

    #[test]
    fn terminal_id_rules() {
        assert!(valid_terminal_id("abcd2345"));
        assert!(!valid_terminal_id(""));
        assert!(!valid_terminal_id("ABCD"));
        assert!(!valid_terminal_id("a-b"));
        assert!(!valid_terminal_id(&"a".repeat(33)));
    }

    #[tokio::test]
    async fn heartbeat_and_report_carry_all_terminals() {
        let dir = tempfile::tempdir().unwrap();
        let (url, seen) = fake_platform().await;
        let a = account(dir.path(), &url);
        a.store.insert_instance(&row("aaaa2222", 41001)).unwrap();
        a.store.insert_instance(&row("bbbb3333", 41002)).unwrap();

        // 心跳请求体带全量终端列表
        assert_eq!(a.heartbeat_request().terminals, Some(vec!["aaaa2222".to_string(), "bbbb3333".to_string()]));
        a.heartbeat_once().await.unwrap();
        // 单独上报：PUT /api/client/v1/terminals {ids}
        a.report_terminals().await.unwrap();

        let seen = seen.lock().unwrap().clone();
        assert_eq!(seen.len(), 2);
        assert_eq!((seen[0].0.as_str(), seen[0].1.as_str()), ("POST", "/api/client/v1/heartbeat"));
        assert_eq!(seen[0].2["terminals"], json!(["aaaa2222", "bbbb3333"]));
        assert_eq!((seen[1].0.as_str(), seen[1].1.as_str()), ("PUT", "/api/client/v1/terminals"));
        assert_eq!(seen[1].2, json!({ "ids": ["aaaa2222", "bbbb3333"] }));
    }

    #[tokio::test]
    async fn report_failure_is_harmless() {
        let dir = tempfile::tempdir().unwrap();
        // 没有服务在听的端口：网络错误，只返回错误，不改动登录状态与界面上的连接错误
        let a = account(dir.path(), "http://127.0.0.1:9");
        assert!(a.report_terminals().await.unwrap_err().is("NETWORK"));
        assert!(a.session().is_some());
        assert!(lock(&a.last_error).is_none());
    }

    #[tokio::test]
    async fn report_terminals_skipped_when_logged_out() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::memory().unwrap());
        let paths = Paths { home: dir.path().to_path_buf(), ttyd: None, mux: None, fonts: None, trzsz: None };
        let a = Account::new(store, paths, "http://127.0.0.1:9", "").unwrap();
        a.report_terminals().await.unwrap();
    }

    /// 去抖：启动报一次；连续 5 次变化只报一次；可重试的失败 30 秒后重报。
    #[tokio::test(start_paused = true)]
    async fn report_loop_debounces_and_retries() {
        let own = watch::channel(0u64).0;
        let list = watch::channel(0u64).0;
        let triggers = Triggers { a: own.subscribe(), b: list.subscribe() };
        let sent = Arc::new(AtomicU32::new(0));
        let fail_next = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (s, f) = (sent.clone(), fail_next.clone());
        tokio::spawn(report_loop(triggers, REPORT_DEBOUNCE, move || {
            let (s, f) = (s.clone(), f.clone());
            async move {
                s.fetch_add(1, Ordering::SeqCst);
                if f.swap(false, Ordering::SeqCst) {
                    Err(PlatformError::Network("down".into()))
                } else {
                    Ok(())
                }
            }
        }));
        let step = |ms| tokio::time::sleep(Duration::from_millis(ms));

        step(200).await;
        assert_eq!(sent.load(Ordering::SeqCst), 0, "去抖时间内不发");
        step(200).await;
        assert_eq!(sent.load(Ordering::SeqCst), 1, "启动时报一次");

        for _ in 0..5 {
            list.send_modify(|v| *v += 1);
            step(100).await;
        }
        assert_eq!(sent.load(Ordering::SeqCst), 1, "还在连续变化，不发");
        step(400).await;
        assert_eq!(sent.load(Ordering::SeqCst), 2, "停下来 300 ms 后只发一次");

        // 登录/换中转的请求同样触发
        own.send_modify(|v| *v += 1);
        step(400).await;
        assert_eq!(sent.load(Ordering::SeqCst), 3);

        // 失败后 30 秒重试
        fail_next.store(true, Ordering::SeqCst);
        list.send_modify(|v| *v += 1);
        step(400).await;
        assert_eq!(sent.load(Ordering::SeqCst), 4);
        step(20_000).await;
        assert_eq!(sent.load(Ordering::SeqCst), 4);
        step(11_000).await;
        assert_eq!(sent.load(Ordering::SeqCst), 5, "30 秒后重试成功");
        step(120_000).await;
        assert_eq!(sent.load(Ordering::SeqCst), 5, "成功后不再重复发送");
    }
}
