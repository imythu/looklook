//! 检查更新与一键更新。
//!
//! - 自动检查：启动后不久查一次，之后按平台给的间隔（至少 1 小时，最多 1 天）定期查；心跳提示有新版本时立即再查。
//! - 手动更新：用户在管理台点“更新”后才下载。安装包来自平台 `/updates/latest` 给出的地址，校验大小与 SHA-256，
//!   解压到数据目录的 `updates/`，然后逐个替换安装目录里的文件：旧文件先改名为 `*.old`（运行中的程序也能改名），
//!   新文件写好再改名到位；中途出错就把改名的文件挪回去。会话保持程序例外，见 `backup_name`。
//! - 重启：由 systemd（`INVOCATION_ID`）或 launchd（`XPC_SERVICE_NAME`）管着时以非零状态退出，让它们用新文件拉起；
//!   否则（Windows 托盘、手动运行）先启动新版本再退出，新版本等旧进程释放单实例锁后接手。
//!   终端里的会话（looklook-mux）不受影响，新版本启动后自动接回。
//! - 提醒：界面显示角标/横幅；用户关掉后同一个版本不再提醒（记在本机数据库，手机和电脑上都一样），有更新的版本时再提醒。
//! - 自动更新（设置里的“自动更新”，默认开）：有新版本、且已经 10 分钟没有打开的终端时自动安装并重启，
//!   不会在用户正在用终端时打断。同一个版本自动安装失败后不再自动重试（直到出了更新的版本或客户端重启），可以手动更新。
//! - 更新完成：新版本启动后界面提示“已更新到 x.y.z”并可查看更新说明（包括用安装包手动更新的情况）。

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use crate::account::{Account, VERSION};
use crate::error::LocalError;
use crate::paths::{Paths, MUX_EXE};
use crate::store::Store;

const DISMISSED_KEY: &str = "update_dismissed";
/// 上次运行的版本（发现版本变了就提示“已更新”）
const LAST_VERSION_KEY: &str = "last_version";
/// 刚更新完的提示：`JustUpdated`，用户看过后删除
const JUST_UPDATED_KEY: &str = "just_updated";
/// 没有打开的终端多久之后才自动更新
const AUTO_IDLE_MS: i64 = 10 * 60 * 1000;
/// 安装包大小上限（防止异常的下载占满磁盘）
const MAX_PACKAGE: u64 = 512 * 1024 * 1024;
/// 新进程等旧进程退出（释放单实例锁）的环境变量
pub const RESTART_ENV: &str = "LOOKLOOK_RESTARTED";
/// 被 systemd / launchd 管着时退出用的状态码（非零：让它们重新拉起）
const RESTART_EXIT_CODE: i32 = 75;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Package {
    pub package: String,
    pub url: String,
    pub sha256: String,
    pub size_bytes: u64,
}

/// 平台 `/updates/latest` 的结果。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Latest {
    #[serde(default)]
    pub available: bool,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub published_at: Option<String>,
    #[serde(default)]
    pub packages: Vec<Package>,
    #[serde(default)]
    pub check_interval_seconds: Option<u64>,
}

/// 更新任务的进度。
#[derive(Debug, Clone, Default, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Job {
    #[default]
    Idle,
    Downloading {
        version: String,
        done: u64,
        total: Option<u64>,
    },
    Installing {
        version: String,
    },
    Restarting {
        version: String,
    },
    Failed {
        code: String,
        detail: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct JustUpdated {
    from: String,
    to: String,
    #[serde(default)]
    notes: String,
    #[serde(default)]
    auto: bool,
}

static OPEN_TERMINALS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
/// 最近一次有终端连着的时刻（毫秒）
static TERMINALS_SEEN: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);

fn touch_terminals() {
    TERMINALS_SEEN.store(crate::util::system_ms(), std::sync::atomic::Ordering::Relaxed);
}

/// 一条打开着的终端连接（浏览器里的终端页）。连着时不自动更新。
pub struct TerminalGuard;

impl TerminalGuard {
    pub fn new() -> Self {
        OPEN_TERMINALS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        touch_terminals();
        Self
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        OPEN_TERMINALS.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        touch_terminals();
    }
}

/// 已经多久没有打开的终端了（毫秒）；有终端连着时为 None。
pub(crate) fn terminals_idle_ms() -> Option<i64> {
    (OPEN_TERMINALS.load(std::sync::atomic::Ordering::Relaxed) == 0).then(|| crate::util::system_ms() - TERMINALS_SEEN.load(std::sync::atomic::Ordering::Relaxed))
}

