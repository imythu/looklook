//! 桌面应用（Windows / macOS）：Tauri 窗口直接显示管理台，系统托盘常驻。
//!
//! 启动后打开应用窗口（不再打开浏览器）；关掉窗口只是隐藏，服务继续在后台运行，
//! 点托盘图标（macOS 也可以点程序坞图标）重新显示。托盘菜单：打开看看 / 启动服务 /
//! 停止服务 / 开机启动 / 退出。开机自启（`run --no-browser`）只启动服务、不弹窗口。
//!
//! macOS：只有窗口显示时程序坞里才有图标（就是应用图标），窗口隐藏后切换成
//! `Accessory` 模式只留菜单栏图标——以前裸二进制跑 tao 事件循环会在程序坞多出一个
//! 通用的“exec”图标。
//!
//! Linux 没有这个文件（整个 `#![cfg(...)]` 关掉），照常靠 systemd 用户服务无界面运行，
//! 也不需要 GTK / WebKitGTK（Cargo.toml 里 tauri 放在按平台生效的依赖里）。
//!
//! 架构：Tauri 的事件循环必须跑在主线程，tokio 运行时挪到后台线程跑服务（`supervisor`），
//! 并交给 Tauri 共用（`tauri::async_runtime::set`）。
#![cfg(any(windows, target_os = "macos"))]

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc as std_mpsc;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use tauri::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
#[cfg(windows)]
use tauri::tray::{MouseButton, MouseButtonState, TrayIconEvent};
use tauri::webview::NewWindowResponse;
use tauri::{AppHandle, Manager, RunEvent, Url, WebviewUrl, WebviewWindowBuilder, WindowEvent, Wry};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

const MAIN: &str = "main";

/// 托盘菜单、窗口标题是原生控件，走不了网页那套 i18n：`src-ui/shared/i18n.js` 选的语言只存在浏览器的
/// `localStorage`（键 `ll_locale`）里，主进程完全拿不到。这里退而求其次，用系统语言猜一下，
/// 猜不到时跟网页端的默认值保持一致，用中文（`i18n.js` 里 `fallbackLng: 'zh-CN'`，
/// `browserLocale()` 也是“不是以 en 开头就当中文”）。
fn tr(zh: &'static str, en: &'static str) -> &'static str {
    static EN: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if *EN.get_or_init(os_locale_is_english) {
        en
    } else {
        zh
    }
}

#[cfg(windows)]
fn os_locale_is_english() -> bool {
    // Windows 上没有 Unix 那套 LANG/LC_ALL 惯例；这里不为了猜语言再加一个注册表/API 依赖，
    // 只在设置了 LANG（比如 MSYS2/WSL 混用环境）时参考一下，猜不到就落回中文，跟网页端默认值一致。
    std::env::var("LANG").ok().map(|l| l.to_ascii_lowercase().starts_with("en")).unwrap_or(false)
}

#[cfg(target_os = "macos")]
fn os_locale_is_english() -> bool {
    if let Ok(out) = std::process::Command::new("defaults").args(["read", "-g", "AppleLanguages"]).output() {
        let s = String::from_utf8_lossy(&out.stdout).to_ascii_lowercase();
        if let Some(first) = s.lines().find(|l| l.contains('"')) {
            return first.contains("\"en");
        }
    }
    std::env::var("LANG").map(|l| l.to_ascii_lowercase().starts_with("en")).unwrap_or(false)
}

enum Cmd {
    Start,
    Stop,
    Exit(std_mpsc::Sender<()>),
}

struct Shell {
    home: Option<PathBuf>,
    listen: Option<SocketAddr>,
    cmd: mpsc::UnboundedSender<Cmd>,
    running: Arc<AtomicBool>,
    /// 当前这次 run() 的管理台地址（服务就绪后才有）。
    url: Arc<std::sync::Mutex<Option<String>>>,
}

