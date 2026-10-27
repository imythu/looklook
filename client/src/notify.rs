//! 通知（docs/TASK_BOARD.md §3.3、§6）：人不在电脑前，也能在飞书里收到“等你处理 / 做完了 / 出错了”并直接处理。
//!
//! - **配置**放在同步集合 `notify`（`feishu`：应用凭证和绑定的人；`rules`：规则），经“设置同步”在用户自己的电脑之间传；
//! - **发**：每台电脑只管自己的会话，每 2 秒看一遍各会话的状态（`Chat::brief`），按规则发卡片；
//!   “等你处理”解除时把卡片改成灰色（§6.4），同一个会话只留一张能点的卡片；
//! - **收**：每台电脑都连长连接，飞书把每个回调随机给其中一台（集群模式）。会话在本机就直接处理；
//!   在别的电脑上就先回一个 toast，再经 WebRTC 用设备签名转给那台（`POST /api/notify/action`），由它处理并更新卡片；
//! - **绑定**：设置页显示 6 位绑定码 = HMAC(App Secret, 时间窗) 的前 6 位，私聊机器人发这个码。收到的电脑自己就能校验，不用存。

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use hmac::{Hmac, Mac};
use looklook_protocol::signing::b64;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

use crate::error::LocalError;
use crate::feishu::card::{self, Act, Attn, Info};
use crate::feishu::{self, ws, Api};
use crate::gateway::App;
use crate::rtc::outbound::PeerCheck;
use crate::settings_sync;

pub const COLLECTION: &str = "notify";
pub const ACTION_PATH: &str = "/api/notify/action";

/// 进入“等你处理”多久还没处理才发（§3.3）
const ATTN_DELAY_MS: i64 = 10_000;
/// 一轮至少跑了这么久，做完才发
const DONE_MIN_MS: i64 = 30_000;
/// 每台电脑每分钟最多发几条，多的合并（§6.5）
const PER_MINUTE: usize = 20;
/// 回调里处理本机操作最多等多久（飞书要 3 秒内应答）
const LOCAL_WAIT: Duration = Duration::from_millis(2300);
const PAIR_WINDOW_MS: i64 = 10 * 60 * 1000;
const MAX_SKEW_MS: i64 = 5 * 60 * 1000;

/// 飞书应用（同步项 `notify/feishu`）
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Feishu {
    pub app_id: String,
    pub app_secret: String,
    /// feishu / lark
    #[serde(default)]
    pub domain: String,
    /// 绑定的人（私聊收通知）
    #[serde(default)]
    pub open_id: Option<String>,
    #[serde(default)]
    pub bound_at: Option<i64>,
    /// 测试卡片上的按钮点通过：回调配好了
    #[serde(default)]
    pub callback_ok: bool,
}

/// 规则（同步项 `notify/rules`，§6.5）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Rules {
    pub done: bool,
    pub failed: bool,
    /// 能在飞书里处理（允许、拒绝、选择、回复）；关掉 = 只通知
    pub actions: bool,
    /// 卡片里显示命令和回复内容
    pub content: bool,
    /// 静音的项目（工作目录）
    pub muted: Vec<String>,
    /// 卡片的语言（设置页保存时带上界面语言）
    pub lang: String,
}

impl Default for Rules {
    fn default() -> Self {
        Self { done: true, failed: true, actions: true, content: true, muted: vec![], lang: "zh-CN".into() }
    }
}

pub fn load_feishu(store: &crate::store::Store) -> Option<Feishu> {
    let v = settings_sync::values(store, COLLECTION).ok()?.remove("feishu")?;
    serde_json::from_value::<Feishu>(v).ok().filter(|f| !f.app_id.is_empty() && !f.app_secret.is_empty())
}

pub fn load_rules(store: &crate::store::Store) -> Rules {
    settings_sync::values(store, COLLECTION).ok().and_then(|mut m| m.remove("rules")).and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default()
}

fn save(app: &App, id: &str, v: Option<Value>) -> Result<(), LocalError> {
    if settings_sync::write_local(&app.store, app.account.clock.now_ms(), COLLECTION, &[(id, v)])? {
        app.sync.poke();
    }
    Ok(())
}

pub fn save_feishu(app: &App, f: Option<&Feishu>) -> Result<(), LocalError> {
    save(app, "feishu", f.map(|f| serde_json::to_value(f).unwrap_or_default()))
}

pub fn save_rules(app: &App, r: &Rules) -> Result<(), LocalError> {
    save(app, "rules", Some(serde_json::to_value(r).unwrap_or_default()))
}

/// 绑定码：HMAC-SHA256(App Secret, 应用 + 10 分钟时间窗) 取 6 位数字
pub fn pair_code(f: &Feishu, now_ms: i64) -> String {
    pair_code_at(f, now_ms.div_euclid(PAIR_WINDOW_MS))
}

fn pair_code_at(f: &Feishu, window: i64) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(f.app_secret.as_bytes()).expect("HMAC 接受任意长度的密钥");
    mac.update(format!("looklook-pair:{}:{window}", f.app_id).as_bytes());
    let d = mac.finalize().into_bytes();
    format!("{:06}", u32::from_be_bytes([d[0], d[1], d[2], d[3]]) % 1_000_000)
}

/// 这个码对不对：当前或上一个时间窗的都算（刚好跨过 10 分钟的边界）
pub fn pair_ok(f: &Feishu, code: &str, now_ms: i64) -> bool {
    let w = now_ms.div_euclid(PAIR_WINDOW_MS);
    code.len() == 6 && (pair_code_at(f, w) == code || pair_code_at(f, w - 1) == code)
}