pub struct Updater {
    store: Arc<Store>,
    paths: Paths,
    account: Arc<Account>,
    shutdown: CancellationToken,
    latest: Mutex<Option<Latest>>,
    checked_at: Mutex<Option<String>>,
    check_error: Mutex<Option<String>>,
    job: Mutex<Job>,
    locale: Mutex<String>,
    /// 已经自动安装过（成功会重启，所以留下来的都是失败的）的版本
    auto_tried: Mutex<Option<String>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// 本平台用的安装包格式。
fn package_kind() -> &'static str {
    if cfg!(windows) {
        "zip"
    } else {
        "tar.gz"
    }
}

/// 启动时的程序路径。替换文件后，Linux 上 `current_exe()` 指向的是改名后被删掉的旧文件，重启要用原来的路径。
static EXE: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();

pub fn exe_path() -> Option<PathBuf> {
    EXE.get_or_init(|| std::env::current_exe().and_then(|p| p.canonicalize()).ok()).clone()
}

/// 随包资源（终端服务、字体等）放在哪：一般和程序同目录，macOS 应用包里是 `Contents/Resources`。
fn resource_dir(exe_dir: &Path) -> PathBuf {
    crate::paths::bundle_resources(exe_dir).unwrap_or_else(|| exe_dir.to_path_buf())
}

/// 能不能自动更新：只有从完整安装包安装的程序可以（安装目录里有随包的终端服务程序）。
/// 返回程序所在目录，或者不能更新的原因（界面据此提示去下载页手动更新）。
pub fn install_dir() -> Result<PathBuf, &'static str> {
    if cfg!(debug_assertions) && std::env::var_os("LOOKLOOK_UPDATE_DEV").is_none() {
        return Err("dev");
    }
    let exe = exe_path().ok_or("unsupported")?;
    let dir = exe.parent().ok_or("unsupported")?.to_path_buf();
    let res = resource_dir(&dir);
    if !res.join(crate::paths::TERM_EXE).is_file() {
        return Err("unsupported");
    }
    // 能不能写（例如以 root 安装到 /opt 却用普通用户运行）
    for d in [&dir, &res] {
        let probe = d.join(".looklook-write-test");
        if std::fs::write(&probe, b"").is_err() {
            return Err("readonly");
        }
        let _ = std::fs::remove_file(&probe);
    }
    Ok(dir)
}

impl Updater {
    pub fn new(store: Arc<Store>, paths: Paths, account: Arc<Account>, shutdown: CancellationToken) -> Arc<Self> {
        Arc::new(Self {
            store,
            paths,
            account,
            shutdown,
            latest: Mutex::new(None),
            checked_at: Mutex::new(None),
            check_error: Mutex::new(None),
            job: Mutex::new(Job::Idle),
            locale: Mutex::new("zh-CN".into()),
            auto_tried: Mutex::new(None),
        })
        .tap_version()
    }

    /// 启动时记下版本；和上次运行的不一样（手动装了新版本）时也提示“已更新”。
    fn tap_version(self: Arc<Self>) -> Arc<Self> {
        let last: Option<String> = self.store.get(LAST_VERSION_KEY).ok().flatten();
        if last.as_deref() != Some(VERSION) {
            let noted = self.just_updated().is_some();
            if let (Some(from), false) = (last, noted) {
                let _ = self.store.set(JUST_UPDATED_KEY, &JustUpdated { from, to: VERSION.into(), notes: String::new(), auto: false });
            }
            let _ = self.store.set(LAST_VERSION_KEY, &VERSION.to_string());
        }
        self
    }

    /// 刚更新到当前版本的提示（还没被看过）。
    fn just_updated(&self) -> Option<JustUpdated> {
        self.store.get::<JustUpdated>(JUST_UPDATED_KEY).ok().flatten().filter(|j| j.to == VERSION)
    }

    /// 用户看过“已更新”提示。
    pub fn seen(&self) -> Result<(), LocalError> {
        self.store.remove(JUST_UPDATED_KEY)?;
        Ok(())
    }

    /// 界面语言（更新说明按它取中文或英文）
    pub fn set_locale(&self, locale: &str) {
        let l = if locale == "en-US" { "en-US" } else { "zh-CN" };
        let changed = *lock(&self.locale) != l;
        *lock(&self.locale) = l.to_string();
        if changed {
            *lock(&self.latest) = None;
        }
    }