/// 供 `main.rs` 调用：接管主线程跑 Tauri 事件循环，退出时直接结束进程。
pub fn run(rt: tokio::runtime::Runtime, home: Option<PathBuf>, listen: Option<SocketAddr>, server: Option<String>, no_browser: bool) -> Result<()> {
    tauri::async_runtime::set(rt.handle().clone());
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<Cmd>();
    let running = Arc::new(AtomicBool::new(true));
    let url = Arc::new(std::sync::Mutex::new(None));
    // 更新后由旧进程拉起的新进程：旧进程还没退出，不能被当成“第二份”而把参数转给它后自己退掉。
    let restarted = std::env::var_os(crate::updater::RESTART_ENV).is_some();
    let shell = Shell { home: home.clone(), listen, cmd: cmd_tx, running: running.clone(), url: url.clone() };

    let mut builder = tauri::Builder::default();
    if !restarted {
        // 再次双击图标（或开始菜单里再点一次）：显示已经在运行的窗口，而不是再启动一份。
        // 登录自启（run --no-browser）撞上已经在运行的应用时什么都不做。
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
            if !argv.iter().any(|a| a == "--no-browser") {
                show_main(app);
            }
        }));
    }
    #[cfg(target_os = "macos")]
    {
        builder = builder.menu(mac_menu);
    }
    let app = builder
        .manage(shell)
        .on_menu_event(on_menu)
        .on_window_event(|window, event| {
            // 关掉主窗口只是隐藏：服务和托盘继续在后台运行。
            if let WindowEvent::CloseRequested { api, .. } = event {
                if window.label() == MAIN {
                    api.prevent_close();
                    let _ = window.hide();
                    dock(window.app_handle(), false);
                }
            }
        })
        .setup(move |app| {
            let handle = app.handle().clone();
            #[cfg(target_os = "macos")]
            autostart::default_on(home.as_ref());
            let items = tray(&handle)?;
            app.manage(items.clone());
            let rt_running = running.clone();
            let rt_url = url.clone();
            std::thread::Builder::new().name("looklook-rt".into()).spawn(move || {
                rt.block_on(supervisor(home, listen, server, cmd_rx, rt_running, rt_url));
            })?;
            tauri::async_runtime::spawn(refresh_menu(items, running.clone()));
            if no_browser {
                dock(&handle, false);
            } else {
                show_main(&handle);
            }
            Ok(())
        })
        .build(tauri::generate_context!())?;
    app.run(|app, event| match event {
        // 窗口都关了不退出（托盘常驻）；只有“退出”菜单（app.exit）才真正退出。
        RunEvent::ExitRequested { code: None, api, .. } => api.prevent_exit(),
        RunEvent::Exit => {
            // 退出前把服务停干净（ttyd/实例收尾、关通道），最多等 10 秒。
            let (tx, rx) = std_mpsc::channel();
            if app.state::<Shell>().cmd.send(Cmd::Exit(tx)).is_ok() {
                let _ = rx.recv_timeout(Duration::from_secs(10));
            }
        }
        #[cfg(target_os = "macos")]
        RunEvent::Reopen { .. } => show_main(app),
        _ => {}
    });
    Ok(())
}

/// 等这次服务就绪；它没有启动（例如同一数据目录已有一份命令行启动的客户端）时，用那一份的地址。
async fn wait_url(app: &AppHandle) -> Option<String> {
    let shell = app.state::<Shell>();
    for _ in 0..120 {
        if let Some(u) = shell.url.lock().unwrap_or_else(|e| e.into_inner()).clone() {
            return Some(u);
        }
        if !shell.running.load(Ordering::Relaxed) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    let fallback = shell.listen.unwrap_or(crate::listen::DEFAULT);
    if crate::already_running(&shell.home, fallback).await {
        return crate::local_url(&shell.home, fallback).ok();
    }
    tracing::error!("管理台没有启动，查看日志 logs/looklook.log");
    None
}

/// 显示（没有就创建）主窗口并置前。
fn show_main(app: &AppHandle) {
    dock(app, true);
    if let Some(w) = app.get_webview_window(MAIN) {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let Some(url) = wait_url(&app).await else { return };
        let Ok(url) = url.parse::<Url>() else { return };
        let handle = app.clone();
        // 创建窗口要在主线程上做。
        let _ = app.run_on_main_thread(move || {
            match window(&handle, MAIN, url).inner_size(1280.0, 820.0).min_inner_size(420.0, 480.0).center().build() {
                Ok(w) => {
                    let _ = w.set_focus();
                }
                Err(e) => tracing::error!(error = %e, "无法创建窗口"),
            }
        });
    });
}