/// 当前绑定码到什么时候失效（下一个时间窗开始时）
pub fn pair_expires(now_ms: i64) -> i64 {
    (now_ms.div_euclid(PAIR_WINDOW_MS) + 1) * PAIR_WINDOW_MS
}

/// “等你处理”是哪一件事：同一件事只发一张卡片
pub fn attn_key(brief: &Value) -> Option<String> {
    if brief["board"] != "attn" {
        return None;
    }
    let a = &brief["attn"];
    Some(match a["pending_id"].as_u64() {
        Some(id) => format!("p{id}"),
        None => format!("{}:{}", a["kind"].as_str().unwrap_or("tui"), a["title"].as_str().unwrap_or_default().chars().take(60).collect::<String>()),
    })
}

/// “等你处理”的卡片该发了：进入 10 秒还没处理、没人正开着、没静音
fn attn_due(brief: &Value, now: i64, muted: bool) -> bool {
    let since = brief["since"].as_i64().unwrap_or(now);
    now - since >= ATTN_DELAY_MS && brief["watched"] != true && !muted
}

/// “等你处理”解除了，卡片上写什么（§6.4）
fn close_text(brief: &Value, en: bool) -> String {
    if attn_key(brief).is_some() {
        tr(en, "已被新的请求替换", "Replaced by a newer request")
    } else if brief["board"] == "stopped" {
        tr(en, "会话已结束", "Session ended")
    } else if brief["phase"] == "tui" {
        tr(en, "已转到终端", "Moved to the terminal")
    } else {
        tr(en, "已在看看或终端里处理", "Handled in LookLook or the terminal")
    }
}

/// 一轮结束了怎么办
#[derive(Debug, PartialEq)]
enum EndNotice {
    /// 没有新结束的一轮
    Nothing,
    /// 做完了但摘要还没补上：下一次再看
    Wait,
    /// 记下，不发
    Record,
    Send { failed: bool },
}

fn end_notice(brief: &Value, last_end: i64, first: bool, rules: &Rules, muted: bool, now: i64) -> EndNotice {
    let turn = &brief["turn"];
    let Some(end) = turn["ended_at"].as_i64().filter(|e| *e > last_end) else { return EndNotice::Nothing };
    // 刚启动（或刚绑定）时已经结束的那一轮不补发
    if first {
        return EndNotice::Record;
    }
    let outcome = turn["outcome"].as_str().unwrap_or_default();
    // 摘要由 Chat::background 稍后补上：做完的一轮等它几秒
    if outcome == "done" && turn["summary"].as_str().is_none_or(str::is_empty) && now - end < 8_000 {
        return EndNotice::Wait;
    }
    let started = turn["started_at"].as_i64().unwrap_or(end);
    let want = match outcome {
        "done" => rules.done && end - started >= DONE_MIN_MS,
        "failed" => rules.failed,
        _ => false,
    };
    let looked = brief["seen_at"].as_i64().unwrap_or(0) >= end || brief["watched"] == true;
    if want && !looked && !muted && now - end < 120_000 {
        EndNotice::Send { failed: outcome == "failed" }
    } else {
        EndNotice::Record
    }
}

/// 发出去的一张卡片
#[derive(Debug, Clone)]
struct Sent {
    msg: Option<String>,
    inst: String,
    /// “等你处理”的卡片：对应哪件事；结束的卡片为 None
    key: Option<String>,
    nonce: String,
    /// 正在处理飞书上点的操作
    busy: bool,
    /// 卡片上的公共信息（改成“已处理”时用）
    info: Owned,
}

/// [`Info`] 的自有版本
#[derive(Debug, Clone, Default)]
struct Owned {
    en: bool,
    agent: String,
    title: String,
    project: String,
    device: String,
    url: String,
    content: bool,
    actions: bool,
}

impl Owned {
    fn info(&self) -> Info<'_> {
        Info { en: self.en, agent: &self.agent, title: &self.title, project: &self.project, device: &self.device, url: &self.url, content: self.content, actions: self.actions }
    }
}

/// 各个会话上次看到的情况
#[derive(Debug, Default)]
struct Seen {
    /// 上次通知过（或开始时已经有）的一轮的结束时间
    last_end: i64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Status {
    pub connected: bool,
    /// 最近一次错误：`{code, msg, at}`
    pub error: Option<Value>,
    pub sent_at: Option<i64>,
}

#[derive(Default)]
pub struct Notifier {
    status: Mutex<Status>,
    /// 消息 nonce → 卡片（最多留 500 张）
    cards: Mutex<VecDeque<Sent>>,
    seen: Mutex<HashMap<String, Seen>>,
    /// 回调去重：event_id → 收到时间
    events: Mutex<HashMap<String, Instant>>,
    sent: Mutex<VecDeque<Instant>>,
    dropped: AtomicUsize,
    /// 长连接正在用的配置（变了就重连）
    conn: Mutex<Option<(String, tokio::task::JoinHandle<()>)>>,
    api: Mutex<Option<(String, Arc<Api>)>>,
    /// 凭证错了：不再重试刷屏，等配置变了再说
    auth_failed: AtomicBool,
    http: std::sync::OnceLock<reqwest::Client>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn nonce() -> String {
    use rand::RngCore;
    let mut b = [0u8; 12];
    rand::thread_rng().fill_bytes(&mut b);
    b64(&b)
}

fn tr(en: bool, zh: &str, e: &str) -> String {
    if en { e.into() } else { zh.into() }
}

fn toast(kind: &str, text: &str) -> Value {
    json!({ "toast": { "type": kind, "content": text } })
}

fn with_card(mut v: Value, card: Value) -> Value {
    v["card"] = json!({ "type": "raw", "data": card });
    v
}

impl Notifier {
    fn http(&self) -> reqwest::Client {
        self.http.get_or_init(|| reqwest::Client::builder().connect_timeout(Duration::from_secs(10)).timeout(Duration::from_secs(20)).build().unwrap_or_default()).clone()
    }

