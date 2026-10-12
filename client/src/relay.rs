//! 远程访问通道：内置 rathole 客户端（详细设计 §4.6.5）。
//!
//! 登录后按平台下发的通道参数生成 rathole 客户端配置（仅当前用户可读），`local_addr`
//! 指向本机网关的“远程入口”（只监听 127.0.0.1，且每个请求都必须带平台签名的网关令牌）。
//! 参数变化（重新登录、令牌轮换、平台对账）时重启通道；退出登录时关闭。
//!
//! 断线重连：rathole 自己会重试（最多每 3 秒一次），但旧连接在睡眠唤醒、换网络后往往已经死了，
//! 要等心跳超时（35 秒）才发现。这里每 5 秒看一眼：电脑刚睡醒、或者本机原有的 IPv4 地址不见了（换了 Wi-Fi、
//! 拔了网线），就立刻重建通道；只是多了一个地址（接上 VPN 等）不动，免得打断正在用的终端。
//! 管理台也可以手动“立即重连”（`POST /api/relay/reconnect`）。
//!
//! 数据通道：浏览器的每个请求都要 rathole 新建一条数据通道，中转那边的连接池按顺序配对。控制通道连着、
//! 数据通道却建不起来时（例如开着 fake-ip 的代理，见 third_party/rathole/README.looklook.md），请求会在中转排队
//! 到超时，而“已连接”和 presence 的旧连接都还在。两道检查：
//! - 本次通道里有 [`DATA_LOST_LIMIT`] 条数据通道连不上中转（每条都让中转的连接池永久少一条），就重建通道；
//! - 看门狗：中转每个周期都会重新请求 presence（要新建一条数据通道），超过 [`stall_limit`] 没收到就重建通道。

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use looklook_protocol::dto::RelayParams;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use crate::account::Account;
use crate::platform::{localhost_addr, resolve_override};
use crate::util::write_private;

/// 远程访问通道的连接状态，给管理台显示：`state` 为 off（未登录）/ connecting / connected / failed；
/// 失败时 `reason` 是给界面翻译用的原因代码，`detail` 是原始错误（排查用）。
#[derive(Clone, Debug, serde::Serialize)]
pub struct RelayStatus {
    pub state: &'static str,
    pub reason: Option<&'static str>,
    pub detail: Option<String>,
    /// 进入当前状态的时间（Unix 毫秒）。
    pub since: u64,
    /// 本次启动以来连续失败的次数（连上后清零）。
    pub failures: u32,
}

static STATUS: Mutex<RelayStatus> = Mutex::new(RelayStatus { state: "off", reason: None, detail: None, since: 0, failures: 0 });

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

pub fn status() -> RelayStatus {
    STATUS.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// 通道刚连上（控制通道建立）时通知：客户端马上向平台发一次心跳，平台立即把这台电脑标为在线。
static CONNECTED: tokio::sync::Notify = tokio::sync::Notify::const_new();

pub fn connected() -> &'static tokio::sync::Notify {
    &CONNECTED
}

/// 要求马上重建通道（手动“立即重连”）。
static RESTART: tokio::sync::Notify = tokio::sync::Notify::const_new();

pub fn request_restart() {
    RESTART.notify_one();
}

/// 远程入口的本机地址（rathole 的 `local_addr`）：数据通道连本机失败不算“连不上中转”。
static LOCAL_ADDR: Mutex<String> = Mutex::new(String::new());

/// 本次通道里连不上中转的数据通道数。
static DATA_LOST: AtomicU32 = AtomicU32::new(0);
/// 连不上中转的数据通道达到这么多条就重建通道（中转的备用连接一共 8 条）。
const DATA_LOST_LIMIT: u32 = 3;
/// 因数据通道问题自动重建，两次之间至少隔这么久。
const AUTO_RESTART_GAP: Duration = Duration::from_secs(60);

/// 最近一次收到带周期的 presence 请求：(时间, 周期)。旧中转不带周期，为 None，看门狗不工作。
static PRESENCE: Mutex<Option<(Instant, Duration)>> = Mutex::new(None);

