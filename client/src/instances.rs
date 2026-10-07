//! 终端实例：每个实例一个 ttyd（只监听 127.0.0.1，随机口令，`-b /i/{id}`），
//! 浏览器只经本机网关的 `/i/{id}/` 访问。
//!
//! 持续运行靠 tmux（专用 socket `-L looklook`，不影响用户自己的 tmux）：
//! ttyd 只是浏览器与 tmux 会话之间的桥，关掉网页、ttyd 重启、客户端重启都不影响会话里的任务。
//! 优先用随包的 looklook-mux（Linux/macOS 是静态编译的 tmux，Windows 是兼容 tmux 命令的 psmux），其次是系统里的 tmux；
//! 都没有（或随包程序无法运行）时退回“直接运行”：关掉网页即结束程序。
//!
//! 账户不可用（未登录、到期、离线过久）时只停 ttyd，不结束 tmux 里的任务；恢复后自动接回。

use std::collections::{HashMap, HashSet};
use std::net::{Ipv4Addr, SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::process::{Child, Command};

use crate::account::Account;
use crate::error::LocalError;
use crate::paths::Paths;
use crate::settings::Settings;
use crate::store::{InstanceRow, Store};
use crate::util::{now_rfc3339, random_id, random_secret};

pub const PORT_RANGE: (u16, u16) = (41000, 41999);
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const TMUX_SOCKET: &str = "looklook";
const LAUNCHES: &[&str] = &["shell", "codex", "claude", "opencode", "dsh", "custom"];

#[derive(Debug, Clone)]
pub enum Backend {
    Tmux(PathBuf),
    Direct,
}

impl Backend {
    pub fn detect(paths: &Paths) -> Self {
        if std::env::var_os("LOOKLOOK_NO_TMUX").is_some() {
            return Backend::Direct;
        }
        if let Some(mux) = paths.mux.as_ref().filter(|m| mux_runs(m)) {
            return Backend::Tmux(mux.clone());
        }
        if cfg!(windows) {
            return Backend::Direct;
        }
        let mut dirs: Vec<PathBuf> = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
        dirs.extend(["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin"].map(PathBuf::from));
        dirs.into_iter().map(|d| d.join("tmux")).find(|p| p.is_file()).map(Backend::Tmux).unwrap_or(Backend::Direct)
    }
    pub fn name(&self) -> &'static str {
        match self {
            Backend::Tmux(_) => "tmux",
            Backend::Direct => "direct",
        }
    }
}

/// 随包程序能不能跑起来（架构不符、缺文件、被安全软件拦截都会失败）：不能就退回直接运行，而不是让每个终端都启动失败。
fn mux_runs(path: &Path) -> bool {
    let mut cmd = std::process::Command::new(path);
    cmd.arg("-V").stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let ok = cmd.output().map(|o| o.status.success()).unwrap_or(false);
    if !ok {
        tracing::warn!(mux = %path.display(), "随包的会话保持程序无法运行，改用系统里的 tmux（没有则直接运行）");
    }
    ok
}

/// 被 ttyd 启动的是会话保持程序（它自己设置工作目录，不用 ttyd 的 -w）。
fn is_mux_program(program: &str) -> bool {
    let stem = Path::new(program).file_stem().and_then(|s| s.to_str()).unwrap_or("");
    matches!(stem, "tmux" | "mux" | "looklook-mux" | "psmux")
}

struct Proc {
    child: Child,
    port: u16,
    auth: String,
    started: Instant,
}

#[derive(Default)]
struct Health {
    /// 连续“刚启动就退出”的次数，超过上限不再自动重启
    quick_exits: u32,
    error: Option<String>,
}

pub struct Instances {
    store: Arc<Store>,
    paths: Paths,
    pub backend: Backend,
    shell: String,
    procs: Mutex<HashMap<String, Proc>>,
    health: Mutex<HashMap<String, Health>>,
    op: tokio::sync::Mutex<()>,
    /// 终端新建/删除时加一：账户据此去抖上报终端列表（多设备路由表，服务端 docs/05 §5）
    changed: tokio::sync::watch::Sender<u64>,
}

#[derive(Debug, Serialize)]
pub struct View {
    #[serde(flatten)]
    pub row: InstanceRow,
    /// ttyd 正在运行，可以打开
    pub running: bool,
    /// tmux 会话（里面的任务）还在；直接运行模式为 null
    pub task_alive: Option<bool>,
    pub started_at: Option<String>,
    pub last_activity_at: Option<String>,
    pub error: Option<String>,
    pub url: String,
    /// 以全部权限启动（仅 Codex / Claude Code / OpenCode）
    pub full_access: bool,
    /// 用户已确认以 root 身份带全部权限启动
    pub root_confirmed: bool,
    /// DSH：网页界面监听的本机端口
    pub web_port: Option<u16>,
}

/// 启动选项（不在 SQLite 的 instances 表里，单独存在 kv：`instance_opts.{id}`）。
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Opts {
    #[serde(default)]
    pub full_access: bool,
    #[serde(default)]
    pub root_confirmed: bool,
    /// DSH 网页界面的端口（新建时选定，之后不变）
    #[serde(default)]
    pub web_port: Option<u16>,
}

/// DSH 网页界面的端口从这里开始找（`dsh web` 自己默认 3080）；避开终端用的 41000–41999。
const WEB_PORT_START: u16 = 3080;

fn opts_key(id: &str) -> String {
    format!("instance_opts.{id}")
}

