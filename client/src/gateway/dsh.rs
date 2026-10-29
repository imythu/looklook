//! DSH 的网页界面在会话标签里打开：`/w/{id}/…` → `dsh web` 监听的本机端口（和管理台同源，iframe 里能用）。
//!
//! DSH 按官方文档支持“去掉前缀的反向代理”（首页带 `<base href="./">`，资源、接口、WebSocket 都按相对地址走）。
//! 它的安全检查：`Host` 必须是本机地址，带 `Origin` 时必须和 `Host` 一致，`sec-fetch-site: cross-site` 拒绝；
//! 登录靠启动口令换来的 Cookie（`HttpOnly`、`SameSite=Strict`，绑定 Host）。
//!
//! 这里由网关自己用口令换 Cookie、转发时带上，浏览器拿不到口令也拿不到 DSH 的 Cookie：
//! 能打开管理台的人才能走到这里（本机入口的访问码、远程入口的网关令牌），远程经 WebRTC 时
//! Service Worker 做出来的响应设不了 Cookie，也照样能用。来源检查在这里做完，再把 `Host`、`Origin` 改成本机地址交给 DSH。

use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use axum::extract::{Request, State};
use axum::http::{header, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::{json, Value};

use super::api::DshLogin;
use super::{api, assets, origin_matches, proxy, Access, App};

/// 每个 DSH 会话登录上的网页界面（端口、口令、换到的 Cookie）。Cookie 绑定 `127.0.0.1:端口`，dsh 重启后照样有效；
/// 转发回 401（换了 `DSH_HOME`、过期）或者连不上（dsh 不在了）时丢掉，下次打开重新找。
fn logins() -> &'static Mutex<HashMap<String, DshLogin>> {
    static C: OnceLock<Mutex<HashMap<String, DshLogin>>> = OnceLock::new();
    C.get_or_init(Default::default)
}

fn cached(id: &str) -> Option<DshLogin> {
    logins().lock().unwrap_or_else(|e| e.into_inner()).get(id).cloned()
}

pub(super) fn logged_in(id: &str) -> bool {
    cached(id).is_some()
}

fn forget(id: &str) {
    logins().lock().unwrap_or_else(|e| e.into_inner()).remove(id);
}

fn remember(id: &str, l: &DshLogin) {
    logins().lock().unwrap_or_else(|e| e.into_inner()).insert(id.to_string(), l.clone());
}

// ---------------- 启动进度和诊断记录 ----------------
//
// 打不开时用户只看到“正在启动”，不知道卡在哪：每次启动 / 打开都按步骤记下来（界面上显示进度，
// 出错时可以一键复制诊断信息反馈），同时写进客户端日志。只在内存里，每个 DSH 留最近一次。

/// 步骤：检查安装 → 启动 → dsh 进程 → 端口监听 → 启动口令 → 登录 → 打开浏览器（在浏览器中打开时）。
/// 用已经在运行的 DSH 时启动、进程、监听三步跳过
pub(super) const STEPS: [&str; 7] = ["install", "start", "process", "listen", "token", "login", "open"];
const LOG_LINES: usize = 200;

#[derive(Default)]
struct Progress {
    /// `tab`（在会话标签里打开）/ `external`（在浏览器中打开）
    flow: &'static str,
    started: Option<Instant>,
    started_at: String,
    /// 步骤 → (wait / run / ok / fail / skip, 细节, 从开始到这一步结束的毫秒数)
    steps: HashMap<&'static str, (&'static str, Option<String>, Option<u128>)>,
    log: VecDeque<String>,
    /// 出错时 dsh 终端里最后的输出
    output: Option<String>,
}

fn progress() -> &'static Mutex<HashMap<String, Progress>> {
    static P: OnceLock<Mutex<HashMap<String, Progress>>> = OnceLock::new();
    P.get_or_init(Default::default)
}

fn with_progress<T>(id: &str, f: impl FnOnce(&mut Progress) -> T) -> T {
    let mut m = progress().lock().unwrap_or_else(|e| e.into_inner());
    f(m.entry(id.to_string()).or_default())
}