/// 中转请求了 presence（见 `gateway::presence`）。`cycle`：中转重新请求的周期（秒）。
pub fn presence_seen(cycle: Option<u64>) {
    let v = cycle.filter(|&c| c > 0).map(|c| (Instant::now(), Duration::from_secs(c)));
    *PRESENCE.lock().unwrap_or_else(|e| e.into_inner()) = v;
}

/// 看门狗的期限：两个周期再加 30 秒（中转打开 presence 最多等 8 秒，失败后每秒重试）。
fn stall_limit(cycle: Duration) -> Duration {
    cycle * 2 + Duration::from_secs(30)
}

/// 通道已经启动了 `started`，最近一次 presence 是 `last`：是不是太久没收到了。
fn stalled(last: Option<(Instant, Duration)>, started: Instant, now: Instant) -> bool {
    let Some((at, cycle)) = last else { return false };
    now.saturating_duration_since(at.max(started)) > stall_limit(cycle)
}

/// rathole 的日志是不是“一条数据通道连不上中转”：连接（含加密握手）的重试都失败了。
/// 连本机远程入口失败（`local`）不算，那时数据通道已经被中转用掉了，不会让连接池少一条。
fn lost_data_channel(msg: &str, local: &str) -> bool {
    msg.contains("Failed to run the data channel") && msg.contains("Failed to connect to") && (local.is_empty() || !msg.contains(local))
}

/// 本机的 IPv4 地址（不含回环）。IPv6 临时地址会定期轮换，不看。
fn local_ipv4() -> Vec<std::net::Ipv4Addr> {
    let mut v: Vec<std::net::Ipv4Addr> = local_ip_address::list_afinet_netifas()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(_, ip)| match ip {
            std::net::IpAddr::V4(v4) if !v4.is_loopback() && !v4.is_link_local() => Some(v4),
            _ => None,
        })
        .collect();
    v.sort();
    v.dedup();
    v
}

/// 网络变化的判断：原来的地址有一个不见了（连接可能就走的它）才算；只是多了地址不算。
fn lost_address(before: &[std::net::Ipv4Addr], now: &[std::net::Ipv4Addr]) -> bool {
    before.iter().any(|ip| !now.contains(ip))
}

/// 两次检查之间，墙上时间比单调时钟多走了这么多：电脑睡过一觉（单调时钟在睡眠时不走）；
/// 或者单调时钟本身就隔了很久（进程被挂起、Windows 上单调时钟也计入睡眠时）。
const WAKE_GAP: Duration = Duration::from_secs(15);

fn woke_up(mono: Duration, wall: Option<Duration>, expected: Duration) -> bool {
    mono > expected + WAKE_GAP || wall.is_some_and(|w| w > mono + WAKE_GAP)
}

fn set_status(state: &'static str, reason: Option<&'static str>, detail: Option<String>) {
    let mut s = STATUS.lock().unwrap_or_else(|e| e.into_inner());
    if state == "connected" && s.state != "connected" {
        CONNECTED.notify_one();
    }
    // 同一个失败原因反复重试时只累加次数，不刷新“开始时间”。
    let same = s.state == state && s.reason == reason;
    s.failures = match state {
        "failed" => s.failures + 1,
        "connected" | "off" => 0,
        _ => s.failures,
    };
    if !same {
        s.since = now_ms();
    }
    s.state = state;
    s.reason = reason;
    s.detail = detail;
}

/// 把 rathole 的错误文本归类成界面能翻译的原因代码。
pub fn classify(msg: &str) -> &'static str {
    let m = msg.to_ascii_lowercase();
    if m.contains("authentication failed") || m.contains("auth") && m.contains("token") {
        "auth"
    } else if m.contains("heartbeat") {
        "heartbeat"
    } else if m.contains("lookup") || m.contains("resolve") || m.contains("name or service") || m.contains("no such host") || m.contains("dns") {
        "dns"
    } else if m.contains("refused") {
        "refused"
    } else if m.contains("timed out") || m.contains("timeout") || m.contains("deadline") {
        "timeout"
    } else if m.contains("unreachable") || m.contains("no route") {
        "network"
    } else if m.contains("noise") || m.contains("handshake") || m.contains("decrypt") {
        "handshake"
    } else {
        "other"
    }
}

