//! 设置在电脑之间同步（docs/SETTINGS_SYNC.md）：同一账户的电脑经 `rtc::outbound` 直接对账，服务端不保存任何同步数据。
//!
//! - **记录**（§4.2）：`sync_record` 每条 `(collection, id)` 只留胜出的一版，带混合逻辑时钟 `hlc`、写入它的节点 `origin`
//!   和那个节点上的序号 `seq`；`sync_seen` 记着每个节点的改动本机看到了第几号（含已被覆盖掉的）；
//! - **合并**（§4.3）：`(hlc, origin)` 大的胜出，每台电脑对同一组记录算出的结果一样；收到的记录原样保存，所以改动能跟着中间的电脑传开；
//! - **对账**（§5.1）：交换摘要 `{origin: 看到的最大 seq}`，各自只发对方没看到的；合并是幂等的；
//! - **只接受电脑**（§5.2）：请求带设备私钥签名，对端用设备列表里的公钥校验（[`Outbound::verify_peer`]），浏览器签不出来；
//! - **什么时候同步**（§5.3）：本机改了同步项、有电脑上线、兜底每 15 分钟；“只看本机”、关掉同步、没登录时不同步。
//!
//! [`Outbound::verify_peer`]: crate::rtc::outbound::Outbound::verify_peer

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use looklook_protocol::signing::b64;
use rusqlite::{params, OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use crate::error::LocalError;
use crate::gateway::App;
use crate::rtc::outbound::PeerCheck;
use crate::store::Store;

/// 设置项的集合（id 是 `Settings` 的字段名，见 settings.rs 的 `SYNCED`）。
pub const SETTINGS: &str = "settings";
pub const EXCHANGE_PATH: &str = "/api/sync/exchange";

const KV_NODE: &str = "sync_node";
const KV_SEQ: &str = "sync_seq";
const KV_ENABLED: &str = "sync_enabled";

/// 一次最多发多少条，超过就分批（§5.1）。
const BATCH: usize = 500;
/// 一次对账最多来回几趟（正常两趟就齐了，分批时多几趟）。
const MAX_ROUNDS: usize = 8;
const MAX_ID: usize = 200;
const MAX_VALUE: usize = 64 * 1024;
const REPLY_MAX: usize = 8 * 1024 * 1024;
/// 请求时间戳与本机可信时钟最多差多少。
const MAX_SKEW_MS: i64 = 5 * 60 * 1000;
/// 本机改了同步项后等多久再发（连着改几下只同步一次）。
const DEBOUNCE: Duration = Duration::from_secs(3);
/// 多久看一次设备列表（发现新上线的电脑）。
const CHECK: Duration = Duration::from_secs(120);
/// 兜底：和每台在线电脑至少这么久对一次账。
const FULL_MS: i64 = 15 * 60 * 1000;
/// 一台电脑一次对账的期限。
const EXCHANGE_LIMIT: Duration = Duration::from_secs(60);
/// 墓碑保留多久（§4.4）。
const TOMBSTONE_MS: i64 = 30 * 24 * 3600 * 1000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    pub collection: String,
    pub id: String,
    /// JSON；删除时为 None。
    pub value: Option<String>,
    #[serde(default)]
    pub deleted: bool,
    pub hlc: String,
    pub origin: String,
    pub seq: i64,
}

/// 每个节点的改动看到了第几号。
pub type Summary = BTreeMap<String, i64>;

// ---------------- 混合逻辑时钟 ----------------

fn hlc(ms: i64, n: u32) -> String {
    format!("{:012x}-{:04x}", ms.max(0), n.min(0xffff))
}

fn parse_hlc(s: &str) -> Option<(i64, u32)> {
    let (ms, n) = s.split_once('-')?;
    if ms.len() != 12 || n.len() != 4 {
        return None;
    }
    Some((i64::from_str_radix(ms, 16).ok()?, u32::from_str_radix(n, 16).ok()?))
}

/// 本机下一次写入用的 hlc：不早于已见过的最大值，本机时钟落后时沿用最大值、计数加 1。
fn next_hlc(tx: &Transaction<'_>, now_ms: i64) -> rusqlite::Result<String> {
    let max: Option<String> = tx.query_row("SELECT MAX(hlc) FROM sync_record", [], |r| r.get(0))?;
    let (ms, n) = max.as_deref().and_then(parse_hlc).unwrap_or((0, 0));
    Ok(if now_ms > ms { hlc(now_ms, 0) } else { hlc(ms, n + 1) })
}