    /// 向平台查询最新版本。
    pub async fn check(&self) -> Result<Latest, LocalError> {
        let locale = lock(&self.locale).clone();
        let path = format!("/updates/latest?target={}&channel=stable&current={VERSION}&locale={locale}", crate::paths::target());
        let r: Result<Latest, _> = self.account.platform().public(&path).await;
        *lock(&self.checked_at) = Some(crate::util::now_rfc3339());
        match r {
            Ok(l) => {
                *lock(&self.check_error) = None;
                *lock(&self.latest) = Some(l.clone());
                Ok(l)
            }
            Err(e) => {
                *lock(&self.check_error) = Some(e.code().to_string());
                Err(e.into())
            }
        }
    }

    /// 后台定期检查。
    pub async fn run(self: Arc<Self>) {
        cleanup_leftovers(&self.paths);
        // 刚启动时算作有人在用：自动更新至少等启动后 10 分钟
        touch_terminals();
        tokio::time::sleep(Duration::from_secs(20)).await;
        let mut next = tokio::time::Instant::now();
        loop {
            // 心跳说有新版本、而上次检查结果还不是它：马上再查
            let hinted = self.account.session().and_then(|s| s.update).map(|u| u.version);
            let known = lock(&self.latest).as_ref().map(|l| l.version.clone());
            let stale = hinted.is_some() && hinted != known;
            if tokio::time::Instant::now() >= next || stale || known.is_none() && lock(&self.check_error).is_none() {
                let interval = match self.check().await {
                    Ok(l) => l.check_interval_seconds.unwrap_or(6 * 3600).clamp(3600, 86400),
                    Err(e) => {
                        tracing::debug!(code = %e.code, "检查更新失败");
                        1800
                    }
                };
                next = tokio::time::Instant::now() + Duration::from_secs(interval);
            }
            if let Some(version) = self.auto_due() {
                tracing::info!(%version, "空闲中，自动安装新版本");
                *lock(&self.auto_tried) = Some(version);
                if let Err(e) = self.start_install_as(true) {
                    tracing::warn!(code = %e.code, "无法自动更新");
                }
            }
            tokio::time::sleep(Duration::from_secs(60)).await;
        }
    }

    /// 现在该不该自动更新：开了自动更新、有本平台的新版本、可以就地更新、这个版本还没自动装过，且终端空闲够久。
    fn auto_due(&self) -> Option<String> {
        if !crate::settings::Settings::load(&self.store).auto_update || self.busy() {
            return None;
        }
        let l = lock(&self.latest).clone().filter(|l| l.available && l.packages.iter().any(|p| p.package == package_kind()))?;
        if lock(&self.auto_tried).as_deref() == Some(l.version.as_str()) || install_dir().is_err() {
            return None;
        }
        terminals_idle_ms().filter(|ms| *ms >= AUTO_IDLE_MS).map(|_| l.version)
    }

    pub fn dismiss(&self, version: &str) -> Result<(), LocalError> {
        self.store.set(DISMISSED_KEY, &version.to_string())?;
        Ok(())
    }

    /// 给界面的状态（`/api/status` 的 `update` 字段）。
    pub fn view(&self) -> Value {
        let latest = lock(&self.latest).clone();
        let dismissed: Option<String> = self.store.get(DISMISSED_KEY).ok().flatten();
        let can = install_dir();
        let available = latest.as_ref().is_some_and(|l| l.available);
        let version = latest.as_ref().map(|l| l.version.clone());
        let has_package = latest.as_ref().is_some_and(|l| l.packages.iter().any(|p| p.package == package_kind()));
        json!({
            "current": VERSION,
            "available": available,
            "version": version,
            "required": latest.as_ref().is_some_and(|l| l.required),
            "notes": latest.as_ref().map(|l| l.notes.clone()).unwrap_or_default(),
            "published_at": latest.as_ref().and_then(|l| l.published_at.clone()),
            "checked_at": lock(&self.checked_at).clone(),
            "check_error": lock(&self.check_error).clone(),
            "dismissed": available && dismissed.is_some() && dismissed == version,
            "can_install": can.is_ok() && has_package,
            "cannot_reason": match (&can, has_package) {
                (Err(r), _) => Some(*r),
                (Ok(_), false) if available => Some("no_package"),
                _ => None,
            },
            "job": &*lock(&self.job),
            "auto": crate::settings::Settings::load(&self.store).auto_update,
            "just_updated": self.just_updated().map(|j| json!({ "from": j.from, "notes": j.notes, "auto": j.auto })),
        })
    }