/// 订阅 rathole 自己的日志事件（“Control channel established”、各种重试告警）来推断连接状态。
/// rathole 没有对外的状态接口，这是最直接的办法；只关心 `rathole` 开头的 target。
pub struct StatusLayer;

#[derive(Default)]
struct MessageVisitor(String);

impl tracing::field::Visit for MessageVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.0 = format!("{value:?}");
        }
    }
}

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for StatusLayer {
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: tracing_subscriber::layer::Context<'_, S>) {
        let meta = event.metadata();
        if !meta.target().starts_with("rathole") {
            return;
        }
        let mut v = MessageVisitor::default();
        event.record(&mut v);
        let msg = v.0;
        if status().state == "off" {
            return;
        }
        if *meta.level() <= tracing::Level::WARN && lost_data_channel(&msg, &LOCAL_ADDR.lock().unwrap_or_else(|e| e.into_inner())) {
            DATA_LOST.fetch_add(1, Ordering::Relaxed);
        }
        if msg.contains("Control channel established") {
            set_status("connected", None, None);
        } else if *meta.level() <= tracing::Level::WARN && msg.contains("Failed to run the control channel") {
            // 只认控制通道的失败。数据通道（每个远程请求一条）的失败也会打 WARN 的
            // “Failed to …”/“Retry in …”，比如本地连接被浏览器提前关闭；那时控制通道仍然在线，
            // 而且不会再打“Control channel established”，状态会一直卡在失败。
            let detail: String = msg.chars().take(400).collect();
            set_status("failed", Some(classify(&msg)), Some(detail));
        }
    }
}

fn toml_str(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into())
}

/// 开发环境的 `relay.xxx.localhost` 直接连本机。
fn remote_addr(server_addr: &str) -> String {
    if let Some(addr) = std::env::var("LOOKLOOK_RELAY_ADDR").ok().filter(|s| !s.is_empty()) {
        return addr;
    }
    match server_addr.rsplit_once(':') {
        Some((host, port)) => match localhost_addr(host).or_else(|| resolve_override(host)) {
            Some(ip) => format!("{ip}:{port}"),
            None => server_addr.to_string(),
        },
        None => server_addr.to_string(),
    }
}

/// 服务端每 10 秒发一次心跳（looklook-server relay/config_writer.rs）；35 秒收不到就重连，
/// 同时仍大于旧中继的 30 秒间隔，先升级客户端也不会反复断开。
pub fn render(p: &RelayParams, local: SocketAddr) -> String {
    format!(
        "[client]\nremote_addr = {}\nheartbeat_timeout = 35\nretry_interval = 3\n\n\
         [client.transport]\ntype = \"noise\"\n\n\
         [client.transport.noise]\nremote_public_key = {}\n\n\
         [client.services.{}]\ntoken = {}\nlocal_addr = {}\n",
        toml_str(&remote_addr(&p.server_addr)),
        toml_str(&p.noise_remote_public_key),
        service_key(&p.service_name),
        toml_str(&p.token),
        toml_str(&local.to_string()),
    )
}

fn service_key(name: &str) -> String {
    let clean: String = name.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-').collect();
    if clean.is_empty() {
        "looklook".into()
    } else {
        clean
    }
}

struct Running {
    fingerprint: String,
    started: Instant,
    shutdown: broadcast::Sender<bool>,
    task: tokio::task::JoinHandle<()>,
}

impl Running {
    async fn stop(self) {
        let _ = self.shutdown.send(true);
        let mut task = self.task;
        if tokio::time::timeout(Duration::from_secs(5), &mut task).await.is_err() {
            task.abort();
        }
    }
}