    pub fn status(&self) -> Status {
        lock(&self.status).clone()
    }

    fn set_error(&self, code: i64, msg: &str) {
        lock(&self.status).error = Some(json!({ "code": code, "msg": msg, "at": crate::util::system_ms() }));
    }

    fn record_err(&self, e: &anyhow::Error) {
        let code = feishu::code_of(e).unwrap_or(-1);
        tracing::warn!(error = %e, "飞书通知");
        self.set_error(code, &e.to_string());
        if feishu::is_auth_error(code) {
            self.auth_failed.store(true, Ordering::SeqCst);
        }
    }

    /// 发消息用的接口客户端（凭证变了就换一个）
    pub fn api(&self, f: &Feishu) -> Arc<Api> {
        let key = format!("{}\n{}\n{}", f.domain, f.app_id, f.app_secret);
        let mut g = lock(&self.api);
        if let Some((k, a)) = g.as_ref() {
            if *k == key {
                return a.clone();
            }
        }
        let a = Arc::new(Api::new(self.http(), &f.domain, &f.app_id, &f.app_secret));
        *g = Some((key, a.clone()));
        a
    }

    /// 配置改了（设置页保存、同步过来）：清掉错误状态，下一轮重新连
    pub fn reset(&self) {
        self.auth_failed.store(false, Ordering::SeqCst);
        lock(&self.status).error = None;
    }

    /// 后台任务：管长连接、每 2 秒检查一遍会话
    pub async fn run(self: Arc<Self>, app: App, shutdown: CancellationToken) {
        let mut tick = 0u32;
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => break,
                _ = tokio::time::sleep(Duration::from_secs(2)) => {}
            }
            tick = tick.wrapping_add(1);
            let f = load_feishu(&app.store);
            if tick.is_multiple_of(3) {
                self.keep_connected(&app, f.as_ref());
            }
            let Some(f) = f else { continue };
            if f.open_id.is_none() || self.auth_failed.load(Ordering::SeqCst) {
                continue;
            }
            self.check(&app, &f).await;
        }
        if let Some((_, h)) = lock(&self.conn).take() {
            h.abort();
        }
    }

    fn keep_connected(self: &Arc<Self>, app: &App, f: Option<&Feishu>) {
        let want = f.map(|f| format!("{}\n{}\n{}", f.domain, f.app_id, f.app_secret));
        let mut g = lock(&self.conn);
        let same = match (&*g, &want) {
            (Some((k, h)), Some(w)) => k == w && !h.is_finished(),
            (None, None) => true,
            _ => false,
        };
        if same {
            return;
        }
        if let Some((_, h)) = g.take() {
            h.abort();
            lock(&self.status).connected = false;
        }
        let (Some(want), Some(f)) = (want, f.cloned()) else { return };
        let me = self.clone();
        let app = app.clone();
        let h = tokio::spawn(async move { me.connect_loop(app, f).await });
        *g = Some((want, h));
    }

    async fn connect_loop(self: Arc<Self>, app: App, f: Feishu) {
        let http = self.http();
        let base = feishu::base(&f.domain);
        let handler = Callbacks { app: app.clone() };
        let mut backoff = 3u64;
        loop {
            let me = self.clone();
            let opened = Instant::now();
            let r = ws::run_once(&http, base, &f.app_id, &f.app_secret, &handler, move || {
                let mut s = lock(&me.status);
                s.connected = true;
                s.error = None;
                me.auth_failed.store(false, Ordering::SeqCst);
            })
            .await;
            lock(&self.status).connected = false;
            if let Err(e) = r {
                self.record_err(&e);
                if feishu::code_of(&e).is_some_and(feishu::is_auth_error) {
                    // 凭证或应用配置不对：改了配置会换掉这个任务；没改的话（比如刚去发布了应用）5 分钟后再试
                    tokio::time::sleep(Duration::from_secs(300)).await;
                    continue;
                }
            }
            if opened.elapsed() > Duration::from_secs(60) {
                backoff = 3;
            }
            let jitter = rand::random::<u64>() % 3;
            tokio::time::sleep(Duration::from_secs(backoff + jitter)).await;
            backoff = (backoff * 2).min(120);
        }
    }