/// 开始一次新的启动 / 打开，清掉上次的记录
pub(super) fn begin(id: &str, flow: &'static str) {
    with_progress(id, |p| {
        *p = Progress { flow, started: Some(Instant::now()), started_at: crate::util::now_rfc3339(), ..Default::default() };
    });
    note(id, &format!("开始（{flow}），客户端 {}，{} {}", env!("CARGO_PKG_VERSION"), std::env::consts::OS, std::env::consts::ARCH));
}

/// 记一行日志（同时写进客户端日志）
pub(super) fn note(id: &str, msg: &str) {
    tracing::info!(id, "DSH：{msg}");
    with_progress(id, |p| {
        let ms = p.started.map_or(0, |t| t.elapsed().as_millis());
        p.log.push_back(format!("+{:>6}ms {msg}", ms));
        while p.log.len() > LOG_LINES {
            p.log.pop_front();
        }
    });
}

/// 某一步的状态：`run` / `ok` / `fail` / `skip`
pub(super) fn step(id: &str, key: &'static str, state: &'static str, detail: Option<String>) {
    let line = format!("[{key}] {state}{}", detail.as_deref().map(|d| format!("：{d}")).unwrap_or_default());
    if state == "fail" {
        tracing::warn!(id, "DSH：{line}");
    }
    with_progress(id, |p| {
        let ms = (state != "run").then(|| p.started.map_or(0, |t| t.elapsed().as_millis()));
        p.steps.insert(key, (state, detail, ms));
    });
    if state != "fail" {
        note(id, &line);
    } else {
        with_progress(id, |p| {
            let ms = p.started.map_or(0, |t| t.elapsed().as_millis());
            p.log.push_back(format!("+{:>6}ms {line}", ms));
        });
    }
}

/// 出错了：记下 dsh 终端里最后的输出
pub(super) async fn fail_with_output(app: &App, id: &str, key: &'static str, detail: String) {
    step(id, key, "fail", Some(detail));
    let out = app.instances.full_output(id).await.map(|o| tail(&o, 60));
    with_progress(id, |p| p.output = out);
}

fn tail(text: &str, lines: usize) -> String {
    let v: Vec<&str> = text.lines().collect();
    v[v.len().saturating_sub(lines)..].join("\n")
}

/// `GET /instances/{id}/dsh/progress`：这次启动 / 打开的步骤、日志；`output` 是 dsh 终端里现在最后的输出
pub(super) async fn progress_json(app: &App, id: &str) -> Value {
    let live = app.instances.full_output(id).await.map(|o| tail(&o, 60));
    with_progress(id, |p| {
        let steps: Vec<Value> = STEPS
            .iter()
            .map(|k| match p.steps.get(k) {
                Some((state, detail, ms)) => json!({ "key": k, "state": state, "detail": detail, "ms": ms }),
                None => json!({ "key": k, "state": "wait" }),
            })
            .collect();
        json!({
            "flow": p.flow,
            "started_at": p.started_at,
            "elapsed_ms": p.started.map(|t| t.elapsed().as_millis()),
            "steps": steps,
            "log": p.log,
            "failed_output": p.output,
            "output": live,
        })
    })
}

/// 确保能打开 DSH，拿到能用的登录（在会话标签里打开时；已经登录上的直接用）。
pub(super) async fn ensure(app: &App, id: &str) -> Result<Option<DshLogin>, crate::error::LocalError> {
    if let Some(l) = cached(id) {
        // DSH 被关掉、在别的端口上重新运行了：重新找
        if tokio::net::TcpStream::connect(("127.0.0.1", l.port)).await.is_ok() {
            return Ok(Some(l));
        }
        forget(id);
    }
    begin(id, "tab");
    step(id, "install", "skip", None);
    login(app, id).await
}

/// 登录 DSH：本机已经有 DSH 在运行（不管是谁启动的、在哪个端口上）就用它；没有才在这个会话的终端里启动一个，
/// 等它启动完（最多 60 秒）。登录上的记下来。
pub(super) async fn login(app: &App, id: &str) -> Result<Option<DshLogin>, crate::error::LocalError> {
    api::drop_legacy_page(app, id).await;
    let v = app.instances.get(id).await?;
    if let Some(l) = running(app, id).await {
        note(id, &format!("用已在运行的 DSH（端口 {}）", l.port));
        for k in ["start", "process", "listen"] {
            step(id, k, "skip", None);
        }
        step(id, "token", "ok", Some("由 DSH 进程给出".into()));
        step(id, "login", "ok", None);
        if v.web_port != Some(l.port) {
            // 以后由看看启动时也用这个端口
            app.instances.set_web_port(id, l.port)?;
        }
        remember(id, &l);
        return Ok(Some(l));
    }
    let port = start(app, id, &v).await?.or(v.web_port).ok_or_else(crate::error::LocalError::not_found)?;
    let login = api::dsh_login(app, id, port).await;
    if let Some(l) = &login {
        remember(id, l);
    }
    Ok(login)
}