/// 升级时导入的旧值用的 hlc（§10）：比之后任何一次真实修改都早。
fn imported_hlc() -> String {
    hlc(0, 0)
}

// ---------------- 存储 ----------------

fn kv_get(tx: &Transaction<'_>, key: &str) -> rusqlite::Result<Option<String>> {
    tx.query_row("SELECT value FROM kv WHERE key=?1", [key], |r| r.get(0)).optional()
}

fn kv_set(tx: &Transaction<'_>, key: &str, value: &str) -> rusqlite::Result<()> {
    tx.execute("INSERT INTO kv(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value", params![key, value])?;
    Ok(())
}

/// 本机的同步节点 id：第一次用时生成，之后不变（和设备 id 无关，重新登录也不换）。
fn node(tx: &Transaction<'_>) -> rusqlite::Result<String> {
    if let Some(v) = kv_get(tx, KV_NODE)? {
        if let Ok(id) = serde_json::from_str::<String>(&v) {
            return Ok(id);
        }
    }
    let id = crate::util::random_id(16);
    kv_set(tx, KV_NODE, &serde_json::to_string(&id).unwrap_or_default())?;
    Ok(id)
}

fn see(tx: &Transaction<'_>, origin: &str, seq: i64) -> rusqlite::Result<()> {
    tx.execute(
        "INSERT INTO sync_seen(origin,max_seq) VALUES(?1,?2) ON CONFLICT(origin) DO UPDATE SET max_seq=MAX(max_seq,excluded.max_seq)",
        params![origin, seq],
    )?;
    Ok(())
}

fn existing(tx: &Transaction<'_>, collection: &str, id: &str) -> rusqlite::Result<Option<Record>> {
    tx.query_row("SELECT collection,id,value,deleted,hlc,origin,seq FROM sync_record WHERE collection=?1 AND id=?2", params![collection, id], row).optional()
}

fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Record> {
    Ok(Record { collection: r.get(0)?, id: r.get(1)?, value: r.get(2)?, deleted: r.get::<_, i64>(3)? != 0, hlc: r.get(4)?, origin: r.get(5)?, seq: r.get(6)? })
}

fn put(tx: &Transaction<'_>, r: &Record) -> rusqlite::Result<()> {
    tx.execute(
        "INSERT INTO sync_record(collection,id,value,deleted,hlc,origin,seq) VALUES(?1,?2,?3,?4,?5,?6,?7)
         ON CONFLICT(collection,id) DO UPDATE SET value=excluded.value,deleted=excluded.deleted,hlc=excluded.hlc,origin=excluded.origin,seq=excluded.seq",
        params![r.collection, r.id, r.value, r.deleted as i64, r.hlc, r.origin, r.seq],
    )?;
    Ok(())
}

/// `a` 是否胜过 `b`（§4.3）。
fn wins(a: &Record, b: &Record) -> bool {
    (&a.hlc, &a.origin) > (&b.hlc, &b.origin)
}

/// 本机改动：值和现在的一样就跳过；返回是否写了。`None` 是删除（写墓碑）。
pub fn write_local(store: &Store, now_ms: i64, collection: &str, items: &[(&str, Option<Value>)]) -> Result<bool> {
    store.tx(|tx| {
        let me = node(tx)?;
        let mut wrote = false;
        for (id, value) in items {
            let value = value.as_ref().map(|v| v.to_string());
            if let Some(old) = existing(tx, collection, id)? {
                if old.value == value && old.deleted == value.is_none() {
                    continue;
                }
            } else if value.is_none() {
                continue;
            }
            let seq: i64 = kv_get(tx, KV_SEQ)?.and_then(|v| v.parse().ok()).unwrap_or(0) + 1;
            kv_set(tx, KV_SEQ, &seq.to_string())?;
            let r = Record { collection: collection.into(), id: (*id).into(), deleted: value.is_none(), value, hlc: next_hlc(tx, now_ms)?, origin: me.clone(), seq };
            put(tx, &r)?;
            see(tx, &me, seq)?;
            wrote = true;
        }
        Ok(wrote)
    })
}

