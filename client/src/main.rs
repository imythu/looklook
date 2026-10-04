//! 看看客户端（Looklook client）。
//!
//! ```text
//! looklook                 启动（默认），并在桌面系统上打开管理台
//! looklook open            在浏览器中打开管理台
//! looklook url             打印管理台地址（本机与局域网）
//! looklook access          查看或修改访问控制（局域网访问、白名单、访问码）
//! looklook login           授权登录（命令行显示授权码、网址与二维码，可在手机上批准）
//! looklook logout          退出登录
//! looklook status          查看状态
//! looklook update          检查并安装新版本（别名 upgrade）
//! ```
//!
//! 命令行的帮助与输出是中英双语的：没有图形界面的服务器上没有界面语言可选。
//!
//! Windows / macOS 上直接启动（双击图标、开始菜单、程序坞）是桌面应用：应用窗口显示管理台，
//! 系统托盘常驻（`src/desktop.rs`）。Windows 发布构建（release，非 debug）去掉控制台窗口
//! （任务 7a），从终端运行时会自动重新接上父进程的控制台（见 `hardening.rs`），命令行子命令一样能用。

// 只在 Windows 的 release 构建去掉控制台；dev 构建（`cargo run`）保留控制台方便调试，
// Linux/macOS 本来就不受这个属性影响（`windows_subsystem` 是 Windows 特有概念，见
// docs/FAQ.md 里 macOS 那节的说明：Finder/LaunchAgent 启动本来就没有控制台）。
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod account;
mod agents;
mod clock;
mod diag;
mod error;
mod gateway;
mod hardening;
mod instances;
mod listen;
mod metrics;
mod paths;
mod platform;
mod relay;
mod settings;
mod shells;
mod store;
mod tools;
mod transfer;
#[cfg(any(windows, target_os = "macos"))]
mod desktop;
mod updater;
mod util;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use crate::account::Account;
use crate::gateway::Inner;
use crate::instances::Instances;
use crate::paths::Paths;
use crate::store::Store;