    fn set_job(&self, j: Job) {
        *lock(&self.job) = j;
    }

    fn busy(&self) -> bool {
        matches!(*lock(&self.job), Job::Downloading { .. } | Job::Installing { .. } | Job::Restarting { .. })
    }

    /// 开始更新（后台进行，进度见 `view().job`）。
    pub fn start_install(self: &Arc<Self>) -> Result<(), LocalError> {
        self.start_install_as(false)
    }

    fn start_install_as(self: &Arc<Self>, auto: bool) -> Result<(), LocalError> {
        if self.busy() {
            return Err(LocalError::new("UPDATE_BUSY"));
        }
        let dir = install_dir().map_err(|r| LocalError::new("UPDATE_UNSUPPORTED").with("reason", r))?;
        self.set_job(Job::Downloading { version: String::new(), done: 0, total: None });
        let me = self.clone();
        tokio::spawn(async move {
            match me.install(&dir, auto).await {
                Ok(version) => {
                    tracing::info!(%version, "更新已安装，正在重启");
                    me.set_job(Job::Restarting { version });
                    request_restart();
                    // 给界面一点时间看到“正在重启”
                    tokio::time::sleep(Duration::from_millis(800)).await;
                    me.shutdown.cancel();
                }
                Err(e) => {
                    let detail = e.params.get("detail").and_then(Value::as_str).unwrap_or_default().to_string();
                    tracing::warn!(code = %e.code, %detail, "更新失败");
                    me.set_job(Job::Failed { code: e.code, detail });
                }
            }
        });
        Ok(())
    }

    async fn install(&self, dir: &Path, auto: bool) -> Result<String, LocalError> {
        let latest = self.check().await?;
        if !latest.available {
            return Err(LocalError::new("UPDATE_NONE"));
        }
        let pkg = latest.packages.iter().find(|p| p.package == package_kind()).cloned().ok_or_else(|| LocalError::new("UPDATE_UNSUPPORTED").with("reason", "no_package"))?;
        let version = latest.version.clone();
        if !looks_like_version(&version) || pkg.sha256.len() != 64 {
            return Err(LocalError::new("UPDATE_FAILED").with("detail", "bad release info"));
        }
        let work = self.paths.home.join("updates");
        let _ = std::fs::remove_dir_all(&work);
        std::fs::create_dir_all(&work)?;
        let archive = work.join(format!("looklook-{version}.{}", package_kind()));
        self.set_job(Job::Downloading { version: version.clone(), done: 0, total: Some(pkg.size_bytes).filter(|n| *n > 0) });
        let sha = self
            .account
            .platform()
            .download(&pkg.url, &archive, MAX_PACKAGE, |done, total| {
                *lock(&self.job) = Job::Downloading { version: version.clone(), done, total: total.or(Some(pkg.size_bytes).filter(|n| *n > 0)) };
            })
            .await
            .map_err(|e| LocalError::new("UPDATE_DOWNLOAD_FAILED").with("detail", e.to_string()))?;
        if !sha.eq_ignore_ascii_case(&pkg.sha256) {
            return Err(LocalError::new("UPDATE_CHECKSUM"));
        }
        self.set_job(Job::Installing { version: version.clone() });
        let staged = work.join("staged");
        let (archive2, staged2) = (archive.clone(), staged.clone());
        tokio::task::spawn_blocking(move || extract(&archive2, &staged2)).await.map_err(|e| LocalError::from(anyhow::anyhow!(e)))??;
        let src = staged.join("looklook");
        let exe = if cfg!(windows) { "looklook.exe" } else { "looklook" };
        if !src.join(exe).is_file() || !src.join(crate::paths::TERM_EXE).is_file() {
            return Err(LocalError::new("UPDATE_FAILED").with("detail", "incomplete package"));
        }
        let (src2, dir2) = (src.clone(), dir.to_path_buf());
        tokio::task::spawn_blocking(move || apply(&src2, &dir2)).await.map_err(|e| LocalError::from(anyhow::anyhow!(e)))??;
        let _ = std::fs::remove_dir_all(&work);
        // 运行文件自校验的基准随新版本重新记录（否则新版本启动时会报“运行文件被改过”）
        let _ = self.store.remove("self_hash");
        // 新版本启动后提示“已更新”，带上这次的更新说明
        let _ = self.store.set(JUST_UPDATED_KEY, &JustUpdated { from: VERSION.into(), to: version.clone(), notes: latest.notes.clone(), auto });
        Ok(version)
    }
}

