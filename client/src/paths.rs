//! 数据目录与随包资源（ttyd、字体、trzsz）的位置。
//!
//! 数据目录：`LOOKLOOK_HOME`，否则按系统惯例（Linux `~/.local/share/looklook`、
//! macOS `~/Library/Application Support/looklook`、Windows `%APPDATA%\looklook`）。
//!
//! 资源目录依次查找：`LOOKLOOK_RESOURCES` → 可执行文件所在目录 → 其 `../lib/looklook`
//! → macOS 应用包的 `Contents/Resources` → 开发时的 `vendor/{target}`（由 `scripts/fetch-vendor.sh` 准备）。

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// 与服务端版本接口一致的平台标识（详细设计 §4.10）。
pub fn target() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => "linux-x86_64",
        ("linux", "aarch64") => "linux-aarch64",
        ("macos", "aarch64") => "darwin-aarch64",
        ("macos", "x86_64") => "darwin-x86_64",
        ("windows", "x86_64") => "windows-x86_64",
        ("windows", "aarch64") => "windows-aarch64",
        _ => "unknown",
    }
}

pub fn os_name() -> &'static str {
    match std::env::consts::OS {
        "macos" => "darwin",
        other => other,
    }
}

/// 随包的终端服务（ttyd）在安装包里的文件名：用看看自己的名字，任务管理器、防火墙提示里不会出现陌生的“ttyd”。
pub const TERM_EXE: &str = if cfg!(windows) { "looklook-term.exe" } else { "looklook-term" };
/// 旧版安装包与开发时 vendor/ 里的原名
const LEGACY_TERM_EXE: &str = if cfg!(windows) { "ttyd.exe" } else { "ttyd" };
/// 随包的会话保持程序（Linux/macOS 是静态编译的 tmux，Windows 是 psmux）：同样改成看看自己的名字。
/// Windows 例外，必须叫 psmux.exe：psmux 每次运行都按进程名（psmux/tmux/pmux）确认登记的会话服务还活着，
/// 改了名的服务会被当成已经退出，登记文件随即被删——新建会话报“failed to create session”。
pub const MUX_EXE: &str = if cfg!(windows) { "psmux.exe" } else { "looklook-mux" };
/// 开发时 vendor/ 里的名字
const LEGACY_MUX_EXE: &str = if cfg!(windows) { "psmux.exe" } else { "mux" };
/// 随包的 trzsz（`trz` / `tsz`）放在资源目录的这个子目录里：会话的 `PATH` 只加这一个目录，
/// 不会把 looklook、looklook-term、安装脚本等一起暴露成命令。
pub const TRZSZ_DIR: &str = "trzsz";
const TRZ_EXE: &str = if cfg!(windows) { "trz.exe" } else { "trz" };
/// 终端服务进程名（清理遗留进程时用来确认身份）
pub const TERM_PROCESS_NAMES: [&str; 2] = ["looklook-term", "ttyd"];

#[derive(Debug, Clone)]
pub struct Paths {
    pub home: PathBuf,
    pub ttyd: Option<PathBuf>,
    pub mux: Option<PathBuf>,
    pub fonts: Option<PathBuf>,
    /// trz / tsz 所在目录（加在终端会话 `PATH` 的最前面）
    pub trzsz: Option<PathBuf>,
}