/// 升级时把现有值导入（§10）：只导入还没有记录的项，hlc 取最早，所以之后任何一次真实修改都优先。
pub fn import(store: &Store, collection: &str, items: &[(&str, Value)]) -> Result<()> {
    store.tx(|tx| {
        let me = node(tx)?;
        for (id, value) in items {
            if existing(tx, collection, id)?.is_some() {
                continue;
            }
            let seq: i64 = kv_get(tx, KV_SEQ)?.and_then(|v| v.parse().ok()).unwrap_or(0) + 1;
            kv_set(tx, KV_SEQ, &seq.to_string())?;
            put(tx, &Record { collection: collection.into(), id: (*id).into(), value: Some(value.to_string()), deleted: false, hlc: imported_hlc(), origin: me.clone(), seq })?;
            see(tx, &me, seq)?;
        }
        Ok(())
    })
}

/// 一个集合里现有的值（不含墓碑）。
pub fn values(store: &Store, collection: &str) -> Result<HashMap<String, Value>> {
    store.tx(|tx| {
        let mut st = tx.prepare("SELECT id,value FROM sync_record WHERE collection=?1 AND deleted=0")?;
        let rows = st.query_map([collection], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?)))?;
        let mut out = HashMap::new();
        for r in rows {
            let (id, v) = r?;
            if let Some(v) = v.and_then(|v| serde_json::from_str(&v).ok()) {
                out.insert(id, v);
            }
        }
        Ok(out)
    })
}

pub fn summary(store: &Store) -> Result<Summary> {
    store.tx(|tx| {
        let mut st = tx.prepare("SELECT origin,max_seq FROM sync_seen")?;
        let rows = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
        rows.collect()
    })
}

/// 对方没看到的记录（按对方的摘要），最多 `limit` 条；第二项是还有没有剩下的。
pub fn missing(store: &Store, theirs: &Summary, limit: usize) -> Result<(Vec<Record>, bool)> {
    let all: Vec<Record> = store.tx(|tx| {
        let mut st = tx.prepare("SELECT collection,id,value,deleted,hlc,origin,seq FROM sync_record ORDER BY origin, seq")?;
        let rows = st.query_map([], row)?;
        rows.collect()
    })?;
    let mut out: Vec<Record> = all.into_iter().filter(|r| r.seq > theirs.get(&r.origin).copied().unwrap_or(0)).collect();
    let more = out.len() > limit;
    out.truncate(limit);
    Ok((out, more))
}

fn valid(r: &Record) -> bool {
    let name = |s: &str| !s.is_empty() && s.len() <= MAX_ID && s.chars().all(|c| c.is_ascii_alphanumeric() || "_-.:".contains(c));
    name(&r.collection)
        && name(&r.id)
        && name(&r.origin)
        && r.seq > 0
        && parse_hlc(&r.hlc).is_some()
        && r.deleted == r.value.is_none()
        && r.value.as_ref().is_none_or(|v| v.len() <= MAX_VALUE && serde_json::from_str::<Value>(v).is_ok())
}

/// 合并收到的记录，返回改了几条。不认识的集合也照样保存，让新版本的数据能经过旧版本的电脑传开。
pub fn merge(store: &Store, records: &[Record]) -> Result<usize> {
    store.tx(|tx| {
        let mut changed = 0;
        for r in records.iter().filter(|r| valid(r)) {
            let take = match existing(tx, &r.collection, &r.id)? {
                Some(old) => wins(r, &old),
                None => true,
            };
            if take {
                put(tx, r)?;
                changed += 1;
            }
            see(tx, &r.origin, r.seq)?;
        }
        Ok(changed)
    })
}

/// 清理超过 30 天的墓碑（§4.4）。
fn purge(store: &Store, now_ms: i64) -> Result<()> {
    let cutoff = hlc(now_ms - TOMBSTONE_MS, 0);
    store.tx(|tx| tx.execute("DELETE FROM sync_record WHERE deleted=1 AND hlc < ?1", [cutoff]).map(|_| ()))
}

pub fn enabled(store: &Store) -> bool {
    store.get::<bool>(KV_ENABLED).ok().flatten().unwrap_or(true)
}

pub fn set_enabled(store: &Store, on: bool) -> Result<()> {
    store.set(KV_ENABLED, &on)
}

// ---------------- 协议 ----------------