/// 插件刚装上时最多等正在运行的 DSH 这么久（DSH 监视补丁文件，实测两三秒内加载）
const PLUGIN_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

/// 装好插件，再找它报上来的、正在运行的 DSH，能登录上的第一个（这个会话的终端里的那个优先，其次最近启动的）。
pub(super) async fn running(app: &App, id: &str) -> Option<DshLogin> {
    let installed = match tokio::task::spawn_blocking(|| crate::agents::ensure_dsh_web_plugin(true)).await {
        Ok(Ok(changed)) => changed,
        Ok(Err(e)) => {
            note(id, &format!("没能装上 DSH 插件：{e}"));
            false
        }
        Err(_) => false,
    };
    let mut found = tokio::task::spawn_blocking(crate::agents::dsh_web_running).await.unwrap_or_default();
    if installed && found.is_empty() {
        // 插件刚装上 / 更新：已经在运行的 DSH 要过一两秒才加载它、报上地址，等一会儿，免得再启动一个
        note(id, "已安装 DSH 插件，等正在运行的 DSH 加载");
        let deadline = tokio::time::Instant::now() + PLUGIN_WAIT;
        while found.is_empty() && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            found = tokio::task::spawn_blocking(crate::agents::dsh_web_running).await.unwrap_or_default();
        }
    }
    let mut ranked = Vec::with_capacity(found.len());
    for w in found {
        ranked.push((!app.instances.owns_pid(id, w.pid).await, w));
    }
    // 稳定排序：同一类里保持最近启动的在前
    ranked.sort_by_key(|(theirs, _)| *theirs);
    for (_, w) in ranked {
        match api::dsh_exchange(w.port, &w.token).await {
            Ok(cookie) => return Some(DshLogin { port: w.port, token: w.token, cookie }),
            Err(e) => note(id, &format!("DSH（pid {}，端口 {}）登录不上：{e}", w.pid, w.port)),
        }
    }
    None
}

/// 刚启动的会话里 shell 还没来得及运行 dsh：这段时间里前台是 shell 不算 dsh 退出了
const SHELL_GRACE_MS: i64 = 10_000;

/// 启动这一步：没在运行、任务退出了，或者终端还在但 dsh 已经退出（前台只剩 shell，例如端口被占用没起来）就重新运行。
/// 端口被别的程序占着时换一个空闲的，返回换成的端口（没换是 None）。
pub(super) async fn start(app: &App, id: &str, v: &crate::instances::View) -> Result<Option<u16>, crate::error::LocalError> {
    let alive = v.running && v.task_alive == Some(true);
    let mut exited = false;
    if alive {
        let young = v.started_at.as_deref().and_then(crate::util::parse_time_ms).is_some_and(|t| crate::util::system_ms() - t < SHELL_GRACE_MS);
        let pane = app.instances.pane(id).await;
        let port = v.web_port.unwrap_or(0);
        match pane {
            Some(p) if crate::chat::is_shell(&p.command) && !young => {
                exited = true;
                note(id, &format!("终端还在，但 dsh 已经退出（前台是 {}），重新运行", p.command));
            }
            // 前台不是 shell，但端口被别的程序占着（终端里跑的是别的，或者 dsh 没监听上）：换个端口重新运行。
            // 用这次输出里的口令能登录上就肯定是它（dsh 把网页服务放到脱离终端的进程里时 lsof 会认错）
            Some(p) => match foreign_holder(app, id, port).await {
                Some(who) if !api::dsh_answers(app, id, port).await => {
                    exited = true;
                    note(id, &format!("前台是 {}，但端口 {port} 是 {who} 在监听，重新运行", p.command));
                }
                _ => {
                    step(id, "start", "ok", Some(format!("已在运行，端口 {port}")));
                    return Ok(None);
                }
            },
            None => {
                step(id, "start", "ok", Some(format!("已在运行，端口 {port}")));
                return Ok(None);
            }
        }
    }
    forget(id);
    let mut moved = None;
    if let Some(port) = v.web_port.filter(|p| !port_free(*p)) {
        let new = crate::instances::Instances::pick_web_port()?;
        let who = port_holder(port).await.map_or_else(|| "不知道是哪个程序".into(), |h| h.who);
        note(id, &format!("端口 {port} 被占用（{who}），换成 {new}"));
        app.instances.set_web_port(id, new)?;
        moved = Some(new);
    }
    let port = moved.or(v.web_port).unwrap_or(0);
    step(id, "start", "run", Some(format!("running={} task_alive={:?} exited={exited}", v.running, v.task_alive)));
    let r = if exited { app.instances.restart(id).await } else { app.instances.start(id).await };
    match r {
        Ok(_) => {
            step(id, "start", "ok", Some(format!("已启动，端口 {port}")));
            Ok(moved)
        }
        Err(e) => {
            fail_with_output(app, id, "start", format!("{} {:?}", e.code, e.params)).await;
            Err(e)
        }
    }
}

