//! 从另一台电脑导入 SSH 主机（docs/SSH_TRANSFER.md）：旧电脑（A，导出方）把选中的主机连同密码、私钥交给新电脑（B，导入方）。
//!
//! - **只走端到端通道**（§4.1）：B 的后端经 `rtc::outbound` 直接调 A 的 `/api/ssh/transfer/*`（A 那边是 `Access::Rtc`），
//!   浏览器只看到进度和结果；中转 HTTP 回退（`Access::Remote`）一律拒绝；
//! - **设备密钥签名的临时密钥协商 + 确认码**（§4.2）：B `start`（先只发临时公钥的承诺，B 的设备私钥签名）→
//!   A 回临时公钥（A 的设备私钥签名）→ B `reveal` 揭示临时公钥。两边各自算出同一把数据密钥和 6 位确认码，
//!   B 显示确认码，用户在 A 上输入（[`Initiator`]、[`Responder`]）；
//! - **在 A 上亲手确认**（§4.3）：确认码只在 A 进程内存里，`approve` 只接受本机界面（打开“允许远程确认”后也接受经 WebRTC 的远程会话，
//!   另要邮箱验证码）；输错 3 次或 5 分钟没确认就作废，同一时刻只有一个待确认的导出；
//! - **数据包**（§4.4）：ChaCha20-Poly1305，关联数据是整个协商过程的摘要；B 解密后用自己的 `secret.key` 重新加密，整批一个事务写入
//!   （[`ssh::import`]），然后逐台测试连接。
//!
//! 秘密不进日志、不进导入记录（只有主机名称），临时密钥和明文用完清零。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use futures_util::StreamExt;
use hkdf::Hkdf;
use looklook_protocol::signing::{b64, b64_decode};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;
use x25519_dalek::{PublicKey, StaticSecret};
use zeroize::Zeroizing;

use crate::error::LocalError;
use crate::gateway::App;
use crate::rtc::outbound::{Link, PeerCheck};
use crate::ssh::{self, Carried, Resolve};
use crate::store::Store;

pub const VERSION: u32 = 1;
const LABEL: &str = "looklook-ssh-transfer-v1";
/// 从 `start` 到 B 收到数据包的期限
pub const TTL: Duration = Duration::from_secs(300);
const MAX_TRIES: u32 = 3;
const POLL: Duration = Duration::from_millis(1500);
/// 用过的 `transfer_id` 记多久（比 [`TTL`] 长就够了）
const USED_TTL: Duration = Duration::from_secs(900);
const LOG_KEY: &str = "ssh_transfer_log";
const LOG_MAX: usize = 50;
const CONFIG_KEY: &str = "ssh_transfer";
/// 经通道收的响应体上限（200 台主机、每台一把大私钥也远小于这个）
const BODY_MAX: usize = 8 * 1024 * 1024;

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn err(code: &str) -> LocalError {
    LocalError::new(code)
}

fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn bytes32(s: &str) -> Result<[u8; 32], LocalError> {
    b64_decode(s).and_then(|b| <[u8; 32]>::try_from(b.as_slice()).ok()).ok_or_else(|| err("SSH_TRANSFER_PROTOCOL"))
}

/// 设备 id、`transfer_id` 只能是这些字符（会拼进签名的规范串，不能带换行）。
fn plain_id(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

// ---------------- 设置 ----------------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// 允许经远程会话（WebRTC，另要邮箱验证码）确认导出（§4.3“远程确认”）。只能在这台电脑的本机界面打开
    pub allow_remote_approve: bool,
}

impl Config {
    pub fn load(store: &Store) -> Self {
        store.get(CONFIG_KEY).ok().flatten().unwrap_or_default()
    }

    pub fn save(&self, store: &Store) -> anyhow::Result<()> {
        store.set(CONFIG_KEY, self)
    }
}

// ---------------- 导入记录（§4.5 留痕） ----------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Record {
    pub at: String,
    /// `export`（这台是旧电脑）/ `import`（这台是新电脑）
    pub dir: String,
    /// 对方电脑的 id 与名称
    pub peer: String,
    #[serde(default)]
    pub peer_name: String,
    /// 主机名称（不含任何秘密）
    pub hosts: Vec<String>,
    /// `ok` / `rejected` / `expired` / `failed` / `canceled`
    pub result: String,
    /// 失败时的错误码
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub detail: String,
}

pub fn log(store: &Store) -> Vec<Record> {
    store.get(LOG_KEY).ok().flatten().unwrap_or_default()
}

fn record(store: &Store, r: Record) {
    tracing::info!(dir = %r.dir, peer = %r.peer, hosts = ?r.hosts, result = %r.result, detail = %r.detail, "SSH 主机导入 / 导出");
    let mut all = log(store);
    all.insert(0, r);
    all.truncate(LOG_MAX);
    if let Err(e) = store.set(LOG_KEY, &all) {
        tracing::warn!(error = %e, "导入记录保存失败");
    }
}

// ---------------- 密钥协商（§4.2） ----------------

/// 要导入的主机：`hosts` 是旧电脑上的 id（用到的跳板机必须也在里面），`agent_keys` 是其中要带上旧电脑默认私钥的“系统密钥”主机。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Selection {
    pub hosts: Vec<String>,
    #[serde(default)]
    pub agent_keys: Vec<String>,
}

impl Selection {
    fn canonical(&self) -> String {
        format!("{}|{}", self.hosts.join(","), self.agent_keys.join(","))
    }

    fn check(&self) -> Result<(), LocalError> {
        let ok = |id: &String| !id.is_empty() && id.len() <= 32 && id.bytes().all(|b| b.is_ascii_alphanumeric());
        let mut uniq = self.hosts.clone();
        uniq.sort();
        uniq.dedup();
        if self.hosts.is_empty() || self.hosts.len() > 200 || uniq.len() != self.hosts.len() || !self.hosts.iter().all(ok) || !self.agent_keys.iter().all(|a| self.hosts.contains(a)) {
            return Err(err("SSH_TRANSFER_HOSTS"));
        }
        Ok(())
    }
}

/// 一次导入的身份：两边都要用到的上下文。
#[derive(Debug, Clone)]
pub struct Ctx {
    pub transfer_id: String,
    /// B：发起导入的新电脑
    pub importer: String,
    /// A：导出主机的旧电脑
    pub exporter: String,
    pub sel: Selection,
}

fn start_message(ctx: &Ctx, ts: i64, commit: &str) -> String {
    format!("{LABEL}\nstart\n{}\n{}\n{}\n{ts}\n{commit}\n{}", ctx.transfer_id, ctx.importer, ctx.exporter, ctx.sel.canonical())
}

fn accept_message(ctx: &Ctx, commit: &str, e: &str, nonce: &str) -> String {
    format!("{LABEL}\naccept\n{}\n{}\n{}\n{commit}\n{e}\n{nonce}", ctx.transfer_id, ctx.importer, ctx.exporter)
}

fn cancel_message(transfer_id: &str, importer: &str, exporter: &str) -> String {
    format!("{LABEL}\ncancel\n{transfer_id}\n{importer}\n{exporter}")
}

fn commit_of(e: &[u8; 32], nonce: &[u8; 32]) -> [u8; 32] {
    Sha256::new().chain_update(e).chain_update(nonce).finalize().into()
}

/// 整个协商过程的摘要：每段带长度前缀，不会两种拼法得到同一串。
fn transcript(ctx: &Ctx, commit: &[u8; 32], a_e: &[u8; 32], nonce_a: &[u8; 32], b_e: &[u8; 32], nonce_b: &[u8; 32]) -> [u8; 32] {
    let sel = ctx.sel.canonical();
    let parts: [&[u8]; 10] = [LABEL.as_bytes(), ctx.transfer_id.as_bytes(), ctx.importer.as_bytes(), ctx.exporter.as_bytes(), commit, a_e, nonce_a, b_e, nonce_b, sel.as_bytes()];
    let mut h = Sha256::new();
    for p in parts {
        h.update((p.len() as u32).to_be_bytes());
        h.update(p);
    }
    h.finalize().into()
}