fn looks_like_version(v: &str) -> bool {
    !v.is_empty() && v.len() <= 64 && v.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+'))
}

fn fail(detail: impl std::fmt::Display) -> LocalError {
    LocalError::new("UPDATE_FAILED").with("detail", detail.to_string())
}

/// 解压安装包。Linux / macOS 自带 tar；Windows 10 起自带的 tar.exe（bsdtar）也能解 zip，不行再用 PowerShell。
fn extract(archive: &Path, to: &Path) -> Result<(), LocalError> {
    std::fs::create_dir_all(to)?;
    let mut tar = std::process::Command::new("tar");
    tar.arg(if cfg!(windows) { "-xf" } else { "-xzf" }).arg(archive).arg("-C").arg(to);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        tar.creation_flags(0x0800_0000);
    }
    let ok = tar.output().map(|o| o.status.success()).unwrap_or(false);
    if ok {
        return Ok(());
    }
    if cfg!(windows) {
        let script = format!(
            "Expand-Archive -LiteralPath '{}' -DestinationPath '{}' -Force",
            archive.display().to_string().replace('\'', "''"),
            to.display().to_string().replace('\'', "''")
        );
        let mut ps = std::process::Command::new("powershell");
        ps.args(["-NoProfile", "-NonInteractive", "-Command", &script]);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            ps.creation_flags(0x0800_0000);
        }
        if ps.output().map(|o| o.status.success()).unwrap_or(false) {
            return Ok(());
        }
    }
    Err(fail("extract failed"))
}

/// 换掉的旧文件名：`looklook` → `looklook.old`；Windows 上旧的 `.old` 还被占用（上一个版本还在跑）删不掉时换个名字。
/// 会话保持程序放进单独的目录、保留原名（`psmux.exe.oldXXXX\psmux.exe`）：psmux 按运行中程序的文件名认自己的会话服务，
/// 改名成 `psmux.exe.old` 后，更新前就在跑的会话服务会被当成已经退出，终端再也打不开。
fn backup_name(dest: &Path) -> PathBuf {
    let name = dest.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    if name == MUX_EXE {
        return mux_backup_dir(dest).join(&name);
    }
    let first = dest.with_file_name(format!("{name}.old"));
    let _ = remove_any(&first);
    if !first.exists() {
        return first;
    }
    dest.with_file_name(format!("{name}.old{}", crate::util::random_id(4)))
}

fn mux_backup_dir(mux: &Path) -> PathBuf {
    mux.with_file_name(format!("{MUX_EXE}.old{}", crate::util::random_id(4)))
}

/// 1.3.1 及以前的更新把还在运行的会话保持程序改名成了 `psmux.exe.old*`，psmux 因此认不出这些会话服务，终端打不开。
/// 启动时（第一次调用会话保持程序之前）把删不掉（还在运行）的这种文件挪进单独的目录、改回原名，会话服务就又能认出来了。
pub fn rescue_mux_backups() {
    let Ok(dir) = install_dir() else { return };
    rescue_mux_backups_in(&resource_dir(&dir));
}

fn rescue_mux_backups_in(dir: &Path) {
    let prefix = format!("{MUX_EXE}.old");
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let stale: Vec<PathBuf> = entries.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.is_file() && p.file_name().is_some_and(|n| n.to_string_lossy().starts_with(&prefix))).collect();
    for path in stale {
        if std::fs::remove_file(&path).is_ok() {
            continue;
        }
        let to = mux_backup_dir(&path);
        let moved = std::fs::create_dir(&to).and_then(|_| std::fs::rename(&path, to.join(MUX_EXE)));
        match moved {
            Ok(()) => tracing::info!(from = %path.display(), to = %to.display(), "旧版本的会话保持程序还在运行，已恢复原名"),
            Err(e) => tracing::warn!(path = %path.display(), error = %e, "无法恢复旧版本会话保持程序的名字"),
        }
    }
}

fn remove_any(p: &Path) -> std::io::Result<()> {
    if p.is_dir() {
        std::fs::remove_dir_all(p)
    } else {
        std::fs::remove_file(p)
    }
}

fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    if from.is_dir() {
        std::fs::create_dir_all(to)?;
        for e in std::fs::read_dir(from)? {
            let e = e?;
            copy_tree(&e.path(), &to.join(e.file_name()))?;
        }
        Ok(())
    } else {
        std::fs::copy(from, to).map(|_| ())
    }
}

/// Windows 上改名偶尔会被别的程序挡住（拒绝访问 / 文件正被使用），例如杀毒软件正在扫描刚复制好的程序，
/// 一般几秒内就放开了：重试一会儿。
fn rename_retry(from: &Path, to: &Path) -> std::io::Result<()> {
    let mut tries = 0;
    loop {
        match std::fs::rename(from, to) {
            Err(e) if cfg!(windows) && tries < 20 && (e.kind() == std::io::ErrorKind::PermissionDenied || e.raw_os_error() == Some(32)) => {
                tries += 1;
                std::thread::sleep(std::time::Duration::from_millis(250));
            }
            r => return r,
        }
    }
}

/// 把解压出来的文件放进安装目录。每一项：旧的改名备份 → 新的放到位；出错时撤销已做的替换。
/// 文件先复制成临时名再改名到位。目录不这样做：Windows 上目录里只要有文件被打开（杀毒软件在扫描刚复制的程序），
/// 整个目录就不能改名（拒绝访问），所以旧目录挪开后直接复制到原位置。
fn apply(src: &Path, dir: &Path) -> Result<(), LocalError> {
    let res = resource_dir(dir);
    let bundled = res != dir;
    let mut items: Vec<PathBuf> = std::fs::read_dir(src)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        // 应用包由 .dmg 安装，不需要压缩包里的安装/卸载脚本
        .filter(|p| !bundled || p.extension().is_none_or(|e| e != "sh"))
        .collect();
    // 主程序最后换：前面任何一步失败，正在运行的版本都完好
    let exe = if cfg!(windows) { "looklook.exe" } else { "looklook" };
    items.sort_by_key(|p| p.file_name().is_some_and(|n| n == exe));
    let mut done: Vec<(PathBuf, Option<PathBuf>)> = vec![];
    let result = (|| -> std::io::Result<()> {
        for item in &items {
            let Some(name) = item.file_name() else { continue };
            // 出错时说明是哪个文件
            let at = |e: std::io::Error| std::io::Error::new(e.kind(), format!("{}: {e}", name.to_string_lossy()));
            let to = if name == exe { dir } else { res.as_path() };
            let dest = to.join(name);
            let tmp = (!item.is_dir()).then(|| to.join(format!(".{}.new", name.to_string_lossy())));
            if let Some(tmp) = &tmp {
                let _ = remove_any(tmp);
                copy_tree(item, tmp).map_err(at)?;
            }
            let backup = if dest.exists() {
                let b = backup_name(&dest);
                if let Some(parent) = b.parent() {
                    std::fs::create_dir_all(parent).map_err(at)?;
                }
                rename_retry(&dest, &b).map_err(at)?;
                Some(b)
            } else {
                None
            };
            done.push((dest.clone(), backup));
            match &tmp {
                Some(tmp) => rename_retry(tmp, &dest).map_err(at)?,
                None => copy_tree(item, &dest).map_err(at)?,
            }
        }
        Ok(())
    })();
    if let Err(e) = result {
        for (dest, backup) in done.into_iter().rev() {
            let _ = remove_any(&dest);
            if let Some(b) = backup {
                let _ = rename_retry(&b, &dest);
            }
        }
        return Err(fail(e));
    }
    // 删掉备份（Windows 上运行中的程序删不掉，留到下次启动时清理）
    for (dest, backup) in done {
        if let Some(b) = backup {
            let _ = remove_any(&b);
            // 会话保持程序的备份目录（程序没在运行时已经删空）
            if b.parent() != dest.parent() {
                let _ = b.parent().map(std::fs::remove_dir);
            }
        }
    }
    Ok(())
}

/// Windows 的压缩包把 trz.exe / tsz.exe 放在顶层（见 paths.rs 的 `TRZSZ_DIR`），启动时挪进 trzsz 目录。
/// 要在找随包 trzsz（`paths::find_trzsz`）之前调用。
pub fn settle_trzsz() {
    let Ok(dir) = install_dir() else { return };
    settle_trzsz_in(&resource_dir(&dir));
}