fn port_free(port: u16) -> bool {
    std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port)).is_ok()
}

pub(super) struct Holder {
    pids: Vec<u32>,
    /// 写进诊断信息的：`node（pid 123）`
    pub who: String,
}

/// 谁在监听这个端口（`lsof`）。没有 lsof（Windows、精简的 Linux）或者没人监听时是 None
pub(super) async fn port_holder(port: u16) -> Option<Holder> {
    if cfg!(windows) {
        return None;
    }
    let out = tokio::process::Command::new("lsof").args(["-nP", &format!("-iTCP:{port}"), "-sTCP:LISTEN", "-Fpc"]).output().await.ok()?;
    parse_lsof(&String::from_utf8_lossy(&out.stdout))
}

/// `lsof -F pc` 的输出：每个进程一行 `p<pid>`，后面跟一行 `c<程序名>`
fn parse_lsof(text: &str) -> Option<Holder> {
    let pids: Vec<u32> = text.lines().filter_map(|l| l.strip_prefix('p')?.parse().ok()).collect();
    let cmd = text.lines().find_map(|l| l.strip_prefix('c')).unwrap_or("?");
    let who = format!("{cmd}（pid {}）", pids.iter().map(u32::to_string).collect::<Vec<_>>().join(", "));
    (!pids.is_empty()).then_some(Holder { pids, who })
}

/// 端口被这个终端以外的程序占着时返回是谁。查不到（没有 lsof、没人监听）当作不是别人
pub(super) async fn foreign_holder(app: &App, id: &str, port: u16) -> Option<String> {
    let h = port_holder(port).await?;
    for pid in &h.pids {
        if app.instances.owns_pid(id, *pid).await {
            return None;
        }
    }
    Some(h.who)
}