#[derive(Parser)]
#[command(
    name = "looklook",
    version,
    about = "看看客户端：随时随地操作你的电脑和 AI 助手\nLooklook client: reach your computer and AI assistants from anywhere",
    after_help = HELP_EXAMPLES
)]
struct Cli {
    /// 数据目录（默认按系统惯例）/ Data directory (system default if omitted)
    #[arg(long, env = "LOOKLOOK_HOME", global = true)]
    home: Option<PathBuf>,
    /// 管理台监听地址，默认 0.0.0.0:1234（被占用时依次 +10）/ Console address, default 0.0.0.0:1234 (steps up by 10 if taken)
    #[arg(long, env = "LOOKLOOK_LISTEN", global = true)]
    listen: Option<SocketAddr>,
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

const HELP_EXAMPLES: &str = "\
没有图形界面的服务器 / Servers without a desktop:
  looklook login            登录：显示授权码和二维码，用手机或任意浏览器批准
                            Sign in: shows a code and QR code to approve from your phone or any browser
  looklook status           查看登录状态 / Show sign-in status
  looklook upgrade          升级到最新版本 / Upgrade to the latest version
  looklook access lan on    允许局域网设备打开管理台 / Let LAN devices open the console

子命令的帮助 / Help for a command: looklook <command> -h";

#[derive(Subcommand)]
enum Cmd {
    /// 启动客户端（默认）/ Start the client (default)
    Run {
        /// 看看服务端地址（仅未登录时生效）/ Looklook server URL (only while signed out)
        #[arg(long, env = "LOOKLOOK_SERVER")]
        server: Option<String>,
        /// 启动后不打开浏览器 / Don't open the browser
        #[arg(long)]
        no_browser: bool,
    },
    /// 在浏览器中打开管理台 / Open the console in the browser
    Open,
    /// 打印管理台地址 / Print the console address
    Url,
    /// 查看或修改访问控制 / Show or change who may open the console
    Access {
        #[command(subcommand)]
        action: Option<AccessCmd>,
    },
    /// 登录看看账户（在手机或任意浏览器上批准）/ Sign in (approve from your phone or any browser)
    Login,
    /// 退出登录 / Sign out
    Logout {
        /// 连不上服务器时也清除本机登录 / Clear the local sign-in even if the server can't be reached
        #[arg(long)]
        force: bool,
    },
    /// 查看状态 / Show status
    Status,
    /// 检查并安装新版本，终端里的任务不受影响 / Check for and install a new version; running tasks keep running
    #[command(visible_alias = "upgrade")]
    Update {
        /// 只检查，不安装 / Only check, don't install
        #[arg(long)]
        check: bool,
    },
}

#[derive(Subcommand)]
enum AccessCmd {
    /// 允许 / 不允许局域网设备打开管理台 / Allow or block LAN devices
    Lan { state: OnOff },
    /// 访问码：on 开启、off 关闭、new 换新的 / Access code: on, off, or new
    Code { action: CodeAction },
    /// 把 IP 或网段加入白名单 / Add an IP or range to the whitelist (e.g. 203.0.113.0/24)
    Allow { ip: String },
    /// 从白名单移除 / Remove from the whitelist
    Remove { ip: String },
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum OnOff {
    On,
    Off,
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum CodeAction {
    On,
    Off,
    New,
}

fn main() -> Result<()> {
    // Windows release 构建去掉了控制台（见上面的 `windows_subsystem` 属性）：从真正的终端
    // 启动时把标准输入输出接回父进程的控制台，命令行子命令才看得见输出；双击/开机启动等
    // 没有父控制台的场景这个调用是无害的空操作。其它平台是空实现。
    hardening::attach_console_if_launched_from_terminal();
    let _ = updater::exe_path(); // 记下程序路径（更新替换文件后重启要用）
    let cli = Cli::parse();
    let rt = tokio::runtime::Runtime::new()?;
    // 命令行子命令优先读正在运行的客户端写下的 run/endpoint，这里只是找不到时的后备
    let cli_listen = cli.listen.unwrap_or(listen::DEFAULT);
    match cli.cmd {
        None => dispatch_run(rt, cli.home, cli.listen, None, false),
        Some(Cmd::Run { server, no_browser }) => dispatch_run(rt, cli.home, cli.listen, server, no_browser),
        Some(Cmd::Open) => open_ui(&local_url(&cli.home, cli_listen)?),
        Some(Cmd::Url) => {
            println!("{}", local_url(&cli.home, cli_listen)?);
            if let Ok(v) = rt.block_on(call(&cli.home, cli_listen, "GET", "/api/access", None)) {
                if let (true, Some(ip)) = (v["allow_lan"].as_bool() == Some(true), v["lan_ips"][0].as_str()) {
                    println!("局域网 / LAN: http://{}:{}/", ip, v["port"]);
                }
            }
            Ok(())
        }
        Some(Cmd::Access { action }) => rt.block_on(cli_access(&cli.home, cli_listen, action)),
        Some(Cmd::Login) => rt.block_on(cli_login(&cli.home, cli_listen)),
        Some(Cmd::Logout { force }) => {
            let v = rt.block_on(call(&cli.home, cli_listen, "POST", "/api/auth/logout", Some(json!({ "force": force }))))?;
            print_status(&v);
            Ok(())
        }
        Some(Cmd::Status) => {
            let v = rt.block_on(call(&cli.home, cli_listen, "GET", "/api/status", None))?;
            print_status(&v["account"]);
            println!("终端方式 / Terminal backend: {}", v["capabilities"]["backend"].as_str().unwrap_or("-"));
            Ok(())
        }
        Some(Cmd::Update { check }) => rt.block_on(cli_update(&cli.home, cli_listen, check)),
    }
}

/// `looklook update`：让正在运行的客户端检查、下载并安装新版本，等它用新版本重新启动。
async fn cli_update(home: &Option<PathBuf>, listen: SocketAddr, check_only: bool) -> Result<()> {
    let u = call(home, listen, "GET", "/api/update", None).await?;
    let current = u["current"].as_str().unwrap_or("-").to_string();
    if u["available"].as_bool() != Some(true) {
        println!("已是最新版本 / Already up to date: {current}");
        return Ok(());
    }
    let version = u["version"].as_str().unwrap_or("-");
    println!("有新版本 / New version: {current} → {version}{}", if u["required"].as_bool() == Some(true) { "（必须更新 / required）" } else { "" });
    if let Some(notes) = u["notes"].as_str().filter(|n| !n.is_empty()) {
        println!("\n{notes}\n");
    }
    if check_only {
        println!("运行 looklook upgrade 安装。/ Run looklook upgrade to install it.");
        return Ok(());
    }
    if u["can_install"].as_bool() != Some(true) {
        let reason = u["cannot_reason"].as_str().unwrap_or("unsupported");
        bail!("不能在这里自动更新（{reason}），请到看看网页的“下载”页下载新版本手动安装\nCan't update automatically here ({reason}); download the new version from the Download page on the Looklook website");
    }
    call(home, listen, "POST", "/api/update/install", Some(json!({}))).await?;
    let mut last = String::new();
    // 下载 + 安装 + 重启，最多等 15 分钟
    for _ in 0..900 {
        tokio::time::sleep(Duration::from_secs(1)).await;
        // 重启期间连不上是正常的
        let Ok(s) = call(home, listen, "GET", "/api/status", None).await else { continue };
        if s["version"].as_str().is_some_and(|v| v != current) {
            println!("已更新到 {0}，客户端已重新启动。/ Updated to {0}; the client has restarted.", s["version"].as_str().unwrap_or("-"));
            return Ok(());
        }
        let job = &s["update"]["job"];
        let line = match job["state"].as_str().unwrap_or("") {
            "downloading" => {
                let mb = |v: &Value| v.as_u64().map(|n| format!("{:.1} MB", n as f64 / 1048576.0));
                format!("正在下载 / Downloading… {} / {}", mb(&job["done"]).unwrap_or_default(), mb(&job["total"]).unwrap_or_else(|| "?".into()))
            }
            "installing" => "正在安装 / Installing…".into(),
            "restarting" => "正在重启 / Restarting…".into(),
            "failed" => {
                let detail = job["detail"].as_str().unwrap_or_default();
                let why = cli_error(job["code"].as_str().unwrap_or("UPDATE_FAILED"), &Value::Null);
                bail!("更新失败 / Update failed: {why}{}", if detail.is_empty() { String::new() } else { format!("（{detail}）") });
            }
            _ => continue,
        };
        if line != last {
            println!("{line}");
            last = line;
        }
    }
    bail!("等了 15 分钟还没完成，运行 looklook status 查看客户端状态\nNot finished after 15 minutes; run looklook status to check the client")
}

/// `None`（直接双击/开机启动）与 `looklook run` 走同一条路：Windows/macOS 是桌面应用（窗口 + 托盘，
/// 见 `src/desktop.rs`），Linux 没有托盘，跟以前一样直接跑在当前线程的 tokio 运行时上。
fn dispatch_run(rt: tokio::runtime::Runtime, home: Option<PathBuf>, listen: Option<SocketAddr>, server: Option<String>, no_browser: bool) -> Result<()> {
    #[cfg(any(windows, target_os = "macos"))]
    {
        desktop::run(rt, home, listen, server, no_browser)
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        rt.block_on(run(home, listen, server, no_browser, CancellationToken::new()))
    }
}

/// 主进程日志（任务 7a）：以前只写 stderr，Windows 去掉控制台后（见 `windows_subsystem`
/// 属性）这些日志会彻底丢失，无法排障。现在固定再写一份到 `{LOOKLOOK_HOME}/logs/looklook.log`
/// （不按大小/日期滚动，简单起见；文件会一直增长，需要的话手动清理）；stderr 那份继续保留，
/// 有终端时（含 Windows 接回控制台之后）照样能看到。
///
/// 用 `std::sync::Once` 包一层：托盘“停止再启动”会再跑一次 `run()`，但全局日志订阅者
/// 只能设置一次，重复调用 `tracing_subscriber::...::init()` 会 panic。
fn init_tracing(paths: &Paths) {
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| {
        use tracing_subscriber::layer::SubscriberExt;
        use tracing_subscriber::util::SubscriberInitExt;
        use tracing_subscriber::EnvFilter;

        let filter = || EnvFilter::try_from_env("LOOKLOOK_LOG").unwrap_or_else(|_| EnvFilter::new("info,rathole=warn,hyper=warn"));

        let file_appender = tracing_appender::rolling::never(paths.home.join("logs"), "looklook.log");
        let (file_writer, guard) = tracing_appender::non_blocking(file_appender);
        // non-blocking writer 靠这个 guard 的 Drop 触发最后一次落盘；进程活多久这个日志就要
        // 活多久，泄漏掉（`Box::leak`）比在某个非退出路径上不小心把它提前 drop 掉更安全。
        Box::leak(Box::new(guard));

        use tracing_subscriber::Layer;

        // 日志过滤只作用在两个输出层上；通道状态层要看到 rathole 的 info 事件（“Control channel
        // established”），所以它有自己的过滤，不受上面 rathole=warn 的限制。
        let file_layer = tracing_subscriber::fmt::layer().with_ansi(false).with_target(false).with_writer(file_writer).with_filter(filter());
        let stderr_layer = tracing_subscriber::fmt::layer().with_target(false).with_writer(std::io::stderr).with_filter(filter());
        let relay_layer = relay::StatusLayer.with_filter(EnvFilter::new("rathole=info"));
        // 错误记录（设置 → 问题与日志）：只要 WARN 以上，第三方库里过于啰嗦的连接错误不要
        diag::init(&paths.home.join("logs"));
        let error_layer = diag::ErrorLayer.with_filter(EnvFilter::new("warn,hyper=error,hyper_util=error,rustls=error,tungstenite=error"));

        tracing_subscriber::registry().with(file_layer).with(stderr_layer).with(relay_layer).with(error_layer).init();
    });
}

async fn run(home: Option<PathBuf>, listen: Option<SocketAddr>, server: Option<String>, no_browser: bool, shutdown: CancellationToken) -> Result<()> {
    let mut paths = Paths::discover(home)?;
    init_tracing(&paths);
    // 更新后由旧版本拉起：等旧进程退出，不再打开浏览器（管理台页面会自己刷新）
    let restarted = std::env::var_os(updater::RESTART_ENV).is_some();
    if restarted {
        std::env::remove_var(updater::RESTART_ENV);
    }
    let no_browser = no_browser || restarted;
    hardening::probe_debugger();
    tracing::info!(home = %paths.home.display(), version = account::VERSION, target = paths::target(), "看看客户端启动");
    if paths.ttyd.is_none() {
        tracing::warn!("没有找到 ttyd，终端无法启动；请使用完整安装包，或设置 TTYD_BIN");
    }
    if paths.fonts.is_none() {
        tracing::warn!("没有找到终端字体，将使用系统字体");
    }
    // 同一个数据目录只能跑一份：端口不再固定（被占用会自动换），不能再靠“端口被占用”判断已经在运行。
    // 锁随本次 run() 结束释放（托盘“停止再启动”在同一进程里重新加锁）。
    let mut locked = lock_instance(&paths);
    if restarted {
        for _ in 0..60 {
            if locked.is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
            locked = lock_instance(&paths);
        }
    }
    let _instance_lock = match locked {
        Ok(lock) => lock,
        Err(()) => {
            // 已经在运行（例如双击了两次）：等它就绪后直接打开管理台。
            let home_opt = Some(paths.home.clone());
            let fallback = listen.unwrap_or(listen::DEFAULT);
            for _ in 0..30 {
                if already_running(&home_opt, fallback).await {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            println!("看看已经在运行。");
            if !no_browser && util::has_desktop() {
                open_ui(&local_url(&home_opt, fallback)?)?;
            }
            return Ok(());
        }
    };
    let store = Arc::new(Store::open(&paths.db())?);
    let ui = listen::bind(listen, &store).await?;
    hardening::check_self_integrity(&store);
    let account = Account::new(store.clone(), paths.clone(), &hardening::default_server(), &hardening::builtin_platform_keys())?;
    if let Some(url) = server {
        if account.platform().base != url.trim_end_matches('/') {
            match account.set_server(&url) {
                Ok(()) => tracing::info!(server = %url, "已切换看看服务端"),
                Err(e) => tracing::warn!(error = %e, "无法切换服务端"),
            }
        }
    }
    updater::rescue_mux_backups(); // 要在第一次调用会话保持程序之前
    updater::settle_trzsz();
    paths.trzsz = paths::find_trzsz();
    let instances = Instances::new(store.clone(), paths.clone());
    let updater = updater::Updater::new(store.clone(), paths.clone(), account.clone(), shutdown.clone());
    let metrics = metrics::Metrics::new();
    let transfers = transfer::Transfers::new(paths.clone());
    instances.recover().await;
    let relay = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
    // 监听 0.0.0.0 时，本机命令行和浏览器仍然用 127.0.0.1 连。
    let ui_addr = match ui.local_addr()? {
        a if a.ip().is_unspecified() => SocketAddr::new(std::net::Ipv4Addr::LOCALHOST.into(), a.port()),
        a => a,
    };
    let relay_addr = relay.local_addr()?;
    let access = gateway::access::AccessConfig::load(&store);
    // 首次运行时生成的访问码要存下来，否则每次启动都不一样。
    access.save(&store)?;
    let _ = std::fs::remove_file(paths.home.join("local-token")); // 旧版本的本机访问口令，已不再使用
    let app = Arc::new(Inner {
        account: account.clone(),
        instances: instances.clone(),
        updater: updater.clone(),
        metrics: metrics.clone(),
        transfers: transfers.clone(),
        store: store.clone(),
        paths: paths.clone(),
        ui_addr,
        relay_addr,
        http: gateway::proxy_client(),
        access: std::sync::RwLock::new(access),
        limiter: Default::default(),
        lan_ips: gateway::access::own_lan_ips(),
        remote: Default::default(),
    });
    std::fs::write(paths.home.join("run").join("endpoint"), ui_addr.to_string())?;
    let open_base = app.write_open_file()?;

    // `shutdown` 现在是调用者传进来的（不再在这里创建）：托盘的“停止”菜单（任务 7b，
    // 见 src/desktop.rs）靠取消同一个 token 来关掉这次 `run()`，不用等 Ctrl+C/SIGTERM。
    // 这些后台任务都必须随本次 run() 结束（见下方收尾）：托盘“停止→启动”在同一进程里重来，
    // 留下的旧任务会和新的并存——两份 rathole 用同一令牌互相挤掉、两份心跳、两份终端守护。
    let mut relay_task = tokio::spawn(relay::run(account.clone(), relay_addr, paths.relay_config(), shutdown.clone()));
    let background = [
        tokio::spawn(account.clone().heartbeat_loop()),
        tokio::spawn(account.clone().report_terminals_loop(instances.subscribe())),
        tokio::spawn(instances.clone().supervise(account.clone())),
        tokio::spawn(updater.clone().run()),
        tokio::spawn(metrics.run()),
        tokio::spawn(transfers.run()),
        {
            let account = account.clone();
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(Duration::from_secs(60)).await;
                    account.persist_clock();
                }
            })
        },
    ];
    let local = tokio::spawn(gateway::serve(ui, gateway::local_app(app.clone()), shutdown.clone()));
    let remote = tokio::spawn(gateway::serve(relay, gateway::relay_app(app.clone()), shutdown.clone()));