/// 管理台窗口。网页里 `window.open` 的本机页面（终端“新标签页打开”等）开成新的应用窗口，
/// 其它网址（看看网站、会员页、授权页）交给系统浏览器。
fn window<'a>(app: &'a AppHandle, label: &str, url: Url) -> WebviewWindowBuilder<'a, Wry, AppHandle> {
    let console = url.clone();
    let handle = app.clone();
    WebviewWindowBuilder::new(app, label, WebviewUrl::External(url))
        .title(tr("看看", "Looklook"))
        .on_document_title_changed(|w, title| {
            let _ = w.set_title(&title);
        })
        .on_new_window(move |target, features| {
            let same = target.as_str() == "about:blank" || (target.origin() == console.origin());
            if !same {
                let t = target.to_string();
                std::thread::spawn(move || crate::util::open_url(&t));
                return NewWindowResponse::Deny;
            }
            static NEXT: AtomicU32 = AtomicU32::new(1);
            let label = format!("w{}", NEXT.fetch_add(1, Ordering::Relaxed));
            match window(&handle, &label, target).window_features(features).inner_size(1100.0, 720.0).build() {
                Ok(window) => NewWindowResponse::Create { window },
                Err(e) => {
                    tracing::warn!(error = %e, "无法打开新窗口");
                    NewWindowResponse::Deny
                }
            }
        })
}

/// macOS：窗口显示时程序坞里有应用图标，全部隐藏后只留菜单栏图标。
fn dock(app: &AppHandle, visible: bool) {
    #[cfg(target_os = "macos")]
    {
        let visible = visible || app.webview_windows().iter().any(|(l, w)| l != MAIN && w.is_visible().unwrap_or(false));
        let policy = if visible { tauri::ActivationPolicy::Regular } else { tauri::ActivationPolicy::Accessory };
        let _ = app.set_activation_policy(policy);
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (app, visible);
}

#[derive(Clone)]
struct Items {
    start: MenuItem<Wry>,
    stop: MenuItem<Wry>,
    auto: CheckMenuItem<Wry>,
}

fn tray(app: &AppHandle) -> tauri::Result<Items> {
    let open = MenuItem::with_id(app, "open", tr("打开看看", "Open Looklook"), true, None::<&str>)?;
    let start = MenuItem::with_id(app, "start", tr("启动服务", "Start service"), false, None::<&str>)?;
    let stop = MenuItem::with_id(app, "stop", tr("停止服务", "Stop service"), true, None::<&str>)?;
    let auto = CheckMenuItem::with_id(app, "autostart", tr("开机启动", "Launch at startup"), true, autostart::is_enabled(), None::<&str>)?;
    let exit = MenuItem::with_id(app, "exit", tr("退出", "Exit"), true, None::<&str>)?;
    let sep1 = PredefinedMenuItem::separator(app)?;
    let sep2 = PredefinedMenuItem::separator(app)?;
    let menu = Menu::with_items(app, &[&open, &start, &stop, &sep1, &auto, &sep2, &exit])?;
    let mut tray = TrayIconBuilder::with_id("main").menu(&menu).tooltip(tr("看看客户端", "Looklook client"));
    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    // Windows 习惯：左键点图标打开窗口，右键出菜单；macOS 点菜单栏图标出菜单。
    #[cfg(windows)]
    {
        tray = tray.show_menu_on_left_click(false).on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
                show_main(tray.app_handle());
            }
        });
    }
    tray.build(app)?;
    Ok(Items { start, stop, auto })
}

fn on_menu(app: &AppHandle, event: MenuEvent) {
    let shell = app.state::<Shell>();
    match event.id().as_ref() {
        "open" => show_main(app),
        "start" => {
            let _ = shell.cmd.send(Cmd::Start);
        }
        "stop" => {
            let _ = shell.cmd.send(Cmd::Stop);
            // 服务停了页面也用不了：收起主窗口，关掉另开的终端窗口。
            for (label, w) in app.webview_windows() {
                let _ = if label == MAIN { w.hide() } else { w.close() };
            }
            dock(app, false);
        }
        "autostart" => {
            // 勾选框点击后自己已经切换了状态，这里按新状态写入，失败再拨回去。
            let item = &app.state::<Items>().auto;
            let on = item.is_checked().unwrap_or(false);
            if let Err(e) = autostart::set_enabled(on) {
                tracing::warn!(error = %e, "无法修改开机启动设置");
                let _ = item.set_checked(!on);
            }
        }
        "exit" | "quit" => app.exit(0),
        _ => {}
    }
}