fn settle_trzsz_in(res: &Path) {
    let sub = res.join(crate::paths::TRZSZ_DIR);
    for name in crate::paths::TRZSZ_EXES {
        let flat = res.join(name);
        if !flat.is_file() {
            continue;
        }
        let dest = sub.join(name);
        let moved = std::fs::create_dir_all(&sub).and_then(|_| {
            // 旧的可能正在终端里运行：改名挪开，下次启动时清理
            let backup = if dest.exists() {
                let b = backup_name(&dest);
                rename_retry(&dest, &b)?;
                Some(b)
            } else {
                None
            };
            rename_retry(&flat, &dest).inspect_err(|_| {
                if let Some(b) = &backup {
                    let _ = std::fs::rename(b, &dest);
                }
            })?;
            if let Some(b) = backup {
                let _ = remove_any(&b);
            }
            Ok(())
        });
        if let Err(e) = moved {
            tracing::warn!(file = %flat.display(), error = %e, "无法把随包的 trzsz 放进 trzsz 目录");
        }
    }
}

/// 启动时清理上次更新留下的备份文件与下载目录。
fn cleanup_leftovers(paths: &Paths) {
    let _ = std::fs::remove_dir_all(paths.home.join("updates"));
    let Ok(dir) = install_dir() else { return };
    let res = resource_dir(&dir);
    let trzsz = res.join(crate::paths::TRZSZ_DIR);
    let dirs = if res == dir { vec![dir, trzsz] } else { vec![dir, res, trzsz] };
    for e in dirs.iter().filter_map(|d| std::fs::read_dir(d).ok()).flatten().filter_map(|e| e.ok()) {
        let name = e.file_name().to_string_lossy().into_owned();
        let backup = name.contains(".old") && !name.starts_with('.');
        let tmp = name.starts_with('.') && name.ends_with(".new");
        // 1.2.0 在 Windows 上把 psmux 改名成 looklook-mux.exe，改名后用不了，现在随包的是 psmux.exe
        let stale_mux = cfg!(windows) && name.eq_ignore_ascii_case("looklook-mux.exe");
        // 会话保持程序的旧文件可能还在被后台任务使用（Windows 上删不掉，忽略即可）
        if backup || tmp || stale_mux {
            let _ = remove_any(&e.path());
        }
    }
}

static RESTART: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn request_restart() {
    RESTART.store(true, std::sync::atomic::Ordering::SeqCst);
}

pub fn restart_requested() -> bool {
    RESTART.load(std::sync::atomic::Ordering::SeqCst)
}

/// 被系统服务管理器管着（它会在非零退出后用新文件重新拉起）。
fn supervised() -> bool {
    std::env::var_os("INVOCATION_ID").is_some() || std::env::var("XPC_SERVICE_NAME").is_ok_and(|v| v.contains("looklook"))
}