    let url = format!("{open_base}/");
    *UI_URL.lock().unwrap_or_else(|e| e.into_inner()) = Some(format!("http://{ui_addr}/"));
    println!("看看客户端已启动：{url}");
    let cfg = app.access();
    match (cfg.allow_lan, app.lan_ips.first()) {
        (true, Some(ip)) => println!("局域网里的设备可以打开：http://{ip}:{}/{}", ui_addr.port(), if cfg.code_enabled { "（需要访问码）" } else { "" }),
        _ => println!("只允许这台电脑自己打开管理台；要让局域网里的设备打开，运行 looklook access lan on"),
    }
    println!("如果这台机器有公网 IP，请不要把端口 {} 开放到公网。", ui_addr.port());
    if !no_browser && util::has_desktop() {
        if let Err(e) = open_ui(&url) {
            tracing::warn!(error = %e, "无法自动打开浏览器，请运行 looklook open");
        }
    }

    // 等系统信号（Ctrl+C/SIGTERM）或者外部（托盘“停止”/“退出”）取消 `shutdown`，谁先到算谁。
    tokio::select! {
        _ = wait_for_signal() => {}
        _ = shutdown.cancelled() => {}
    }
    tracing::info!("正在退出…（终端里的任务会继续运行）");
    UI_URL.lock().unwrap_or_else(|e| e.into_inner()).take();
    shutdown.cancel(); // 上面两条路径都可能先到；再调一次是幂等的，确保下面用到 shutdown.clone() 的任务一定会收到取消
    for task in &background {
        task.abort();
    }
    instances.shutdown().await;
    account.persist_clock();
    let _ = tokio::time::timeout(Duration::from_secs(3), async {
        let _ = local.await;
        let _ = remote.await;
    })
    .await;
    // 关远程通道（rathole 自己最多等 5 秒，超时就 abort）。
    if tokio::time::timeout(Duration::from_secs(6), &mut relay_task).await.is_err() {
        relay_task.abort();
    }
    if updater::restart_requested() {
        drop(_instance_lock);
        updater::restart_now();
    }
    Ok(())
}

/// 这次 run() 的本机管理台地址（桌面应用窗口加载它；没在运行时为空）。
/// 窗口始终用本机地址：“打开地址”设置是给浏览器和局域网用的。
static UI_URL: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

#[cfg_attr(not(any(windows, target_os = "macos")), allow(dead_code))]
fn ui_url() -> Option<String> {
    UI_URL.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

async fn wait_for_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut term = signal(SignalKind::terminate()).expect("signal");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

fn open_ui(url: &str) -> Result<()> {
    if util::open_url(url) {
        Ok(())
    } else {
        bail!("无法打开浏览器，请手动访问：{url}")
    }
}

/// 正在运行的客户端的本机地址（本机访问不需要访问码）。
fn endpoint(home: &Option<PathBuf>, listen: SocketAddr) -> Result<String> {
    let paths = Paths::discover(home.clone())?;
    let fallback = if listen.ip().is_unspecified() { SocketAddr::new(std::net::Ipv4Addr::LOCALHOST.into(), listen.port()) } else { listen };
    Ok(std::fs::read_to_string(paths.home.join("run").join("endpoint")).map(|s| s.trim().to_string()).unwrap_or_else(|_| fallback.to_string()))
}

/// 浏览器打开的地址：设置了打开地址时用它（写在 `run/open`），否则用本机地址。
fn local_url(home: &Option<PathBuf>, listen: SocketAddr) -> Result<String> {
    let addr = endpoint(home, listen)?;
    let paths = Paths::discover(home.clone())?;
    let base = std::fs::read_to_string(paths.home.join("run").join("open")).map(|s| s.trim().to_string()).unwrap_or_else(|_| format!("http://{addr}"));
    Ok(format!("{base}/"))
}

/// 数据目录的单实例锁（`run/lock`）。`Err` 表示另一份已经持有；文件系统不支持加锁时不拦。
fn lock_instance(paths: &Paths) -> std::result::Result<Option<std::fs::File>, ()> {
    let path = paths.home.join("run").join("lock");
    let file = match std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(&path) {
        Ok(f) => f,
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "无法创建实例锁");
            return Ok(None);
        }
    };
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(std::fs::TryLockError::WouldBlock) => Err(()),
        Err(std::fs::TryLockError::Error(e)) => {
            tracing::warn!(error = %e, "无法加实例锁");
            Ok(None)
        }
    }
}