/// 协商出来的数据密钥与确认码。
pub struct Agreed {
    key: Zeroizing<[u8; 32]>,
    code: Zeroizing<String>,
    transcript: [u8; 32],
}

/// 不打印密钥和确认码
impl std::fmt::Debug for Agreed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Agreed(..)")
    }
}

impl Agreed {
    pub fn code(&self) -> &str {
        &self.code
    }

    fn cipher(&self) -> ChaCha20Poly1305 {
        ChaCha20Poly1305::new((&*self.key).into())
    }

    fn seal(&self, ctx: &Ctx, created_at: i64, plain: &[u8]) -> Result<Packet, LocalError> {
        let nonce: [u8; 12] = crate::util::random_bytes();
        let ct = self.cipher().encrypt(Nonce::from_slice(&nonce), Payload { msg: plain, aad: &self.transcript }).map_err(|_| anyhow::anyhow!("加密失败"))?;
        Ok(Packet {
            v: VERSION,
            transfer_id: ctx.transfer_id.clone(),
            from_dev: ctx.exporter.clone(),
            to_dev: ctx.importer.clone(),
            created_at,
            nonce: b64(&nonce),
            ciphertext: b64(&ct),
        })
    }

    fn open(&self, p: &Packet) -> Result<Zeroizing<Vec<u8>>, LocalError> {
        let nonce = b64_decode(&p.nonce).filter(|n| n.len() == 12).ok_or_else(|| err("SSH_TRANSFER_PROTOCOL"))?;
        let ct = b64_decode(&p.ciphertext).ok_or_else(|| err("SSH_TRANSFER_PROTOCOL"))?;
        self.cipher().decrypt(Nonce::from_slice(&nonce), Payload { msg: &ct, aad: &self.transcript }).map(Zeroizing::new).map_err(|_| err("SSH_TRANSFER_TAMPERED"))
    }
}

fn agree(secret: &StaticSecret, peer: &[u8; 32], transcript: [u8; 32]) -> Result<Agreed, LocalError> {
    let shared = secret.diffie_hellman(&PublicKey::from(*peer));
    // 对方给的是小子群上的点：协商结果与我们的私钥无关
    if !shared.was_contributory() {
        return Err(err("SSH_TRANSFER_PROTOCOL"));
    }
    let hk = Hkdf::<Sha256>::new(Some(&transcript), shared.as_bytes());
    let mut key = Zeroizing::new([0u8; 32]);
    hk.expand(b"looklook ssh-transfer v1 key", &mut *key).expect("hkdf 32");
    let mut c = Zeroizing::new([0u8; 4]);
    hk.expand(b"looklook ssh-transfer v1 code", &mut *c).expect("hkdf 4");
    let code = Zeroizing::new(format!("{:06}", u32::from_be_bytes(*c) % 1_000_000));
    Ok(Agreed { key, code, transcript })
}

/// B 这边：先只给承诺，收到 A 的临时公钥后再揭示自己的。
pub struct Initiator {
    secret: StaticSecret,
    public: [u8; 32],
    nonce: [u8; 32],
}

impl Default for Initiator {
    fn default() -> Self {
        let secret = StaticSecret::random_from_rng(rand::rngs::OsRng);
        let public = PublicKey::from(&secret).to_bytes();
        Self { secret, public, nonce: crate::util::random_bytes() }
    }
}

impl Initiator {
    pub fn commit(&self) -> [u8; 32] {
        commit_of(&self.public, &self.nonce)
    }

    pub fn reveal(&self) -> RevealReq {
        RevealReq { transfer_id: String::new(), e: b64(&self.public), nonce: b64(&self.nonce) }
    }

    pub fn finish(&self, ctx: &Ctx, a_e: &[u8; 32], nonce_a: &[u8; 32]) -> Result<Agreed, LocalError> {
        let t = transcript(ctx, &self.commit(), a_e, nonce_a, &self.public, &self.nonce);
        agree(&self.secret, a_e, t)
    }
}

/// A 这边：收到承诺就给出自己的临时公钥，B 揭示后核对承诺。
pub struct Responder {
    secret: StaticSecret,
    public: [u8; 32],
    nonce: [u8; 32],
    commit: [u8; 32],
}

impl Responder {
    pub fn new(commit: [u8; 32]) -> Self {
        let secret = StaticSecret::random_from_rng(rand::rngs::OsRng);
        let public = PublicKey::from(&secret).to_bytes();
        Self { secret, public, nonce: crate::util::random_bytes(), commit }
    }

    pub fn finish(&self, ctx: &Ctx, b_e: &[u8; 32], nonce_b: &[u8; 32]) -> Result<Agreed, LocalError> {
        if !ct_eq(&commit_of(b_e, nonce_b), &self.commit) {
            return Err(err("SSH_TRANSFER_PROTOCOL"));
        }
        let t = transcript(ctx, &self.commit, &self.public, &self.nonce, b_e, nonce_b);
        agree(&self.secret, b_e, t)
    }
}