/// 后台任务：让通道状态跟随登录状态；`shutdown` 取消时关闭通道并返回。
/// 必须跟着本次 `run()` 结束：托盘“停止→启动”在同一进程里重来，旧通道若还在，
/// 两个 rathole 客户端会用同一令牌互相挤掉对方的控制通道（数据通道报 early eof）。
pub async fn run(account: Arc<Account>, local: SocketAddr, config_path: PathBuf, shutdown: CancellationToken) {
    let mut changed = account.subscribe();
    let mut current: Option<Running> = None;
    let mut addrs = local_ipv4();
    let mut last_auto: Option<Instant> = None;
    *LOCAL_ADDR.lock().unwrap_or_else(|e| e.into_inner()) = local.to_string();
    loop {
        let want = account.session().map(|s| s.relay);
        let fingerprint = want.as_ref().map(|p| serde_json::to_string(p).unwrap_or_default());
        let finished = current.as_ref().is_some_and(|r| r.task.is_finished());
        if finished || current.as_ref().map(|r| &r.fingerprint) != fingerprint.as_ref() {
            if let Some(r) = current.take() {
                if r.fingerprint.as_str() != fingerprint.as_deref().unwrap_or_default() {
                    // 换了中转或参数：新中转是否定期请求 presence，等它请求了再说
                    presence_seen(None);
                }
                r.stop().await;
                tracing::info!("远程访问通道已关闭");
            }
            if want.is_none() {
                set_status("off", None, None);
            }
            if finished {
                // rathole 因配置或其他错误退出：稍后重试，避免空转。
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(5)) => {}
                    _ = shutdown.cancelled() => break,
                }
            }
            if let (Some(p), Some(fp)) = (want, fingerprint) {
                set_status("connecting", None, None);
                DATA_LOST.store(0, Ordering::Relaxed);
                match start(&p, local, &config_path) {
                    Ok((shutdown, task)) => {
                        tracing::info!(server = %remote_addr(&p.server_addr), "远程访问通道已启动");
                        current = Some(Running { fingerprint: fp, started: Instant::now(), shutdown, task });
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "写入远程访问配置失败");
                        set_status("failed", Some("config"), Some(e.to_string()));
                    }
                }
            } else {
                let _ = std::fs::remove_file(&config_path);
            }
        }
        let tick = Duration::from_secs(5);
        let (mono0, wall0) = (std::time::Instant::now(), SystemTime::now());
        let restart = tokio::select! {
            r = changed.changed() => { if r.is_err() { break } None }
            _ = tokio::time::sleep(tick) => {
                let now = local_ipv4();
                let lost = lost_address(&addrs, &now);
                addrs = now;
                if woke_up(mono0.elapsed(), SystemTime::now().duration_since(wall0).ok(), tick) {
                    Some("wake")
                } else if lost {
                    Some("network")
                } else {
                    data_channel_trouble(current.as_ref(), &mut last_auto)
                }
            }
            _ = RESTART.notified() => Some("manual"),
            _ = shutdown.cancelled() => break,
        };
        // 只有真要重建时才取走 current：元组里的 `current.take()` 不管 restart 是否为 None 都会执行，
        // 每 5 秒的检查就会把正常的通道丢掉、下一轮重新启动，接着每次都触发一次立即心跳（被平台 429）。
        if let Some(why) = restart {
            if let Some(r) = current.take() {
                tracing::info!(reason = why, "重建远程访问通道");
                r.stop().await;
            }
        }
    }
    if let Some(r) = current.take() {
        r.stop().await;
    }
    set_status("off", None, None);
}

/// 数据通道出了问题、该重建通道了：返回原因（`data_channel` 连不上中转、`stalled` 收不到 presence）。
/// 两次之间至少隔 [`AUTO_RESTART_GAP`]，网络一直不通时不至于每 5 秒重建一次。
fn data_channel_trouble(current: Option<&Running>, last_auto: &mut Option<Instant>) -> Option<&'static str> {
    let r = current?;
    if status().state != "connected" || last_auto.is_some_and(|t| t.elapsed() < AUTO_RESTART_GAP) {
        return None;
    }
    let lost = DATA_LOST.load(Ordering::Relaxed);
    let last = *PRESENCE.lock().unwrap_or_else(|e| e.into_inner());
    let why = if lost >= DATA_LOST_LIMIT {
        tracing::warn!(lost, "数据通道连不上中转，重建远程访问通道");
        "data_channel"
    } else if stalled(last, r.started, Instant::now()) {
        tracing::warn!(since = ?last.map(|(at, _)| at.elapsed()), "很久没收到中转的 presence 请求（数据通道可能建不起来），重建远程访问通道");
        "stalled"
    } else {
        return None;
    };
    *last_auto = Some(Instant::now());
    Some(why)
}