/// 上次写下的地址上是不是正在运行的看看（短超时：那个端口上可能是别的、不应答的程序）。
async fn already_running(home: &Option<PathBuf>, listen: SocketAddr) -> bool {
    let Ok(addr) = endpoint(home, listen) else { return false };
    let Ok(c) = reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(2)).build() else { return false };
    let Ok(resp) = c.get(format!("http://{addr}/api/status")).header(gateway::CLIENT_HEADER, "1").send().await else { return false };
    resp.status().is_success() && resp.json::<Value>().await.is_ok_and(|v| v["capabilities"].is_object())
}

/// 命令行子命令通过本机接口操作正在运行的客户端。
async fn call(home: &Option<PathBuf>, listen: SocketAddr, method: &str, path: &str, body: Option<Value>) -> Result<Value> {
    let addr = endpoint(home, listen)?;
    let c = reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(60)).build()?;
    let mut req = c.request(method.parse()?, format!("http://{addr}{path}")).header(gateway::CLIENT_HEADER, "1");
    if let Some(b) = body {
        req = req.json(&b);
    }
    let resp = req.send().await.context(NOT_RUNNING)?;
    let status = resp.status();
    let v: Value = resp.json().await.unwrap_or(Value::Null);
    if !status.is_success() {
        let code = v["error"]["code"].as_str().unwrap_or("INTERNAL");
        bail!("{}", cli_error(code, &v["error"]["params"]));
    }
    Ok(v)
}