    /// 一个会话的卡片信息
    fn info(&self, app: &App, id: &str, brief: &Value, rules: &Rules) -> Option<Owned> {
        let row = app.store.instance(id).ok().flatten()?;
        let en = rules.lang.starts_with("en");
        let agent = if brief["agent"] == "codex" { "Codex" } else { "Claude" };
        let title = brief["title"].as_str().filter(|t| !t.is_empty()).unwrap_or(&row.name).to_string();
        let project = std::path::Path::new(&row.workdir).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| row.workdir.clone());
        let device = crate::settings::Settings::load(&app.store).device_name();
        Some(Owned { en, agent: agent.into(), title, project, device, url: page_url(app, &format!("s/{id}")), content: rules.content, actions: rules.actions })
    }

    async fn check(&self, app: &App, f: &Feishu) {
        let rules = load_rules(&app.store);
        let now = crate::util::system_ms();
        let api = self.api(f);
        let open_id = f.open_id.clone().unwrap_or_default();
        let own = app.account.session().map(|s| s.device_id).unwrap_or_default();
        let ids = app.chat.ids();
        for id in &ids {
            let Some(brief) = app.chat.brief(id) else { continue };
            let muted = app.store.instance(id).ok().flatten().is_some_and(|r| rules.muted.contains(&r.workdir));
            let key = attn_key(&brief);
            // 这个会话现在挂着的卡片
            let live = lock(&self.cards).iter().rev().find(|s| s.inst == *id && s.key.is_some()).cloned();
            // 正在处理飞书上点的操作：卡片由那边更新，这里先不动
            let busy = live.as_ref().is_some_and(|s| s.busy);
            if let Some(s) = live.as_ref().filter(|_| !busy) {
                if s.key != key {
                    self.close(&api, s, &close_text(&brief, s.info.en)).await;
                }
            }
            if let Some(k) = key.as_ref().filter(|_| !busy) {
                let fresh = live.as_ref().is_none_or(|s| s.key.as_ref() != Some(k));
                if fresh && attn_due(&brief, now, muted) {
                    if let Some(info) = self.info(app, id, &brief, &rules) {
                        self.send_attn(&api, &open_id, &own, id, k, &brief, info).await;
                    }
                }
            }
            // 一轮结束
            let turn = &brief["turn"];
            let first = !lock(&self.seen).contains_key(id);
            let last = lock(&self.seen).entry(id.clone()).or_default().last_end;
            let notice = end_notice(&brief, last, first, &rules, muted, now);
            if let (EndNotice::Record | EndNotice::Send { .. }, Some(end)) = (&notice, turn["ended_at"].as_i64()) {
                lock(&self.seen).entry(id.clone()).or_default().last_end = end;
            }
            if let (EndNotice::Send { failed }, Some(end), Some(info)) = (notice, turn["ended_at"].as_i64(), self.info(app, id, &brief, &rules)) {
                let started = turn["started_at"].as_i64().unwrap_or(end);
                let text = if failed { turn["error"].as_str() } else { turn["summary"].as_str() }.unwrap_or_default();
                let n = nonce();
                let base = Act { dev: own.clone(), inst: id.clone(), n: n.clone(), ..Default::default() };
                let c = card::ended(&info.info(), failed, text, end - started, &base);
                let msg = self.send(&api, &open_id, &c).await;
                self.remember(Sent { msg, inst: id.clone(), key: None, nonce: n, busy: false, info });
            }
        }
        // 会话没了：清掉记录
        lock(&self.seen).retain(|k, _| ids.contains(k));
        // 合并发的那一条
        let dropped = self.dropped.load(Ordering::SeqCst);
        if dropped > 0 && self.budget() {
            self.dropped.store(0, Ordering::SeqCst);
            let c = card::digest(rules.lang.starts_with("en"), dropped, &page_url(app, "tasks"));
            self.send(&api, &open_id, &c).await;
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn send_attn(&self, api: &Api, open_id: &str, own: &str, id: &str, key: &str, brief: &Value, info: Owned) {
        let a = &brief["attn"];
        let options: Vec<String> = a["options"].as_array().map(|o| o.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
        let limit_wait = options.iter().position(|o| o.starts_with(crate::chat::screen::LIMIT_AUTO));
        let kind = a["kind"].as_str().unwrap_or("dialog");
        let at = Attn { kind, tool: a["tool"].as_str().unwrap_or_default(), detail: a["detail"].as_str().unwrap_or_default(), risk: a["risk"].as_str(), options: &options, limit_wait };
        let n = nonce();
        let base = Act { dev: own.into(), inst: id.into(), p: key.into(), n: n.clone(), ..Default::default() };
        let c = card::attn(&info.info(), &at, &base);
        let msg = self.send(api, open_id, &c).await;
        self.remember(Sent { msg, inst: id.into(), key: Some(key.into()), nonce: n, busy: false, info });
    }

    fn remember(&self, s: Sent) {
        let mut g = lock(&self.cards);
        g.push_back(s);
        while g.len() > 500 {
            g.pop_front();
        }
    }

    /// 这一分钟还能不能发
    fn budget(&self) -> bool {
        let mut g = lock(&self.sent);
        while g.front().is_some_and(|t| t.elapsed() > Duration::from_secs(60)) {
            g.pop_front();
        }
        g.len() < PER_MINUTE
    }

    /// 发一张卡片；超出每分钟上限时不发、记一笔（之后合并成一条）。返回消息 id
    async fn send(&self, api: &Api, open_id: &str, c: &Value) -> Option<String> {
        if !self.budget() {
            self.dropped.fetch_add(1, Ordering::SeqCst);
            return None;
        }
        lock(&self.sent).push_back(Instant::now());
        // 发给同一个人每秒最多 5 条
        tokio::time::sleep(Duration::from_millis(220)).await;
        match api.send_card(open_id, c).await {
            Ok(id) => {
                let mut s = lock(&self.status);
                s.sent_at = Some(crate::util::system_ms());
                s.error = None;
                Some(id)
            }
            Err(e) => {
                self.record_err(&e);
                None
            }
        }
    }

    /// 把一张“等你处理”的卡片改成灰色，写明结果
    async fn close(&self, api: &Api, s: &Sent, result: &str) {
        lock(&self.cards).retain(|c| c.nonce != s.nonce);
        if let Some(msg) = &s.msg {
            if let Err(e) = api.patch_card(msg, &card::resolved(&s.info.info(), result)).await {
                tracing::debug!(error = %e, "飞书：卡片没改成已处理");
            }
        }
    }

    fn card_by_nonce(&self, n: &str) -> Option<Sent> {
        lock(&self.cards).iter().find(|s| s.nonce == n).cloned()
    }

    fn set_busy(&self, n: &str, busy: bool) {
        if let Some(s) = lock(&self.cards).iter_mut().find(|s| s.nonce == n) {
            s.busy = busy;
        }
    }

    /// 在本机处理卡片上的操作。返回 (toast, 换上的卡片)
    pub async fn act(&self, app: &App, act: &Act, text: Option<&str>) -> Result<(String, Option<Value>), LocalError> {
        let en = load_rules(&app.store).lang.starts_with("en");
        let Some(sent) = self.card_by_nonce(&act.n).filter(|s| s.inst == act.inst) else {
            return Ok((tr(en, "这张卡片已经过期了，打开看看处理", "This card has expired. Open LookLook instead."), None));
        };
        self.set_busy(&sent.nonce, true);
        let r = self.act_on(app, act, text, &sent, en).await;
        match &r {
            Ok((_, Some(_))) => lock(&self.cards).retain(|c| c.nonce != sent.nonce),
            _ => self.set_busy(&sent.nonce, false),
        }
        r
    }

    async fn act_on(&self, app: &App, act: &Act, text: Option<&str>, sent: &Sent, en: bool) -> Result<(String, Option<Value>), LocalError> {
        let info = sent.info.clone();
        let brief = app.chat.brief(&act.inst);
        let stale = || -> (String, Option<Value>) {
            let r = tr(en, "已经处理过了", "Already handled");
            (r.clone(), Some(card::resolved(&info.info(), &r)))
        };
        let said = text.map(str::trim).filter(|t| !t.is_empty()).map(|t| t.chars().take(1000).collect::<String>());
        // 结束卡片上的回复：送进会话，卡片换成“已发给 AI”
        if act.a == "say" && sent.key.is_none() {
            let text = said.ok_or_else(|| LocalError::invalid("text", "required"))?;
            app.chat.input(&act.inst, &text, &[]).await?;
            tracing::info!(inst = %act.inst, "飞书：回复了一句话");
            return Ok((tr(en, "已发给 AI", "Sent to the AI"), Some(card::said(&info.info(), &text))));
        }
        let Some(brief) = brief else { return Ok(stale()) };
        if attn_key(&brief).as_deref() != Some(act.p.as_str()) || sent.key.as_deref() != Some(act.p.as_str()) {
            return Ok(stale());
        }
        let a = &brief["attn"];
        let option = |i: Option<usize>| -> Result<String, LocalError> {
            i.and_then(|i| a["options"][i].as_str()).map(String::from).ok_or_else(|| LocalError::invalid("i", "invalid"))
        };
        let result = match act.a.as_str() {
            "allow" | "deny" => {
                let id = a["pending_id"].as_u64().ok_or_else(|| LocalError::new("CHAT_STALE"))?;
                app.chat.answer(&act.inst, &crate::chat::PermissionAnswer { id, decision: act.a.clone(), message: None, answers: None })?;
                if act.a == "allow" { tr(en, "已允许 · 在飞书上", "Allowed · in Feishu") } else { tr(en, "已拒绝 · 在飞书上", "Denied · in Feishu") }
            }
            // 提问卡片上写的回复就是答案
            "answer" | "say" => {
                let id = a["pending_id"].as_u64().ok_or_else(|| LocalError::new("CHAT_STALE"))?;
                let label = if act.a == "say" { said.ok_or_else(|| LocalError::invalid("text", "required"))? } else { option(act.i)? };
                let q = a["question"].as_str().or(a["detail"].as_str()).unwrap_or_default().to_string();
                app.chat.answer(&act.inst, &crate::chat::PermissionAnswer { id, decision: "answer".into(), message: None, answers: Some(json!({ q: label })) })?;
                tr(en, &format!("已回答「{label}」· 在飞书上"), &format!("Answered “{label}” · in Feishu"))
            }
            "dialog" => {
                let label = option(act.i)?;
                let title = a["title"].as_str().unwrap_or_default().to_string();
                app.chat.dialog_choose(&act.inst, &title, &label, act.i.unwrap_or(0)).await?;
                tr(en, &format!("已选「{label}」· 在飞书上"), &format!("Chose “{label}” · in Feishu"))
            }
            _ => return Err(LocalError::invalid("a", "invalid")),
        };
        tracing::info!(inst = %act.inst, action = %act.a, "飞书：在卡片上处理了");
        Ok((result.clone(), Some(card::resolved(&info.info(), &result))))
    }

    /// 回调：卡片上的按钮
    async fn on_card(&self, app: &App, ev: &Value) -> Option<Value> {
        let f = load_feishu(&app.store)?;
        let en = load_rules(&app.store).lang.starts_with("en");
        let operator = ev["operator"]["open_id"].as_str().unwrap_or_default();
        let act: Act = serde_json::from_value(ev["action"]["value"].clone()).ok()?;
        let text = ev["action"]["form_value"]["text"].as_str().map(String::from);
        if act.a == "test" {
            if f.open_id.as_deref() != Some(operator) {
                return Some(toast("error", &tr(en, "不是绑定的飞书账号", "Not the bound Feishu account")));
            }
            if !f.callback_ok {
                let _ = save_feishu(app, Some(&Feishu { callback_ok: true, ..f.clone() }));
            }
            return Some(with_card(toast("success", &tr(en, "回调已接通", "Callbacks work")), card::test(en, &act, true)));
        }
        if f.open_id.as_deref() != Some(operator) {
            tracing::warn!(operator, "飞书：不是绑定的人点了卡片");
            return Some(toast("error", &tr(en, "不是绑定的飞书账号", "Not the bound Feishu account")));
        }
        if !load_rules(&app.store).actions {
            return Some(toast("info", &tr(en, "已设置为只通知，打开看看处理", "Notify-only mode. Open LookLook to respond.")));
        }
        let own = app.account.session().map(|s| s.device_id).unwrap_or_default();
        if act.dev == own {
            // 放到单独的任务里：超过 3 秒也不会被取消，做完后自己更新卡片
            let msg_id = self.card_by_nonce(&act.n).and_then(|s| s.msg);
            let (me, app2, act2) = (app.notify.clone(), app.clone(), act.clone());
            let mut job = tokio::spawn(async move { me.act(&app2, &act2, text.as_deref()).await });
            let failed = |e: LocalError| toast("error", &tr(en, &format!("没处理成：{}", e.code), &format!("Failed: {}", e.code)));
            return Some(match tokio::time::timeout(LOCAL_WAIT, &mut job).await {
                Ok(Ok(Ok((t, Some(c))))) => with_card(toast("success", &t), c),
                Ok(Ok(Ok((t, None)))) => toast("info", &t),
                Ok(Ok(Err(e))) => failed(e),
                Ok(Err(_)) => failed(LocalError::new("INTERNAL")),
                Err(_) => {
                    let api = self.api(&f);
                    tokio::spawn(async move {
                        if let (Ok(Ok((_, Some(c)))), Some(m)) = (job.await, msg_id) {
                            let _ = api.patch_card(&m, &c).await;
                        }
                    });
                    toast("info", &tr(en, "处理中…", "Working on it…"))
                }
            });
        }
        // 会话在别的电脑上：先应答，再转过去
        let devices = app.outbound.devices(&app.account, false).await.ok();
        let dev = devices.as_ref().and_then(|d| d.items.iter().find(|x| x.id == act.dev));
        let name = dev.map(|d| d.name.clone()).unwrap_or_default();
        if !dev.is_some_and(|d| d.online) {
            return Some(toast("warning", &tr(en, &format!("「{name}」不在线"), &format!("“{name}” is offline"))));
        }
        let app = app.clone();
        let operator = operator.to_string();
        tokio::spawn(async move {
            if let Err(e) = forward(&app, &act, text.as_deref(), &operator).await {
                tracing::warn!(error = ?e, dev = %act.dev, "飞书：转给会话所在的电脑失败");
            }
        });
        Some(toast("info", &tr(en, &format!("已发送到「{name}」"), &format!("Sent to “{name}”"))))
    }

    /// 事件：私聊机器人（绑定）
    async fn on_message(&self, app: &App, ev: &Value) {
        let Some(f) = load_feishu(&app.store) else { return };
        let msg = &ev["message"];
        if msg["chat_type"] != "p2p" || msg["message_type"] != "text" {
            return;
        }
        let Some(open_id) = ev["sender"]["sender_id"]["open_id"].as_str() else { return };
        let text = msg["content"].as_str().and_then(|c| serde_json::from_str::<Value>(c).ok()).and_then(|c| c["text"].as_str().map(|t| t.trim().to_string())).unwrap_or_default();
        let en = load_rules(&app.store).lang.starts_with("en");
        let api = self.api(&f);
        let reply = if pair_ok(&f, &text, crate::util::system_ms()) {
            let bound = Feishu { open_id: Some(open_id.into()), bound_at: Some(crate::util::system_ms()), ..f };
            if let Err(e) = save_feishu(app, Some(&bound)) {
                tracing::warn!(error = ?e, "飞书：绑定没存上");
                return;
            }
            self.reset();
            tracing::info!("飞书：绑定了新的账号");
            tr(en, "已绑定。看看的通知会发到这里。", "Bound. LookLook notifications will arrive here.")
        } else if text.len() == 6 && text.chars().all(|c| c.is_ascii_digit()) {
            tr(en, "绑定码不对或已过期，请在看看的设置页里看最新的码。", "Wrong or expired code. Check the latest code in LookLook settings.")
        } else if f.open_id.as_deref() == Some(open_id) {
            tr(en, "要回复 AI，请在通知卡片的输入框里写。", "To reply to the AI, use the input box on a notification card.")
        } else {
            tr(en, "请发送看看设置页里显示的 6 位绑定码。", "Send the 6-digit code shown in LookLook settings.")
        };
        if let Err(e) = api.send_text(open_id, &reply).await {
            self.record_err(&e);
        }
    }

    /// 收到的一条事件或回调
    async fn on_event(&self, app: &App, v: Value) -> Option<Value> {
        let header = &v["header"];
        if let Some(id) = header["event_id"].as_str() {
            let mut g = lock(&self.events);
            g.retain(|_, at| at.elapsed() < Duration::from_secs(15 * 60));
            if g.insert(id.to_string(), Instant::now()).is_some() {
                return None;
            }
        }
        match header["event_type"].as_str() {
            Some("card.action.trigger") => self.on_card(app, &v["event"]).await,
            Some("im.message.receive_v1") => {
                // 回复要发消息，不挡着这条事件的应答
                let me = app.notify.clone();
                let app = app.clone();
                let ev = v["event"].clone();
                tokio::spawn(async move { me.on_message(&app, &ev).await });
                None
            }
            _ => None,
        }
    }

    /// 设置页“发一条测试”
    pub async fn test(&self, app: &App) -> Result<(), LocalError> {
        let f = load_feishu(&app.store).ok_or_else(|| LocalError::new("NOTIFY_NOT_CONFIGURED"))?;
        let open_id = f.open_id.clone().ok_or_else(|| LocalError::new("NOTIFY_NOT_BOUND"))?;
        let en = load_rules(&app.store).lang.starts_with("en");
        let act = Act { a: "test".into(), n: nonce(), ..Default::default() };
        self.api(&f).send_card(&open_id, &card::test(en, &act, false)).await.map_err(|e| feishu_error(&e))?;
        Ok(())
    }
}

/// 飞书接口的错误给界面：`NOTIFY_FEISHU`，带飞书的错误码
pub fn feishu_error(e: &anyhow::Error) -> LocalError {
    LocalError::new("NOTIFY_FEISHU").with("code", feishu::code_of(e).unwrap_or(-1)).with("msg", e.to_string())
}

/// 稳定地址下的页面（手机上点“打开”）
fn page_url(app: &App, path: &str) -> String {
    match app.account.session().map(|s| s.user_host_url).filter(|u| !u.is_empty()) {
        Some(u) => format!("{}/{path}", u.trim_end_matches('/')),
        None => format!("http://{}/{path}", app.ui_addr),
    }
}

struct Callbacks {
    app: App,
}

impl ws::Handler for Callbacks {
    fn handle(&self, payload: Value) -> impl std::future::Future<Output = Option<Value>> + Send {
        let app = self.app.clone();
        async move { app.notify.on_event(&app, payload).await }
    }
}

// ---------------- 电脑之间转发卡片操作 ----------------

#[derive(Serialize, Deserialize)]
pub struct ActionReq {
    pub v: u8,
    pub from: String,
    pub to: String,
    pub ts: i64,
    pub key: String,
    pub sig: String,
    /// [`ActionPayload`] 的 JSON：签名签的是它的摘要
    pub payload: String,
}

#[derive(Serialize, Deserialize)]
struct ActionPayload {
    act: Act,
    text: Option<String>,
    operator: String,
}

fn message(from: &str, to: &str, ts: i64, payload: &str) -> String {
    format!("looklook-notify-v1\n{from}\n{to}\n{ts}\n{}", b64(&Sha256::digest(payload.as_bytes())))
}

async fn forward(app: &App, act: &Act, text: Option<&str>, operator: &str) -> Result<(), LocalError> {
    let own = app.account.session().ok_or_else(|| LocalError::new("NOT_LOGGED_IN"))?.device_id;
    let link = app.outbound.link(&app.account, &act.dev).await.map_err(|e| e.local())?;
    let payload = serde_json::to_string(&ActionPayload { act: act.clone(), text: text.map(String::from), operator: operator.into() }).map_err(anyhow::Error::from)?;
    let ts = app.account.clock.now_ms();
    let (key, sig) = app.account.sign_device(&message(&own, &act.dev, ts, &payload)).ok_or_else(|| LocalError::new("NOT_LOGGED_IN"))?;
    let req = ActionReq { v: 1, from: own, to: act.dev.clone(), ts, key, sig, payload };
    crate::rtc::outbound::post_json::<Value>(&link, ACTION_PATH, &req, 64 * 1024, "NOTIFY_PROTOCOL").await?;
    Ok(())
}

/// 另一台电脑转来的卡片操作（`POST /api/notify/action`，只接受 `Access::Rtc`）：校验签名和操作人，处理后自己更新卡片。
pub async fn receive(app: &App, req: ActionReq) -> Result<Value, LocalError> {
    let own = app.account.session().ok_or_else(|| LocalError::new("NOT_LOGGED_IN"))?.device_id;
    if req.v != 1 || req.to != own || (req.ts - app.account.clock.now_ms()).abs() > MAX_SKEW_MS {
        return Err(LocalError::new("NOTIFY_SIGNATURE"));
    }
    let msg = message(&req.from, &req.to, req.ts, &req.payload);
    let mut check = app.outbound.verify_peer(&app.account, &req.from, &req.key, &msg, &req.sig, false).await?;
    if matches!(check, PeerCheck::Bad) {
        check = app.outbound.verify_peer(&app.account, &req.from, &req.key, &msg, &req.sig, true).await?;
    }
    if !matches!(check, PeerCheck::Ok(_)) {
        tracing::warn!(dev = %req.from, "飞书：转来的操作签名不对");
        return Err(LocalError::new("NOTIFY_SIGNATURE"));
    }
    let p: ActionPayload = serde_json::from_str(&req.payload).map_err(|_| LocalError::new("NOTIFY_PROTOCOL"))?;
    let f = load_feishu(&app.store).ok_or_else(|| LocalError::new("NOTIFY_NOT_CONFIGURED"))?;
    if f.open_id.as_deref() != Some(p.operator.as_str()) || p.act.dev != own || !load_rules(&app.store).actions {
        return Err(LocalError::new("FORBIDDEN"));
    }
    let msg_id = app.notify.card_by_nonce(&p.act.n).and_then(|s| s.msg);
    let (toast, card) = app.notify.act(app, &p.act, p.text.as_deref()).await?;
    if let (Some(msg_id), Some(card)) = (msg_id, card) {
        if let Err(e) = app.notify.api(&f).patch_card(&msg_id, &card).await {
            tracing::debug!(error = %e, "飞书：转来的操作处理了，卡片没改上");
        }
    }
    Ok(json!({ "toast": toast }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app_cfg() -> Feishu {
        Feishu { app_id: "cli_a".into(), app_secret: "s3cret".into(), ..Default::default() }
    }

    #[test]
    fn pair_code_windows() {
        let f = app_cfg();
        let t = 1_800_000_000_000;
        let code = pair_code(&f, t);
        assert_eq!(code.len(), 6);
        assert!(code.chars().all(|c| c.is_ascii_digit()));
        assert!(pair_ok(&f, &code, t));
        // 下一个时间窗还认上一个窗的码，再往后就不认了
        assert!(pair_ok(&f, &code, t + PAIR_WINDOW_MS));
        assert!(!pair_ok(&f, &code, t + 2 * PAIR_WINDOW_MS));
        // 换了 Secret 码就不一样
        assert_ne!(pair_code(&Feishu { app_secret: "other".into(), ..app_cfg() }, t), code);
        assert!(!pair_ok(&f, "12345", t));
        assert!(pair_expires(t) > t && pair_expires(t) - t <= PAIR_WINDOW_MS);
    }

    #[test]
    fn attn_keys() {
        assert_eq!(attn_key(&json!({ "board": "working" })), None);
        assert_eq!(attn_key(&json!({ "board": "attn", "attn": { "kind": "permission", "pending_id": 7 } })).as_deref(), Some("p7"));
        assert_eq!(attn_key(&json!({ "board": "attn", "attn": { "kind": "limit", "title": "额度" } })).as_deref(), Some("limit:额度"));
        assert_eq!(attn_key(&json!({ "board": "attn", "attn": { "kind": "dialog" } })).as_deref(), Some("dialog:"));
    }

    fn ended(outcome: &str, started: i64, ended: i64, summary: Option<&str>) -> Value {
        json!({ "board": "ended", "seen_at": 0, "turn": { "started_at": started, "ended_at": ended, "outcome": outcome, "summary": summary } })
    }

    #[test]
    fn end_rules() {
        let r = Rules::default();
        let now = 1_000_000;
        // 跑了 60 秒做完：发
        assert_eq!(end_notice(&ended("done", now - 70_000, now - 10_000, Some("好了")), 0, false, &r, false, now), EndNotice::Send { failed: false });
        // 发过了就不再发
        assert_eq!(end_notice(&ended("done", now - 70_000, now - 10_000, Some("好了")), now - 10_000, false, &r, false, now), EndNotice::Nothing);
        // 不到 30 秒、刚启动、静音、中断、太久以前：只记下
        assert_eq!(end_notice(&ended("done", now - 20_000, now - 10_000, Some("好了")), 0, false, &r, false, now), EndNotice::Record);
        assert_eq!(end_notice(&ended("done", now - 70_000, now - 10_000, Some("好了")), 0, true, &r, false, now), EndNotice::Record);
        assert_eq!(end_notice(&ended("done", now - 70_000, now - 10_000, Some("好了")), 0, false, &r, true, now), EndNotice::Record);
        assert_eq!(end_notice(&ended("interrupted", now - 70_000, now - 10_000, None), 0, false, &r, false, now), EndNotice::Record);
        assert_eq!(end_notice(&ended("failed", now - 900_000, now - 300_000, None), 0, false, &r, false, now), EndNotice::Record);
        // 出错不看用时；关掉“出错了”就不发
        assert_eq!(end_notice(&ended("failed", now - 2_000, now - 1_000, None), 0, false, &r, false, now), EndNotice::Send { failed: true });
        let off = Rules { failed: false, ..Rules::default() };
        assert_eq!(end_notice(&ended("failed", now - 2_000, now - 1_000, None), 0, false, &off, false, now), EndNotice::Record);
        // 摘要还没有：等几秒；过了 8 秒照样发
        assert_eq!(end_notice(&ended("done", now - 70_000, now - 2_000, None), 0, false, &r, false, now), EndNotice::Wait);
        assert_eq!(end_notice(&ended("done", now - 70_000, now - 9_000, None), 0, false, &r, false, now), EndNotice::Send { failed: false });
        // 有人看过（或正开着）
        let mut seen = ended("done", now - 70_000, now - 10_000, Some("好了"));
        seen["seen_at"] = json!(now - 5_000);
        assert_eq!(end_notice(&seen, 0, false, &r, false, now), EndNotice::Record);
        seen["seen_at"] = json!(0);
        seen["watched"] = json!(true);
        assert_eq!(end_notice(&seen, 0, false, &r, false, now), EndNotice::Record);
    }

    #[test]
    fn attn_rules() {
        let now = 1_000_000;
        let b = |since: i64, watched: bool| json!({ "board": "attn", "since": since, "watched": watched, "attn": { "kind": "permission", "pending_id": 1 } });
        assert!(!attn_due(&b(now - 5_000, false), now, false));
        assert!(attn_due(&b(now - 10_000, false), now, false));
        assert!(!attn_due(&b(now - 60_000, true), now, false));
        assert!(!attn_due(&b(now - 60_000, false), now, true));
        assert_eq!(close_text(&b(now, false), false), "已被新的请求替换");
        assert_eq!(close_text(&json!({ "board": "stopped" }), false), "会话已结束");
        assert_eq!(close_text(&json!({ "board": "idle", "phase": "tui" }), true), "Moved to the terminal");
        assert_eq!(close_text(&json!({ "board": "working", "phase": "running" }), false), "已在看看或终端里处理");
    }

    #[test]
    fn per_minute_budget() {
        let n = Notifier::default();
        for _ in 0..PER_MINUTE {
            assert!(n.budget());
            lock(&n.sent).push_back(Instant::now());
        }
        assert!(!n.budget());
        if let Some(t) = lock(&n.sent).front_mut() {
            *t = Instant::now() - Duration::from_secs(61);
        }
        assert!(n.budget());
    }

    #[test]
    fn rules_default_and_partial() {
        let r: Rules = serde_json::from_value(json!({ "done": false })).unwrap();
        assert!(!r.done && r.failed && r.actions && r.content);
        assert_eq!(r.lang, "zh-CN");
    }

    #[test]
    fn signed_message_binds_payload() {
        assert_ne!(message("a", "b", 1, "{}"), message("a", "b", 1, "{ }"));
        assert!(message("a", "b", 1, "{}").starts_with("looklook-notify-v1\na\nb\n1\n"));
    }
}