fn start(p: &RelayParams, local: SocketAddr, path: &std::path::Path) -> anyhow::Result<(broadcast::Sender<bool>, tokio::task::JoinHandle<()>)> {
    write_private(path, render(p, local).as_bytes())?;
    let (tx, rx) = broadcast::channel(1);
    let cli = rathole::Cli { config_path: Some(path.to_path_buf()), server: false, client: true, genkey: None };
    let task = tokio::spawn(async move {
        if let Err(e) = rathole::run(cli, rx).await {
            tracing::warn!(error = %e, "远程访问通道退出");
            let msg = format!("{e:#}");
            set_status("failed", Some(classify(&msg)), Some(msg.chars().take(400).collect()));
        }
    });
    Ok((tx, task))
}

/// 测一条线路的延迟（毫秒）：向中转直连入口（443）的 `/_ll/ping` 发请求。第一次请求建立连接
/// （DNS、TCP、TLS），不计入；之后在同一连接上再发 `PING_SAMPLES` 次，取最小值，约等于一次往返。
/// 不走系统代理：远程通道本身也是直连中转。
pub async fn ping(url: &str) -> Result<u32, String> {
    const PING_SAMPLES: usize = 3;
    let mut b = reqwest::Client::builder();
    // 与通道相同的开发用解析覆盖（`*.localhost`、`LOOKLOOK_RESOLVE`）
    if let Some(u) = reqwest::Url::parse(url).ok().filter(|u| u.host_str().is_some()) {
        let host = u.host_str().unwrap_or_default();
        if let Some(ip) = localhost_addr(host).or_else(|| resolve_override(host)) {
            b = b.resolve(host, SocketAddr::new(ip, u.port_or_known_default().unwrap_or(443)));
        }
    }
    let http = b
        .no_proxy()
        .timeout(Duration::from_secs(3))
        .pool_max_idle_per_host(1)
        .user_agent(concat!("looklook-client/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| e.to_string())?;
    let once = || async {
        let t = std::time::Instant::now();
        let r = http.get(url).send().await.map_err(|e| ping_error(&e))?;
        if !r.status().is_success() {
            return Err(format!("HTTP {}", r.status().as_u16()));
        }
        Ok(t.elapsed())
    };
    once().await?;
    let mut best = Duration::MAX;
    for _ in 0..PING_SAMPLES {
        best = best.min(once().await?);
    }
    Ok(best.as_millis().clamp(1, u32::MAX as u128) as u32)
}

fn ping_error(e: &reqwest::Error) -> String {
    tracing::debug!(error = ?e, "线路测速失败");
    if e.is_timeout() {
        "timeout".into()
    } else if e.is_connect() {
        "connect".into()
    } else {
        "network".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_wake_and_lost_addresses() {
        let s = Duration::from_secs;
        assert!(!woke_up(s(5), Some(s(5)), s(5)));
        assert!(!woke_up(s(6), Some(s(19)), s(5)), "时钟校准等小偏差不算");
        assert!(woke_up(s(5), Some(s(600)), s(5)), "睡了 10 分钟：墙上时间多走了");
        assert!(woke_up(s(600), Some(s(600)), s(5)), "单调时钟也计入睡眠的系统");
        assert!(!woke_up(s(5), None, s(5)), "墙上时间往回调了");
        let a: std::net::Ipv4Addr = "192.168.1.5".parse().unwrap();
        let b: std::net::Ipv4Addr = "10.0.0.8".parse().unwrap();
        assert!(!lost_address(&[a], &[a, b]), "多了一个地址（接上 VPN）不重连");
        assert!(lost_address(&[a, b], &[b]), "原来的地址没了");
        assert!(lost_address(&[a], &[]));
        assert!(!lost_address(&[], &[a]));
    }

    #[test]
    fn classifies_errors() {
        assert_eq!(classify("Failed to connect to x: Connection refused (os error 111)"), "refused");
        assert_eq!(classify("failed to lookup address information: Name or service not known"), "dns");
        assert_eq!(classify("Authentication failed: u1: Service not exist"), "auth");
        assert_eq!(classify("Heartbeat timed out"), "heartbeat");
        assert_eq!(classify("connection timed out"), "timeout");
        assert_eq!(classify("something odd"), "other");
    }

    #[test]
    fn counts_only_data_channels_that_cannot_reach_the_relay() {
        let local = "127.0.0.1:41999";
        assert!(lost_data_channel("Failed to run the data channel: Failed to connect to 198.18.0.7:2333: timed out after 5s", local));
        assert!(lost_data_channel(
            "Failed to run the data channel: Failed to connect to 203.0.113.7:2333: Failed to do noise handshake: early eof",
            local
        ));
        assert!(!lost_data_channel("Failed to run the data channel: Failed to connect to 127.0.0.1:41999: Connection refused", local), "连本机失败不算");
        assert!(!lost_data_channel("Failed to connect to 198.18.0.7:2333: timed out after 5s. Retry in 100ms", local), "还在重试");
        assert!(!lost_data_channel("Failed to run the data channel: early eof", local), "备用通道被中转关掉");
    }

    #[test]
    fn watchdog_waits_for_a_presence_cycle_aware_relay() {
        let t0 = Instant::now();
        let s = Duration::from_secs;
        let cycle = Some((t0, s(60)));
        assert!(!stalled(None, t0, t0 + s(3600)), "旧中转不定期请求 presence，不检查");
        assert!(!stalled(cycle, t0, t0 + s(150)));
        assert!(stalled(cycle, t0, t0 + s(151)));
        assert!(!stalled(cycle, t0 + s(100), t0 + s(200)), "通道刚重建，从重建时算起");
        assert!(!stalled(Some((t0 + s(120), s(60))), t0, t0 + s(200)), "收到了新的 presence");
    }

    #[test]
    fn renders_client_config() {
        let p = RelayParams {
            server_addr: "relay.looklook.localhost:2333".into(),
            transport: "noise".into(),
            noise_remote_public_key: "abc=".into(),
            service_name: "u1024".into(),
            token: "t\"k".into(),
            name: "r1".into(),
            gateway_mac_key: String::new(),
        };
        let t = render(&p, "127.0.0.1:41999".parse().unwrap());
        assert!(t.contains("remote_addr = \"127.0.0.1:2333\""));
        assert!(t.contains("[client.services.u1024]"));
        assert!(t.contains("token = \"t\\\"k\""));
        assert!(t.contains("local_addr = \"127.0.0.1:41999\""));
    }

    #[tokio::test]
    async fn ping_measures_round_trips_on_one_connection() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let hits = Arc::new(AtomicUsize::new(0));
        let conns = Arc::new(AtomicUsize::new(0));
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        let (h, c) = (hits.clone(), conns.clone());
        tokio::spawn(async move {
            loop {
                let (s, _) = l.accept().await.unwrap();
                c.fetch_add(1, Ordering::SeqCst);
                let h = h.clone();
                tokio::spawn(async move {
                    let svc = hyper::service::service_fn(move |req: hyper::Request<hyper::body::Incoming>| {
                        h.fetch_add(1, Ordering::SeqCst);
                        let status = if req.uri().path() == "/_ll/ping" { 204 } else { 404 };
                        async move { Ok::<_, std::convert::Infallible>(hyper::Response::builder().status(status).body(String::new()).unwrap()) }
                    });
                    let _ = hyper::server::conn::http1::Builder::new().serve_connection(hyper_util::rt::TokioIo::new(s), svc).await;
                });
            }
        });
        let ms = ping(&format!("http://{addr}/_ll/ping")).await.unwrap();
        assert!((1..1000).contains(&ms), "{ms}");
        assert_eq!(hits.load(Ordering::SeqCst), 4);
        assert_eq!(conns.load(Ordering::SeqCst), 1, "samples reuse the warm-up connection");
        assert_eq!(ping(&format!("http://{addr}/nope")).await, Err("HTTP 404".into()));
        let closed = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap().local_addr().unwrap();
        assert_eq!(ping(&format!("http://{closed}/_ll/ping")).await, Err("connect".into()));
    }
}