const NOT_RUNNING: &str = "连不上看看客户端，请先启动它：looklook run（安装为服务时：systemctl --user start looklook）\n\
Can't reach the Looklook client. Start it with: looklook run (installed as a service: systemctl --user start looklook)";

/// 授权登录：打印授权码、网址和二维码，等用户在任意设备（通常是手机）的浏览器里批准。
/// 没有图形界面的服务器就靠它登录；Ctrl+C 取消。
async fn cli_login(home: &Option<PathBuf>, listen: SocketAddr) -> Result<()> {
    let st = call(home, listen, "GET", "/api/status", None).await?;
    if st["account"]["logged_in"].as_bool() == Some(true) {
        print_status(&st["account"]);
        println!("要换账户，先运行 looklook logout / To switch accounts, run looklook logout first");
        return Ok(());
    }
    let v = call(home, listen, "POST", "/api/auth/login", Some(json!({}))).await?;
    let url = v["verification_url"].as_str().unwrap_or("-");
    println!("用手机扫描二维码，或在任意设备的浏览器中打开下面的网址，核对授权码后批准这台电脑：");
    println!("Scan the QR code with your phone, or open the link in any browser, check the code and approve this computer:");
    if let Some(qr) = qr_text(url) {
        println!("\n{qr}");
    }
    println!("  网址 / Link: {url}");
    println!("  授权码 / Code: {}", v["user_code"].as_str().unwrap_or("-"));
    let minutes = v["expires_in"].as_u64().map(|s| s.div_ceil(60)).unwrap_or(10);
    println!("\n等待批准…（{minutes} 分钟内有效，Ctrl+C 取消）/ Waiting for approval… (valid for {minutes} min, Ctrl+C to cancel)");
    if util::has_desktop() {
        util::open_url(url);
    }
    let wait = async {
        loop {
            tokio::time::sleep(Duration::from_secs(2)).await;
            let s = call(home, listen, "GET", "/api/auth/status", None).await?;
            match s["state"].as_str() {
                Some("approved") => return Ok(()),
                Some("failed") => {
                    let code = s["error"]["code"].as_str().unwrap_or("INTERNAL");
                    let mut msg = cli_error(code, &s["error"]["params"]);
                    if code == "DEVICE_LIMIT_REACHED" {
                        if let Ok(st) = call(home, listen, "GET", "/api/status", None).await {
                            if let Some(server) = st["account"]["server"].as_str() {
                                msg.push_str(&format!("\n  {}/devices", server.trim_end_matches('/')));
                            }
                        }
                    }
                    bail!("{msg}")
                }
                Some("idle") => bail!("授权已取消 / Sign-in was cancelled"),
                _ => {}
            }
        }
    };
    tokio::select! {
        r = wait => r?,
        _ = tokio::signal::ctrl_c() => {
            let _ = call(home, listen, "POST", "/api/auth/login/cancel", Some(json!({}))).await;
            bail!("授权已取消 / Sign-in was cancelled")
        }
    }
    let v = call(home, listen, "GET", "/api/status", None).await?;
    println!("✓ 登录成功 / Signed in");
    print_status(&v["account"]);
    Ok(())
}

