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
/// 有对话界面的启动类型（Claude Code：docs/CLAUDE_UI.md；Codex：docs/CODEX_UI.md）；
/// 前端 `src-ui/shared/chat.js` 的 `CHAT_LAUNCHES` 要一起改。
pub const CHAT_LAUNCHES: &[&str] = &["claude", "codex"];

fn has_chat(launch: &str) -> bool {
    CHAT_LAUNCHES.contains(&launch)
}

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
    /// 新建时填的“第一句话”：只在第一次建会话时用一次（重启、开机自启不会再发），见 [`Instances::ensure_session`]
    first_prompt: Mutex<HashMap<String, String>>,
    /// 新建时要接着的以前的对话：同第一句话，只在第一次建会话时用一次
    first_resume: Mutex<HashMap<String, Resume>>,
    /// agent 会话名（终端 ID → 名字）和算出来的时间
    titles: Mutex<(Option<Instant>, HashMap<String, String>)>,
}

/// 接着以前的对话（会话页的“以前的对话”，见 history.rs）：`fork` 为复制一份接着，原来的不动
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resume {
    pub id: String,
    #[serde(default)]
    pub fork: bool,
}

/// 这个终端接着的是哪个对话（`instance_resume.{id}`），以前的对话列表据此标出“已在看看里”
pub fn resume_key(id: &str) -> String {
    format!("instance_resume.{id}")
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
    /// Claude Code 带对话界面启动（docs/CLAUDE_UI.md）
    pub chat_ui: bool,
    /// 名字是用户自己起的（新建时填了名称、之后改过名）；界面上 agent 的会话名为主，它为辅
    pub name_custom: bool,
    /// agent 自己的会话名：对话标题（/rename、自动标题），否则 agent 设置的终端标题（见 [`Instances::agent_titles`]）
    pub agent_title: Option<String>,
    /// 在 Docker 容器里运行（docker.rs）
    pub container: Option<crate::docker::Container>,
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
    /// Claude Code：注入 hooks 启动，管理台多一个“对话”视图（docs/CLAUDE_UI.md）
    #[serde(default)]
    pub chat_ui: bool,
    /// 名字是用户自己起的
    #[serde(default)]
    pub name_custom: bool,
}

/// 第一句话最长多少个字符（太长的请在会话里粘贴）
const PROMPT_MAX: usize = 4000;

/// DSH 网页界面的端口从这里开始找（`dsh web` 自己默认 3080）；避开终端用的 41000–41999。
const WEB_PORT_START: u16 = 3080;

fn opts_key(id: &str) -> String {
    format!("instance_opts.{id}")
}

/// 会话所在的 Docker 容器（只在新建时设置）
fn container_key(id: &str) -> String {
    format!("instance_container.{id}")
}

/// agent 会话名多久重新算一次（要读对话记录、问 tmux）
const TITLE_TTL: Duration = Duration::from_secs(10);