#[derive(Serialize, Deserialize)]
pub struct ExchangeReq {
    pub v: u8,
    pub from: String,
    pub to: String,
    pub ts: i64,
    pub key: String,
    pub sig: String,
    /// [`Payload`] 的 JSON：签名签的是它的摘要，所以原样带着字符串。
    pub payload: String,
}

#[derive(Serialize, Deserialize)]
struct Payload {
    summary: Summary,
    records: Vec<Record>,
}

#[derive(Serialize, Deserialize)]
pub struct ExchangeResp {
    summary: Summary,
    records: Vec<Record>,
    more: bool,
}

fn message(from: &str, to: &str, ts: i64, payload: &str) -> String {
    format!("looklook-sync-v1\n{from}\n{to}\n{ts}\n{}", b64(&Sha256::digest(payload.as_bytes())))
}

fn err(code: &str) -> LocalError {
    LocalError::new(code)
}

/// 本机能不能和别的电脑同步：登录了、能使用、没“只看本机”、没关掉同步。
fn ready(app: &App) -> Option<String> {
    let own = app.account.session()?.device_id;
    (app.account.gate().allowed() && !crate::gateway::local_only(app) && enabled(&app.store)).then_some(own)
}

/// 对端：处理另一台电脑发来的对账请求（`POST /api/sync/exchange`，只接受 `Access::Rtc`，见 api.rs）。
pub async fn exchange(app: &App, req: ExchangeReq) -> Result<ExchangeResp, LocalError> {
    let own = app.account.session().ok_or_else(|| err("NOT_LOGGED_IN"))?.device_id;
    if !enabled(&app.store) || crate::gateway::local_only(app) {
        return Err(err("SYNC_DISABLED"));
    }
    if req.v != 1 || req.to != own || (req.ts - app.account.clock.now_ms()).abs() > MAX_SKEW_MS {
        return Err(err("SYNC_SIGNATURE"));
    }
    let msg = message(&req.from, &req.to, req.ts, &req.payload);
    // 先用缓存的设备列表，验不过再取最新的（对方可能刚轮换了密钥）
    let mut check = app.outbound.verify_peer(&app.account, &req.from, &req.key, &msg, &req.sig, false).await?;
    if matches!(check, PeerCheck::Bad) {
        check = app.outbound.verify_peer(&app.account, &req.from, &req.key, &msg, &req.sig, true).await?;
    }
    match check {
        PeerCheck::Ok(_) => {}
        PeerCheck::Unsupported => return Err(err("SYNC_UNSUPPORTED")),
        PeerCheck::Bad => {
            tracing::warn!(dev = %req.from, "设置同步：对方的签名不对");
            return Err(err("SYNC_SIGNATURE"));
        }
    }
    let p: Payload = serde_json::from_str(&req.payload).map_err(|_| err("SYNC_PROTOCOL"))?;
    if p.records.len() > BATCH {
        return Err(err("SYNC_PROTOCOL"));
    }
    let changed = merge(&app.store, &p.records)?;
    if changed > 0 {
        tracing::info!(dev = %req.from, changed, "设置同步：收到另一台电脑的改动");
    }
    let (records, more) = missing(&app.store, &p.summary, BATCH)?;
    app.sync.synced(&req.from, app.account.clock.now_ms());
    Ok(ExchangeResp { summary: summary(&app.store)?, records, more })
}

/// 发起方：和一台电脑对一次账（§5.1）。
async fn dial(app: &App, own: &str, dev: &str) -> Result<usize, LocalError> {
    let link = app.outbound.link(&app.account, dev).await.map_err(|e| e.local())?;
    // 第一趟只带摘要，拿到对方的摘要后再发对方缺的；双方都没有剩下的就齐了
    let mut records = Vec::new();
    let mut changed = 0;
    for _ in 0..MAX_ROUNDS {
        let payload = serde_json::to_string(&Payload { summary: summary(&app.store)?, records }).map_err(anyhow::Error::from)?;
        let ts = app.account.clock.now_ms();
        let (key, sig) = app.account.sign_device(&message(own, dev, ts, &payload)).ok_or_else(|| err("NOT_LOGGED_IN"))?;
        let req = ExchangeReq { v: 1, from: own.into(), to: dev.into(), ts, key, sig, payload };
        let resp: ExchangeResp = crate::rtc::outbound::post_json(&link, EXCHANGE_PATH, &req, REPLY_MAX, "SYNC_PROTOCOL").await?.ok_or_else(|| err("SYNC_PROTOCOL"))?;
        changed += merge(&app.store, &resp.records)?;
        records = missing(&app.store, &resp.summary, BATCH)?.0;
        if records.is_empty() && !resp.more {
            break;
        }
    }
    Ok(changed)
}