/// 网址的终端二维码（两行像素拼一个字符）。颜色反过来画：深色背景的终端最常见，
/// 这样扫出来是白底黑码，和 `qrencode -t UTF8` 一样。
fn qr_text(url: &str) -> Option<String> {
    use qrcode::render::unicode::Dense1x2;
    let code = qrcode::QrCode::with_error_correction_level(url, qrcode::EcLevel::L).ok()?;
    Some(code.render::<Dense1x2>().dark_color(Dense1x2::Light).light_color(Dense1x2::Dark).quiet_zone(true).build())
}

/// `looklook access`：查看或修改访问控制，然后打印当前状态。
async fn cli_access(home: &Option<PathBuf>, listen: SocketAddr, action: Option<AccessCmd>) -> Result<()> {
    let body = match action {
        None => None,
        Some(AccessCmd::Lan { state }) => Some(json!({ "allow_lan": matches!(state, OnOff::On) })),
        Some(AccessCmd::Code { action: CodeAction::On }) => Some(json!({ "code_enabled": true })),
        Some(AccessCmd::Code { action: CodeAction::Off }) => Some(json!({ "code_enabled": false })),
        Some(AccessCmd::Code { action: CodeAction::New }) => Some(json!({ "new_code": true })),
        Some(AccessCmd::Allow { ip }) => Some(json!({ "add_ip": ip })),
        Some(AccessCmd::Remove { ip }) => Some(json!({ "remove_ip": ip })),
    };
    let v = match body {
        Some(b) => call(home, listen, "PUT", "/api/access", Some(b)).await?,
        None => call(home, listen, "GET", "/api/access", None).await?,
    };
    let on = |k: &str| if v[k].as_bool() == Some(true) { "开启 / on" } else { "关闭 / off" };
    println!("这台电脑自己 / This computer: 总是可以打开，不需要访问码 / always allowed, no code needed");
    match v["lan_ips"][0].as_str() {
        Some(ip) => println!("局域网访问 / LAN access: {}（http://{ip}:{}/）", on("allow_lan"), v["port"]),
        None => println!("局域网访问 / LAN access: {}", on("allow_lan")),
    }
    let ips: Vec<&str> = v["allowed_ips"].as_array().map(|a| a.iter().filter_map(|x| x.as_str()).collect()).unwrap_or_default();
    println!("白名单 / Whitelist: {}", if ips.is_empty() { "（无 / none）".to_string() } else { ips.join(", ") });
    println!("访问码 / Access code: {}{}", on("code_enabled"), if v["code_enabled"].as_bool() == Some(true) { format!(", {}", v["code"].as_str().unwrap_or("")) } else { String::new() });
    if !ips.is_empty() && v["code_enabled"].as_bool() != Some(true) {
        println!("提示：已添加白名单，建议开启访问码 / Tip: with a whitelist, turn on the access code: looklook access code on");
    }
    Ok(())
}