impl Paths {
    pub fn discover(home_override: Option<PathBuf>) -> Result<Self> {
        let home = match home_override {
            Some(h) => h,
            None => dirs::data_dir().context("无法确定数据目录，请设置 LOOKLOOK_HOME")?.join("looklook"),
        };
        std::fs::create_dir_all(&home).with_context(|| format!("无法创建数据目录 {}", home.display()))?;
        // 数据目录里有登录状态与通道令牌，只允许当前用户访问。
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700));
        }
        std::fs::create_dir_all(home.join("logs"))?;
        std::fs::create_dir_all(home.join("run"))?;
        let dirs = resource_dirs();
        let ttyd = std::env::var_os("TTYD_BIN")
            .map(PathBuf::from)
            .or_else(|| dirs.iter().flat_map(|d| [d.join(TERM_EXE), d.join(LEGACY_TERM_EXE)]).find(|p| p.is_file()));
        let mux = std::env::var_os("LOOKLOOK_MUX_BIN")
            .map(PathBuf::from)
            .or_else(|| dirs.iter().flat_map(|d| [d.join(MUX_EXE), d.join(LEGACY_MUX_EXE)]).find(|p| p.is_file()));
        let fonts = dirs.iter().map(|d| d.join("fonts")).find(|p| p.join("fonts.css").is_file());
        let trzsz = dirs.iter().map(|d| d.join(TRZSZ_DIR)).find(|p| p.join(TRZ_EXE).is_file());
        Ok(Self { home, ttyd, mux, fonts, trzsz })
    }

    pub fn db(&self) -> PathBuf {
        self.home.join("looklook.db")
    }
    pub fn device_key(&self) -> PathBuf {
        self.home.join("device.key")
    }
    pub fn pending_key(&self) -> PathBuf {
        self.home.join("device.key.new")
    }
    pub fn relay_config(&self) -> PathBuf {
        self.home.join("run").join("relay-client.toml")
    }
    pub fn tmux_conf(&self) -> PathBuf {
        self.home.join("run").join("tmux.conf")
    }
    pub fn instance_log(&self, id: &str) -> PathBuf {
        self.home.join("logs").join(format!("{id}.log"))
    }
    pub fn instance_pid(&self, id: &str) -> PathBuf {
        self.home.join("run").join(format!("{id}.pid"))
    }
    /// 所有终端的附件目录的上级（粘贴截图、“发给 AI”的文件）
    pub fn uploads(&self) -> PathBuf {
        self.home.join("uploads")
    }
    /// 一个终端的附件目录 `{LOOKLOOK_HOME}/uploads/{实例 id}/`
    pub fn attach_dir(&self, id: &str) -> PathBuf {
        self.uploads().join(id)
    }
    /// 终端会话用的 `PATH`：随包 trzsz 的目录放在最前面。没有随包 trzsz 时为 None（不改动 `PATH`）。
    pub fn session_path(&self) -> Option<std::ffi::OsString> {
        let dir = self.trzsz.as_ref()?;
        let mut v = vec![dir.clone()];
        if let Some(cur) = std::env::var_os("PATH") {
            v.extend(std::env::split_paths(&cur).filter(|p| p != dir));
        }
        std::env::join_paths(v).ok()
    }
}

/// macOS 应用包（`Looklook.app/Contents/MacOS/looklook`）的资源目录 `Contents/Resources`。
pub fn bundle_resources(exe_dir: &Path) -> Option<PathBuf> {
    if !cfg!(target_os = "macos") || !exe_dir.ends_with("Contents/MacOS") {
        return None;
    }
    Some(exe_dir.parent()?.join("Resources"))
}

fn resource_dirs() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(d) = std::env::var_os("LOOKLOOK_RESOURCES") {
        v.push(PathBuf::from(d));
    }
    if let Some(exe_dir) = std::env::current_exe().ok().and_then(|p| p.canonicalize().ok()).and_then(|p| p.parent().map(Path::to_path_buf)) {
        v.push(exe_dir.clone());
        v.push(exe_dir.join("../lib/looklook"));
        if let Some(r) = bundle_resources(&exe_dir) {
            v.push(r);
        }
    }
    if cfg!(debug_assertions) {
        // 开发时：ttyd 在 vendor/{target}/，字体在 vendor/fonts/。
        let vendor = Path::new(env!("CARGO_MANIFEST_DIR")).join("vendor");
        v.push(vendor.join(target()));
        v.push(vendor);
    }
    v
}