// ---------------- 通道上的消息 ----------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StartReq {
    pub v: u32,
    pub transfer_id: String,
    pub importer: String,
    pub exporter: String,
    /// B 签名用的设备密钥 id
    pub key_id: String,
    /// B 发起时的时间（毫秒）
    pub ts: i64,
    /// `SHA-256(B_e ‖ nonce_B)`
    pub commit: String,
    #[serde(flatten)]
    pub sel: Selection,
    pub sig: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StartResp {
    pub transfer_id: String,
    pub key_id: String,
    pub e: String,
    pub nonce: String,
    pub sig: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RevealReq {
    pub transfer_id: String,
    pub e: String,
    pub nonce: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdReq {
    pub transfer_id: String,
    /// `cancel` 要 B 的设备签名
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub key_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub sig: String,
}

/// 数据包（§4.4）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Packet {
    pub v: u32,
    pub transfer_id: String,
    pub from_dev: String,
    pub to_dev: String,
    pub created_at: i64,
    pub nonce: String,
    pub ciphertext: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FetchResp {
    /// `pending` / `ready` / `rejected` / `expired`
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub packet: Option<Packet>,
}

#[derive(Serialize, Deserialize)]
struct Contents {
    hosts: Vec<Carried>,
}

// ---------------- 状态 ----------------

enum Phase {
    /// 回了临时公钥，等 B 揭示
    Accepted(Responder),
    /// 等用户在这台电脑上输入确认码
    Pending { agreed: Agreed, tries: u32 },
    /// 确认码对了，正在取秘密、加密（远程确认时还在核对邮箱验证码）
    Approving,
    Ready(Packet),
    Rejected,
}

/// A 上的一次导出。
struct Export {
    ctx: Ctx,
    importer_name: String,
    /// 确认框里列出的主机（名称、地址、会带上什么）
    hosts: Vec<Value>,
    created: Instant,
    started_at: String,
    phase: Phase,
}

impl Export {
    fn names(&self) -> Vec<String> {
        self.hosts.iter().filter_map(|h| h["name"].as_str().map(str::to_string)).collect()
    }

    fn record(&self, store: &Store, result: &str, detail: &str) {
        record(
            store,
            Record { at: crate::util::now_rfc3339(), dir: "export".into(), peer: self.ctx.importer.clone(), peer_name: self.importer_name.clone(), hosts: self.names(), result: result.into(), detail: detail.into() },
        );
    }
}

/// B 上的一次导入（界面轮询 `GET /api/ssh/import/{id}`）。
struct Import {
    exporter: String,
    exporter_name: String,
    names: Vec<String>,
    created: Instant,
    /// `connecting` / `waiting` / `importing` / `testing` / `done` / `failed` / `canceled`
    state: &'static str,
    code: Option<Zeroizing<String>>,
    error: Option<LocalError>,
    imported: Vec<ssh::Imported>,
    tests: HashMap<String, Value>,
    cancel: CancellationToken,
}

impl Import {
    fn active(&self) -> bool {
        matches!(self.state, "connecting" | "waiting" | "importing")
    }

    fn view(&self, id: &str) -> Value {
        json!({
            "id": id,
            "from": self.exporter,
            "from_name": self.exporter_name,
            "hosts": self.names,
            "state": self.state,
            "code": self.code.as_deref().map(String::as_str).filter(|_| self.state == "waiting"),
            "expires_in": TTL.saturating_sub(self.created.elapsed()).as_secs(),
            "error": self.error.as_ref().map(|e| json!({ "code": e.code, "params": e.params })),
            "imported": self.imported,
            "tests": self.tests,
        })
    }
}

#[derive(Default)]
pub struct SshTransfer {
    exports: Mutex<HashMap<String, Export>>,
    imports: Mutex<HashMap<String, Import>>,
    /// 用过的 `transfer_id`（防重放）
    used: Mutex<HashMap<String, Instant>>,
}

impl SshTransfer {
    /// 过期的导出作废（用户看到过的记一笔）。
    fn purge(&self, store: &Store) {
        let gone: Vec<Export> = {
            let mut m = lock(&self.exports);
            let ids: Vec<String> = m.iter().filter(|(_, e)| e.created.elapsed() > TTL).map(|(k, _)| k.clone()).collect();
            ids.iter().filter_map(|k| m.remove(k)).collect()
        };
        for e in gone {
            if !matches!(e.phase, Phase::Accepted(_) | Phase::Rejected) {
                e.record(store, "expired", "");
            }
        }
        lock(&self.used).retain(|_, t| t.elapsed() < USED_TTL);
        lock(&self.imports).retain(|_, i| i.created.elapsed() < USED_TTL || i.active());
    }

}

fn own_device(app: &App) -> Result<String, LocalError> {
    app.account.session().map(|s| s.device_id).ok_or_else(|| err("NOT_LOGGED_IN"))
}

/// 用平台登记的设备公钥（§8：设备列表带上每台电脑当前有效的公钥）校验另一台电脑的签名，返回它的名称。
async fn verify_peer(app: &App, dev: &str, key_id: &str, message: &str, sig: &str) -> Result<String, LocalError> {
    match app.outbound.verify_peer(&app.account, dev, key_id, message, sig, true).await? {
        PeerCheck::Ok(name) => Ok(name),
        // 旧服务端没有下发公钥：没法校验，宁可不做
        PeerCheck::Unsupported => Err(err("SSH_TRANSFER_UNSUPPORTED")),
        PeerCheck::Bad => {
            tracing::warn!(dev, "SSH 导入：对方的签名不对");
            Err(err("SSH_TRANSFER_SIGNATURE"))
        }
    }
}

fn sign(app: &App, message: &str) -> Result<(String, String), LocalError> {
    app.account.sign_device(message).ok_or_else(|| err("NOT_LOGGED_IN"))
}

// ---------------- A：导出方（经 WebRTC 被 B 调用） ----------------

/// `POST /api/ssh/transfer/list`：主机概况（不含秘密），以及“系统密钥”主机可以带上的默认私钥。
pub async fn list(app: &App) -> Result<Value, LocalError> {
    let store = app.store.clone();
    let (items, agent_key) = tokio::task::spawn_blocking(move || (ssh::briefs(&store), ssh::default_identity())).await.map_err(anyhow::Error::from)?;
    Ok(json!({ "v": VERSION, "items": items, "agent_key": agent_key }))
}

/// `POST /api/ssh/transfer/start`
pub async fn start(app: &App, req: StartReq) -> Result<StartResp, LocalError> {
    app.ssh_transfer.purge(&app.store);
    let own = own_device(app)?;
    if req.v != VERSION {
        return Err(err("SSH_TRANSFER_UNSUPPORTED"));
    }
    if !plain_id(&req.transfer_id) || req.transfer_id.len() != 32 || !plain_id(&req.importer) || !plain_id(&req.key_id) {
        return Err(err("SSH_TRANSFER_PROTOCOL"));
    }
    if req.exporter != own || req.importer == own {
        return Err(err("SSH_TRANSFER_WRONG_DEVICE"));
    }
    req.sel.check()?;
    let now = app.account.clock.now_ms();
    if (now - req.ts).abs() > TTL.as_millis() as i64 {
        return Err(err("SSH_TRANSFER_EXPIRED"));
    }
    if lock(&app.ssh_transfer.used).contains_key(&req.transfer_id) {
        return Err(err("SSH_TRANSFER_REPLAY"));
    }
    let commit = bytes32(&req.commit)?;
    let ctx = Ctx { transfer_id: req.transfer_id.clone(), importer: req.importer.clone(), exporter: own, sel: req.sel.clone() };
    let importer_name = verify_peer(app, &req.importer, &req.key_id, &start_message(&ctx, req.ts, &req.commit), &req.sig).await?;
    // 选中的主机都要在，用到的跳板机也要一起选上；带默认私钥的只能是“系统密钥”主机
    let hosts = ssh::list(&app.store);
    let mut shown = Vec::new();
    for id in &ctx.sel.hosts {
        let h = hosts.iter().find(|h| &h.id == id).ok_or_else(|| err("SSH_TRANSFER_HOSTS"))?;
        if !h.jump.is_empty() && !ctx.sel.hosts.contains(&h.jump) {
            return Err(err("SSH_TRANSFER_HOSTS"));
        }
        let agent_key = ctx.sel.agent_keys.contains(id);
        if agent_key && h.auth != "agent" {
            return Err(err("SSH_TRANSFER_HOSTS"));
        }
        shown.push(json!({ "id": h.id, "name": h.name, "target": format!("{}@{}:{}", h.user, h.host, h.port), "auth": h.auth, "key_path": h.key_path, "key_stored": h.key_stored, "agent_key": agent_key }));
    }
    let briefs = {
        let store = app.store.clone();
        tokio::task::spawn_blocking(move || ssh::briefs(&store)).await.map_err(anyhow::Error::from)?
    };
    for s in &mut shown {
        if let Some(b) = briefs.iter().find(|b| s["id"] == b.id) {
            s["carries"] = json!(b.carries);
            s["passphrase"] = json!(b.passphrase);
        }
    }
    let responder = Responder::new(commit);
    let (e, nonce) = (b64(&responder.public), b64(&responder.nonce));
    let (key_id, sig) = sign(app, &accept_message(&ctx, &req.commit, &e, &nonce))?;
    {
        let mut m = lock(&app.ssh_transfer.exports);
        // 同一时刻只有一个待确认的导出
        if m.values().any(|x| !matches!(x.phase, Phase::Rejected | Phase::Ready(_))) {
            return Err(err("SSH_TRANSFER_BUSY"));
        }
        lock(&app.ssh_transfer.used).insert(req.transfer_id.clone(), Instant::now());
        m.insert(
            req.transfer_id.clone(),
            Export { ctx, importer_name, hosts: shown, created: Instant::now(), started_at: crate::util::now_rfc3339(), phase: Phase::Accepted(responder) },
        );
    }
    Ok(StartResp { transfer_id: req.transfer_id, key_id, e, nonce, sig })
}

/// `POST /api/ssh/transfer/reveal`：核对承诺，算出确认码，等用户确认。
pub fn reveal(app: &App, req: RevealReq) -> Result<(), LocalError> {
    app.ssh_transfer.purge(&app.store);
    let (b_e, nonce_b) = (bytes32(&req.e)?, bytes32(&req.nonce)?);
    let mut m = lock(&app.ssh_transfer.exports);
    let x = m.get_mut(&req.transfer_id).ok_or_else(|| err("SSH_TRANSFER_EXPIRED"))?;
    let Phase::Accepted(r) = &x.phase else { return Err(err("SSH_TRANSFER_PROTOCOL")) };
    match r.finish(&x.ctx, &b_e, &nonce_b) {
        Ok(agreed) => {
            x.phase = Phase::Pending { agreed, tries: 0 };
            tracing::info!(transfer = %req.transfer_id, importer = %x.ctx.importer, "SSH 导出等待确认");
            Ok(())
        }
        Err(e) => {
            m.remove(&req.transfer_id);
            Err(e)
        }
    }
}

/// `POST /api/ssh/transfer/fetch`：确认前是 `pending`；数据包只给一次。
pub fn fetch(app: &App, req: IdReq) -> FetchResp {
    app.ssh_transfer.purge(&app.store);
    let mut m = lock(&app.ssh_transfer.exports);
    let status = match m.get(&req.transfer_id).map(|x| &x.phase) {
        Some(Phase::Ready(_)) => {
            let x = m.remove(&req.transfer_id).expect("ready");
            x.record(&app.store, "ok", "");
            let Phase::Ready(p) = x.phase else { unreachable!() };
            return FetchResp { status: "ready".into(), packet: Some(p) };
        }
        Some(Phase::Rejected) => {
            m.remove(&req.transfer_id);
            "rejected"
        }
        Some(_) => "pending",
        None => "expired",
    };
    FetchResp { status: status.into(), packet: None }
}

/// `POST /api/ssh/transfer/cancel`：B 那边放弃了（要 B 的签名）。
pub async fn cancel_export(app: &App, req: IdReq) -> Result<(), LocalError> {
    let importer = lock(&app.ssh_transfer.exports).get(&req.transfer_id).map(|x| (x.ctx.importer.clone(), x.ctx.exporter.clone()));
    let Some((importer, exporter)) = importer else { return Ok(()) };
    verify_peer(app, &importer, &req.key_id, &cancel_message(&req.transfer_id, &importer, &exporter), &req.sig).await?;
    if let Some(x) = lock(&app.ssh_transfer.exports).remove(&req.transfer_id) {
        if !matches!(x.phase, Phase::Accepted(_)) {
            x.record(&app.store, "canceled", "");
        }
    }
    Ok(())
}

/// `GET /api/ssh/transfer/pending`：等这台电脑确认的导出（不含确认码）。
pub fn pending(app: &App) -> Value {
    app.ssh_transfer.purge(&app.store);
    let m = lock(&app.ssh_transfer.exports);
    let items: Vec<Value> = m
        .iter()
        .filter_map(|(id, x)| {
            let Phase::Pending { tries, .. } = &x.phase else { return None };
            Some(json!({
                "id": id,
                "from": x.ctx.importer,
                "from_name": x.importer_name,
                "hosts": x.hosts,
                "started_at": x.started_at,
                "expires_in": TTL.saturating_sub(x.created.elapsed()).as_secs(),
                "tries_left": MAX_TRIES - tries,
            }))
        })
        .collect();
    drop(m);
    // 最近一次导出成功：界面再提醒一次，万一不是本人操作可以马上发现（§4.5）
    let last = log(&app.store).into_iter().find(|r| r.dir == "export" && r.result == "ok").map(|r| json!({ "at": r.at, "peer_name": r.peer_name, "hosts": r.hosts }));
    json!({ "items": items, "last_export": last })
}

/// `POST /api/ssh/transfer/{id}/approve`：确认码对了才取出秘密、加密，等 B 来取。
/// `email_code`：远程确认（`remote`）时必须有，确认码对了以后再交给平台核对。
pub async fn approve(app: &App, id: &str, code: &str, email_code: Option<&str>, remote: bool) -> Result<Value, LocalError> {
    app.ssh_transfer.purge(&app.store);
    let email_code = email_code.map(str::trim).filter(|c| !c.is_empty());
    if remote && email_code.is_none() {
        return Err(err("SSH_TRANSFER_EMAIL_REQUIRED"));
    }
    let input: String = code.chars().filter(char::is_ascii_digit).collect();
    let (agreed, tries, ctx) = {
        let mut m = lock(&app.ssh_transfer.exports);
        let x = m.get_mut(id).ok_or_else(|| err("SSH_TRANSFER_EXPIRED"))?;
        let Phase::Pending { agreed, tries } = &mut x.phase else { return Err(err("SSH_TRANSFER_STATE")) };
        if !ct_eq(input.as_bytes(), agreed.code().as_bytes()) {
            *tries += 1;
            let left = MAX_TRIES.saturating_sub(*tries);
            if left == 0 {
                let x = m.remove(id).expect("pending");
                x.record(&app.store, "failed", "SSH_TRANSFER_CODE_LOCKED");
                return Err(err("SSH_TRANSFER_CODE_LOCKED"));
            }
            return Err(err("SSH_TRANSFER_CODE_WRONG").with("remaining", left));
        }
        let Phase::Pending { agreed, tries } = std::mem::replace(&mut x.phase, Phase::Approving) else { unreachable!() };
        (agreed, tries, x.ctx.clone())
    };
    // 出错时放回去，用户可以再试（邮箱验证码输错等）
    let put_back = |agreed: Agreed| {
        if let Some(x) = lock(&app.ssh_transfer.exports).get_mut(id) {
            x.phase = Phase::Pending { agreed, tries };
        }
    };
    if remote {
        if let Err(e) = app.account.ssh_export_verify(email_code.unwrap_or_default()).await {
            put_back(agreed);
            return Err(e.into());
        }
    }
    let (store, home, sel) = (app.store.clone(), app.paths.home.clone(), ctx.sel.clone());
    let n = sel.hosts.len();
    let created_at = app.account.clock.now_ms();
    let (sealed, agreed) = tokio::task::spawn_blocking(move || {
        let sealed = ssh::export(&store, &home, &sel.hosts, &sel.agent_keys).and_then(|hosts| {
            let plain = Zeroizing::new(serde_json::to_vec(&Contents { hosts }).map_err(anyhow::Error::from)?);
            agreed.seal(&ctx, created_at, &plain)
        });
        (sealed, agreed)
    })
    .await
    .map_err(anyhow::Error::from)?;
    match sealed {
        Ok(packet) => {
            drop(agreed);
            if let Some(x) = lock(&app.ssh_transfer.exports).get_mut(id) {
                x.phase = Phase::Ready(packet);
            }
            tracing::info!(transfer = %id, remote, "SSH 导出已确认");
            Ok(json!({ "ok": true, "hosts": n }))
        }
        Err(e) => {
            put_back(agreed);
            Err(e)
        }
    }
}

/// `POST /api/ssh/transfer/{id}/reject`
pub fn reject(app: &App, id: &str) -> Result<(), LocalError> {
    let mut m = lock(&app.ssh_transfer.exports);
    let x = m.get_mut(id).ok_or_else(|| err("SSH_TRANSFER_EXPIRED"))?;
    if !matches!(x.phase, Phase::Pending { .. }) {
        return Err(err("SSH_TRANSFER_STATE"));
    }
    x.phase = Phase::Rejected;
    x.record(&app.store, "rejected", "");
    Ok(())
}

/// 远程确认前给账户邮箱发验证码（只有确实有这个待确认的导出时才发）。
pub async fn email_code(app: &App, id: &str) -> Result<(), LocalError> {
    let ok = lock(&app.ssh_transfer.exports).get(id).is_some_and(|x| matches!(x.phase, Phase::Pending { .. }));
    if !ok {
        return Err(err("SSH_TRANSFER_EXPIRED"));
    }
    app.account.ssh_export_email_code().await.map_err(Into::into)
}

// ---------------- B：导入方 ----------------

/// 这台电脑经 WebRTC 调另一台电脑的接口（POST JSON）。204 返回 None。
async fn call<T: DeserializeOwned>(link: &Arc<Link>, path: &str, body: &impl Serialize) -> Result<Option<T>, LocalError> {
    crate::rtc::outbound::post_json(link, path, body, BODY_MAX, "SSH_TRANSFER_PROTOCOL").await
}

async fn link(app: &App, dev: &str) -> Result<Arc<Link>, LocalError> {
    app.outbound.link(&app.account, dev).await.map_err(|e| e.local())
}

/// 可以从哪些电脑导入：同一账户、在线的其他电脑。
pub async fn sources(app: &App) -> Result<Value, LocalError> {
    let own = own_device(app)?;
    let d = app.outbound.devices(&app.account, true).await?;
    let items: Vec<Value> = d.items.iter().filter(|x| x.id != own).map(|x| json!({ "id": x.id, "name": x.name, "os": x.os, "online": x.online })).collect();
    Ok(json!({ "items": items }))
}

/// 另一台电脑上的主机概况。
pub async fn remote_hosts(app: &App, from: &str) -> Result<Value, LocalError> {
    if from == own_device(app)? {
        return Err(err("SSH_TRANSFER_WRONG_DEVICE"));
    }
    let l = link(app, from).await?;
    match call::<Value>(&l, "/api/ssh/transfer/list", &json!({})).await {
        // 旧版本的看看没有这个接口
        Err(e) if e.code == "NOT_FOUND" => Err(err("SSH_TRANSFER_UNSUPPORTED")),
        Err(e) => Err(e),
        Ok(v) => Ok(v.unwrap_or_default()),
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ImportReq {
    pub from: String,
    #[serde(flatten)]
    pub sel: Selection,
    /// 旧电脑上的 id → 和这台电脑上同名主机冲突时的处理
    #[serde(default)]
    pub resolve: HashMap<String, Resolve>,
    /// 主机名称（界面显示进度用，不参与协议）
    #[serde(default)]
    pub names: Vec<String>,
}

/// `POST /api/ssh/import/start`：开始导入，返回 id；进度用 [`view`] 查询。
pub async fn begin(app: &App, req: ImportReq) -> Result<Value, LocalError> {
    let own = own_device(app)?;
    if req.from == own || !plain_id(&req.from) {
        return Err(err("SSH_TRANSFER_WRONG_DEVICE"));
    }
    req.sel.check()?;
    app.ssh_transfer.purge(&app.store);
    let id = data_encoding::HEXLOWER.encode(&crate::util::random_bytes::<16>());
    let name = app.outbound.devices(&app.account, false).await.ok().and_then(|d| d.items.into_iter().find(|x| x.id == req.from).map(|x| x.name)).unwrap_or_default();
    let cancel = CancellationToken::new();
    {
        let mut m = lock(&app.ssh_transfer.imports);
        if m.values().any(Import::active) {
            return Err(err("SSH_TRANSFER_BUSY"));
        }
        let names = if req.names.is_empty() { req.sel.hosts.clone() } else { req.names.iter().take(200).map(|n| n.chars().take(40).collect()).collect() };
        m.insert(
            id.clone(),
            Import {
                exporter: req.from.clone(),
                exporter_name: name,
                names,
                created: Instant::now(),
                state: "connecting",
                code: None,
                error: None,
                imported: Vec::new(),
                tests: HashMap::new(),
                cancel: cancel.clone(),
            },
        );
    }
    let ctx = Ctx { transfer_id: id.clone(), importer: own, exporter: req.from.clone(), sel: req.sel.clone() };
    let app2 = app.clone();
    tokio::spawn(async move {
        let r = tokio::select! {
            r = drive(&app2, &ctx, req.resolve) => r,
            _ = cancel.cancelled() => Err(err("SSH_TRANSFER_CANCELED")),
            _ = tokio::time::sleep(TTL + Duration::from_secs(5)) => Err(err("SSH_TRANSFER_EXPIRED")),
        };
        if let Err(e) = r {
            finish_failed(&app2, &ctx, e).await;
        }
    });
    Ok(json!({ "id": id }))
}

fn set_import(app: &App, id: &str, f: impl FnOnce(&mut Import)) {
    if let Some(i) = lock(&app.ssh_transfer.imports).get_mut(id) {
        f(i);
    }
}

async fn finish_failed(app: &App, ctx: &Ctx, e: LocalError) {
    let canceled = e.code == "SSH_TRANSFER_CANCELED";
    let mut names = Vec::new();
    let mut tell = false;
    set_import(app, &ctx.transfer_id, |i| {
        tell = i.state == "waiting" || i.state == "connecting";
        i.state = if canceled { "canceled" } else { "failed" };
        i.code = None;
        names = i.names.clone();
        i.error = Some(e.clone());
    });
    let result = match e.code.as_str() {
        "SSH_TRANSFER_CANCELED" => "canceled",
        "SSH_TRANSFER_REJECTED" => "rejected",
        "SSH_TRANSFER_EXPIRED" => "expired",
        _ => "failed",
    };
    let peer_name = lock(&app.ssh_transfer.imports).get(&ctx.transfer_id).map(|i| i.exporter_name.clone()).unwrap_or_default();
    record(&app.store, Record { at: crate::util::now_rfc3339(), dir: "import".into(), peer: ctx.exporter.clone(), peer_name, hosts: names, result: result.into(), detail: e.code.clone() });
    // 放弃了：告诉旧电脑，那边的待确认横幅马上消失（不成也没关系，5 分钟后自己作废）
    if tell && (canceled || result == "expired") {
        if let Ok((key_id, sig)) = sign(app, &cancel_message(&ctx.transfer_id, &ctx.importer, &ctx.exporter)) {
            if let Ok(l) = link(app, &ctx.exporter).await {
                let _ = call::<Value>(&l, "/api/ssh/transfer/cancel", &IdReq { transfer_id: ctx.transfer_id.clone(), key_id, sig }).await;
            }
        }
    }
}

/// 一次导入从头到尾：协商、等确认、取数据包、导入、测试连接。
async fn drive(app: &App, ctx: &Ctx, resolve: HashMap<String, Resolve>) -> Result<(), LocalError> {
    let l = link(app, &ctx.exporter).await?;
    let me = Initiator::default();
    let commit = b64(&me.commit());
    let ts = app.account.clock.now_ms();
    let (key_id, sig) = sign(app, &start_message(ctx, ts, &commit))?;
    let req = StartReq { v: VERSION, transfer_id: ctx.transfer_id.clone(), importer: ctx.importer.clone(), exporter: ctx.exporter.clone(), key_id, ts, commit: commit.clone(), sel: ctx.sel.clone(), sig };
    let resp: StartResp = match call(&l, "/api/ssh/transfer/start", &req).await {
        Err(e) if e.code == "NOT_FOUND" => return Err(err("SSH_TRANSFER_UNSUPPORTED")),
        r => r?.ok_or_else(|| err("SSH_TRANSFER_PROTOCOL"))?,
    };
    if resp.transfer_id != ctx.transfer_id {
        return Err(err("SSH_TRANSFER_PROTOCOL"));
    }
    let name = verify_peer(app, &ctx.exporter, &resp.key_id, &accept_message(ctx, &commit, &resp.e, &resp.nonce), &resp.sig).await?;
    let agreed = me.finish(ctx, &bytes32(&resp.e)?, &bytes32(&resp.nonce)?)?;
    let mut reveal = me.reveal();
    reveal.transfer_id = ctx.transfer_id.clone();
    call::<Value>(&l, "/api/ssh/transfer/reveal", &reveal).await?;
    drop(me);
    set_import(app, &ctx.transfer_id, |i| {
        i.state = "waiting";
        i.exporter_name = name;
        i.code = Some(Zeroizing::new(agreed.code().to_string()));
    });

    let packet = loop {
        tokio::time::sleep(POLL).await;
        // 连接断了就重连（换了网络等）；数据包只给一次，取到之前一直可以再问
        let l = link(app, &ctx.exporter).await?;
        let r: FetchResp = call(&l, "/api/ssh/transfer/fetch", &IdReq { transfer_id: ctx.transfer_id.clone(), key_id: String::new(), sig: String::new() }).await?.ok_or_else(|| err("SSH_TRANSFER_PROTOCOL"))?;
        match r.status.as_str() {
            "pending" => continue,
            "ready" => break r.packet.ok_or_else(|| err("SSH_TRANSFER_PROTOCOL"))?,
            "rejected" => return Err(err("SSH_TRANSFER_REJECTED")),
            _ => return Err(err("SSH_TRANSFER_EXPIRED")),
        }
    };
    set_import(app, &ctx.transfer_id, |i| {
        i.state = "importing";
        i.code = None;
    });
    let now = app.account.clock.now_ms();
    if packet.v != VERSION || packet.transfer_id != ctx.transfer_id || packet.to_dev != ctx.importer || packet.from_dev != ctx.exporter {
        return Err(err("SSH_TRANSFER_PROTOCOL"));
    }
    if (now - packet.created_at).abs() > TTL.as_millis() as i64 {
        return Err(err("SSH_TRANSFER_EXPIRED"));
    }
    let plain = agreed.open(&packet)?;
    drop(agreed);
    let contents: Contents = serde_json::from_slice(&plain).map_err(|_| err("SSH_TRANSFER_PROTOCOL"))?;
    drop(plain);
    // 带来的正好是选中的那些
    let mut got: Vec<String> = contents.hosts.iter().filter_map(|c| c.host.as_ref().map(|h| h.id.clone())).collect();
    let mut want = ctx.sel.hosts.clone();
    got.sort();
    want.sort();
    if got != want {
        return Err(err("SSH_TRANSFER_PROTOCOL"));
    }
    let (store, home) = (app.store.clone(), app.paths.home.clone());
    let imported = tokio::task::spawn_blocking(move || ssh::import(&store, &home, contents.hosts, &resolve)).await.map_err(anyhow::Error::from)??;
    let (peer_name, names) = lock(&app.ssh_transfer.imports).get(&ctx.transfer_id).map(|i| (i.exporter_name.clone(), imported.iter().map(|x| x.name.clone()).collect())).unwrap_or_default();
    record(&app.store, Record { at: crate::util::now_rfc3339(), dir: "import".into(), peer: ctx.exporter.clone(), peer_name, hosts: names, result: "ok".into(), detail: String::new() });
    set_import(app, &ctx.transfer_id, |i| {
        i.state = "testing";
        i.imported = imported.clone();
    });

    // 导入后逐台测试连接（同时最多 4 台）
    let all = ssh::list(&app.store);
    let to_test: Vec<ssh::SshHost> = all.into_iter().filter(|h| imported.iter().any(|x| x.action != "skipped" && x.id == h.id)).collect();
    let tests: Vec<(String, Value)> = futures_util::stream::iter(to_test)
        .map(|h| async move {
            let id = h.id.clone();
            let r = match ssh::test(app.store.clone(), app.paths.home.clone(), h).await {
                Ok(rep) => serde_json::to_value(rep).unwrap_or_default(),
                Err(e) => json!({ "error": { "code": e.code, "params": e.params } }),
            };
            (id, r)
        })
        .buffer_unordered(4)
        .collect()
        .await;
    set_import(app, &ctx.transfer_id, |i| {
        i.tests = tests.into_iter().collect();
        i.state = "done";
    });
    Ok(())
}

/// `GET /api/ssh/import/{id}`
pub fn view(app: &App, id: &str) -> Option<Value> {
    lock(&app.ssh_transfer.imports).get(id).map(|i| i.view(id))
}

/// `POST /api/ssh/import/{id}/cancel`
pub fn cancel_import(app: &App, id: &str) -> Result<(), LocalError> {
    let m = lock(&app.ssh_transfer.imports);
    let i = m.get(id).ok_or_else(LocalError::not_found)?;
    if i.active() {
        i.cancel.cancel();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> Ctx {
        Ctx { transfer_id: "0123456789abcdef0123456789abcdef".into(), importer: "devb".into(), exporter: "deva".into(), sel: Selection { hosts: vec!["h1".into(), "h2".into()], agent_keys: vec![] } }
    }

    /// 正常协商：两边的数据密钥、确认码一样，数据包能解开。
    #[test]
    fn both_sides_agree_and_packet_round_trips() {
        let c = ctx();
        let b = Initiator::default();
        let a = Responder::new(b.commit());
        let at_b = b.finish(&c, &a.public, &a.nonce).unwrap();
        let at_a = a.finish(&c, &b.public, &b.nonce).unwrap();
        assert_eq!(at_a.code(), at_b.code());
        assert_eq!(at_a.code().len(), 6);
        assert!(at_a.code().bytes().all(|x| x.is_ascii_digit()));
        let p = at_a.seal(&c, 1, b"secret stuff").unwrap();
        assert_eq!((p.from_dev.as_str(), p.to_dev.as_str()), ("deva", "devb"));
        assert!(!p.ciphertext.contains("secret"));
        assert_eq!(&**at_b.open(&p).unwrap(), b"secret stuff");
        // 数据包被改过
        let mut bad = p.clone();
        let mut ct = b64_decode(&bad.ciphertext).unwrap();
        ct[0] ^= 1;
        bad.ciphertext = b64(&ct);
        assert_eq!(at_b.open(&bad).unwrap_err().code, "SSH_TRANSFER_TAMPERED");
    }

    /// 揭示的临时公钥和承诺对不上
    #[test]
    fn reveal_must_match_commit() {
        let c = ctx();
        let b = Initiator::default();
        let a = Responder::new(b.commit());
        let other = Initiator::default();
        assert_eq!(a.finish(&c, &other.public, &b.nonce).unwrap_err().code, "SSH_TRANSFER_PROTOCOL");
        assert_eq!(a.finish(&c, &b.public, &other.nonce).unwrap_err().code, "SSH_TRANSFER_PROTOCOL");
    }

    /// 中间人（平台被攻破）两边各协商一把钥匙：两边的确认码对不上，A 那边的密钥也解不开 B 期待的数据包。
    #[test]
    fn man_in_the_middle_gets_different_codes() {
        let c = ctx();
        let b = Initiator::default();
        // 中间人对 A 冒充 B（用自己的临时钥匙），对 B 冒充 A
        let m_to_a = Initiator::default();
        let a = Responder::new(m_to_a.commit());
        let m_to_b = Responder::new(b.commit());
        let at_a = a.finish(&c, &m_to_a.public, &m_to_a.nonce).unwrap();
        let at_b = b.finish(&c, &m_to_b.public, &m_to_b.nonce).unwrap();
        assert_ne!(at_a.code(), at_b.code(), "百万分之一的概率会碰巧相同");
        let p = at_a.seal(&c, 1, b"x").unwrap();
        assert!(at_b.open(&p).is_err());
    }

    /// 选择的主机列表、设备 id 进了摘要：改了任何一样，码就不同。
    #[test]
    fn transcript_binds_selection_and_devices() {
        let c = ctx();
        let b = Initiator::default();
        let a = Responder::new(b.commit());
        let base = b.finish(&c, &a.public, &a.nonce).unwrap();
        let mut c2 = c.clone();
        c2.sel.hosts.push("h3".into());
        assert_ne!(base.transcript, b.finish(&c2, &a.public, &a.nonce).unwrap().transcript);
        let mut c3 = c.clone();
        c3.importer = "devc".into();
        assert_ne!(base.transcript, b.finish(&c3, &a.public, &a.nonce).unwrap().transcript);
        // 签名的规范串也带着选择
        assert_ne!(start_message(&c, 1, "x"), start_message(&c2, 1, "x"));
    }

    /// 小子群上的点（全零公钥）：拒绝
    #[test]
    fn low_order_point_is_rejected() {
        let b = Initiator::default();
        assert_eq!(b.finish(&ctx(), &[0u8; 32], &[1u8; 32]).unwrap_err().code, "SSH_TRANSFER_PROTOCOL");
    }

    #[test]
    fn selection_rules() {
        let s = |h: &[&str], a: &[&str]| Selection { hosts: h.iter().map(|x| x.to_string()).collect(), agent_keys: a.iter().map(|x| x.to_string()).collect() };
        assert!(s(&["ab12", "cd34"], &["cd34"]).check().is_ok());
        assert!(s(&[], &[]).check().is_err());
        assert!(s(&["ab12", "ab12"], &[]).check().is_err(), "重复");
        assert!(s(&["ab,12"], &[]).check().is_err(), "会混进规范串的分隔符");
        assert!(s(&["ab12"], &["zz99"]).check().is_err(), "带私钥的必须是选中的");
        assert!(plain_id("dev-1_A") && !plain_id("dev\n1") && !plain_id(""));
    }

    #[test]
    fn records_are_capped_and_newest_first() {
        let store = Store::memory().unwrap();
        for i in 0..(LOG_MAX + 5) {
            record(&store, Record { at: i.to_string(), dir: "import".into(), peer: "d".into(), peer_name: String::new(), hosts: vec![], result: "ok".into(), detail: String::new() });
        }
        let all = log(&store);
        assert_eq!(all.len(), LOG_MAX);
        assert_eq!(all[0].at, (LOG_MAX + 4).to_string());
    }
}

/// 两台电脑走完整个流程：B 经假平台建 WebRTC 连到 A 的真路由（`Access::Rtc`），A 在本机确认，B 导入后测试连接；
/// 另有拒绝、确认码输错作废、伪造签名、非 WebRTC 来源被拒。
#[cfg(test)]
pub(crate) mod e2e {
    use std::path::Path;

    use axum::body::{Body, Bytes};
    use axum::extract::Request;
    use axum::http::{HeaderMap, StatusCode, Uri};
    use axum::response::IntoResponse;
    use ed25519_dalek::SigningKey;
    use looklook_protocol::dto::{DeviceKey, RemoteDevice, RemoteDevices};
    use looklook_protocol::rtc::{ClientConnectResponse, ConnectRequest};
    use looklook_protocol::signing::{self, response_canonical, HDR_NONCE, HDR_PLATFORM_KEY, HDR_PLATFORM_SIGNATURE};
    use tower::ServiceExt;

    use super::*;
    use crate::account::test_support;
    use crate::gateway::{self, Access, Inner};
    use crate::paths::Paths;
    use crate::rtc::{bridge, peer::Pc};

    pub(crate) fn app(home: &Path, server: &str, dev: &str, key: SigningKey) -> App {
        let store = Arc::new(Store::memory().unwrap());
        let paths = Paths { home: home.into(), ttyd: None, mux: None, fonts: None, trzsz: None };
        let account = test_support::logged_in(store.clone(), paths.clone(), server, dev, key);
        let instances = crate::instances::Instances::new(store.clone(), paths.clone());
        Arc::new(Inner {
            account: account.clone(),
            instances: instances.clone(),
            updater: crate::updater::Updater::new(store.clone(), paths.clone(), account.clone(), CancellationToken::new()),
            metrics: crate::metrics::Metrics::new(),
            relay_watch: crate::relay_watch::RelayWatch::new(account.clone()),
            transfers: crate::transfer::Transfers::new(paths.clone()),
            chat: crate::chat::Chat::new(store.clone(), paths.clone(), instances),
            store,
            paths,
            ui_addr: "127.0.0.1:1".parse().unwrap(),
            relay_addr: "127.0.0.1:2".parse().unwrap(),
            http: gateway::proxy_client(),
            access: Default::default(),
            limiter: Default::default(),
            resolved: Default::default(),
            lan_ips: vec![],
            remote: Default::default(),
            rtc: Default::default(),
            outbound: Default::default(),
            ssh_transfer: Default::default(),
            sync: Default::default(),
        })
    }

    pub(crate) type Target = Arc<Mutex<Option<axum::Router>>>;

    /// 假平台：设备列表带两台电脑的公钥；B 的 offer 交给 A 的真路由（经 bridge，请求是 `Access::Rtc`）。
    pub(crate) async fn platform(keys: Vec<(&'static str, SigningKey)>, target: Target) -> String {
        let svc = axum::Router::new().fallback(move |uri: Uri, headers: HeaderMap, body: Bytes| {
            let (keys, target) = (keys.clone(), target.clone());
            async move {
                let path = uri.path().to_string();
                let (status, out) = if path.ends_with("/rtc/devices") {
                    let items = keys
                        .iter()
                        .map(|(id, k)| RemoteDevice {
                            id: id.to_string(),
                            name: format!("{id}-pc"),
                            os: "linux".into(),
                            online: true,
                            terminals: vec![],
                            keys: vec![DeviceKey { id: format!("k-{id}"), public_key: b64(k.verifying_key().as_bytes()) }],
                        })
                        .collect();
                    (StatusCode::OK, serde_json::to_vec(&RemoteDevices { items, default: None, max: 7, ice_servers: vec![] }).unwrap())
                } else if path.ends_with("/rtc/connect") {
                    let req: ConnectRequest = serde_json::from_slice(&body).unwrap();
                    assert_eq!(req.dev, "deva");
                    let router = lock(&target).clone().expect("A 的路由");
                    let mut pc = Pc::answer(&[], &req.offer).unwrap();
                    let answer = pc.local_answer(Duration::from_secs(3)).await.unwrap();
                    let ctx = bridge::Ctx { router, host: "alice.looklook.test".into(), origin: "http://alice.looklook.test".into(), sid: "s".into(), device: "deva".into(), terms: Default::default(), expires: tokio::time::Instant::now() + Duration::from_secs(3600) };
                    tokio::spawn(async move {
                        while let Some(ch) = pc.incoming.recv().await {
                            tokio::spawn(bridge::serve(ctx.clone(), ch));
                        }
                        drop(pc);
                    });
                    (StatusCode::OK, serde_json::to_vec(&ClientConnectResponse { answer, host: "alice.looklook.test".into(), origin: "http://alice.looklook.test".into() }).unwrap())
                } else {
                    (StatusCode::NO_CONTENT, vec![])
                };
                let nonce = headers.get(HDR_NONCE).and_then(|v| v.to_str().ok()).unwrap_or_default().to_string();
                let sig = signing::sign(&test_support::platform_key(), &response_canonical(&nonce, status.as_u16(), &out));
                (status, [(HDR_PLATFORM_KEY, "p1".to_string()), (HDR_PLATFORM_SIGNATURE, sig), ("content-type", "application/json".to_string())], out).into_response()
            }
        });
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", l.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(l, svc).await.unwrap() });
        url
    }

    fn host(name: &str, auth: &str, secret: Option<&str>, jump: &str) -> ssh::HostInput {
        serde_json::from_value(json!({
            "name": name, "host": "127.0.0.1", "port": 9, "user": "deploy", "auth": auth, "jump": jump, "allow_ai": true,
            "key_stored": auth == "key", "key_text": (auth == "key").then(|| format!("-----BEGIN OPENSSH {}\nc2VjcmV0LWtleS1ib2R5\n-----END OPENSSH {}\n", "PRIVATE KEY-----", "PRIVATE KEY-----")),
            "secret": secret,
        }))
        .unwrap()
    }

    async fn wait(b: &App, id: &str, done: impl Fn(&Value) -> bool) -> Value {
        for _ in 0..400 {
            let v = view(b, id).expect("import");
            if done(&v) {
                return v;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("等不到：{}", view(b, id).unwrap());
    }

    async fn begin_import(b: &App, hosts: &[&str]) -> String {
        let req = ImportReq { from: "deva".into(), sel: Selection { hosts: hosts.iter().map(|s| s.to_string()).collect(), agent_keys: vec![] }, resolve: Default::default(), names: vec![] };
        begin(b, req).await.unwrap()["id"].as_str().unwrap().to_string()
    }

    /// 经 A 的路由发一个请求（带指定的来源），返回状态码与错误码。
    pub(crate) async fn via_router(a: &App, access: Access, path: &str, body: Value) -> (StatusCode, String) {
        let mut req = Request::builder().method("POST").uri(path).header("content-type", "application/json").header(crate::gateway::CLIENT_HEADER, "1").body(Body::from(body.to_string())).unwrap();
        req.extensions_mut().insert(access);
        let r = gateway::router(a.clone()).oneshot(req).await.unwrap();
        let status = r.status();
        let v: Value = serde_json::from_slice(&axum::body::to_bytes(r.into_body(), 1 << 20).await.unwrap()).unwrap_or_default();
        (status, v["error"]["code"].as_str().unwrap_or_default().to_string())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn import_between_two_computers() {
        let (ka, kb) = (SigningKey::from_bytes(&[11u8; 32]), SigningKey::from_bytes(&[22u8; 32]));
        let target: Target = Default::default();
        let url = platform(vec![("deva", ka.clone()), ("devb", kb.clone())], target.clone()).await;
        let (ha, hb) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let a = app(ha.path(), &url, "deva", ka);
        let b = app(hb.path(), &url, "devb", kb);
        *lock(&target) = Some(gateway::router(a.clone()));

        let bastion = ssh::save(&a.store, ha.path(), None, host("bastion", "password", Some("hunter2-pw"), "")).unwrap().host;
        let web = ssh::save(&a.store, ha.path(), None, host("web", "key", Some("key-passphrase"), &bastion.id)).unwrap().host;

        // A 上 B 用的接口只接受 WebRTC：中转回退、本机都拒绝；确认只接受本机（没开远程确认）
        let rtc = Access::Rtc { origin: "http://alice.looklook.test".into() };
        let remote = Access::Remote { host: "alice.looklook.test".into(), proto: "https".into() };
        assert_eq!(via_router(&a, remote.clone(), "/api/ssh/transfer/list", json!({})).await.1, "SSH_TRANSFER_NEED_RTC");
        assert_eq!(via_router(&a, Access::Local, "/api/ssh/transfer/list", json!({})).await.1, "SSH_TRANSFER_NEED_RTC");
        assert_eq!(via_router(&a, rtc.clone(), "/api/ssh/transfer/x/approve", json!({ "code": "123456" })).await.1, "SSH_TRANSFER_LOCAL_APPROVE");
        assert_eq!(via_router(&a, remote, "/api/ssh/transfer/x/approve", json!({ "code": "123456" })).await.1, "SSH_TRANSFER_NEED_RTC");

        // B 列出 A 的主机：不含秘密
        let listed = remote_hosts(&b, "deva").await.unwrap();
        assert_eq!(listed["items"].as_array().unwrap().len(), 2);
        assert!(!listed.to_string().contains("hunter2"));

        // 伪造的 start：签名不是 B 的设备私钥
        let fake = Initiator::default();
        let ctx = Ctx { transfer_id: "f".repeat(32), importer: "devb".into(), exporter: "deva".into(), sel: Selection { hosts: vec![bastion.id.clone()], agent_keys: vec![] } };
        let commit = b64(&fake.commit());
        let ts = a.account.clock.now_ms();
        let forged = StartReq { v: VERSION, transfer_id: ctx.transfer_id.clone(), importer: "devb".into(), exporter: "deva".into(), key_id: "k-devb".into(), ts, commit: commit.clone(), sel: ctx.sel.clone(), sig: signing::sign(&SigningKey::from_bytes(&[33u8; 32]), &start_message(&ctx, ts, &commit)) };
        assert_eq!(start(&a, forged).await.unwrap_err().code, "SSH_TRANSFER_SIGNATURE");
        // 只选了 web 没选它的跳板机
        let id = begin_import(&b, &[&web.id]).await;
        assert_eq!(wait(&b, &id, |v| v["state"] == "failed").await["error"]["code"], "SSH_TRANSFER_HOSTS");

        // 正常流程
        let id = begin_import(&b, &[&bastion.id, &web.id]).await;
        let v = wait(&b, &id, |v| v["state"] == "waiting").await;
        let code = v["code"].as_str().unwrap().to_string();
        assert_eq!(v["from_name"], "deva-pc");
        let p = pending(&a);
        assert_eq!(p["items"].as_array().unwrap().len(), 1);
        assert!(!p.to_string().contains(&code), "A 的接口不给确认码");
        assert_eq!(p["items"][0]["from_name"], "devb-pc");
        // 同一时刻只有一个待确认的导出
        let again = ImportReq { from: "deva".into(), sel: Selection { hosts: vec![bastion.id.clone()], agent_keys: vec![] }, resolve: Default::default(), names: vec![] };
        assert_eq!(begin(&b, again).await.unwrap_err().code, "SSH_TRANSFER_BUSY");
        let wrong = if code == "000000" { "111111" } else { "000000" };
        let e = approve(&a, &id, wrong, None, false).await.unwrap_err();
        assert_eq!((e.code.as_str(), e.params["remaining"].as_u64()), ("SSH_TRANSFER_CODE_WRONG", Some(2)));
        assert_eq!(approve(&a, &id, &code, None, true).await.unwrap_err().code, "SSH_TRANSFER_EMAIL_REQUIRED");
        approve(&a, &id, &format!("{} {}", &code[..3], &code[3..]), None, false).await.unwrap();
        let v = wait(&b, &id, |v| v["state"] == "done").await;
        assert_eq!(v["imported"].as_array().unwrap().len(), 2);
        let hosts = ssh::list(&b.store);
        let (nb, nw) = (hosts.iter().find(|h| h.name == "bastion").unwrap(), hosts.iter().find(|h| h.name == "web").unwrap());
        assert_eq!(nw.jump, nb.id);
        assert!(!nb.allow_ai && !nw.allow_ai);
        assert_eq!(ssh::secret(&b.store, hb.path(), &nb.id).unwrap().as_deref(), Some("hunter2-pw"));
        assert_eq!(ssh::secret(&b.store, hb.path(), &nw.id).unwrap().as_deref(), Some("key-passphrase"));
        assert!(v["tests"].as_object().unwrap().contains_key(&nb.id), "导入后测试了连接");
        // 两边都有记录，记录与进度里没有任何秘密
        let (la, lb) = (log(&a.store), log(&b.store));
        assert_eq!((la[0].dir.as_str(), la[0].result.as_str()), ("export", "ok"));
        assert_eq!((lb[0].dir.as_str(), lb[0].result.as_str()), ("import", "ok"));
        for text in [serde_json::to_string(&la).unwrap(), serde_json::to_string(&lb).unwrap(), v.to_string()] {
            for s in ["hunter2-pw", "key-passphrase", "c2VjcmV0LWtleS1ib2R5"] {
                assert!(!text.contains(s), "{s} 出现在 {text}");
            }
        }
        // 数据包只给一次
        assert_eq!(fetch(&a, IdReq { transfer_id: id.clone(), key_id: String::new(), sig: String::new() }).status, "expired");

        // A 上拒绝
        let id = begin_import(&b, &[&bastion.id]).await;
        wait(&b, &id, |v| v["state"] == "waiting").await;
        reject(&a, &id).unwrap();
        assert_eq!(wait(&b, &id, |v| v["state"] == "failed").await["error"]["code"], "SSH_TRANSFER_REJECTED");
        assert_eq!(log(&a.store)[0].result, "rejected");

        // 确认码输错 3 次：作废，B 那边随之失败
        let id = begin_import(&b, &[&bastion.id]).await;
        let code = wait(&b, &id, |v| v["state"] == "waiting").await["code"].as_str().unwrap().to_string();
        let wrong = if code == "000000" { "111111" } else { "000000" };
        assert_eq!(approve(&a, &id, wrong, None, false).await.unwrap_err().code, "SSH_TRANSFER_CODE_WRONG");
        assert_eq!(approve(&a, &id, wrong, None, false).await.unwrap_err().code, "SSH_TRANSFER_CODE_WRONG");
        assert_eq!(approve(&a, &id, wrong, None, false).await.unwrap_err().code, "SSH_TRANSFER_CODE_LOCKED");
        assert_eq!(approve(&a, &id, &code, None, false).await.unwrap_err().code, "SSH_TRANSFER_EXPIRED", "作废后对的码也没用");
        assert_eq!(wait(&b, &id, |v| v["state"] == "failed").await["error"]["code"], "SSH_TRANSFER_EXPIRED");

        // B 取消：A 那边的待确认随之消失
        let id = begin_import(&b, &[&bastion.id]).await;
        wait(&b, &id, |v| v["state"] == "waiting").await;
        cancel_import(&b, &id).unwrap();
        wait(&b, &id, |v| v["state"] == "canceled").await;
        for _ in 0..50 {
            if pending(&a)["items"].as_array().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(pending(&a)["items"].as_array().unwrap().is_empty());
        assert_eq!(log(&a.store)[0].result, "canceled");
    }
}