/// 更新后重启（run() 收尾完成后调用，不返回）。
pub fn restart_now() -> ! {
    if supervised() {
        tracing::info!("交给系统服务重新启动新版本");
        std::process::exit(RESTART_EXIT_CODE);
    }
    match exe_path() {
        Some(exe) => {
            let mut cmd = std::process::Command::new(exe);
            cmd.args(std::env::args_os().skip(1)).env(RESTART_ENV, "1").stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
            #[cfg(unix)]
            unsafe {
                use std::os::unix::process::CommandExt;
                // 新会话：不随启动它的终端一起被关掉
                cmd.pre_exec(|| {
                    libc::setsid();
                    Ok(())
                });
            }
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                const DETACHED_PROCESS: u32 = 0x0000_0008;
                const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
                cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
            }
            if let Err(e) = cmd.spawn() {
                tracing::error!(error = %e, "无法启动新版本，请手动启动看看");
            }
        }
        None => tracing::error!("找不到程序路径，请手动启动看看"),
    }
    std::process::exit(0);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminals_keep_auto_update_waiting() {
        touch_terminals();
        assert!(terminals_idle_ms().is_some_and(|ms| ms < AUTO_IDLE_MS));
        let g = TerminalGuard::new();
        assert_eq!(terminals_idle_ms(), None, "有终端连着");
        drop(g);
        TERMINALS_SEEN.store(crate::util::system_ms() - AUTO_IDLE_MS, std::sync::atomic::Ordering::Relaxed);
        assert!(terminals_idle_ms().is_some_and(|ms| ms >= AUTO_IDLE_MS));
    }

    #[test]
    fn versions() {
        assert!(looks_like_version("1.2.0") && looks_like_version("1.2.0-beta.1"));
        assert!(!looks_like_version("") && !looks_like_version("../x") && !looks_like_version("1 2"));
    }

    #[test]
    fn apply_replaces_files_and_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let (src, dir) = (tmp.path().join("src"), tmp.path().join("app"));
        std::fs::create_dir_all(src.join("fonts")).unwrap();
        std::fs::create_dir_all(dir.join("fonts")).unwrap();
        std::fs::write(src.join("looklook"), "new").unwrap();
        std::fs::write(src.join("fonts/a.css"), "new-font").unwrap();
        std::fs::write(src.join("extra.txt"), "x").unwrap();
        std::fs::write(dir.join("looklook"), "old").unwrap();
        std::fs::write(dir.join("fonts/old.css"), "old-font").unwrap();
        std::fs::write(dir.join("keep.txt"), "k").unwrap();
        apply(&src, &dir).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("looklook")).unwrap(), "new");
        assert_eq!(std::fs::read_to_string(dir.join("fonts/a.css")).unwrap(), "new-font");
        assert!(!dir.join("fonts/old.css").exists(), "目录整体替换");
        assert!(dir.join("extra.txt").is_file() && dir.join("keep.txt").is_file());
        let names: Vec<String> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        assert!(!names.iter().any(|n| n.contains(".old") || n.ends_with(".new")), "{names:?}");
    }

    #[test]
    fn mux_backup_keeps_its_name() {
        let tmp = tempfile::tempdir().unwrap();
        let (src, dir) = (tmp.path().join("src"), tmp.path().join("app"));
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(src.join(MUX_EXE), "new").unwrap();
        std::fs::write(dir.join(MUX_EXE), "old").unwrap();
        let b = backup_name(&dir.join(MUX_EXE));
        assert_eq!(b.file_name().unwrap(), MUX_EXE);
        assert!(b.parent().unwrap().file_name().unwrap().to_string_lossy().starts_with(&format!("{MUX_EXE}.old")));
        apply(&src, &dir).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join(MUX_EXE)).unwrap(), "new");
        let names: Vec<String> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        assert_eq!(names, vec![MUX_EXE.to_string()]);
    }

    #[test]
    fn rescue_removes_unused_mux_backups() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join(format!("{MUX_EXE}.old")), "x").unwrap();
        std::fs::write(tmp.path().join(MUX_EXE), "new").unwrap();
        rescue_mux_backups_in(tmp.path());
        let names: Vec<String> = std::fs::read_dir(tmp.path()).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        assert_eq!(names, vec![MUX_EXE.to_string()]);
    }

    #[test]
    fn settle_moves_flat_trzsz_into_its_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let [trz, tsz] = crate::paths::TRZSZ_EXES;
        let sub = tmp.path().join(crate::paths::TRZSZ_DIR);
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join(trz), "old").unwrap();
        std::fs::write(tmp.path().join(trz), "new").unwrap();
        std::fs::write(tmp.path().join(tsz), "new").unwrap();
        settle_trzsz_in(tmp.path());
        assert_eq!(std::fs::read_to_string(sub.join(trz)).unwrap(), "new");
        assert_eq!(std::fs::read_to_string(sub.join(tsz)).unwrap(), "new");
        assert!(!tmp.path().join(trz).exists() && !tmp.path().join(tsz).exists());
        let names: Vec<String> = std::fs::read_dir(&sub).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        assert_eq!(names.len(), 2, "{names:?}");
        settle_trzsz_in(tmp.path()); // 已经挪好时什么也不做
        assert_eq!(std::fs::read_to_string(sub.join(trz)).unwrap(), "new");
    }

    #[cfg(unix)]
    #[test]
    fn extracts_tar_gz() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("pkg/looklook")).unwrap();
        std::fs::write(tmp.path().join("pkg/looklook/looklook"), "bin").unwrap();
        let archive = tmp.path().join("p.tar.gz");
        let ok = std::process::Command::new("tar").arg("-czf").arg(&archive).arg("-C").arg(tmp.path().join("pkg")).arg("looklook").status().unwrap().success();
        assert!(ok);
        extract(&archive, &tmp.path().join("out")).unwrap();
        assert_eq!(std::fs::read_to_string(tmp.path().join("out/looklook/looklook")).unwrap(), "bin");
    }
}