/// Codex 也有对话界面了（docs/CODEX_UI.md）。之前 Codex 终端的 `chat_ui` 是被强制关掉的，不是用户选的，
/// 所以升级后给已有的 Codex 终端打开一次（和新建时的默认一样），下次启动生效；之后用户关掉的不再动。
fn enable_codex_chat_once(store: &Store) {
    const DONE: &str = "migrate.codex_chat_ui";
    if store.get::<bool>(DONE).ok().flatten().is_some() {
        return;
    }
    let Ok(rows) = store.instances() else { return };
    for r in rows.iter().filter(|r| r.launch == "codex") {
        let mut o: Opts = store.get(&opts_key(&r.id)).ok().flatten().unwrap_or_default();
        if !o.chat_ui {
            o.chat_ui = true;
            let _ = store.set(&opts_key(&r.id), &o);
        }
    }
    let _ = store.set(DONE, &true);
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
    pub chat_ui: Option<bool>,
    /// 第一句话（会话页开始页的输入框）：AI 工具作为启动参数交给它，普通终端当作启动后运行的命令。只在新建时有效
    pub prompt: Option<String>,
    /// 接着以前的对话（只在新建时有效，和第一句话不能同时用）
    pub resume: Option<Resume>,
    /// DSH 网页界面的端口（只在新建时由网关填写，不接受请求里的值）
    #[serde(skip)]
    pub web_port: Option<u16>,
    /// 名字是用户自己填的（不是按第一句话、对话标题取的）
    pub name_custom: Option<bool>,
    /// 在 Docker 容器里运行：容器名和容器里的目录（只在新建时有效）
    pub container: Option<ContainerInput>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ContainerInput {
    pub name: String,
    pub dir: String,
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
        if matches!(backend, Backend::Tmux(_)) {
            enable_codex_chat_once(&store);
        }
        Arc::new(Self {
            store,
            paths,
            backend,
            shell: crate::shells::login_shell(),
            procs: Mutex::new(HashMap::new()),
            health: Mutex::new(HashMap::new()),
            op: tokio::sync::Mutex::new(()),
            changed: tokio::sync::watch::channel(0).0,
            first_prompt: Mutex::new(HashMap::new()),
            first_resume: Mutex::new(HashMap::new()),
            titles: Mutex::new((None, HashMap::new())),
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
        let mut views: Vec<View> = rows.into_iter().map(|r| self.view(r, sessions.as_ref())).collect();
        let stale = lock(&self.titles).0.is_none_or(|at| at.elapsed() >= TITLE_TTL);
        if stale {
            let titles = self.agent_titles(&views).await;
            *lock(&self.titles) = (Some(Instant::now()), titles);
            let cache = lock(&self.titles);
            for v in &mut views {
                v.agent_title = cache.1.get(&v.row.id).cloned();
            }
        }
        Ok(views)
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
            chat_ui: opts.chat_ui,
            name_custom: opts.name_custom,
            agent_title: lock(&self.titles).1.get(&row.id).cloned(),
            container: self.container(&row.id),
            running,
            task_alive: sessions.map(|_| sess.is_some()),
            started_at: sess.map(|(c, _)| ts(*c)),
            last_activity_at: sess.map(|(_, a)| ts(*a)),
            error: lock(&self.health).get(&row.id).and_then(|h| h.error.clone()),
            url: format!("/i/{}/", row.id),
            row,
        }
    }

    pub fn container(&self, id: &str) -> Option<crate::docker::Container> {
        self.store.get::<crate::docker::Container>(&container_key(id)).ok().flatten()
    }

    /// agent 的会话名（Claude Code / Codex / OpenCode 的终端，终端 ID → 名字）：
    /// 1. 对话标题（history.rs：/rename > 自动标题 > 摘要 > 第一句话）。是哪个对话：对话界面 hooks 记下的会话号、
    ///    接着以前的对话时的会话号；都没有时，同一文件夹里只有这一个该工具的终端在跑，就取它启动之后最新的那个对话。
    /// 2. agent 设置的终端标题（tmux `pane_title`，例如 Claude Code 的“✳ 修登录页”），去掉前面的状态符号。
    /// 容器里的会话拿不到对话记录，只用 2。
    async fn agent_titles(&self, views: &[View]) -> HashMap<String, String> {
        let live: Vec<&View> = views.iter().filter(|v| v.task_alive == Some(true) && crate::history::AGENTS.contains(&v.row.launch.as_str())).collect();
        let mut out = HashMap::new();
        if live.is_empty() {
            return out;
        }
        let panes = self.pane_titles().await;
        let local: Vec<&&View> = live.iter().filter(|v| v.container.is_none()).collect();
        let mut convs = vec![];
        for agent in crate::history::AGENTS {
            if local.iter().any(|v| v.row.launch == agent) {
                convs.extend(crate::history::list(crate::history::Query { agent: Some(agent.to_string()), limit: Some(500), ..Default::default() }).await);
            }
        }
        let bound: HashMap<&str, String> = local
            .iter()
            .filter_map(|v| {
                let id = v.row.id.as_str();
                let conv = crate::chat::binding(&self.store, id).map(|b| b.session_id).or_else(|| self.store.get::<String>(&resume_key(id)).ok().flatten())?;
                Some((id, conv))
            })
            .collect();
        let taken: HashSet<&str> = bound.values().map(String::as_str).collect();
        for v in &live {
            let id = v.row.id.as_str();
            let conv = if v.container.is_some() {
                None
            } else if let Some(c) = bound.get(id) {
                convs.iter().find(|x| x.agent == v.row.launch && &x.id == c)
            } else {
                // 同一文件夹里同一工具没绑定的终端只有这一个时，取它启动之后最新的对话
                let peers = local.iter().filter(|o| o.row.launch == v.row.launch && !bound.contains_key(o.row.id.as_str()) && crate::history::same_dir(&o.row.workdir, &v.row.workdir)).count();
                let started = v.started_at.as_deref().and_then(crate::util::parse_time_ms).unwrap_or(i64::MAX);
                (peers == 1)
                    .then(|| convs.iter().filter(|x| x.agent == v.row.launch && !taken.contains(x.id.as_str()) && x.updated_ms >= started - 5000 && crate::history::same_dir(&x.workdir, &v.row.workdir)).max_by_key(|x| x.updated_ms))
                    .flatten()
            };
            let title = conv.map(|c| c.title.clone()).filter(|t| !t.trim().is_empty()).or_else(|| panes.get(&Self::session_name(id)).cloned());
            if let Some(t) = title {
                out.insert(id.to_string(), clip_title(&t));
            }
        }
        out
    }

    /// 每个 tmux 会话当前面板的标题（agent 用 OSC 设置的）。没设置过的（默认是主机名）、shell 的提示符标题不算。
    async fn pane_titles(&self) -> HashMap<String, String> {
        let Backend::Tmux(_) = self.backend else { return HashMap::new() };
        let Ok(out) = self.tmux(&["list-sessions", "-F", "#{session_name}\t#{host}\t#{pane_title}"]).await else { return HashMap::new() };
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| {
                let mut it = l.splitn(3, '\t');
                let (name, host, title) = (it.next()?, it.next()?, it.next()?);
                Some((name.to_string(), clean_pane_title(title, host)?))
            })
            .collect()
    }

    pub fn opts(&self, id: &str) -> Opts {
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

    /// 新建容器里的会话：容器名、目录合法，容器在运行，要用的 AI 工具容器里装了。
    async fn check_container(c: &ContainerInput, launch: &str) -> Result<crate::docker::Container, LocalError> {
        let (name, dir) = (c.name.trim(), c.dir.trim().trim_end_matches('/'));
        let dir = if dir.is_empty() { "/" } else { dir };
        if !crate::docker::valid_name(name) {
            return Err(LocalError::invalid("container", "invalid"));
        }
        if !crate::docker::valid_dir(dir) {
            return Err(LocalError::new("WORKDIR_NOT_FOUND").with("path", dir));
        }
        if !matches!(launch, "shell" | "claude" | "codex" | "opencode" | "custom") {
            return Err(LocalError::invalid("launch", "invalid"));
        }
        let p = crate::docker::probe(name).await?;
        if crate::history::AGENTS.contains(&launch) && !p.tools.iter().any(|t| t == launch) {
            return Err(LocalError::new("TOOL_NOT_IN_CONTAINER").with("tool", launch).with("container", name));
        }
        Ok(crate::docker::Container { name: name.to_string(), dir: dir.to_string(), root: p.root })
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
        if !has_chat(&row.launch) {
            opts.chat_ui = false;
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
        let launch = input.launch.clone().unwrap_or_else(|| "shell".into());
        // 容器：先探测（最多几秒，不占着新建锁）
        let container = match &input.container {
            Some(c) => Some(Self::check_container(c, &launch).await?),
            None => None,
        };
        let _g = self.op.lock().await;
        let settings = Settings::load(&self.store);
        let now = now_rfc3339();
        let mut row = InstanceRow {
            id: loop {
                let id = random_id(8);
                if self.store.instance(&id)?.is_none() {
                    break id;
                }
            },
            name: input.name.unwrap_or_default(),
            workdir: input.workdir.filter(|w| !w.trim().is_empty()).unwrap_or(settings.default_workdir.clone()),
            launch,
            command: input.command.unwrap_or_default(),
            shell: input.shell.unwrap_or_default(),
            auto_start: input.auto_start.unwrap_or(false),
            want_running: false,
            port: self.free_port(None)?,
            created_at: now.clone(),
            updated_at: now,
        };
        // 容器里的会话：tmux 在这台电脑上，工作目录用默认文件夹；容器里的目录交给 docker exec -w
        if container.is_some() {
            row.workdir = settings.default_workdir.clone();
        }
        let name_custom = input.name_custom.unwrap_or(false) && !row.name.trim().is_empty();
        if row.name.trim().is_empty() {
            let rows = self.store.instances()?;
            let taken: HashSet<&str> = rows.iter().map(|r| r.name.as_str()).collect();
            row.name = default_name(&row.launch, container.as_ref().map_or(&row.workdir, |c| &c.dir), &taken);
        }
        self.validate(&mut row)?;
        if row.launch == "dsh" && self.dsh()?.is_some() {
            return Err(LocalError::new("DSH_EXISTS"));
        }
        let mut opts = Opts {
            full_access: input.full_access.unwrap_or(false),
            root_confirmed: input.root_confirmed.unwrap_or(false),
            web_port: input.web_port,
            // 新建的 Claude Code 终端默认带对话界面（需要会话保持）
            chat_ui: input.chat_ui.unwrap_or(matches!(self.backend, Backend::Tmux(_))),
            name_custom,
        };
        if container.is_some() {
            // 容器里的 root 不是这台电脑的 root，不用确认；没有对话界面
            opts.root_confirmed = opts.full_access;
            opts.chat_ui = false;
        }
        Self::validate_opts(&row, &mut opts)?;
        let prompt = input.prompt.map(|p| p.trim().to_string()).filter(|p| !p.is_empty());
        if let Some(p) = &prompt {
            if p.chars().count() > PROMPT_MAX {
                return Err(LocalError::invalid("prompt", "too_long"));
            }
            if !matches!(row.launch.as_str(), "shell" | "claude" | "codex" | "opencode") {
                return Err(LocalError::invalid("prompt", "invalid"));
            }
        }
        if let Some(r) = &input.resume {
            if container.is_some() {
                return Err(LocalError::invalid("resume", "invalid"));
            }
            if !crate::history::valid_id(&r.id) || !crate::history::AGENTS.contains(&row.launch.as_str()) || prompt.is_some() {
                return Err(LocalError::invalid("resume", "invalid"));
            }
            if r.fork && row.launch == "opencode" {
                return Err(LocalError::invalid("resume", "invalid"));
            }
            // 直接运行模式下命令在每次打开终端时运行，没法只接着一次
            if !matches!(self.backend, Backend::Tmux(_)) {
                return Err(LocalError::new("RESUME_NEEDS_PERSISTENT"));
            }
        }
        self.store.insert_instance(&row)?;
        self.store.set(&opts_key(&row.id), &opts)?;
        if let Some(c) = &container {
            self.store.set(&container_key(&row.id), c)?;
        }
        if let Some(r) = input.resume {
            if !r.fork {
                self.store.set(&resume_key(&row.id), &r.id)?;
            }
            lock(&self.first_resume).insert(row.id.clone(), r);
        }
        // 只有会话保持（tmux）时才在建会话时用上；直接运行模式下忽略
        if let (Some(p), Backend::Tmux(_)) = (prompt, &self.backend) {
            lock(&self.first_prompt).insert(row.id.clone(), p);
        }
        self.bump();
        Ok(self.view(row, None))
    }

    pub async fn update(&self, id: &str, input: Input) -> Result<View, LocalError> {
        let _g = self.op.lock().await;
        let mut row = self.store.instance(id)?.ok_or_else(LocalError::not_found)?;
        let renamed = input.name.as_deref().is_some_and(|v| v.trim() != row.name);
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
        if let Some(v) = input.chat_ui {
            opts.chat_ui = v;
        }
        if renamed {
            opts.name_custom = true;
        }
        if self.container(id).is_some() {
            opts.root_confirmed = opts.full_access;
            opts.chat_ui = false;
        }
        Self::validate_opts(&row, &mut opts)?;
        row.updated_at = now_rfc3339();
        self.store.update_instance(&row)?;
        self.store.set(&opts_key(id), &opts)?;
        drop(_g);
        self.get(id).await
    }

    /// 删除终端：还在运行时先停止（结束里面启动的所有程序，见 [`Instances::stop`]），不留下没人管的进程。
    /// 界面上删除前会提示正在运行的程序会被结束。
    pub async fn delete(&self, id: &str) -> Result<(), LocalError> {
        self.get(id).await?;
        self.stop(id).await?;
        let _g = self.op.lock().await;
        self.store.delete_instance(id)?;
        self.bump();
        let _ = self.store.remove(&opts_key(id));
        let _ = self.store.remove(&container_key(id));
        let _ = self.store.remove(&resume_key(id));
        lock(&self.health).remove(id);
        lock(&self.first_prompt).remove(id);
        lock(&self.first_resume).remove(id);
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
            // 会话在这里先建好；Windows 上 ttyd 只负责接入，会话不在时由 ensure_attachable 在打开终端时补建。
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
    /// 会话里启动的所有程序（包括忽略 SIGHUP 的、放到后台的）一并结束，见 proctree.rs。
    pub async fn stop(&self, id: &str) -> Result<(), LocalError> {
        let _g = self.op.lock().await;
        self.store.set_want_running(id, false)?;
        let doomed = self.task_processes(id).await;
        crate::proctree::signal(&doomed);
        self.kill_ttyd(id).await;
        if let Backend::Tmux(_) = self.backend {
            let _ = self.tmux(&["kill-session", "-t", &exact(&Self::session_name(id))]).await;
        }
        let _ = std::fs::remove_file(self.paths.instance_pid(id));
        if !doomed.is_empty() {
            let forced = crate::proctree::reap(doomed).await;
            if forced > 0 {
                tracing::info!(id, forced, "强制结束了不响应退出信号的程序");
            }
        }
        Ok(())
    }

    /// 终端里运行着的程序：tmux 模式是会话里各面板的进程及其子孙（Linux 上加上带着会话标记、已脱离父子关系的），
    /// 直接运行时是 ttyd 的子孙（不含 ttyd 本身，它由 kill_ttyd 结束）。
    async fn task_processes(&self, id: &str) -> crate::proctree::Doomed {
        match self.backend {
            Backend::Tmux(_) => {
                let target = format!("{}:", exact(&Self::session_name(id)));
                let pids = match self.tmux(&["list-panes", "-s", "-t", &target, "-F", "#{pane_pid}"]).await {
                    Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).lines().filter_map(|l| l.trim().parse().ok()).collect(),
                    _ => vec![],
                };
                if pids.is_empty() {
                    return Default::default();
                }
                // `#{pid}`：tmux 服务进程号；`#{session_id}`：形如 `$5`
                let marker = match self.tmux(&["display-message", "-p", "-t", &target, "#{pid} #{session_id}"]).await {
                    Ok(out) if out.status.success() => parse_marker(&String::from_utf8_lossy(&out.stdout)),
                    _ => None,
                };
                crate::proctree::collect(&pids, marker)
            }
            Backend::Direct => {
                let Some(ttyd) = lock(&self.procs).get(id).and_then(|p| p.child.id()) else { return Default::default() };
                let mut d = crate::proctree::collect(&[ttyd], None);
                // Windows 上没有进程表，靠 `taskkill /T` 从 ttyd 往下结束整棵树
                if cfg!(unix) {
                    d.exclude(ttyd);
                }
                d
            }
        }
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
    ///
    /// Windows 上 ttyd 只运行很短的“接入会话”命令：它把要运行的命令拼进一块约 256 字节的固定缓冲区，
    /// 超长就溢出崩溃（安装目录、配置文件、工作目录都是完整路径，工作目录深一点、带中文或启动命令长一点
    /// 就会超）。会话由客户端在浏览器连上之前按完整参数创建
    /// （[`Instances::ensure_attachable`]），效果与 `new-session -A` 相同，也不经过 ttyd 的本地编码转换。
    fn command_for(&self, row: &InstanceRow) -> Result<Vec<String>, LocalError> {
        if cfg!(windows) {
            if let Backend::Tmux(tmux) = &self.backend {
                return Ok(vec![tmux.display().to_string(), "-L".into(), TMUX_SOCKET.into(), "attach-session".into(), "-t".into(), Self::session_name(&row.id)]);
            }
        }
        let (program, typed) = self.program_for(row)?;
        Ok(match &self.backend {
            Backend::Tmux(tmux) => {
                let mut v = vec![tmux.display().to_string()];
                v.extend(self.tmux_base_args());
                v.extend(["new-session", "-A", "-s", &Self::session_name(&row.id), "-c", &row.workdir].map(String::from));
                v.extend(self.session_env_args(row));
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
        self.program_with(row, None, None)
    }

    /// 同 [`Instances::program_for`]，带上新建时的第一句话：AI 工具接在启动命令后面作为参数
    /// （`claude '…'`、`codex '…'`、`opencode --prompt '…'`，都会以这句话开始一次交互式会话），普通终端把它当作启动后运行的命令。
    /// 要接着以前的对话时换成各工具接着的命令（[`Instances::resume_command`]）。
    fn program_with(&self, row: &InstanceRow, prompt: Option<&str>, resume: Option<&Resume>) -> Result<(Vec<String>, Option<String>), LocalError> {
        let shell = crate::shells::resolve(&row.shell, &Settings::load(&self.store).default_shell)?;
        if let Some(c) = self.container(&row.id) {
            let docker = crate::docker::binary().unwrap_or_else(|| "docker".into());
            let cmd = crate::docker::exec_command(&docker, &c, self.container_inner(row, &c, prompt).as_deref(), shell.family);
            return Ok(match shell.run(&cmd) {
                Some(v) => (v, None),
                None => (shell.interactive(), Some(cmd)),
            });
        }
        let launch = match prompt {
            None if resume.is_some() => resume.and_then(|r| self.resume_command(row, r)),
            None => self.launch_command(row),
            Some(p) => match row.launch.as_str() {
                "shell" => Some(p.to_string()),
                "opencode" => self.launch_command(row).map(|c| format!("{c} --prompt {}", crate::shells::quote_arg(p, shell.family))),
                "claude" | "codex" => self.launch_command(row).map(|c| format!("{c} {}", crate::shells::quote_arg(p, shell.family))),
                _ => self.launch_command(row),
            },
        };
        Ok(match launch.filter(|c| !c.trim().is_empty()) {
            None => (shell.interactive(), None),
            Some(cmd) => match shell.run(&self.model_wrap(row, &with_launch_program(row, &cmd, shell.family), shell.family)) {
                Some(v) => (v, None),
                None => (shell.interactive(), Some(cmd)),
            },
        })
    }

    /// 容器里运行的命令（POSIX 写法）：工具的启动命令（按容器里是不是 root 决定加不加 `IS_SANDBOX=1`）加上第一句话；
    /// 普通终端为 None（打开容器里的 shell，第一句话不用）。不注入 hooks、不加模型服务商环境（容器有自己的配置）。
    fn container_inner(&self, row: &InstanceRow, c: &crate::docker::Container, prompt: Option<&str>) -> Option<String> {
        let base = launch_command_line(row, self.opts(&row.id), c.root)?;
        Some(match (prompt, row.launch.as_str()) {
            (Some(p), "opencode") => format!("{base} --prompt {}", crate::docker::quote_inner(p)),
            (Some(p), "claude" | "codex") => format!("{base} {}", crate::docker::quote_inner(p)),
            _ => base,
        })
    }

    /// Claude Code / Codex 选了第三方模型服务商时，按 shell 的写法加上接口地址与 Key（见 model.rs）。
    /// 容器里的会话不加（命令已经整个包在 docker exec 里）。
    fn model_wrap(&self, row: &InstanceRow, cmd: &str, family: crate::shells::Family) -> String {
        if self.container(&row.id).is_some() {
            return cmd.to_string();
        }
        match crate::model::launch_env(&self.store, &self.paths.home, &row.launch) {
            Some(env) => env.wrap(cmd, family),
            None => cmd.to_string(),
        }
    }

    /// 启动类型对应的命令行（含“全部权限”参数）；普通终端为 None。
    fn launch_command(&self, row: &InstanceRow) -> Option<String> {
        let cmd = launch_command_line(row, self.opts(&row.id), crate::platform::is_root())?;
        match self.chat_settings(row) {
            Some(settings) => Some(with_chat(row, &cmd, &settings, None)),
            None => Some(cmd),
        }
    }

    /// 接着以前的对话的命令（含“全部权限”参数）：`claude --resume <id> [--fork-session]`、
    /// `codex resume|fork <id>`、`opencode --session <id>`。
    fn resume_command(&self, row: &InstanceRow, r: &Resume) -> Option<String> {
        let base = launch_command_line(row, self.opts(&row.id), crate::platform::is_root())?;
        let fork = if r.fork { " --fork-session" } else { "" };
        match row.launch.as_str() {
            "claude" => Some(match self.chat_settings(row) {
                Some(settings) => format!("{}{fork}", with_chat_hooks(&base, &row.id, &settings, Some(&r.id))),
                None => format!("{base} --resume {}{fork}", r.id),
            }),
            "codex" => {
                let sub = if r.fork { "fork" } else { "resume" };
                // 对话界面：照样带看看的 profile 和 --no-daemon
                if self.chat_settings(row).is_some() {
                    return Some(crate::chat::codex_launch::with_chat_sub(&base, &row.id, sub, &r.id));
                }
                let flags = base.strip_prefix("codex").unwrap_or("").trim();
                Some(format!("codex {sub}{}{} {}", if flags.is_empty() { "" } else { " " }, flags, r.id))
            }
            "opencode" => Some(format!("{base} --session {}", r.id)),
            _ => None,
        }
    }

    /// 带对话界面的终端：hooks 配置文件（Claude Code 的 `--settings` 文件、Codex 的 profile；顺便确保它已经写好）。
    /// 不是这类终端时为 None。
    fn chat_settings(&self, row: &InstanceRow) -> Option<PathBuf> {
        if !has_chat(&row.launch) || !self.opts(&row.id).chat_ui || !matches!(self.backend, Backend::Tmux(_)) {
            return None;
        }
        let written = match row.launch.as_str() {
            "codex" => crate::chat::codex_launch::ensure_profile(&self.paths),
            _ => crate::chat::hooks::ensure_settings(&self.paths),
        };
        match written {
            Ok(p) => Some(p),
            Err(e) => {
                tracing::warn!(error = %e, launch = %row.launch, "无法写入对话界面的 hooks 配置，按普通方式启动");
                None
            }
        }
    }

    /// 对话界面“以对话模式重新启动”：在面板的 shell 里接着某个会话启动 Claude Code / Codex（带 hooks、全部权限等原来的参数）。
    pub fn chat_relaunch_command(&self, id: &str, session: Option<&str>) -> Result<String, LocalError> {
        let row = self.store.instance(id)?.ok_or_else(LocalError::not_found)?;
        let settings = self.chat_settings(&row).ok_or_else(|| LocalError::new("CHAT_UNAVAILABLE"))?;
        let cmd = launch_command_line(&row, self.opts(id), crate::platform::is_root()).ok_or_else(|| LocalError::new("CHAT_UNAVAILABLE"))?;
        let cmd = with_chat(&row, &cmd, &settings, session);
        // 面板里是哪种 shell 就按哪种写法加环境变量
        let shell = crate::shells::resolve(&row.shell, &Settings::load(&self.store).default_shell)?;
        let cmd = with_launch_program(&row, &cmd, shell.family);
        Ok(self.model_wrap(&row, &crate::shells::adapt_for(&cmd, shell.family), shell.family))
    }

    // ---------------- tmux ----------------

    /// 新会话的环境：`PATH` 最前面加上随包 trzsz 的目录。tmux 服务早已在运行时（例如客户端升级后接着用原来的服务），
    /// 新会话用的是服务启动时的环境，光改我们自己进程的环境不够，要用 `new-session -e` 传进去。
    /// `-e` 要 tmux 3.2 以上，只对随包的 looklook-mux 使用；psmux 不确定是否支持，Windows 靠进程环境。
    fn session_env_args(&self, row: &InstanceRow) -> Vec<String> {
        let Backend::Tmux(tmux) = &self.backend else { return vec![] };
        if cfg!(windows) || self.paths.mux.as_ref() != Some(tmux) {
            return vec![];
        }
        let mut v = vec![];
        if let Some(p) = self.paths.session_path().and_then(|p| p.into_string().ok()) {
            v.extend(["-e".into(), format!("PATH={p}")]);
        }
        // 对话界面：面板里手动重开的 claude 也带着终端标识（启动命令上也加了，这里是给之后在 shell 里运行的）
        if has_chat(&row.launch) && self.opts(&row.id).chat_ui {
            v.extend(["-e".into(), format!("{}={}", crate::chat::hooks::INSTANCE_ENV, row.id)]);
        }
        v
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

    /// Windows：浏览器每次连上终端之前确保会话存在，不在就按完整参数新建（ttyd 只负责接入，见 `command_for`）。
    /// 与 tmux 的 `new-session -A` 一样，程序退出后重新打开终端会再运行一次启动命令。
    pub async fn ensure_attachable(&self, id: &str) {
        if !cfg!(windows) || !matches!(self.backend, Backend::Tmux(_)) {
            return;
        }
        let _g = self.op.lock().await;
        let Ok(Some(row)) = self.store.instance(id) else { return };
        if let Err(e) = self.ensure_session(&row).await {
            tracing::warn!(id, error = ?e, "无法创建终端会话");
        }
    }

    pub(crate) async fn ensure_session(&self, row: &InstanceRow) -> Result<(), LocalError> {
        let name = Self::session_name(&row.id);
        if self.tmux(&["has-session", "-t", &exact(&name)]).await?.status.success() {
            return Ok(());
        }
        // 第一句话只用一次：取出来，建会话失败也不再用（用户重新启动时就是普通启动）
        let prompt = lock(&self.first_prompt).remove(&row.id);
        let resume = lock(&self.first_resume).remove(&row.id);
        let (program, typed) = self.program_with(row, prompt.as_deref(), resume.as_ref())?;
        let mut args: Vec<String> = ["new-session", "-d", "-s", &name, "-c", &row.workdir, "-x", "120", "-y", "36"].map(String::from).to_vec();
        args.extend(self.session_env_args(row));
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

    // ---------------- 对话界面用的 tmux 操作（chat/mod.rs） ----------------

    /// `send-keys`：`keys` 是 tmux 的按键名（`Enter`、`Escape`、`C-j`、`2` …）。
    pub async fn send_keys(&self, id: &str, keys: &[&str]) -> Result<(), LocalError> {
        let target = format!("{}:", exact(&Self::session_name(id)));
        let mut args = vec!["send-keys", "-t", &target];
        args.extend_from_slice(keys);
        self.tmux_ok(&args).await
    }

    /// 按字面送出文字（不解释按键名）。`--` 不能少：以 `-` 开头的文字会被 tmux 当成选项。
    pub async fn send_literal(&self, id: &str, text: &str) -> Result<(), LocalError> {
        let target = format!("{}:", exact(&Self::session_name(id)));
        self.tmux_ok(&["send-keys", "-t", &target, "-l", "--", text]).await
    }

    /// 以 bracketed paste 粘贴（图片路径用这个，TUI 才会把它变成图片附件；超长文字也用这个）。
    pub async fn paste(&self, id: &str, text: &str) -> Result<(), LocalError> {
        let buffer = format!("ll_chat_{id}");
        let Backend::Tmux(tmux) = &self.backend else { return Err(LocalError::new("NO_TMUX")) };
        let mut cmd = Command::new(tmux);
        cmd.args(self.tmux_base_args()).args(["load-buffer", "-b", &buffer, "-"]).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::piped());
        self.apply_env(&mut cmd);
        #[cfg(windows)]
        cmd.creation_flags(CREATE_NO_WINDOW);
        let mut child = cmd.spawn().map_err(|e| LocalError::new("START_FAILED").with("detail", e.to_string()))?;
        if let Some(mut stdin) = child.stdin.take() {
            use tokio::io::AsyncWriteExt;
            stdin.write_all(text.as_bytes()).await?;
        }
        let out = tokio::time::timeout(Duration::from_secs(10), child.wait_with_output())
            .await
            .map_err(|_| LocalError::new("START_FAILED").with("detail", "tmux timeout"))??;
        if !out.status.success() {
            return Err(LocalError::new("START_FAILED").with("detail", String::from_utf8_lossy(&out.stderr).trim()));
        }
        let target = format!("{}:", exact(&Self::session_name(id)));
        self.tmux_ok(&["paste-buffer", "-p", "-d", "-b", &buffer, "-t", &target]).await
    }

    /// 当前可见的屏幕（不含回滚缓冲区），带颜色属性（判断灰色的推荐提示）。会话不在时返回 None。
    pub async fn screen(&self, id: &str) -> Option<String> {
        let Backend::Tmux(_) = self.backend else { return None };
        let target = format!("{}:", exact(&Self::session_name(id)));
        let out = self.tmux(&["capture-pane", "-p", "-e", "-t", &target]).await.ok()?;
        out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    }

    async fn tmux_ok(&self, args: &[&str]) -> Result<(), LocalError> {
        let out = self.tmux(args).await?;
        if out.status.success() {
            Ok(())
        } else {
            Err(LocalError::new("START_FAILED").with("detail", String::from_utf8_lossy(&out.stderr).trim()))
        }
    }

    /// 终端里最近的输出（tmux 面板与回滚缓冲区最后约 200 行）。非 tmux 后端或会话不在时返回 None。
    pub async fn recent_output(&self, id: &str) -> Option<String> {
        self.capture(id, "-200").await
    }

    /// “选择文字”看法用：最近约 3000 行输出（折行已接回原来的长行）。非 tmux 后端或会话不在时返回 None。
    pub async fn text(&self, id: &str) -> Option<String> {
        self.capture(id, "-3000").await
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
    let theme = if s.theme == "agent" {
        // AI 助手命令行风格：暖色的近黑背景、米白文字、陶土橙光标与强调色，ANSI 颜色压低饱和度，长时间看不累
        r##"{"background":"#1a1915","foreground":"#e8e4dc","cursor":"#d97757","cursorAccent":"#1a1915","selectionBackground":"#4a3f37","selectionForeground":"#faf8f3","black":"#24221e","red":"#e06c5a","green":"#8fbf7a","yellow":"#e3b35c","blue":"#7aa2d6","magenta":"#c48bc9","cyan":"#6fbfb5","white":"#d8d3c8","brightBlack":"#6e6a61","brightRed":"#ef8a76","brightGreen":"#a9d394","brightYellow":"#f0c97e","brightBlue":"#9dbde6","brightMagenta":"#d8a8dc","brightCyan":"#92d6cd","brightWhite":"#faf8f3"}"##
    } else if s.theme == "light" {
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

/// 对话界面：启动命令前加终端标识，后面加 hooks 配置（`--settings`），要接着某个会话时加 `--resume`。
/// 环境变量用 POSIX 写法，PowerShell / cmd 由 shells.rs 改成各自的写法。
/// 对话界面的启动命令：Claude Code 加 `--settings`，Codex 选用看看的 profile。
fn with_chat(row: &InstanceRow, cmd: &str, settings: &Path, resume: Option<&str>) -> String {
    match row.launch.as_str() {
        "codex" => crate::chat::codex_launch::with_chat(cmd, &row.id, resume),
        _ => with_chat_hooks(cmd, &row.id, settings, resume),
    }
}

/// 程序换成完整路径：
/// - Claude Code 不在 PATH 里（例如只有 Claude 桌面版自带的那一份）；
/// - Windows 上的 Codex 直接调 `codex.exe`：npm 装的 Codex 在 PowerShell 里先找到的是 `codex.ps1`，
///   默认的执行策略（Restricted）不让运行脚本，终端里只看到“无法加载文件 …codex.ps1”；`codex.cmd` 又会吃掉参数里的特殊字符。
fn with_launch_program(row: &InstanceRow, cmd: &str, family: crate::shells::Family) -> String {
    let program = match row.launch.as_str() {
        "claude" => crate::agents::claude_launch_program(),
        "codex" => crate::chat::codex_launch::program().map(|p| p.display().to_string()),
        _ => None,
    };
    match program {
        Some(p) => with_program(cmd, &row.launch, &p, family),
        None => cmd.to_string(),
    }
}

/// 把命令开头（跳过 `NAME=值` 这样的环境变量）的程序名换成完整路径，按 shell 的写法加引号。
/// 不是以这个程序名开头的命令原样返回。
fn with_program(cmd: &str, tool: &str, program: &str, family: crate::shells::Family) -> String {
    use crate::shells::Family;
    let mut prefix = String::new();
    let mut rest = cmd;
    while let Some((word, tail)) = rest.split_once(' ') {
        let Some((k, _)) = word.split_once('=') else { break };
        if k.is_empty() || !k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            break;
        }
        prefix.push_str(word);
        prefix.push(' ');
        rest = tail;
    }
    let Some(args) = rest.strip_prefix(tool).filter(|a| a.is_empty() || a.starts_with(' ')) else { return cmd.to_string() };
    let quoted = match family {
        Family::Pwsh => format!("& '{}'", program.replace('\'', "''")),
        Family::Cmd => format!("\"{program}\""),
        Family::Nu => format!("^'{program}'"),
        Family::Posix | Family::Other => format!("'{}'", program.replace('\'', "'\\''")),
    };
    format!("{prefix}{quoted}{args}")
}

pub fn with_chat_hooks(cmd: &str, id: &str, settings: &Path, resume: Option<&str>) -> String {
    let path = settings.display().to_string().replace('"', "");
    let resume = resume.filter(|s| s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')).map(|s| format!(" --resume {s}")).unwrap_or_default();
    format!("{}={id} {cmd} --settings \"{path}\"{resume}", crate::chat::hooks::INSTANCE_ENV)
}

/// agent 会话名最长多少个字符（太长的界面上也只显示一行）
const TITLE_MAX: usize = 80;

fn clip_title(t: &str) -> String {
    let t = t.split_whitespace().collect::<Vec<_>>().join(" ");
    if t.chars().count() <= TITLE_MAX {
        return t;
    }
    format!("{}…", t.chars().take(TITLE_MAX - 1).collect::<String>())
}

/// agent 设置的终端标题：去掉前面的状态符号（Claude Code 的 ✳、转圈的 ⠂ 等），OpenCode 的 `OC | ` 前缀；
/// 没设置过的（tmux 默认是主机名）、只是工具名或 shell 名、像路径和 `用户@主机:目录` 的提示符标题都不算。
fn clean_pane_title(title: &str, host: &str) -> Option<String> {
    let raw = title.trim();
    if raw.starts_with('/') || raw.starts_with('~') || (raw.contains('@') && raw.contains(':')) {
        return None;
    }
    let t = raw.trim_start_matches(|c: char| !c.is_alphanumeric()).trim();
    let t = t.strip_prefix("OC | ").unwrap_or(t).trim();
    let lower = t.to_lowercase();
    let host = host.trim().to_lowercase();
    const GENERIC: [&str; 13] = ["claude code", "claude", "codex", "openai codex", "opencode", "bash", "zsh", "sh", "fish", "pwsh", "powershell", "tmux", "node"];
    if t.is_empty() || lower == host || host.split('.').next() == Some(lower.as_str()) || GENERIC.contains(&lower.as_str()) {
        return None;
    }
    Some(t.to_string())
}

/// 没填名字时的默认名：带上文件夹名，一眼看出是哪个项目——`my-app · Claude Code`，普通终端就是 `my-app`；
/// 重名时加序号（`my-app · Claude Code 2`）。文件夹是主目录或根目录（名字说明不了什么）时用工具名加序号。
fn default_name(launch: &str, workdir: &str, taken: &HashSet<&str>) -> String {
    let tool = match launch {
        "codex" => Some("Codex"),
        "claude" => Some("Claude Code"),
        "opencode" => Some("OpenCode"),
        "dsh" => return "DSH".into(),
        _ => None,
    };
    let base = match (folder_name(workdir), tool) {
        (Some(dir), Some(tool)) => format!("{dir} · {tool}"),
        (Some(dir), None) => dir,
        (None, tool) => {
            let tool = tool.unwrap_or("终端");
            return (1..).map(|n| format!("{tool} {n}")).find(|n| !taken.contains(n.as_str())).unwrap_or_default();
        }
    };
    // 名字最长 40 个字符：太长的文件夹名截短，留出序号的位置
    let base: String = if base.chars().count() > 36 {
        match tool {
            Some(tool) => format!("{}… · {tool}", folder_name(workdir).unwrap_or_default().chars().take(35 - tool.chars().count() - 3).collect::<String>()),
            None => format!("{}…", base.chars().take(35).collect::<String>()),
        }
    } else {
        base
    };
    if !taken.contains(base.as_str()) {
        return base;
    }
    (2..).map(|n| format!("{base} {n}")).find(|n| !taken.contains(n.as_str())).unwrap_or(base)
}

/// 工作目录最后一级的名字；主目录、根目录（含 `C:\`）、空的返回 None。
fn folder_name(workdir: &str) -> Option<String> {
    let full = expand_home(workdir.trim());
    let trimmed = full.trim_end_matches(['/', '\\']);
    if trimmed.is_empty() || dirs::home_dir().is_some_and(|h| Path::new(trimmed) == h) {
        return None;
    }
    let name = trimmed.rsplit(['/', '\\']).next()?.trim();
    (!name.is_empty() && !name.ends_with(':') && name != "~").then(|| name.to_string())
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

/// `display-message -p "#{pid} #{session_id}"` 的输出 → (tmux 服务进程号, 会话号)。
fn parse_marker(s: &str) -> Option<(u32, u32)> {
    let mut it = s.split_whitespace();
    let server = it.next()?.parse().ok()?;
    let session = it.next()?.strip_prefix('$')?.parse().ok()?;
    Some((server, session))
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
    #[test]
    fn pane_titles_from_agents() {
        assert_eq!(super::clean_pane_title("✳ 修登录页", "box").as_deref(), Some("修登录页"));
        assert_eq!(super::clean_pane_title("⠂ Fix the login page", "box").as_deref(), Some("Fix the login page"));
        assert_eq!(super::clean_pane_title("OC | 重构设置页", "box").as_deref(), Some("重构设置页"));
        for t in ["box", "box.local", "✳ Claude Code", "codex", "~/w/app", "/home/a", "me@box:~/w", ""] {
            assert_eq!(super::clean_pane_title(t, "box.local"), None, "{t}");
        }
        assert_eq!(super::clip_title(&"字".repeat(100)).chars().count(), 80);
    }

    use super::*;

    #[test]
    fn tmux_session_marker() {
        assert_eq!(parse_marker("4321 $7\n"), Some((4321, 7)));
        assert_eq!(parse_marker("4321 7"), None);
        assert_eq!(parse_marker(""), None);
    }


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

    /// 第一句话只在第一次建会话时用：普通终端把它当作启动后运行的命令，会话重建后不会再运行。
    /// 需要能用的 tmux（随包的或系统的），默认不跑：`cargo test -- --ignored`。
    #[tokio::test]
    #[ignore]
    async fn first_prompt_runs_once() {
        let home = tempfile::tempdir().unwrap();
        let paths = Paths::discover(Some(home.path().to_path_buf())).unwrap();
        let inst = Instances::new(Arc::new(Store::memory().unwrap()), paths);
        assert!(matches!(inst.backend, Backend::Tmux(_)), "没有 tmux");
        let input = Input {
            workdir: Some(home.path().display().to_string()),
            launch: Some("shell".into()),
            shell: Some("bash".into()),
            prompt: Some("echo \"first-$((40+2))\" 'it'\\''s'".into()),
            ..Default::default()
        };
        let id = inst.create(input).await.unwrap().row.id;
        let row = inst.store.instance(&id).unwrap().unwrap();
        inst.ensure_session(&row).await.unwrap();
        tokio::time::sleep(Duration::from_millis(1500)).await;
        let out = inst.recent_output(&id).await.expect("会话应该在");
        assert!(out.contains("first-42 it's"), "第一句话没有运行：{out}");
        let name = exact(&Instances::session_name(&id));
        inst.tmux(&["kill-session", "-t", &name]).await.unwrap();
        inst.ensure_session(&row).await.unwrap();
        tokio::time::sleep(Duration::from_millis(1500)).await;
        let out = inst.recent_output(&id).await.expect("会话应该在");
        assert!(!out.contains("first-42"), "会话重建后又运行了第一句话：{out}");
        let _ = inst.tmux(&["kill-server"]).await;
    }

    #[tokio::test]
    async fn resume_commands() {
        let home = tempfile::tempdir().unwrap();
        let paths = Paths::discover(Some(home.path().to_path_buf())).unwrap();
        let inst = Instances::new(Arc::new(Store::memory().unwrap()), paths);
        if !matches!(inst.backend, Backend::Tmux(_)) {
            return;
        }
        let id = "019a-x_1";
        let mk = |launch: &str, full: bool, fork: bool| Input {
            workdir: Some(home.path().display().to_string()),
            launch: Some(launch.into()),
            full_access: Some(full),
            root_confirmed: Some(true),
            chat_ui: Some(false),
            resume: Some(Resume { id: id.into(), fork }),
            ..Default::default()
        };
        let cmd = |v: View, fork: bool| inst.resume_command(&v.row, &Resume { id: id.into(), fork }).unwrap();
        let v = inst.create(mk("codex", true, false)).await.unwrap();
        assert_eq!(cmd(v, false), "codex resume --yolo 019a-x_1");
        let v = inst.create(mk("codex", false, true)).await.unwrap();
        assert_eq!(cmd(v, true), "codex fork 019a-x_1");
        let v = inst.create(mk("claude", false, true)).await.unwrap();
        assert_eq!(inst.store.get::<String>(&resume_key(&v.row.id)).unwrap(), None, "复制的一份不算接着原来的");
        assert_eq!(cmd(v, true), "claude --resume 019a-x_1 --fork-session");
        let v = inst.create(mk("opencode", false, false)).await.unwrap();
        assert_eq!(inst.store.get::<String>(&resume_key(&v.row.id)).unwrap().as_deref(), Some(id));
        assert_eq!(cmd(v, false), "opencode --session 019a-x_1");
        assert!(inst.create(mk("opencode", false, true)).await.is_err(), "OpenCode 不能复制一份");
        assert!(inst.create(mk("shell", false, false)).await.is_err());
        let mut bad = mk("claude", false, false);
        bad.resume = Some(Resume { id: "x; rm -rf ~".into(), fork: false });
        assert!(inst.create(bad).await.is_err());
        let mut both = mk("claude", false, false);
        both.prompt = Some("hi".into());
        assert!(inst.create(both).await.is_err());
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
        let on = Opts { full_access: true, root_confirmed: true, web_port: None, chat_ui: false, name_custom: false };
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
        let mut o = Opts { full_access: true, root_confirmed: true, web_port: None, chat_ui: false, name_custom: false };
        Instances::validate_opts(&r, &mut o).unwrap();
        assert!(o == Opts::default(), "shell 终端忽略全部权限选项");
        r.launch = "claude".into();
        let mut o = Opts { full_access: false, root_confirmed: true, web_port: None, chat_ui: false, name_custom: false };
        Instances::validate_opts(&r, &mut o).unwrap();
        assert!(!o.root_confirmed);
        let mut o = Opts { full_access: true, root_confirmed: false, web_port: None, chat_ui: false, name_custom: false };
        assert_eq!(Instances::validate_opts(&r, &mut o).is_err(), crate::platform::is_root());
        r.launch = "dsh".into();
        let mut o = Opts { full_access: true, root_confirmed: true, web_port: None, chat_ui: false, name_custom: false };
        assert!(Instances::validate_opts(&r, &mut o).is_err(), "DSH 必须有网页端口");
        let mut o = Opts { full_access: true, root_confirmed: true, web_port: Some(3080), chat_ui: false, name_custom: false };
        Instances::validate_opts(&r, &mut o).unwrap();
        assert!(o == Opts { web_port: Some(3080), ..Opts::default() }, "DSH 没有全部权限选项");
    }

    #[test]
    fn claude_by_full_path() {
        use crate::shells::Family;
        let p = r"C:\Users\a\AppData\Roaming\Claude\claude-code\2.1.299\f0584be0fbaf\claude.exe";
        let cmd = "LOOKLOOK_INSTANCE=abcd2345 claude --settings \"C:/x/claude-hooks.json\"";
        assert_eq!(with_program(cmd, "claude", p, Family::Pwsh), format!("LOOKLOOK_INSTANCE=abcd2345 & '{p}' --settings \"C:/x/claude-hooks.json\""));
        // 环境变量前缀之后的程序名换成路径，换完再由 adapt_for 改成 PowerShell 的写法
        assert_eq!(
            crate::shells::adapt_for(&with_program(cmd, "claude", p, Family::Pwsh), Family::Pwsh),
            format!("$env:LOOKLOOK_INSTANCE='abcd2345'; & '{p}' --settings \"C:/x/claude-hooks.json\"")
        );
        assert_eq!(with_program("IS_SANDBOX=1 claude --dangerously-skip-permissions", "claude", "/opt/it's/claude", Family::Posix), "IS_SANDBOX=1 '/opt/it'\\''s/claude' --dangerously-skip-permissions");
        assert_eq!(with_program("claude", "claude", p, Family::Cmd), format!("\"{p}\""));
        assert_eq!(with_program("claudette --x", "claude", p, Family::Pwsh), "claudette --x");
        assert_eq!(with_program("codex --yolo", "claude", p, Family::Pwsh), "codex --yolo");
    }

    #[test]
    fn chat_hooks_wrap_the_launch_command() {
        let p = Path::new("/home/a/.local/share/looklook/run/claude-hooks.json");
        assert_eq!(
            with_chat_hooks("IS_SANDBOX=1 claude --dangerously-skip-permissions", "abcd2345", p, None),
            "LOOKLOOK_INSTANCE=abcd2345 IS_SANDBOX=1 claude --dangerously-skip-permissions --settings \"/home/a/.local/share/looklook/run/claude-hooks.json\""
        );
        assert!(with_chat_hooks("claude", "x", p, Some("96e9b84c-16d9-4bc7-ab76-f92861e5f141")).ends_with(" --resume 96e9b84c-16d9-4bc7-ab76-f92861e5f141"));
        assert!(!with_chat_hooks("claude", "x", p, Some("x; rm -rf /")).contains("rm"));
        let mut r = row();
        r.launch = "claude".into();
        let mut o = Opts { chat_ui: true, ..Opts::default() };
        Instances::validate_opts(&r, &mut o).unwrap();
        assert!(o.chat_ui);
        r.launch = "codex".into();
        Instances::validate_opts(&r, &mut o).unwrap();
        assert!(o.chat_ui, "Codex 也有对话界面");
        r.launch = "opencode".into();
        Instances::validate_opts(&r, &mut o).unwrap();
        assert!(!o.chat_ui, "OpenCode 没有对话界面");
    }

    #[test]
    fn names_and_paths() {
        let none = HashSet::new();
        assert_eq!(default_name("claude", "/srv/my-app", &none), "my-app · Claude Code");
        assert_eq!(default_name("shell", "/srv/my-app/", &none), "my-app");
        assert_eq!(default_name("codex", r"D:\code\web", &none), "web · Codex");
        assert_eq!(default_name("dsh", "/srv/my-app", &none), "DSH");
        let taken = HashSet::from(["my-app · Claude Code", "my-app · Claude Code 2", "Claude Code 1"]);
        assert_eq!(default_name("claude", "/srv/my-app", &taken), "my-app · Claude Code 3");
        // 主目录、根目录：用工具名加序号
        assert_eq!(default_name("claude", "~", &taken), "Claude Code 2");
        assert_eq!(default_name("shell", "/", &none), "终端 1");
        assert_eq!(default_name("shell", r"C:\", &none), "终端 1");
        let long = default_name("claude", &format!("/srv/{}", "x".repeat(60)), &none);
        assert!(long.chars().count() <= 36 && long.ends_with("… · Claude Code"), "{long}");
        assert!(Instances::is_reserved_port(41500));
        assert!(!Instances::is_reserved_port(5173));
        if let Some(h) = dirs::home_dir() {
            assert_eq!(expand_home("~/x"), format!("{}/x", h.display()));
        }
    }
}