#[derive(Debug, Default, Deserialize)]
pub struct Input {
    pub name: Option<String>,
    pub workdir: Option<String>,
    pub launch: Option<String>,
    pub command: Option<String>,
    pub shell: Option<String>,
    pub auto_start: Option<bool>,
    pub full_access: Option<bool>,
    pub root_confirmed: Option<bool>,
    /// DSH 网页界面的端口（只在新建时由网关填写，不接受请求里的值）
    #[serde(skip)]
    pub web_port: Option<u16>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Instances {
    pub fn new(store: Arc<Store>, paths: Paths) -> Arc<Self> {
        if cfg!(windows) {
            let _ = std::fs::create_dir_all(paths.home.join("mux"));
        }
        let backend = Backend::detect(&paths);
        tracing::info!(backend = backend.name(), ttyd = ?paths.ttyd, mux = ?paths.mux, "终端后端");
        Arc::new(Self {
            store,
            paths,
            backend,
            shell: crate::shells::login_shell(),
            procs: Mutex::new(HashMap::new()),
            health: Mutex::new(HashMap::new()),
            op: tokio::sync::Mutex::new(()),
            changed: tokio::sync::watch::channel(0).0,
        })
    }

    /// 订阅“终端列表变了”（新建、删除）。
    pub fn subscribe(&self) -> tokio::sync::watch::Receiver<u64> {
        self.changed.subscribe()
    }

    fn bump(&self) {
        self.changed.send_modify(|v| *v += 1);
    }

    pub fn shell(&self) -> &str {
        &self.shell
    }

    pub fn session_name(id: &str) -> String {
        format!("ll_{id}")
    }

    // ---------------- 查询 ----------------

    pub async fn list(&self) -> Result<Vec<View>, LocalError> {
        let rows = self.store.instances()?;
        let sessions = self.tmux_sessions().await;
        Ok(rows.into_iter().map(|r| self.view(r, sessions.as_ref())).collect())
    }

    pub async fn get(&self, id: &str) -> Result<View, LocalError> {
        let row = self.store.instance(id)?.ok_or_else(LocalError::not_found)?;
        let sessions = self.tmux_sessions().await;
        Ok(self.view(row, sessions.as_ref()))
    }

    fn view(&self, row: InstanceRow, sessions: Option<&HashMap<String, (i64, i64)>>) -> View {
        let running = lock(&self.procs).contains_key(&row.id);
        let sess = sessions.and_then(|m| m.get(&Self::session_name(&row.id)));
        let ts = |s: i64| crate::util::fmt_ms(s * 1000);
        let opts = self.opts(&row.id);
        View {
            full_access: opts.full_access,
            root_confirmed: opts.root_confirmed,
            web_port: opts.web_port,
            running,
            task_alive: sessions.map(|_| sess.is_some()),
            started_at: sess.map(|(c, _)| ts(*c)),
            last_activity_at: sess.map(|(_, a)| ts(*a)),
            error: lock(&self.health).get(&row.id).and_then(|h| h.error.clone()),
            url: format!("/i/{}/", row.id),
            row,
        }
    }

    fn opts(&self, id: &str) -> Opts {
        self.store.get::<Opts>(&opts_key(id)).ok().flatten().unwrap_or_default()
    }

    /// 网关转发用：运行中的 ttyd 端口与 Basic 认证头。
    pub fn upstream(&self, id: &str) -> Option<(u16, String)> {
        lock(&self.procs).get(id).map(|p| (p.port, p.auth.clone()))
    }

    pub fn is_reserved_port(port: u16) -> bool {
        (PORT_RANGE.0..=PORT_RANGE.1).contains(&port)
    }

    /// 这台电脑上的 DSH（只有一个：一个 `dsh web` 进程管理所有项目文件夹）。
    pub fn dsh(&self) -> Result<Option<InstanceRow>, LocalError> {
        Ok(self.store.instances()?.into_iter().find(|r| r.launch == "dsh"))
    }

    /// 给 DSH 网页界面挑一个现在没被占用的端口：3080 起往后找，都不行再让系统分配。
    pub fn pick_web_port() -> Result<u16, LocalError> {
        let bind = |p: u16| TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, p))).ok();
        if let Some(p) = (WEB_PORT_START..WEB_PORT_START + 100).find(|p| bind(*p).is_some()) {
            return Ok(p);
        }
        bind(0).and_then(|l| l.local_addr().ok()).map(|a| a.port()).ok_or_else(|| LocalError::new("NO_FREE_PORT"))
    }

    // ---------------- 增删改 ----------------

    fn validate(&self, row: &mut InstanceRow) -> Result<(), LocalError> {
        row.name = row.name.trim().chars().filter(|c| !c.is_control()).take(40).collect();
        if row.name.is_empty() {
            return Err(LocalError::invalid("name", "required"));
        }
        if !LAUNCHES.contains(&row.launch.as_str()) {
            return Err(LocalError::invalid("launch", "invalid"));
        }
        row.command = row.command.trim().to_string();
        if row.launch == "custom" && row.command.is_empty() {
            return Err(LocalError::invalid("command", "required"));
        }
        if row.command.len() > 2000 || row.command.contains('\n') {
            return Err(LocalError::invalid("command", "too_long"));
        }
        row.shell = crate::shells::validate(&row.shell)?;
        row.workdir = expand_home(row.workdir.trim());
        // 目录不存在时启动会递归创建；这里只拒绝空值、相对路径和已存在的非目录。
        let wd = Path::new(&row.workdir);
        if row.workdir.is_empty() || !wd.is_absolute() || (wd.exists() && !wd.is_dir()) {
            return Err(LocalError::new("WORKDIR_NOT_FOUND").with("path", &row.workdir));
        }
        Ok(())
    }

    /// 全部权限选项：只对 Codex / Claude Code / OpenCode 有意义（DSH 的权限在它自己的网页里设置）；
    /// root 运行时必须确认。DSH 必须有网页端口。
    fn validate_opts(row: &InstanceRow, opts: &mut Opts) -> Result<(), LocalError> {
        if row.launch == "dsh" {
            if opts.web_port.is_none() {
                return Err(LocalError::invalid("web_port", "required"));
            }
        } else {
            opts.web_port = None;
        }
        if !matches!(row.launch.as_str(), "codex" | "claude" | "opencode") {
            opts.full_access = false;
            opts.root_confirmed = false;
            return Ok(());
        }
        if !opts.full_access {
            opts.root_confirmed = false;
        } else if crate::platform::is_root() && !opts.root_confirmed {
            return Err(LocalError::invalid("root_confirmed", "required"));
        }
        Ok(())
    }

    pub async fn create(&self, input: Input) -> Result<View, LocalError> {
        let _g = self.op.lock().await;
        let settings = Settings::load(&self.store);
        let launch = input.launch.unwrap_or_else(|| "shell".into());
        let now = now_rfc3339();
        let mut row = InstanceRow {
            id: loop {
                let id = random_id(8);
                if self.store.instance(&id)?.is_none() {
                    break id;
                }
            },
            name: input.name.unwrap_or_default(),
            workdir: input.workdir.filter(|w| !w.trim().is_empty()).unwrap_or(settings.default_workdir),
            launch,
            command: input.command.unwrap_or_default(),
            shell: input.shell.unwrap_or_default(),
            auto_start: input.auto_start.unwrap_or(false),
            want_running: false,
            port: self.free_port(None)?,
            created_at: now.clone(),
            updated_at: now,
        };
        if row.name.trim().is_empty() {
            row.name = default_name(&row.launch, self.store.instances()?.len() + 1);
        }
        self.validate(&mut row)?;
        if row.launch == "dsh" && self.dsh()?.is_some() {
            return Err(LocalError::new("DSH_EXISTS"));
        }
        let mut opts = Opts {
            full_access: input.full_access.unwrap_or(false),
            root_confirmed: input.root_confirmed.unwrap_or(false),
            web_port: input.web_port,
        };
        Self::validate_opts(&row, &mut opts)?;
        self.store.insert_instance(&row)?;
        self.store.set(&opts_key(&row.id), &opts)?;
        self.bump();
        Ok(self.view(row, None))
    }

    pub async fn update(&self, id: &str, input: Input) -> Result<View, LocalError> {
        let _g = self.op.lock().await;
        let mut row = self.store.instance(id)?.ok_or_else(LocalError::not_found)?;
        if let Some(v) = input.name {
            row.name = v;
        }
        if let Some(v) = input.workdir {
            row.workdir = v;
        }
        if let Some(v) = input.launch {
            // DSH 带着自己的端口和网页映射，不能和别的启动方式互相改
            if (v == "dsh") != (row.launch == "dsh") {
                return Err(LocalError::invalid("launch", "invalid"));
            }
            row.launch = v;
        }
        if let Some(v) = input.command {
            row.command = v;
        }
        if let Some(v) = input.shell {
            row.shell = v;
        }
        if let Some(v) = input.auto_start {
            row.auto_start = v;
        }
        self.validate(&mut row)?;
        let mut opts = self.opts(id);
        if let Some(v) = input.full_access {
            opts.full_access = v;
        }
        if let Some(v) = input.root_confirmed {
            opts.root_confirmed = v;
        }
        Self::validate_opts(&row, &mut opts)?;
        row.updated_at = now_rfc3339();
        self.store.update_instance(&row)?;
        self.store.set(&opts_key(id), &opts)?;
        drop(_g);
        self.get(id).await
    }

    /// 删除已停止的终端。还在运行（终端服务或后台任务仍在）时拒绝：先停止，免得误删正在干活的任务。
    pub async fn delete(&self, id: &str) -> Result<(), LocalError> {
        let v = self.get(id).await?;
        if v.running || v.task_alive == Some(true) {
            return Err(LocalError::new("INSTANCE_RUNNING"));
        }
        self.stop(id).await?;
        let _g = self.op.lock().await;
        self.store.delete_instance(id)?;
        self.bump();
        let _ = self.store.remove(&opts_key(id));
        lock(&self.health).remove(id);
        let _ = std::fs::remove_file(self.paths.instance_log(id));
        // 附件目录（粘贴的截图、“发给 AI”的文件）随终端一起删掉
        let _ = std::fs::remove_dir_all(self.paths.attach_dir(id));
        Ok(())
    }

    // ---------------- 启动 / 停止 ----------------

    /// 启动：tmux 模式先建会话（启动命令立刻开始运行），再起 ttyd。
    pub async fn start(&self, id: &str) -> Result<View, LocalError> {
        // 用户手动启动：清掉之前的失败记录。
        lock(&self.health).remove(id);
        self.resume(id, true).await?;
        self.get(id).await
    }

    /// 启动或接回（看护任务使用，保留失败计数）。`create_session`：没有 tmux 会话时立即新建并运行启动命令；
    /// 否则只起 ttyd，会话在下次打开终端时才创建——用户已经退出的命令不会被擅自重新运行。
    async fn resume(&self, id: &str, create_session: bool) -> Result<(), LocalError> {
        let _g = self.op.lock().await;
        let mut row = self.store.instance(id)?.ok_or_else(LocalError::not_found)?;
        // 工作目录不存在就递归创建（新建终端时可以直接填一个还没有的路径）。
        if let Err(e) = std::fs::create_dir_all(&row.workdir) {
            tracing::warn!(path = %row.workdir, error = %e, "无法创建工作目录");
            let code = if e.kind() == std::io::ErrorKind::PermissionDenied { "WORKDIR_DENIED" } else { "WORKDIR_NOT_FOUND" };
            return Err(LocalError::new(code).with("path", &row.workdir));
        }
        {
            // Windows 的 psmux 同样支持 `new-session -A`（有则接入、无则新建），两边走同一条路。
            if matches!(self.backend, Backend::Tmux(_)) && create_session {
                self.ensure_session(&row).await?;
            }
            if !lock(&self.procs).contains_key(id) {
                self.spawn_ttyd(&mut row).await?;
            }
            self.store.set_want_running(id, true)?;
        }
        Ok(())
    }

    /// 停止：结束 ttyd，并结束 tmux 会话（其中的任务会被关闭，界面上需要二次确认）。
    pub async fn stop(&self, id: &str) -> Result<(), LocalError> {
        let _g = self.op.lock().await;
        self.store.set_want_running(id, false)?;
        self.kill_ttyd(id).await;
        if let Backend::Tmux(_) = self.backend {
            let _ = self.tmux(&["kill-session", "-t", &exact(&Self::session_name(id))]).await;
        }
        let _ = std::fs::remove_file(self.paths.instance_pid(id));
        Ok(())
    }

    pub async fn restart(&self, id: &str) -> Result<View, LocalError> {
        self.stop(id).await?;
        self.start(id).await
    }

    async fn kill_ttyd(&self, id: &str) {
        let p = lock(&self.procs).remove(id);
        if let Some(mut p) = p {
            let _ = p.child.start_kill();
            let _ = tokio::time::timeout(Duration::from_secs(3), p.child.wait()).await;
        }
    }

    fn free_port(&self, current: Option<u16>) -> Result<u16, LocalError> {
        let used: HashSet<u16> = self.store.used_ports()?.into_iter().collect();
        let bindable = |p: u16| TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, p))).is_ok();
        if let Some(p) = current.filter(|p| bindable(*p)) {
            return Ok(p);
        }
        (PORT_RANGE.0..=PORT_RANGE.1)
            .find(|p| !used.contains(p) && bindable(*p))
            .ok_or_else(|| LocalError::new("NO_FREE_PORT"))
    }

    async fn spawn_ttyd(&self, row: &mut InstanceRow) -> Result<(), LocalError> {
        let ttyd = self.paths.ttyd.clone().ok_or_else(|| LocalError::new("TTYD_MISSING"))?;
        let port = self.free_port(Some(row.port))?;
        if port != row.port {
            row.port = port;
            row.updated_at = now_rfc3339();
            self.store.update_instance(row)?;
        }
        let secret = random_secret();
        let settings = Settings::load(&self.store);
        let args = ttyd_args(row, &secret, &settings, &self.command_for(row)?);
        let log = open_log(&self.paths.instance_log(&row.id))?;
        // ttyd 平时几乎不输出，先写一行启动记录，运行记录才不会是空的。
        {
            use std::io::Write;
            let mut f = &log;
            let _ = writeln!(f, "[{}] ttyd start port={port} launch={} workdir={}", now_rfc3339(), row.launch, row.workdir);
        }
        let mut cmd = Command::new(&ttyd);
        cmd.args(&args).stdin(Stdio::null()).stdout(log.try_clone()?).stderr(log).kill_on_drop(true);
        self.apply_env(&mut cmd);
        #[cfg(target_os = "linux")]
        unsafe {
            // 客户端被强制结束时 ttyd 跟着退出（tmux 会话不受影响）。
            cmd.pre_exec(|| {
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
                Ok(())
            });
        }
        #[cfg(windows)]
        {
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        let child = cmd.spawn().map_err(|e| LocalError::new("START_FAILED").with("detail", e.to_string()))?;
        if let Some(pid) = child.id() {
            let _ = std::fs::write(self.paths.instance_pid(&row.id), pid.to_string());
        }
        let auth = format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(format!("looklook:{secret}")));
        lock(&self.procs).insert(row.id.clone(), Proc { child, port, auth, started: Instant::now() });
        // 等 ttyd 开始监听，界面随后打开终端时不会碰到连接失败。
        for _ in 0..40 {
            if tokio::net::TcpStream::connect((Ipv4Addr::LOCALHOST, port)).await.is_ok() {
                return Ok(());
            }
            let exited = lock(&self.procs).get_mut(&row.id).map(|p| matches!(p.child.try_wait(), Ok(Some(_)))).unwrap_or(true);
            if exited {
                lock(&self.procs).remove(&row.id);
                return Err(LocalError::new("START_FAILED").with("detail", log_tail(&self.paths.instance_log(&row.id))));
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        Ok(())
    }

    /// 实例的程序与参数（传给 ttyd）。tmux 模式下是“接入会话，没有就新建”（`new-session -A`），
    /// 关掉网页再打开时接回同一个会话。
    fn command_for(&self, row: &InstanceRow) -> Result<Vec<String>, LocalError> {
        let (program, typed) = self.program_for(row)?;
        Ok(match &self.backend {
            Backend::Tmux(tmux) => {
                let mut v = vec![tmux.display().to_string()];
                v.extend(self.tmux_base_args());
                v.extend(["new-session", "-A", "-s", &Self::session_name(&row.id), "-c", &row.workdir].map(String::from));
                v.extend(self.session_env_args());
                v.push("--".into());
                v.extend(program);
                v
            }
            // 直接运行没有会话可以“打”命令进去：认不出的 shell 改用自动选择的 shell 运行命令。
            Backend::Direct => match typed {
                Some(cmd) => {
                    let auto = crate::shells::resolve("", "")?;
                    auto.run(&self.model_wrap(row, &cmd, auto.family)).unwrap_or_else(|| auto.interactive())
                }
                None => program,
            },
        })
    }

    /// 在终端选用的 shell 里运行启动命令（普通终端只打开 shell）。认不出的 shell 没法通过参数传命令：
    /// 返回打开 shell 的命令行，外加要在会话建好后输入的命令（未加模型服务商环境，输入前再按实际的 shell 加）。
    fn program_for(&self, row: &InstanceRow) -> Result<(Vec<String>, Option<String>), LocalError> {
        let shell = crate::shells::resolve(&row.shell, &Settings::load(&self.store).default_shell)?;
        Ok(match self.launch_command(row).filter(|c| !c.trim().is_empty()) {
            None => (shell.interactive(), None),
            Some(cmd) => match shell.run(&self.model_wrap(row, &cmd, shell.family)) {
                Some(v) => (v, None),
                None => (shell.interactive(), Some(cmd)),
            },
        })
    }

    /// Claude Code / Codex 选了第三方模型服务商时，按 shell 的写法加上接口地址与 Key（见 model.rs）。
    fn model_wrap(&self, row: &InstanceRow, cmd: &str, family: crate::shells::Family) -> String {
        match crate::model::launch_env(&self.store, &self.paths.home, &row.launch) {
            Some(env) => env.wrap(cmd, family),
            None => cmd.to_string(),
        }
    }

    /// 启动类型对应的命令行（含“全部权限”参数）；普通终端为 None。
    fn launch_command(&self, row: &InstanceRow) -> Option<String> {
        launch_command_line(row, self.opts(&row.id), crate::platform::is_root())
    }

    // ---------------- tmux ----------------

    /// 新会话的环境：`PATH` 最前面加上随包 trzsz 的目录。tmux 服务早已在运行时（例如客户端升级后接着用原来的服务），
    /// 新会话用的是服务启动时的环境，光改我们自己进程的环境不够，要用 `new-session -e` 传进去。
    /// `-e` 要 tmux 3.2 以上，只对随包的 looklook-mux 使用；psmux 不确定是否支持，Windows 靠进程环境。
    fn session_env_args(&self) -> Vec<String> {
        let Backend::Tmux(tmux) = &self.backend else { return vec![] };
        if cfg!(windows) || self.paths.mux.as_ref() != Some(tmux) {
            return vec![];
        }
        match self.paths.session_path().and_then(|p| p.into_string().ok()) {
            Some(p) => vec!["-e".into(), format!("PATH={p}")],
            None => vec![],
        }
    }

    /// 终端当前面板：shell 进程号、当前目录、前台命令（文件传输用）。非 tmux 后端或会话不在时返回 None。
    pub async fn pane(&self, id: &str) -> Option<Pane> {
        let Backend::Tmux(_) = self.backend else { return None };
        let target = format!("{}:", exact(&Self::session_name(id)));
        let out = self.tmux(&["display-message", "-p", "-t", &target, "#{pane_pid}\t#{pane_current_path}\t#{pane_current_command}"]).await.ok()?;
        if !out.status.success() {
            return None;
        }
        parse_pane(&String::from_utf8_lossy(&out.stdout))
    }

    fn tmux_base_args(&self) -> Vec<String> {
        let mut v: Vec<String> = if cfg!(windows) { vec![] } else { vec!["-u".into()] };
        v.extend(["-L".into(), TMUX_SOCKET.into(), "-f".into(), self.paths.tmux_conf().display().to_string()]);
        v
    }

    async fn tmux(&self, args: &[&str]) -> Result<std::process::Output, LocalError> {
        let Backend::Tmux(tmux) = &self.backend else { return Err(LocalError::new("NO_TMUX")) };
        let _ = write_tmux_conf(&self.paths.tmux_conf());
        let mut cmd = Command::new(tmux);
        cmd.args(self.tmux_base_args()).args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        self.apply_env(&mut cmd);
        #[cfg(windows)]
        cmd.creation_flags(CREATE_NO_WINDOW);
        let out = tokio::time::timeout(Duration::from_secs(10), cmd.output())
            .await
            .map_err(|_| LocalError::new("START_FAILED").with("detail", "tmux timeout"))?
            .map_err(|e| LocalError::new("START_FAILED").with("detail", e.to_string()))?;
        Ok(out)
    }

    async fn ensure_session(&self, row: &InstanceRow) -> Result<(), LocalError> {
        let name = Self::session_name(&row.id);
        if self.tmux(&["has-session", "-t", &exact(&name)]).await?.status.success() {
            return Ok(());
        }
        let (program, typed) = self.program_for(row)?;
        let mut args: Vec<String> = ["new-session", "-d", "-s", &name, "-c", &row.workdir, "-x", "120", "-y", "36"].map(String::from).to_vec();
        args.extend(self.session_env_args());
        args.push("--".into());
        args.extend(program);
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let out = self.tmux(&refs).await?;
        if !out.status.success() {
            return Err(LocalError::new("START_FAILED").with("detail", String::from_utf8_lossy(&out.stderr).trim()));
        }
        // 认不出的 shell（例如 wsl）：等它起来，再把启动命令“打”进去。
        if let Some(cmd) = typed {
            let cmd = self.model_wrap(row, &cmd, crate::shells::Family::Other);
            tokio::time::sleep(Duration::from_millis(800)).await;
            let target = format!("{}:", exact(&name));
            let _ = self.tmux(&["send-keys", "-t", &target, "-l", &cmd]).await;
            let _ = self.tmux(&["send-keys", "-t", &target, "Enter"]).await;
        }
        Ok(())
    }

    /// 终端里最近的输出（tmux 面板与回滚缓冲区最后约 200 行）。非 tmux 后端或会话不在时返回 None。
    pub async fn recent_output(&self, id: &str) -> Option<String> {
        self.capture(id, "-200").await
    }

    /// 终端的全部输出（含整个回滚缓冲区）。
    pub async fn full_output(&self, id: &str) -> Option<String> {
        self.capture(id, "-").await
    }

    async fn capture(&self, id: &str, start: &str) -> Option<String> {
        let Backend::Tmux(_) = self.backend else { return None };
        let target = format!("{}:", exact(&Self::session_name(id)));
        let out = self.tmux(&["capture-pane", "-p", "-J", "-S", start, "-t", &target]).await.ok()?;
        if !out.status.success() {
            return None;
        }
        let text = String::from_utf8_lossy(&out.stdout);
        Some(text.trim_end().to_string())
    }

    /// 会话名 → (创建时间, 最后活动时间)，秒。非 tmux 后端返回 None。
    async fn tmux_sessions(&self) -> Option<HashMap<String, (i64, i64)>> {
        let Backend::Tmux(_) = self.backend else { return None };
        let out = self.tmux(&["list-sessions", "-F", "#{session_name} #{session_created} #{session_activity}"]).await.ok()?;
        // 没有 tmux 服务（还没有任何会话）时 list-sessions 失败，视为空。
        Some(
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .filter_map(|l| {
                    let mut it = l.split_whitespace();
                    let name = it.next()?.to_string();
                    let c = it.next()?.parse().ok()?;
                    let a = it.next()?.parse().ok()?;
                    Some((name, (c, a)))
                })
                .collect(),
        )
    }

    // ---------------- 启动恢复与看护 ----------------

    /// 客户端启动：清理上次遗留的 ttyd；决定哪些实例需要恢复。
    pub async fn recover(&self) {
        let _ = write_tmux_conf(&self.paths.tmux_conf());
        let sessions = self.tmux_sessions().await;
        for row in self.store.instances().unwrap_or_default() {
            kill_stale(&self.paths.instance_pid(&row.id));
            if !row.want_running {
                continue;
            }
            if let Some(s) = &sessions {
                // 电脑重启等原因会话已经不在：除非设置了“开机自动启动”，否则不擅自重新运行命令。
                if !s.contains_key(&Self::session_name(&row.id)) && !row.auto_start {
                    let _ = self.store.set_want_running(&row.id, false);
                }
            }
        }
    }

    /// 后台看护：跟随账户状态停止/接回 ttyd；ttyd 意外退出时重启。
    pub async fn supervise(self: Arc<Self>, account: Arc<Account>) {
        let mut suspended = false;
        loop {
            let allowed = account.gate().allowed();
            if !allowed {
                if !suspended && !lock(&self.procs).is_empty() {
                    tracing::info!(reason = ?account.gate().reason(), "账户当前不可用，暂停所有终端入口（任务继续运行）");
                }
                let ids: Vec<String> = lock(&self.procs).keys().cloned().collect();
                for id in ids {
                    self.kill_ttyd(&id).await;
                }
                suspended = true;
            } else {
                suspended = false;
                self.reap();
                let rows = self.store.instances().unwrap_or_default();
                for row in rows.into_iter().filter(|r| r.want_running) {
                    let gave_up = lock(&self.health).get(&row.id).is_some_and(|h| h.quick_exits >= 3);
                    if lock(&self.procs).contains_key(&row.id) || gave_up {
                        continue;
                    }
                    // 开机自动启动的终端要立即运行命令；其余只接回已有会话。
                    if let Err(e) = self.resume(&row.id, row.auto_start).await {
                        tracing::warn!(id = %row.id, error = ?e.code, "恢复终端失败");
                        let mut h = lock(&self.health);
                        let entry = h.entry(row.id.clone()).or_default();
                        entry.quick_exits += 1;
                        entry.error = Some(e.code.clone());
                    }
                }
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    }

    /// 回收已退出的 ttyd，记录“刚启动就退出”的次数。
    fn reap(&self) {
        let mut exited = vec![];
        for (id, p) in lock(&self.procs).iter_mut() {
            if let Ok(Some(status)) = p.child.try_wait() {
                exited.push((id.clone(), status, p.started.elapsed()));
            }
        }
        for (id, status, alive) in exited {
            lock(&self.procs).remove(&id);
            tracing::warn!(id = %id, ?status, "终端服务退出");
            let mut h = lock(&self.health);
            let e = h.entry(id.clone()).or_default();
            if alive < Duration::from_secs(10) {
                e.quick_exits += 1;
            } else {
                e.quick_exits = 0;
            }
            e.error = Some("EXITED".into());
        }
    }

    pub async fn shutdown(&self) {
        let ids: Vec<String> = lock(&self.procs).keys().cloned().collect();
        for id in ids {
            self.kill_ttyd(&id).await;
        }
    }

    pub fn capabilities(&self) -> serde_json::Value {
        json!({
            "backend": self.backend.name(),
            "persistent": matches!(self.backend, Backend::Tmux(_)),
            "ttyd": self.paths.ttyd.is_some(),
            "fonts": self.paths.fonts.is_some(),
            "os": crate::paths::os_name(),
            "root": crate::platform::is_root(),
        })
    }
}

/// ttyd 参数。终端字体、配色在这里通过 `-t` 传给浏览器端的 xterm.js。
pub fn ttyd_args(row: &InstanceRow, secret: &str, s: &Settings, command: &[String]) -> Vec<String> {
    let theme = if s.theme == "light" {
        r##"{"background":"#fbfbfd","foreground":"#1f2330","cursor":"#4f46e5","cursorAccent":"#ffffff","selectionBackground":"#c7d2fe","black":"#1f2330","red":"#d9364f","green":"#12a150","yellow":"#b7791f","blue":"#4f46e5","magenta":"#a23fd1","cyan":"#0891b2","white":"#d4d7e1","brightBlack":"#6b7285","brightRed":"#e5536a","brightGreen":"#1fb866","brightYellow":"#c98f2b","brightBlue":"#6366f1","brightMagenta":"#b95ee0","brightCyan":"#06b6d4","brightWhite":"#ffffff"}"##
    } else {
        r##"{"background":"#11131a","foreground":"#e3e6ee","cursor":"#a5b4fc","cursorAccent":"#11131a","selectionBackground":"#3b3f63","black":"#1b1e27","red":"#f06a7e","green":"#3ccf7c","yellow":"#e0a846","blue":"#818cf8","magenta":"#c084fc","cyan":"#22d3ee","white":"#d6d9e2","brightBlack":"#5c6375","brightRed":"#ff8a9b","brightGreen":"#6ee7a0","brightYellow":"#f5c46a","brightBlue":"#a5b4fc","brightMagenta":"#d8b4fe","brightCyan":"#67e8f9","brightWhite":"#ffffff"}"##
    };
    let mut a: Vec<String> = vec![
        "-i".into(),
        "127.0.0.1".into(),
        "-p".into(),
        row.port.to_string(),
        "-b".into(),
        format!("/i/{}", row.id),
        "-W".into(),
        "-c".into(),
        format!("looklook:{secret}"),
        "-P".into(),
        "10".into(),
    ];
    // ttyd 先把 `-t` 的值当 JSON 解析：字符串值必须写成 JSON 字符串，否则
    // `"JetBrains Mono", "LXGW WenKai Mono"` 只会取到第一个字体、`123` 会变成数字。
    let js = |v: &str| serde_json::to_string(v).unwrap_or_default();
    let opts = [
        format!("fontFamily={}", js(&s.font_family())),
        format!("fontSize={}", s.font_size),
        "lineHeight=1.15".into(),
        "rendererType=dom".into(),
        "disableLeaveAlert=true".into(),
        "disableResizeOverlay=true".into(),
        "cursorBlink=true".into(),
        // 随包 trz / tsz 的浏览器端（ttyd 自带）。enableZmodem 保持关闭：tmux 会吞掉 ZMODEM 的控制字节，
        // 开了还会和管理台检测 rz/sz 横幅抢输出（docs/FILE_TRANSFER.md §2、§3.5）。
        "enableTrzsz=true".into(),
        format!("titleFixed={}", js(&row.name)),
        format!("theme={theme}"),
    ];
    for o in opts {
        a.push("-t".into());
        a.push(o);
    }
    if !command.first().is_some_and(|c| is_mux_program(c)) {
        a.push("-w".into());
        a.push(row.workdir.clone());
    }
    a.extend(command.iter().cloned());
    a
}

/// Codex：`--yolo`（即 `--dangerously-bypass-approvals-and-sandbox`，不再询问、不启用沙箱）。
/// Claude Code：`--dangerously-skip-permissions`；它在 root 下会拒绝运行，需要同时设置 `IS_SANDBOX=1`。
/// OpenCode：`--auto`（没有明确拒绝的权限都自动同意）。
/// DSH：`dsh web` 只监听 127.0.0.1 的网页界面，经本机网页映射打开（本机、远程都是同一个地址）；
/// 工作目录是默认项目，别的项目文件夹在网页里添加。
pub fn launch_command_line(row: &InstanceRow, opts: Opts, root: bool) -> Option<String> {
    match row.launch.as_str() {
        "codex" if opts.full_access => Some("codex --yolo".into()),
        "codex" => Some("codex".into()),
        "claude" if opts.full_access && root => Some("IS_SANDBOX=1 claude --dangerously-skip-permissions".into()),
        "claude" if opts.full_access => Some("claude --dangerously-skip-permissions".into()),
        "claude" => Some("claude".into()),
        "opencode" if opts.full_access => Some("opencode --auto".into()),
        "opencode" => Some("opencode".into()),
        "dsh" => opts.web_port.map(|p| format!("dsh web --no-open --port {p}")),
        "custom" => Some(row.command.clone()),
        _ => None,
    }
}

fn default_name(launch: &str, n: usize) -> String {
    match launch {
        "codex" => format!("Codex {n}"),
        "claude" => format!("Claude Code {n}"),
        "opencode" => format!("OpenCode {n}"),
        "dsh" => "DSH".into(),
        _ => format!("终端 {n}"),
    }
}

fn expand_home(p: &str) -> String {
    if p == "~" || p.starts_with("~/") {
        if let Some(h) = dirs::home_dir() {
            return format!("{}{}", h.display(), &p[1..]);
        }
    }
    p.to_string()
}

impl Instances {
    /// 子进程环境：保证 UTF-8（中文输入输出正常），去掉外层 tmux 的变量；Windows 上固定 psmux 的数据目录。
    fn apply_env(&self, cmd: &mut Command) {
        apply_env(cmd);
        // 随包的 trz / tsz：tmux 服务、ttyd（直接运行模式下的 shell）都从这里继承 PATH
        if let Some(p) = self.paths.session_path() {
            cmd.env("PATH", p);
        }
        // psmux 按 USERPROFILE 找会话登记文件（端口、密钥），每次启动还会清理“没有登记的”会话进程。
        // 我们启动的、ttyd 启动的、开机自启时环境不同的进程必须看到同一个目录，否则会互相找不到、
        // 甚至把对方的会话当孤儿结束——关掉网页再打开就接不回原来的终端。放在看看自己的数据目录，
        // 也和用户自己用的 psmux 互不干扰。预热会话（warm）是给交互使用的提速，这里只会多出空闲进程。
        if cfg!(windows) {
            cmd.env("PSMUX_DATA_DIR", self.paths.home.join("mux")).env("PSMUX_NO_WARM", "1");
        }
    }
}

/// 子进程环境：保证 UTF-8（中文输入输出正常），并去掉外层 tmux 的变量。
fn apply_env(cmd: &mut Command) {
    cmd.env_remove("TMUX").env_remove("TMUX_PANE");
    let utf8 = ["LC_ALL", "LC_CTYPE", "LANG"]
        .iter()
        .find_map(|k| std::env::var(k).ok().filter(|v| !v.is_empty()))
        .is_some_and(|v| v.to_ascii_lowercase().replace('-', "").contains("utf8"));
    if !utf8 && !cfg!(windows) {
        cmd.env("LANG", if cfg!(target_os = "macos") { "en_US.UTF-8" } else { "C.UTF-8" });
    }
    cmd.env("COLORTERM", "truecolor");
}

/// tmux 面板信息（`display-message` 的输出）
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pane {
    pub pid: u32,
    pub cwd: String,
    pub command: String,
}

fn parse_pane(out: &str) -> Option<Pane> {
    let line = out.lines().next()?;
    let mut it = line.splitn(3, '\t');
    let pid = it.next()?.trim().parse().ok()?;
    let cwd = it.next().unwrap_or("").to_string();
    let command = it.next().unwrap_or("").trim().to_string();
    Some(Pane { pid, cwd, command })
}

/// tmux 的 `-t` 精确匹配会话名（不然 `a1` 会匹配 `a10`）；psmux 不认 `=` 前缀，用原名。
fn exact(name: &str) -> String {
    if cfg!(windows) {
        name.to_string()
    } else {
        format!("={name}")
    }
}

fn write_tmux_conf(path: &Path) -> std::io::Result<()> {
    // 鼠标滚轮翻看历史；拖选复制经 OSC 52 交给浏览器剪贴板（终端页注入的脚本处理）。
    let term = if Path::new("/usr/share/terminfo/t/tmux-256color").exists() || Path::new("/lib/terminfo/t/tmux-256color").exists() {
        "tmux-256color"
    } else {
        "screen-256color"
    };
    // Windows 的 psmux 只给它认识的选项（未知选项可能让配置整体加载失败）。
    let conf = if cfg!(windows) {
        "# 由看看客户端生成，请勿修改\nset -g mouse on\nset -g status off\nset -g history-limit 50000\nset -g escape-time 10\n".to_string()
    } else {
        format!(
        "# 由看看客户端生成，请勿修改\n\
         set -g mouse on\nset -g status off\nset -g history-limit 50000\nset -g escape-time 10\n\
         set -g focus-events on\nset -g set-clipboard on\nset -g default-terminal \"{term}\"\n\
         set -ga terminal-overrides \",xterm-256color:RGB\"\nsetw -g aggressive-resize on\n\
         set -g detach-on-destroy off\n"
        )
    };
    if std::fs::read_to_string(path).ok().as_deref() == Some(conf.as_str()) {
        return Ok(());
    }
    std::fs::write(path, conf)
}

fn open_log(path: &Path) -> Result<std::fs::File, LocalError> {
    // 日志只保留最近一次启动的内容，最多约 1 MB。
    if std::fs::metadata(path).map(|m| m.len() > 1_000_000).unwrap_or(false) {
        let _ = std::fs::remove_file(path);
    }
    Ok(std::fs::OpenOptions::new().create(true).append(true).open(path)?)
}

pub fn log_tail(path: &Path) -> String {
    let s = std::fs::read_to_string(path).unwrap_or_default();
    let lines: Vec<&str> = s.lines().rev().take(30).collect();
    lines.into_iter().rev().collect::<Vec<_>>().join("\n")
}

/// 上次运行遗留的 ttyd（客户端被强制结束）：只结束确实是终端服务的进程（新名 looklook-term，旧版为 ttyd）。
fn kill_stale(pid_file: &Path) {
    let Some(pid) = std::fs::read_to_string(pid_file).ok().and_then(|s| s.trim().parse::<u32>().ok()) else { return };
    let _ = std::fs::remove_file(pid_file);
    if !process_name(pid).is_some_and(|n| is_term_process(&n)) {
        return;
    }
    tracing::info!(pid, "结束遗留的终端服务进程");
    #[cfg(unix)]
    unsafe {
        libc::kill(pid as i32, libc::SIGTERM);
    }
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("taskkill").args(["/PID", &pid.to_string(), "/F"]).output();
    }
}

fn is_term_process(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    crate::paths::TERM_PROCESS_NAMES.iter().any(|t| n.contains(t))
}

fn process_name(pid: u32) -> Option<String> {
    if cfg!(target_os = "linux") {
        return std::fs::read_to_string(format!("/proc/{pid}/comm")).ok().map(|s| s.trim().to_string());
    }
    let out = if cfg!(windows) {
        std::process::Command::new("tasklist").args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"]).output().ok()?
    } else {
        std::process::Command::new("ps").args(["-p", &pid.to_string(), "-o", "comm="]).output().ok()?
    };
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!s.is_empty() && !s.starts_with("INFO:")).then_some(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row() -> InstanceRow {
        InstanceRow {
            id: "abcd2345".into(),
            name: "我的终端".into(),
            workdir: "/tmp".into(),
            launch: "shell".into(),
            command: String::new(),
            shell: String::new(),
            auto_start: false,
            want_running: false,
            port: 41001,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    /// 端到端：随包的 looklook-mux 保持会话——ttyd 结束后任务仍在运行，输出还能读到。
    /// 需要 vendor/ 里有本平台的 ttyd 与 mux（scripts/fetch-vendor.sh），所以默认不跑：`cargo test -- --ignored`。
    #[tokio::test]
    #[ignore]
    async fn bundled_mux_keeps_session_after_ttyd_exits() {
        let home = tempfile::tempdir().unwrap();
        let paths = Paths::discover(Some(home.path().to_path_buf())).unwrap();
        assert!(paths.mux.is_some() && paths.ttyd.is_some(), "缺少 vendor/ 里的 ttyd 或 mux");
        let inst = Instances::new(Arc::new(Store::memory().unwrap()), paths);
        assert!(matches!(inst.backend, Backend::Tmux(_)));
        let input = Input {
            name: Some("e2e".into()),
            workdir: Some(home.path().display().to_string()),
            launch: Some("custom".into()),
            command: Some("echo e2e-marker-123; sleep 120".into()),
            ..Default::default()
        };
        let id = inst.create(input).await.unwrap().row.id;
        inst.start(&id).await.unwrap();
        tokio::time::sleep(Duration::from_millis(1500)).await;
        // 关掉 ttyd（相当于关掉网页 / 客户端退出）：会话与任务继续运行
        inst.shutdown().await;
        assert!(inst.upstream(&id).is_none());
        let out = inst.recent_output(&id).await.expect("会话应该还在");
        assert!(out.contains("e2e-marker-123"), "输出里没有标记：{out}");
        // 停止才结束会话
        inst.stop(&id).await.unwrap();
        assert!(inst.recent_output(&id).await.is_none());
        let _ = inst.tmux(&["kill-server"]).await;
    }

    #[test]
    fn pane_info_parses_tab_separated_fields() {
        assert_eq!(
            parse_pane("4242\t/home/a/my proj\tbash\n"),
            Some(Pane { pid: 4242, cwd: "/home/a/my proj".into(), command: "bash".into() })
        );
        assert_eq!(parse_pane("x\t/\tbash"), None);
        assert_eq!(parse_pane(""), None);
    }

    #[test]
    fn ttyd_enables_trzsz_but_not_zmodem() {
        let a = ttyd_args(&row(), "x", &Settings::default(), &["/bin/bash".into()]);
        assert!(a.contains(&"enableTrzsz=true".to_string()));
        assert!(!a.iter().any(|x| x.contains("enableZmodem")));
    }

    #[test]
    fn stale_term_process_names() {
        assert!(is_term_process("looklook-term"));
        assert!(is_term_process("\"looklook-term.exe\",\"4242\",\"Console\""));
        assert!(is_term_process("ttyd.exe"));
        // 客户端自己绝不能被当作遗留进程结束
        assert!(!is_term_process("looklook"));
        assert!(!is_term_process("looklook.exe"));
    }

    #[test]
    fn ttyd_args_bind_loopback_with_base_path_and_credential() {
        let a = ttyd_args(&row(), "s3cret", &Settings::default(), &["/usr/bin/tmux".into(), "new-session".into()]);
        let joined = a.join(" ");
        assert!(joined.starts_with("-i 127.0.0.1 -p 41001 -b /i/abcd2345 -W -c looklook:s3cret"));
        assert!(joined.contains(r#"fontFamily="\"JetBrains Mono\", \"LXGW WenKai Mono\""#));
        assert!(joined.contains("titleFixed=\"我的终端\""));
        // 字体栈作为 JSON 字符串传给 ttyd，解析后是完整的字体列表
        let ff = a.iter().find_map(|x| x.strip_prefix("fontFamily=")).unwrap();
        let parsed: String = serde_json::from_str(ff).unwrap();
        assert!(parsed.contains("LXGW WenKai Mono") && parsed.ends_with("monospace"));
        assert!(!a.contains(&"-w".to_string()), "tmux 自己设置工作目录");
        assert_eq!(a.last().map(String::as_str), Some("new-session"));

        let direct = ttyd_args(&row(), "x", &Settings::default(), &["/bin/bash".into(), "-l".into()]);
        let i = direct.iter().position(|s| s == "-w").unwrap();
        assert_eq!(direct[i + 1], "/tmp");
    }

    #[test]
    fn mux_program_names_are_recognized() {
        for p in ["/usr/bin/tmux", "/opt/looklook/looklook-mux", "/x/looklook-mux.exe", "/x/psmux.exe", "/x/mux"] {
            assert!(is_mux_program(p), "{p}");
        }
        assert!(!is_mux_program("/bin/bash") && !is_mux_program("powershell.exe"));
        let a = ttyd_args(&row(), "x", &Settings::default(), &["/opt/looklook/looklook-mux".into(), "attach-session".into()]);
        assert!(!a.contains(&"-w".to_string()));
    }

    #[test]
    fn full_access_flags() {
        let mut r = row();
        let on = Opts { full_access: true, root_confirmed: true, web_port: None };
        assert_eq!(launch_command_line(&r, Opts::default(), false), None);
        r.launch = "codex".into();
        assert_eq!(launch_command_line(&r, Opts::default(), false).as_deref(), Some("codex"));
        assert_eq!(launch_command_line(&r, on, true).as_deref(), Some("codex --yolo"));
        r.launch = "claude".into();
        assert_eq!(launch_command_line(&r, on, false).as_deref(), Some("claude --dangerously-skip-permissions"));
        assert_eq!(launch_command_line(&r, on, true).as_deref(), Some("IS_SANDBOX=1 claude --dangerously-skip-permissions"));
        assert_eq!(launch_command_line(&r, Opts::default(), true).as_deref(), Some("claude"));
        r.launch = "opencode".into();
        assert_eq!(launch_command_line(&r, on, true).as_deref(), Some("opencode --auto"));
        assert_eq!(launch_command_line(&r, Opts::default(), false).as_deref(), Some("opencode"));
        r.launch = "dsh".into();
        let web = Opts { web_port: Some(3080), ..Opts::default() };
        assert_eq!(launch_command_line(&r, web, false).as_deref(), Some("dsh web --no-open --port 3080"));
    }

    #[test]
    fn opts_validation() {
        let mut r = row();
        let mut o = Opts { full_access: true, root_confirmed: true, web_port: None };
        Instances::validate_opts(&r, &mut o).unwrap();
        assert!(o == Opts::default(), "shell 终端忽略全部权限选项");
        r.launch = "claude".into();
        let mut o = Opts { full_access: false, root_confirmed: true, web_port: None };
        Instances::validate_opts(&r, &mut o).unwrap();
        assert!(!o.root_confirmed);
        let mut o = Opts { full_access: true, root_confirmed: false, web_port: None };
        assert_eq!(Instances::validate_opts(&r, &mut o).is_err(), crate::platform::is_root());
        r.launch = "dsh".into();
        let mut o = Opts { full_access: true, root_confirmed: true, web_port: None };
        assert!(Instances::validate_opts(&r, &mut o).is_err(), "DSH 必须有网页端口");
        let mut o = Opts { full_access: true, root_confirmed: true, web_port: Some(3080) };
        Instances::validate_opts(&r, &mut o).unwrap();
        assert!(o == Opts { web_port: Some(3080), ..Opts::default() }, "DSH 没有全部权限选项");
    }

    #[test]
    fn names_and_paths() {
        assert_eq!(default_name("claude", 2), "Claude Code 2");
        assert_eq!(default_name("shell", 1), "终端 1");
        assert!(Instances::is_reserved_port(41500));
        assert!(!Instances::is_reserved_port(5173));
        if let Some(h) = dirs::home_dir() {
            assert_eq!(expand_home("~/x"), format!("{}/x", h.display()));
        }
    }
}