/// 菜单项按服务状态置灰：运行中只能“停止服务”，已停止只能“启动服务”。
async fn refresh_menu(items: Items, running: Arc<AtomicBool>) {
    let mut shown = None;
    loop {
        let now = running.load(Ordering::Relaxed);
        if shown != Some(now) {
            shown = Some(now);
            let _ = items.start.set_enabled(!now);
            let _ = items.stop.set_enabled(now);
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

#[cfg(target_os = "macos")]
fn mac_menu(app: &AppHandle) -> tauri::Result<Menu<Wry>> {
    use tauri::menu::SubmenuBuilder;
    // 自己的“退出”：走 app.exit 先停服务。编辑菜单不能省，否则网页里 ⌘C / ⌘V 不起作用。
    let quit = MenuItem::with_id(app, "quit", tr("退出看看", "Quit Looklook"), true, Some("CmdOrCtrl+Q"))?;
    let main = SubmenuBuilder::new(app, tr("看看", "Looklook")).about(None).separator().hide().hide_others().show_all().separator().item(&quit).build()?;
    let edit = SubmenuBuilder::new(app, tr("编辑", "Edit")).undo().redo().separator().cut().copy().paste().select_all().build()?;
    let view = SubmenuBuilder::new(app, tr("窗口", "Window")).minimize().maximize().separator().close_window().build()?;
    Menu::with_items(app, &[&main, &edit, &view])
}

/// 托盘事件桥到 tokio：管理“当前是否在跑服务”，Start/Stop 只是创建/取消一个
/// `CancellationToken` 并等 `run()` 的 `JoinHandle` 结束。
async fn supervisor(
    home: Option<PathBuf>,
    listen: Option<SocketAddr>,
    server: Option<String>,
    mut rx: mpsc::UnboundedReceiver<Cmd>,
    running: Arc<AtomicBool>,
    url: Arc<std::sync::Mutex<Option<String>>>,
) {
    // 窗口代替浏览器，run() 自己不再打开浏览器。
    let mut current = Some(spawn_server(home.clone(), listen, server));
    let mut tick = tokio::time::interval(Duration::from_millis(250));
    loop {
        // 服务自己退出了（比如端口被占用、启动失败）也要反映到托盘上，这样“启动服务”才会重新可用。
        if current.as_ref().is_some_and(|(_, h)| h.is_finished()) {
            current = None;
        }
        running.store(current.is_some(), Ordering::Relaxed);
        *url.lock().unwrap_or_else(|e| e.into_inner()) = if current.is_some() { crate::ui_url() } else { None };
        let cmd = tokio::select! {
            cmd = rx.recv() => match cmd {
                Some(cmd) => cmd,
                None => break,
            },
            _ = tick.tick() => continue,
        };
        match cmd {
            Cmd::Start => {
                if current.is_none() {
                    current = Some(spawn_server(home.clone(), listen, None));
                }
            }
            Cmd::Stop => stop_current(&mut current).await,
            Cmd::Exit(ack) => {
                stop_current(&mut current).await;
                running.store(false, Ordering::Relaxed);
                let _ = ack.send(());
                return;
            }
        }
    }
}

async fn stop_current(current: &mut Option<(CancellationToken, tokio::task::JoinHandle<()>)>) {
    if let Some((token, handle)) = current.take() {
        token.cancel();
        // 等实例收尾（ttyd/tmux）、避免“停止”卡死太久；超时也继续，进程该退还是退。
        if tokio::time::timeout(Duration::from_secs(10), handle).await.is_err() {
            tracing::warn!("服务停止超时");
        }
    }
}

fn spawn_server(home: Option<PathBuf>, listen: Option<SocketAddr>, server: Option<String>) -> (CancellationToken, tokio::task::JoinHandle<()>) {
    let token = CancellationToken::new();
    let child = token.clone();
    let handle = tokio::spawn(async move {
        if let Err(e) = crate::run(home, listen, server, true, child).await {
            tracing::error!(error = %e, "看看服务未能启动");
        }
    });
    (token, handle)
}

/// 开机启动（详细设计任务 7c）：托盘“开机启动”勾选框的读写。已有的安装脚本
/// （`packaging/install-windows.bat` 的“启动”文件夹快捷方式、`packaging/install-macos.sh` 的
/// LaunchAgent）在安装时就配置好了自启动，这里是给用户在托盘里随时开关的运行时开关。
mod autostart {
    #[cfg(windows)]
    mod imp {
        use std::path::PathBuf;

        // 安装包（packaging/nsis-hooks.nsh，旧的 install-windows.bat 也一样）在“启动”文件夹里建
        // “看看.lnk”；这里为了不额外引入 COM 依赖，打开开关时放一个小的 .cmd 启动脚本。
        // 两者任意一个存在就算开着，关掉时两个都删。
        const FILE_NAME: &str = "looklook-tray-autostart.cmd";
        const SHORTCUT: &str = "看看.lnk";

        fn startup() -> Option<PathBuf> {
            dirs::config_dir().map(|d| d.join("Microsoft").join("Windows").join("Start Menu").join("Programs").join("Startup"))
        }

        pub fn is_enabled() -> bool {
            startup().is_some_and(|d| d.join(FILE_NAME).is_file() || d.join(SHORTCUT).is_file())
        }

        pub fn set_enabled(on: bool) -> std::io::Result<()> {
            let dir = startup().ok_or_else(|| std::io::Error::other("无法确定“启动”文件夹的位置"))?;
            let path = dir.join(FILE_NAME);
            if !on {
                match std::fs::remove_file(dir.join(SHORTCUT)) {
                    Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
                    _ => {}
                }
            }
            if on && is_enabled() {
                return Ok(());
            }
            if on {
                if let Some(dir) = path.parent() {
                    std::fs::create_dir_all(dir)?;
                }
                let exe = std::env::current_exe()?;
                let script = format!("@echo off\r\nstart \"\" \"{}\" run --no-browser\r\n", exe.display());
                std::fs::write(&path, script)
            } else {
                match std::fs::remove_file(&path) {
                    Ok(()) => Ok(()),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                    Err(e) => Err(e),
                }
            }
        }
    }
    #[cfg(windows)]
    pub use imp::{is_enabled, set_enabled};

    #[cfg(target_os = "macos")]
    mod imp {
        use std::path::PathBuf;

        // 和 install-macos.sh 用同一个 Label（`com.looklook.client`）与同一个 plist 文件：
        // 托盘开关操作的就是安装脚本创建的那个登录项，不会出现两份自启动。
        const LABEL: &str = "com.looklook.client";

        fn plist_path() -> Option<PathBuf> {
            dirs::home_dir().map(|h| h.join("Library/LaunchAgents").join(format!("{LABEL}.plist")))
        }

        pub fn is_enabled() -> bool {
            plist_path().map(|p| p.is_file()).unwrap_or(false)
        }

        pub fn set_enabled(on: bool) -> std::io::Result<()> {
            let path = plist_path().ok_or_else(|| std::io::Error::new(std::io::ErrorKind::Other, "无法确定用户目录"))?;
            if on {
                if let Some(dir) = path.parent() {
                    std::fs::create_dir_all(dir)?;
                }
                let exe = std::env::current_exe()?;
                let plist = format!(
                    "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
                     <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
                     <plist version=\"1.0\">\n<dict>\n  <key>Label</key><string>{LABEL}</string>\n  \
                     <key>ProgramArguments</key>\n  <array><string>{}</string><string>run</string><string>--no-browser</string></array>\n  \
                     <key>RunAtLoad</key><true/>\n</dict>\n</plist>\n",
                    exe.display()
                );
                // 只写文件，下次登录时生效：现在 bootstrap 会立刻再启动一份。
                std::fs::write(&path, plist)?;
            } else {
                // 只删文件：bootout 会把由登录项启动的当前进程也一起结束。
                let _ = std::fs::remove_file(&path);
            }
            Ok(())
        }
    }
    #[cfg(target_os = "macos")]
    pub use imp::{is_enabled, set_enabled};

    /// macOS 第一次打开应用时默认开启登录自启（远程访问要求客户端一直在运行），之后以用户的选择为准。
    /// 旧版安装脚本的登录项（同一个 Label）指向 ~/Applications/Looklook，在这里换成应用包，
    /// 并等旧进程放开数据目录的锁。
    #[cfg(target_os = "macos")]
    pub fn default_on(home: Option<&std::path::PathBuf>) {
        let Ok(paths) = crate::paths::Paths::discover(home.cloned()) else { return };
        let marker = paths.home.join("autostart.default");
        if marker.exists() {
            return;
        }
        let _ = std::fs::write(&marker, b"");
        let plist = dirs::home_dir().map(|h| h.join("Library/LaunchAgents/com.looklook.client.plist"));
        let old = plist.and_then(|p| std::fs::read_to_string(p).ok());
        let exe = std::env::current_exe().map(|e| e.display().to_string()).unwrap_or_default();
        if old.as_deref().is_some_and(|o| o.contains(&exe)) {
            return;
        }
        if old.is_some() {
            // 旧版安装脚本的登录项还在运行旧程序：结束它（不是当前进程，路径不同）。
            let uid = unsafe { libc::getuid() };
            let _ = std::process::Command::new("launchctl").arg("bootout").arg(format!("gui/{uid}/com.looklook.client")).status();
        }
        if let Err(e) = set_enabled(true) {
            tracing::warn!(error = %e, "无法开启登录自启");
        }
        let lock = paths.home.join("run").join("lock");
        for _ in 0..30 {
            let free = std::fs::OpenOptions::new().write(true).create(true).truncate(false).open(&lock).is_ok_and(|f| f.try_lock().is_ok());
            if free {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
    }
}