/// `/w/{id}/…`
pub async fn web(State(app): State<App>, mut req: Request) -> Response {
    let access = req.extensions().get::<Access>().cloned().unwrap_or(Access::Local);
    let path = req.uri().path().to_string();
    let rest = path.strip_prefix("/w/").unwrap_or("");
    let (id, sub) = rest.split_once('/').map_or((rest, ""), |(id, sub)| (id, sub));
    let id = id.to_string();
    // 写请求、WebSocket 都会带 Origin：必须是管理台自己（DSH 那边只会看到改过的本机地址）
    if req.headers().contains_key(header::ORIGIN) && !origin_matches(req.headers(), &access) {
        return (StatusCode::FORBIDDEN, "origin").into_response();
    }
    let web_port = match app.instances.get(&id).await {
        Ok(v) => match v.web_port {
            Some(p) => p,
            None => return assets::message_page(StatusCode::NOT_FOUND, "not_running"),
        },
        Err(_) => return assets::message_page(StatusCode::NOT_FOUND, "not_running"),
    };
    let index = sub.is_empty() && req.method() == Method::GET;
    // 首页才会启动 DSH；别的请求用已有的登录，没有时找一下正在运行的（DSH 重启、换了端口后它的页面自己重连，
    // 这样就能接上），找不到就让 DSH 回 401
    let login = if index {
        match ensure(&app, &id).await {
            Ok(Some(l)) => Some(l),
            Ok(None) => return assets::message_page(StatusCode::SERVICE_UNAVAILABLE, "upstream_down"),
            Err(e) => return e.into_response(),
        }
    } else {
        match cached(&id) {
            Some(l) => Some(l),
            None => running(&app, &id).await.inspect(|l| remember(&id, l)),
        }
    };
    let port = login.as_ref().map_or(web_port, |l| l.port);
    let cookie = login.map(|l| l.cookie);
    let local = format!("127.0.0.1:{port}");
    let query = req.uri().query().map(|q| format!("?{q}")).unwrap_or_default();
    let Ok(uri) = format!("/{sub}{query}").parse() else { return StatusCode::BAD_REQUEST.into_response() };
    *req.uri_mut() = uri;
    let h = req.headers_mut();
    // 浏览器带来的 Cookie 是管理台的，不交给 DSH；只带网关换到的那个
    h.remove(header::COOKIE);
    if let Some(c) = cookie.as_deref().and_then(|c| HeaderValue::from_str(c).ok()) {
        h.insert(header::COOKIE, c);
    }
    if h.contains_key(header::ORIGIN) {
        if let Ok(v) = HeaderValue::from_str(&format!("http://{local}")) {
            h.insert(header::ORIGIN, v);
        }
    }
    h.remove("sec-fetch-site");
    h.remove(header::REFERER);
    let base = format!("/w/{id}");
    let opts = proxy::Opts {
        auth: None,
        host: Some(local.clone()),
        inject: index.then_some(proxy::WEB_TAGS),
        rewrite_location: Some((vec![format!("http://{local}"), format!("http://localhost:{port}")], base)),
        terminal: false,
    };
    let mut r = proxy::forward(&app.http, req, port, opts).await;
    if r.status() == StatusCode::BAD_GATEWAY || r.status() == StatusCode::GATEWAY_TIMEOUT {
        // dsh 不在了（被关掉、换了端口）：下次打开重新找或者重新启动
        forget(&id);
    }
    if r.status() == StatusCode::UNAUTHORIZED {
        // Cookie 不认了（换了 DSH_HOME、过期）：首页就重新载入一次，下次重新登录
        forget(&id);
        if index {
            return axum::response::Redirect::to(&format!("/w/{id}/{query}")).into_response();
        }
    }
    // DSH 不会再发 Cookie（口令只在首页交换时用）；万一发了也不交给浏览器
    r.headers_mut().remove(header::SET_COOKIE);
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 下面两个端到端测试都改进程的 `DSH_HOME`：一个一个跑
    static DSH_ENV: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    /// 端到端，要真的 dsh：`LOOKLOOK_TEST_DSH=/path/to/dsh cargo test -- --ignored reuses_running_dsh`。
    /// 先运行一个“用户自己的” DSH（看看的插件还没装），再经网关打开 `/w/{id}/`：装上插件、DSH 热加载后用上它，
    /// 不另起一个；用户换个端口重新运行后，再打开也能接上。
    #[tokio::test]
    #[ignore]
    async fn reuses_running_dsh() {
        use axum::body::Body;
        use tower::ServiceExt;
        let Ok(bin) = std::env::var("LOOKLOOK_TEST_DSH") else { return };
        let _env = DSH_ENV.lock().await;
        let tmp = tempfile::tempdir().unwrap();
        let dsh_home = tmp.path().join("dsh");
        std::env::set_var("DSH_HOME", &dsh_home);
        let user = |port: u16| {
            tokio::process::Command::new(&bin)
                .args(["--profile", "web", "--no-open", "--port", &port.to_string()])
                .env("DSH_HOME", &dsh_home)
                .current_dir(tmp.path())
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .kill_on_drop(true)
                .spawn()
                .unwrap()
        };
        async fn listening(port: u16, on: bool) {
            for _ in 0..240 {
                if tokio::net::TcpStream::connect(("127.0.0.1", port)).await.is_ok() == on {
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            }
            panic!("端口 {port} 等不到 {}", if on { "监听" } else { "关闭" });
        }
        let mut first = user(39701);
        listening(39701, true).await;
        assert!(crate::agents::dsh_web_running().is_empty(), "插件还没装");

        let app = crate::ssh_transfer::e2e::app(&tmp.path().join("ll"), "http://127.0.0.1:9", "dev1", ed25519_dalek::SigningKey::from_bytes(&[7; 32]));
        let input = crate::instances::Input {
            name: Some("DSH".into()),
            workdir: Some(tmp.path().display().to_string()),
            launch: Some("dsh".into()),
            web_port: Some(39709),
            ..Default::default()
        };
        let id = app.instances.create(input).await.unwrap().row.id;
        let open = |app: App| {
            let req = Request::builder().uri(format!("/w/{id}/")).header(header::HOST, "127.0.0.1:1").body(Body::empty()).unwrap();
            async move {
                let r = super::super::router(app).oneshot(req).await.unwrap();
                let status = r.status();
                let body = axum::body::to_bytes(r.into_body(), 1 << 22).await.unwrap();
                (status, String::from_utf8_lossy(&body).into_owned())
            }
        };
        let (status, body) = open(app.clone()).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body.contains("./assets/index-") && body.contains("/static/web.js"), "是 DSH 的首页，注入了脚本：{body}");
        let v = app.instances.get(&id).await.unwrap();
        assert_eq!(v.web_port, Some(39701), "用的是用户的 DSH");
        assert!(!v.running, "没有另起一个");

        // 用户换个端口重新运行：缓存的登录连不上了，重新找
        first.kill().await.unwrap();
        listening(39701, false).await;
        let _second = user(39702);
        listening(39702, true).await;
        for _ in 0..40 {
            if crate::agents::dsh_web_running().iter().any(|w| w.port == 39702) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }
        let (status, body) = open(app.clone()).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(app.instances.get(&id).await.unwrap().web_port, Some(39702));
        assert!(!app.instances.get(&id).await.unwrap().running);
    }

    /// 端到端，要真的 dsh 在登录 shell 的 PATH 里、有 tmux：`LOOKLOOK_TEST_DSH=1 cargo test -- --ignored starts_own_dsh`。
    /// 本机没有 DSH 在运行：在这个会话的终端里启动一个，用插件给的口令登录上。
    #[tokio::test]
    #[ignore]
    async fn starts_own_dsh() {
        use axum::body::Body;
        use tower::ServiceExt;
        if std::env::var_os("LOOKLOOK_TEST_DSH").is_none() {
            return;
        }
        let _env = DSH_ENV.lock().await;
        let tmp = tempfile::tempdir().unwrap();
        std::env::set_var("DSH_HOME", tmp.path().join("dsh"));
        for d in ["logs", "run"] {
            std::fs::create_dir_all(tmp.path().join("ll").join(d)).unwrap();
        }
        let app = crate::ssh_transfer::e2e::app(&tmp.path().join("ll"), "http://127.0.0.1:9", "dev1", ed25519_dalek::SigningKey::from_bytes(&[7; 32]));
        assert!(matches!(app.instances.backend, crate::instances::Backend::Tmux(_)), "要有 tmux");
        let input = crate::instances::Input {
            name: Some("DSH".into()),
            workdir: Some(tmp.path().display().to_string()),
            launch: Some("dsh".into()),
            web_port: Some(39711),
            ..Default::default()
        };
        let id = app.instances.create(input).await.unwrap().row.id;
        let req = Request::builder().uri(format!("/w/{id}/")).header(header::HOST, "127.0.0.1:1").body(Body::empty()).unwrap();
        let r = super::super::router(app.clone()).oneshot(req).await.unwrap();
        let status = r.status();
        let body = String::from_utf8_lossy(&axum::body::to_bytes(r.into_body(), 1 << 22).await.unwrap()).into_owned();
        let progress = progress_json(&app, &id).await;
        let _ = app.instances.stop(&id).await;
        assert_eq!(status, StatusCode::OK, "{body}\n{progress:#}");
        assert!(body.contains("./assets/index-"), "{body}");
        let log = progress["log"].to_string();
        assert!(log.contains("[token] ok"), "{progress:#}");
        assert!(crate::agents::dsh_web_running().iter().all(|w| w.port == 39711));
    }

    #[test]
    fn lsof_output() {
        let h = parse_lsof("p123\ncnode\nf21\np456\ncnode\nf22\n").unwrap();
        assert_eq!((h.pids, h.who.as_str()), (vec![123, 456], "node（pid 123, 456）"));
        assert!(parse_lsof("").is_none());
    }
}