fn cli_error(code: &str, params: &Value) -> String {
    match code {
        "AUTHORIZATION_EXPIRED" => "授权已过期，请重新运行 looklook login / The code expired; run looklook login again".into(),
        "AUTHORIZATION_DENIED" => "授权被拒绝 / Sign-in was denied".into(),
        "DEVICE_ALREADY_LOGGED_IN" => {
            let name = params["device_name"].as_str().unwrap_or("另一台电脑 / another computer");
            format!(
                "这个账户已经在“{name}”上登录。请先在那台电脑上退出，或到看看网页的“登录的电脑”里让它退出登录。\n\
                 This account is already signed in on \"{name}\". Sign out there first, or sign it out under Devices on the Looklook website."
            )
        }
        "DEVICE_LIMIT_REACHED" => device_limit_text(params),
        "MEMBERSHIP_EXPIRED" => "会员已到期，请先在看看网页续费 / Membership expired; renew it on the Looklook website".into(),
        "ACCOUNT_DISABLED" => "账户已停用 / This account is disabled".into(),
        "NETWORK" => "连不上看看服务器，请检查网络 / Can't reach the Looklook server; check the network".into(),
        "ALREADY_LOGGED_IN" => "已经登录了 / Already signed in".into(),
        "CLIENT_VERSION_UNSUPPORTED" => "客户端版本太旧，请先运行 looklook upgrade / This client is too old; run looklook upgrade".into(),
        "VALIDATION_FAILED" => match params["rule"].as_str() {
            Some("ip_format") => "IP 格式不对，例如 203.0.113.7 或 203.0.113.0/24 / Invalid IP, e.g. 203.0.113.7 or 203.0.113.0/24".into(),
            Some("ip_prefix") => "网段长度不对，IPv4 是 /8 到 /32 / Invalid prefix length; IPv4 takes /8 to /32".into(),
            Some("ip_too_wide") => "范围太大，不能对整个互联网开放 / Range too wide; it can't open to the whole internet".into(),
            Some("ip_loopback") => "这台电脑自己本来就可以打开，不用添加 / This computer is always allowed; no need to add it".into(),
            Some("ip_too_many") => "白名单最多 32 项 / The whitelist holds at most 32 entries".into(),
            Some(r) => format!("填写的内容不正确 / Invalid input: {r}"),
            None => "填写的内容不正确 / Invalid input".into(),
        },
        "REMOTE_FORBIDDEN" => "这个操作只能在这台电脑或局域网里进行 / Only allowed from this computer or the LAN".into(),
        "UPDATE_BUSY" => "已经在更新了 / An update is already running".into(),
        "UPDATE_NONE" => "已是最新版本 / Already up to date".into(),
        "UPDATE_CHECKSUM" => "下载的安装包校验不通过，请重试 / The download failed its checksum; try again".into(),
        "UPDATE_DOWNLOAD_FAILED" => "下载安装包失败，请检查网络后重试 / Download failed; check the network and try again".into(),
        "UPDATE_UNSUPPORTED" => "不能在这里自动更新，请到看看网页的“下载”页手动安装 / Can't update automatically here; install from the Download page".into(),
        "UPDATE_FAILED" => "安装没有完成，原来的版本保持不变 / Install didn't finish; the previous version is unchanged".into(),
        other => format!("操作失败 / Failed: {other}"),
    }
}

