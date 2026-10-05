//! 远程访问通道：内置 rathole 客户端（详细设计 §4.6.5）。
//!
//! 登录后按平台下发的通道参数生成 rathole 客户端配置（仅当前用户可读），`local_addr`
//! 指向本机网关的“远程入口”（只监听 127.0.0.1，且每个请求都必须带平台签名的网关令牌）。
//! 参数变化（重新登录、令牌轮换、平台对账）时重启通道；退出登录时关闭。

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

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
    loop {
        let want = account.session().map(|s| s.relay);
        let fingerprint = want.as_ref().map(|p| serde_json::to_string(p).unwrap_or_default());
        let finished = current.as_ref().is_some_and(|r| r.task.is_finished());
        if finished || current.as_ref().map(|r| &r.fingerprint) != fingerprint.as_ref() {
            if let Some(r) = current.take() {
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
                match start(&p, local, &config_path) {
                    Ok((shutdown, task)) => {
                        tracing::info!(server = %remote_addr(&p.server_addr), "远程访问通道已启动");
                        current = Some(Running { fingerprint: fp, shutdown, task });
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
        tokio::select! {
            r = changed.changed() => if r.is_err() { break },
            _ = tokio::time::sleep(Duration::from_secs(5)) => {}
            _ = shutdown.cancelled() => break,
        }
    }
    if let Some(r) = current.take() {
        r.stop().await;
    }
    set_status("off", None, None);
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
    fn classifies_errors() {
        assert_eq!(classify("Failed to connect to x: Connection refused (os error 111)"), "refused");
        assert_eq!(classify("failed to lookup address information: Name or service not known"), "dns");
        assert_eq!(classify("Authentication failed: u1: Service not exist"), "auth");
        assert_eq!(classify("Heartbeat timed out"), "heartbeat");
        assert_eq!(classify("connection timed out"), "timeout");
        assert_eq!(classify("something odd"), "other");
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