#[derive(Default)]
pub struct Syncer {
    poke: Notify,
    /// 电脑 → 上次对账成功的时间（Unix 毫秒，界面显示用）。
    last: Mutex<HashMap<String, i64>>,
    /// 这次运行期间确认是旧版本（没有同步接口）的电脑，不再找它。
    old: Mutex<HashSet<String>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Syncer {
    /// 本机改了同步项。
    pub fn poke(&self) {
        self.poke.notify_one();
    }

    fn synced(&self, dev: &str, now_ms: i64) {
        lock(&self.old).remove(dev);
        lock(&self.last).insert(dev.to_string(), now_ms);
    }

    /// 界面：开关与每台电脑的同步情况（`GET /api/sync`）。
    pub async fn view(&self, app: &App) -> Value {
        let on = enabled(&app.store);
        let own = app.account.session().map(|s| s.device_id);
        let mut peers = Vec::new();
        if let (Some(own), false) = (own, crate::gateway::local_only(app)) {
            if let Ok(d) = app.outbound.devices(&app.account, false).await {
                let last = lock(&self.last).clone();
                let old = lock(&self.old).clone();
                for x in d.items.into_iter().filter(|x| x.id != own) {
                    peers.push(json!({ "id": x.id, "name": x.name, "online": x.online, "last": last.get(&x.id), "old": old.contains(&x.id) }));
                }
            }
        }
        json!({ "enabled": on, "local_only": crate::gateway::local_only(app), "peers": peers })
    }

    /// 后台任务（§5.3）。
    pub async fn run(self: Arc<Self>, app: App, shutdown: CancellationToken) {
        let mut seen_online: HashSet<String> = HashSet::new();
        loop {
            let poked = tokio::select! {
                _ = shutdown.cancelled() => return,
                _ = self.poke.notified() => true,
                _ = tokio::time::sleep(CHECK) => false,
            };
            if poked {
                // 连着改几下只同步一次
                tokio::select! {
                    _ = shutdown.cancelled() => return,
                    _ = tokio::time::sleep(DEBOUNCE) => {}
                }
            }
            let Some(own) = ready(&app) else {
                seen_online.clear();
                continue;
            };
            let now = app.account.clock.now_ms();
            if let Err(e) = purge(&app.store, now) {
                tracing::warn!(error = %e, "设置同步：清理墓碑失败");
            }
            let devices = match app.outbound.devices(&app.account, false).await {
                Ok(d) => d,
                Err(_) => continue,
            };
            let online: HashSet<String> = devices.items.iter().filter(|d| d.online && d.id != own).map(|d| d.id.clone()).collect();
            for dev in &online {
                if lock(&self.old).contains(dev) {
                    continue;
                }
                let stale = lock(&self.last).get(dev).is_none_or(|t| now - t >= FULL_MS);
                if !(poked || stale || !seen_online.contains(dev)) {
                    continue;
                }
                match tokio::time::timeout(EXCHANGE_LIMIT, dial(&app, &own, dev)).await {
                    Ok(Ok(changed)) => {
                        self.synced(dev, app.account.clock.now_ms());
                        if changed > 0 {
                            tracing::info!(%dev, changed, "设置同步：从另一台电脑拿到改动");
                        }
                    }
                    Ok(Err(e)) if e.status == axum::http::StatusCode::NOT_FOUND => {
                        tracing::info!(%dev, "设置同步：那台电脑的看看版本太旧，这次运行期间不再找它");
                        lock(&self.old).insert(dev.clone());
                    }
                    Ok(Err(e)) => tracing::info!(%dev, code = %e.code, "设置同步：这次没同步上，稍后再试"),
                    Err(_) => tracing::info!(%dev, "设置同步：超时，稍后再试"),
                }
            }
            seen_online = online;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Store {
        Store::memory().unwrap()
    }

    /// 两个库互相对账到齐（模拟 dial + exchange，不经网络）。
    fn sync(a: &Store, b: &Store) {
        for _ in 0..4 {
            let (to_b, _) = missing(a, &summary(b).unwrap(), BATCH).unwrap();
            merge(b, &to_b).unwrap();
            let (to_a, _) = missing(b, &summary(a).unwrap(), BATCH).unwrap();
            merge(a, &to_a).unwrap();
        }
    }

    fn theme(s: &Store) -> Option<Value> {
        values(s, SETTINGS).unwrap().get("theme").cloned()
    }

    #[test]
    fn later_write_wins_everywhere() {
        let (a, b, c) = (store(), store(), store());
        write_local(&a, 1_000, SETTINGS, &[("theme", Some(json!("mint")))]).unwrap();
        write_local(&b, 2_000, SETTINGS, &[("theme", Some(json!("sunset")))]).unwrap();
        sync(&a, &b);
        sync(&b, &c);
        for s in [&a, &b, &c] {
            assert_eq!(theme(s), Some(json!("sunset")));
        }
    }

    #[test]
    fn changes_travel_through_a_middle_computer() {
        let (a, b, c) = (store(), store(), store());
        write_local(&a, 1_000, SETTINGS, &[("font_size", Some(json!(18)))]).unwrap();
        sync(&a, &b);
        sync(&b, &c);
        assert_eq!(values(&c, SETTINGS).unwrap().get("font_size"), Some(&json!(18)));
    }

    #[test]
    fn a_slow_clock_still_wins_after_seeing_newer() {
        let (a, b) = (store(), store());
        write_local(&a, 10_000_000, SETTINGS, &[("theme", Some(json!("mint")))]).unwrap();
        sync(&a, &b);
        // b 的时钟慢了一小时，但它是在看到 a 的改动之后改的
        write_local(&b, 10_000_000 - 3_600_000, SETTINGS, &[("theme", Some(json!("light")))]).unwrap();
        sync(&a, &b);
        assert_eq!(theme(&a), Some(json!("light")));
    }

    #[test]
    fn same_value_is_not_a_change() {
        let a = store();
        assert!(write_local(&a, 1, SETTINGS, &[("theme", Some(json!("mint")))]).unwrap());
        assert!(!write_local(&a, 2, SETTINGS, &[("theme", Some(json!("mint")))]).unwrap());
    }

    #[test]
    fn imported_values_lose_to_any_real_edit() {
        let (a, b) = (store(), store());
        import(&a, SETTINGS, &[("theme", json!("dark"))]).unwrap();
        write_local(&b, 5, SETTINGS, &[("theme", Some(json!("mint")))]).unwrap();
        sync(&a, &b);
        assert_eq!(theme(&a), Some(json!("mint")));
        // 已有记录时不再导入
        import(&a, SETTINGS, &[("theme", json!("dark"))]).unwrap();
        assert_eq!(theme(&a), Some(json!("mint")));
    }

    #[test]
    fn two_imports_pick_the_same_winner_on_both_sides() {
        let (a, b) = (store(), store());
        import(&a, SETTINGS, &[("theme", json!("dark"))]).unwrap();
        import(&b, SETTINGS, &[("theme", json!("light"))]).unwrap();
        sync(&a, &b);
        assert_eq!(theme(&a), theme(&b));
    }

    #[test]
    fn summary_does_not_go_back_after_overwrite() {
        let (a, b) = (store(), store());
        write_local(&a, 1, SETTINGS, &[("theme", Some(json!("mint")))]).unwrap();
        sync(&a, &b);
        write_local(&b, 2, SETTINGS, &[("theme", Some(json!("light")))]).unwrap();
        sync(&a, &b);
        // a 的那条已被覆盖，但 b 仍记得看到过：a 不会再被要求重发
        let (again, _) = missing(&a, &summary(&b).unwrap(), BATCH).unwrap();
        assert!(again.is_empty(), "{again:?}");
    }

    #[test]
    fn tombstones_sync_and_expire() {
        let (a, b) = (store(), store());
        write_local(&a, 1_000, "ssh_host", &[("h1", Some(json!({"host": "x"})))]).unwrap();
        sync(&a, &b);
        write_local(&a, 2_000, "ssh_host", &[("h1", None)]).unwrap();
        sync(&a, &b);
        assert!(values(&b, "ssh_host").unwrap().is_empty());
        purge(&b, 2_000 + TOMBSTONE_MS + 1).unwrap();
        let (left, _) = missing(&b, &Summary::new(), BATCH).unwrap();
        assert!(left.is_empty());
    }

    #[test]
    fn bad_records_are_ignored() {
        let a = store();
        let bad = Record { collection: "settings".into(), id: "theme".into(), value: Some("not json".into()), deleted: false, hlc: hlc(1, 0), origin: "x".into(), seq: 1 };
        assert_eq!(merge(&a, &[bad.clone()]).unwrap(), 0);
        assert_eq!(merge(&a, &[Record { hlc: "zz".into(), value: Some("1".into()), ..bad.clone() }]).unwrap(), 0);
        assert_eq!(merge(&a, &[Record { value: Some("1".into()), ..bad }]).unwrap(), 1);
    }

    #[test]
    fn hlc_orders_as_text() {
        assert!(hlc(1, 5) < hlc(2, 0));
        assert!(hlc(2, 0) < hlc(2, 1));
        assert_eq!(parse_hlc(&hlc(0x1234, 7)), Some((0x1234, 7)));
    }

    /// 两台电脑经真的路由和 WebRTC 对账（借用 SSH 导入的双机测试环境：假平台把 B 的连接交给 A 的路由）。
    #[tokio::test(flavor = "multi_thread")]
    async fn two_computers_sync_over_rtc() {
        use crate::gateway::Access;
        use crate::settings::Settings;
        use crate::ssh_transfer::e2e::{app, platform, via_router, Target};
        use ed25519_dalek::SigningKey;

        let (ka, kb) = (SigningKey::from_bytes(&[11u8; 32]), SigningKey::from_bytes(&[22u8; 32]));
        let target: Target = Default::default();
        let url = platform(vec![("deva", ka.clone()), ("devb", kb.clone())], target.clone()).await;
        let (ha, hb) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let a = app(ha.path(), &url, "deva", ka);
        let b = app(hb.path(), &url, "devb", kb);
        *lock_target(&target) = Some(crate::gateway::router(a.clone()));

        // B 改配色，A 改字号和工作文件夹（不同步的字段）；B 去和 A 对账后两边一致
        let now = b.account.clock.now_ms();
        assert!(Settings { theme: "mint".into(), ..Settings::load(&b.store) }.save(&b.store, now).unwrap());
        Settings { font_size: 20, default_workdir: "/only-a".into(), ..Settings::load(&a.store) }.save(&a.store, now + 1).unwrap();
        dial(&b, "devb", "deva").await.unwrap();
        for s in [&a, &b] {
            let s = Settings::load(&s.store);
            assert_eq!((s.theme.as_str(), s.font_size), ("mint", 20));
        }
        assert_ne!(Settings::load(&b.store).default_workdir, "/only-a");
        // 对端记下了和谁同步过（界面显示用）
        assert!(a.sync.last.lock().unwrap().contains_key("devb"));

        // 只接受经 WebRTC、带设备签名的请求：中转回退、本机、浏览器（签不出名）都不行
        let forged = json!({ "v": 1, "from": "devb", "to": "deva", "ts": a.account.clock.now_ms(), "key": "k-devb", "sig": "AAAA", "payload": "{\"summary\":{},\"records\":[]}" });
        let rtc = Access::Rtc { origin: "http://alice.looklook.test".into() };
        let remote = Access::Remote { host: "alice.looklook.test".into(), proto: "https".into() };
        assert_eq!(via_router(&a, remote, EXCHANGE_PATH, forged.clone()).await.1, "SYNC_NEED_RTC");
        assert_eq!(via_router(&a, Access::Local, EXCHANGE_PATH, forged.clone()).await.1, "SYNC_NEED_RTC");
        assert_eq!(via_router(&a, rtc.clone(), EXCHANGE_PATH, forged.clone()).await.1, "SYNC_SIGNATURE");
        let mut not_me = forged.clone();
        not_me["to"] = "devc".into();
        assert_eq!(via_router(&a, rtc, EXCHANGE_PATH, not_me).await.1, "SYNC_SIGNATURE");

        // A 关掉同步：B 来对账被拒
        set_enabled(&a.store, false).unwrap();
        assert_eq!(dial(&b, "devb", "deva").await.unwrap_err().code, "SYNC_DISABLED");
    }

    fn lock_target(t: &crate::ssh_transfer::e2e::Target) -> std::sync::MutexGuard<'_, Option<axum::Router>> {
        t.lock().unwrap_or_else(|e| e.into_inner())
    }
}