/// 登录被拒：已经有上限台数的电脑登录了。列出这些电脑，提示到网页上让其中一台退出登录。
fn device_limit_text(params: &Value) -> String {
    let devices = params["devices"].as_array().cloned().unwrap_or_default();
    let max = params["max"].as_i64().map(|n| n.to_string()).unwrap_or_else(|| devices.len().to_string());
    let mut out = format!("这个账户最多同时在 {max} 台电脑上登录，现在已经满了：\nThis account can be signed in on at most {max} computers, and that's full:");
    for d in &devices {
        let name = d["name"].as_str().filter(|s| !s.is_empty()).unwrap_or("未命名电脑 / unnamed");
        let os = d["os"].as_str().filter(|s| !s.is_empty()).map(|s| format!("（{s}）")).unwrap_or_default();
        let state = if d["online"].as_bool() == Some(true) {
            "在线 / online".to_string()
        } else {
            match d["last_seen_at"].as_str() {
                Some(t) => format!("最后在线 / last seen {}", t.replace('T', " ").split('.').next().unwrap_or(t).trim_end_matches('Z')),
                None => "不在线 / offline".to_string(),
            }
        };
        out.push_str(&format!("\n  · {name}{os}  {state}"));
    }
    out.push_str("\n请到看看网页的“我的电脑”里让其中一台退出登录，然后再登录。\nSign one of them out under My computers on the Looklook website, then sign in again.");
    out
}

fn print_status(v: &Value) {
    let v = if v.get("account").is_some() { &v["account"] } else { v };
    if v["logged_in"].as_bool() != Some(true) {
        println!("未登录 / Not signed in（服务端 / server: {}）", v["server"].as_str().unwrap_or("-"));
        println!("登录 / Sign in: looklook login");
        return;
    }
    let s = &v["session"];
    println!("已登录 / Signed in: {}（{}）", s["user"]["nickname"].as_str().unwrap_or(""), s["user"]["username"].as_str().unwrap_or(""));
    println!("专属地址 / Your address: {}", s["user_host_url"].as_str().unwrap_or("-"));
    println!("可以使用到 / Membership until: {}", s["membership_expires_at"].as_str().unwrap_or("-"));
    if v["allowed"].as_bool() != Some(true) {
        println!("当前不可用 / Unavailable: {}", v["reason"].as_str().unwrap_or("-"));
    }
}
