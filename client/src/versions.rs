//! Linux：程序的各个版本放在数据目录里，容器只映射数据目录一个目录就能保留自动更新（参考 Claude Code 的原生安装）。
//!
//! 布局：
//! - `{home}/versions/<版本>/`：完整的安装包内容（looklook、looklook-term、looklook-mux、trzsz/、fonts/……）。
//!   先放进 `versions/.<版本>.tmp-xxxx/`，完整了再一次改名到位，不会出现写了一半的版本目录。
//! - `{home}/bin/looklook`、`bin/stop.sh`、`bin/trzsz`：指向某个版本的软链（先建临时软链再改名替换）。
//!   主机服务、终端里的 SSH 命令、Claude Code 的 hook 都用这些稳定的路径，旧版本目录被清理后也能用。
//!   软链只指向确认能用的版本（运行稳定后才切过去，或者用户主动安装的版本），新版本靠启动时跳转用上。
//!
//! 启动时（`main` 最开始）跳转：`versions/` 里有比自己高、且没被判定为坏版本的版本时，带原参数 exec 过去。
//! 只跳到更高的版本，不会来回跳；镜像或 `/opt` 里的程序比数据目录里的都新时运行自己（`run` 时把自己也放进 `versions/`）。
//! 坏版本保护：`run` 正常工作 [`OK_AFTER`] 后在自己的版本目录里写 `.ok`；没有 `.ok` 的版本每次由 `run` 跳过去前
//! `.tries` 加 1，到 [`MAX_TRIES`] 次就不再跳（例如一启动就崩溃，服务管理器反复拉起）。
//! 别的子命令（hook、`status`……）只跳到有 `.ok` 的版本：新版本还没证明自己之前，坏了也不连累它们。
//! 清理：留下正在运行的、它之前最近的 1 个能用的、比它高但还没验证的；有进程（`/proc/*/exe`）在用的目录不删。
//!
//! macOS、Windows 不用这些，仍然就地替换安装目录里的文件（`updater::apply`）。

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use semver::Version;

use crate::account::VERSION;

/// 设成 1 时启动不跳转到数据目录里的版本（排查问题用；安装脚本取安装包自己的版本号也用它）
pub const NO_VERSIONS_ENV: &str = "LOOKLOOK_NO_VERSIONS";
/// 跳转时记下原来的入口（重启、日志用）；新进程读到后从环境里删掉，不传给终端里的程序
pub const ENTRY_ENV: &str = "LOOKLOOK_ENTRY";
/// 留下的版本个数：正在运行的 + 它之前最近的 1 个（回滚用）
const KEEP_VERSIONS: usize = 2;
/// 没有 `.ok` 的版本最多尝试启动几次
const MAX_TRIES: u32 = 3;
/// `run` 正常工作多久后算这个版本能用
pub const OK_AFTER: Duration = Duration::from_secs(60);
/// 残留的临时目录多久以后才清理（避免和正在进行的安装冲突）
const TMP_MAX_AGE: Duration = Duration::from_secs(3600);
const OK_FILE: &str = ".ok";
const TRIES_FILE: &str = ".tries";
const EXE: &str = "looklook";
/// 稳定入口里的软链：程序、`stop.sh`（主机服务的 ExecStop 用）、trzsz 目录（终端会话的 `PATH` 用）
const LINKS: [&str; 3] = [EXE, "stop.sh", crate::paths::TRZSZ_DIR];

/// 这个平台用不用数据目录里的版本。开发构建默认不用（`cargo run` 不要跳到装好的版本），设了 `LOOKLOOK_UPDATE_DEV` 时用。
pub fn enabled() -> bool {
    cfg!(target_os = "linux") && (!cfg!(debug_assertions) || std::env::var_os("LOOKLOOK_UPDATE_DEV").is_some())
}

/// 设了 `LOOKLOOK_NO_VERSIONS`：不跳转，也不动数据目录里的版本（不复制自己、不改稳定入口、不清理）
pub fn suppressed() -> bool {
    std::env::var_os(NO_VERSIONS_ENV).is_some_and(|v| !v.is_empty() && v != "0")
}

pub fn versions_dir(home: &Path) -> PathBuf {
    home.join("versions")
}

pub fn bin_dir(home: &Path) -> PathBuf {
    home.join("bin")
}

fn parse(name: &str) -> Option<Version> {
    // 只认规范写法（`1.2.0`、`1.2.0-beta.1`），`v1.2`、`01.2.0` 之类当作不合法的目录
    Version::parse(name).ok().filter(|v| v.to_string() == name)
}

fn current() -> Version {
    Version::parse(VERSION).unwrap_or_else(|_| Version::new(0, 0, 0))
}

/// `versions/` 里的一个版本目录。
#[derive(Debug, Clone)]
struct Entry {
    version: Version,
    dir: PathBuf,
    ok: bool,
    tries: u32,
}

impl Entry {
    fn read(dir: PathBuf, version: Version) -> Self {
        let ok = dir.join(OK_FILE).is_file();
        let tries = std::fs::read_to_string(dir.join(TRIES_FILE)).ok().and_then(|s| s.trim().parse().ok()).unwrap_or(0);
        Self { version, dir, ok, tries }
    }
    /// 试过 [`MAX_TRIES`] 次都没能正常运行
    fn bad(&self) -> bool {
        !self.ok && self.tries >= MAX_TRIES
    }
    fn exe(&self) -> PathBuf {
        self.dir.join(EXE)
    }
}

/// 合法的版本目录（有主程序），从高到低。
fn scan(home: &Path) -> Vec<Entry> {
    let Ok(rd) = std::fs::read_dir(versions_dir(home)) else { return vec![] };
    let mut v: Vec<Entry> = rd
        .filter_map(|e| e.ok())
        .filter_map(|e| Some(Entry::read(e.path(), parse(e.file_name().to_str()?)?)))
        .filter(|e| e.exe().is_file())
        .collect();
    v.sort_by(|a, b| b.version.cmp(&a.version));
    v
}

/// 跳转目标：比 `me` 高的最高版本。`run` 跳到没被判定为坏版本的（还没验证的也去试），别的子命令只跳到验证过（`.ok`）的。
fn pick<'a>(entries: &'a [Entry], me: &Version, run: bool) -> Option<&'a Entry> {
    entries.iter().filter(|e| e.version > *me && if run { !e.bad() } else { e.ok }).max_by(|a, b| a.version.cmp(&b.version))
}

/// 命令行里的数据目录，规则和 clap 的 `--home`（`LOOKLOOK_HOME`）、`Paths::discover` 一样。只读，不创建目录。
fn home_from_args(args: &[OsString]) -> Option<PathBuf> {
    let mut it = args.iter().skip(1);
    while let Some(a) = it.next() {
        let Some(s) = a.to_str() else { continue };
        if s == "--" {
            break;
        }
        if s == "--home" {
            return it.next().map(PathBuf::from);
        }
        if let Some(h) = s.strip_prefix("--home=") {
            return Some(PathBuf::from(h));
        }
    }
    std::env::var_os("LOOKLOOK_HOME").filter(|h| !h.is_empty()).map(PathBuf::from).or_else(crate::paths::default_home)
}

/// 这次是不是启动客户端（`looklook`、`looklook run …`）。只有它计入坏版本的尝试次数：
/// 别的子命令（Claude Code 的 hook、`status`、`ssh`……）调用得很频繁，新版本还没来得及证明自己就会被用光次数。
fn is_run(args: &[OsString]) -> bool {
    // 带值的全局选项和 run 的选项（写成 `--home x` 时跳过值）
    const WITH_VALUE: [&str; 4] = ["--home", "--listen", "--local-from", "--server"];
    let mut it = args.iter().skip(1);
    while let Some(a) = it.next() {
        let s = a.to_string_lossy();
        if WITH_VALUE.contains(&s.as_ref()) {
            it.next();
        } else if matches!(s.as_ref(), "-h" | "--help" | "-V" | "--version") {
            return false;
        } else if !s.starts_with('-') {
            return s == "run";
        }
    }
    true
}

/// 启动时跳转到数据目录里更高的版本（成功时不返回）。在单实例锁、日志之前调用；出错时打印警告并继续运行自己。
pub fn jump() {
    if !enabled() || suppressed() {
        return;
    }
    let args: Vec<OsString> = std::env::args_os().collect();
    let Some(home) = home_from_args(&args) else { return };
    let entries = scan(&home);
    let run = is_run(&args);
    for e in &entries {
        if e.version > current() && e.bad() && run {
            eprintln!("看看：{} 试了 {} 次都没能正常启动，跳过 / Looklook: skipping {} after {} failed starts", e.version, e.tries, e.version, e.tries);
        }
    }
    let Some(target) = pick(&entries, &current(), run) else { return };
    let me = std::env::current_exe().and_then(|p| p.canonicalize()).ok();
    if run && !target.ok {
        let _ = std::fs::write(target.dir.join(TRIES_FILE), (target.tries + 1).to_string());
    }
    use std::os::unix::process::CommandExt;
    let mut cmd = std::process::Command::new(target.exe());
    cmd.args(&args[1..]);
    if std::env::var_os(ENTRY_ENV).is_none() {
        if let Some(me) = &me {
            cmd.env(ENTRY_ENV, me);
        }
    }
    let e = cmd.exec();
    eprintln!("看看：无法启动 {}（{e}），继续运行 {VERSION} / Looklook: can't start {} ({e}); running {VERSION}", target.exe().display(), target.version);
}

/// 跳转前的入口（`LOOKLOOK_ENTRY`），在 `main` 里尽早调用一次：读出来后从环境里删掉。
pub fn entry() -> Option<PathBuf> {
    static ENTRY: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    ENTRY
        .get_or_init(|| {
            let v = std::env::var_os(ENTRY_ENV).map(PathBuf::from);
            std::env::remove_var(ENTRY_ENV);
            v
        })
        .clone()
}

/// 要存下来、以后再运行的看看程序路径（终端里的 SSH 命令、Claude Code 的 hook）：
/// 有稳定入口 `{home}/bin/looklook` 时用它，旧版本目录被清理后命令也还能用；否则用当前程序。
pub fn stable_exe(home: &Path) -> Option<PathBuf> {
    let link = bin_dir(home).join(EXE);
    if enabled() && link.is_file() {
        return Some(link);
    }
    crate::updater::exe_path()
}

/// 终端会话 `PATH` 里的 trzsz 目录：从版本目录运行时用稳定的 `{home}/bin/trzsz`（会话可能比这个版本目录活得久）。
pub fn stable_trzsz(home: &Path, found: PathBuf) -> PathBuf {
    let link = bin_dir(home).join(crate::paths::TRZSZ_DIR);
    let in_versions = found.canonicalize().is_ok_and(|f| versions_dir(home).canonicalize().is_ok_and(|v| f.starts_with(v)));
    if enabled() && in_versions && link.join(crate::paths::TRZSZ_EXES[0]).is_file() {
        return link;
    }
    found
}

fn tmp_name(version: &str) -> String {
    format!(".{version}.tmp-{}", crate::util::random_id(6))
}

/// 把一个完整的安装包目录放成 `versions/<版本>`：先挪（不同文件系统时复制）到临时目录，完整了再改名到位。
/// 同一个版本已经存在时（重新安装）替换它：Linux 上正在运行的程序不受影响。`copy` 可注入，测试用来模拟复制中途失败。
fn place_with(src: &Path, home: &Path, version: &str, mv: bool, copy: impl Fn(&Path, &Path) -> std::io::Result<()>) -> std::io::Result<PathBuf> {
    let vdir = versions_dir(home);
    std::fs::create_dir_all(&vdir)?;
    let tmp = vdir.join(tmp_name(version));
    let moved = mv && std::fs::rename(src, &tmp).is_ok();
    if !moved {
        if let Err(e) = copy(src, &tmp) {
            let _ = std::fs::remove_dir_all(&tmp);
            return Err(e);
        }
    }
    // 状态文件不跟着安装包走（复制的是一个已经运行过的版本目录时）
    for f in [OK_FILE, TRIES_FILE] {
        let _ = std::fs::remove_file(tmp.join(f));
    }
    let dest = vdir.join(version);
    if dest.exists() {
        let old = vdir.join(tmp_name(version));
        if let Err(e) = std::fs::rename(&dest, &old) {
            let _ = std::fs::remove_dir_all(&tmp);
            return Err(e);
        }
        let _ = std::fs::remove_dir_all(&old);
    }
    std::fs::rename(&tmp, &dest).inspect_err(|_| {
        let _ = std::fs::remove_dir_all(&tmp);
    })?;
    Ok(dest)
}

fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    let meta = std::fs::symlink_metadata(from)?;
    if meta.is_dir() {
        std::fs::create_dir_all(to)?;
        for e in std::fs::read_dir(from)? {
            let e = e?;
            copy_tree(&e.path(), &to.join(e.file_name()))?;
        }
        Ok(())
    } else if meta.file_type().is_symlink() {
        std::os::unix::fs::symlink(std::fs::read_link(from)?, to)
    } else {
        std::fs::copy(from, to).map(|_| ())
    }
}

/// 自动更新：把解压、校验好的安装包（`staged/looklook`）放成 `versions/<版本>`。
/// 改名到位之前先在临时目录里运行一次 `--version`：数据目录挂成了 noexec、架构不对、文件坏了时直接报错，
/// 不会出现“装好了、重启后却还是旧版本、接着又自动更新”的循环。
pub fn install_staged(home: &Path, src: &Path, version: &str) -> Result<PathBuf, String> {
    let out = std::process::Command::new(src.join(EXE)).arg("--version").env(NO_VERSIONS_ENV, "1").output();
    match out {
        Ok(o) if o.status.success() && String::from_utf8_lossy(&o.stdout).split_whitespace().any(|w| w == version) => {}
        Ok(o) => return Err(format!("new version doesn't run: {}", String::from_utf8_lossy(&o.stdout).trim())),
        Err(e) => return Err(format!("new version doesn't run: {e}")),
    }
    place_with(src, home, version, true, copy_tree).map_err(|e| e.to_string())
}

/// 把 `bin/` 里的软链指向 `dir` 这个版本目录：每个软链先建成临时名再改名替换（原子）。
fn link(home: &Path, dir: &Path) -> std::io::Result<()> {
    let bin = bin_dir(home);
    std::fs::create_dir_all(&bin)?;
    let name = dir.file_name().ok_or_else(|| std::io::Error::other("bad version dir"))?;
    for item in LINKS {
        if std::fs::symlink_metadata(dir.join(item)).is_err() {
            // 这个版本没有（例如旧版本的安装包没有 stop.sh）：不留指向别的版本的旧软链
            let _ = std::fs::remove_file(bin.join(item));
            continue;
        }
        let target = Path::new("..").join("versions").join(name).join(item);
        let tmp = bin.join(format!(".{item}.tmp-{}", crate::util::random_id(6)));
        std::os::unix::fs::symlink(&target, &tmp)?;
        std::fs::rename(&tmp, bin.join(item)).inspect_err(|_| {
            let _ = std::fs::remove_file(&tmp);
        })?;
    }
    Ok(())
}

/// `bin/looklook` 现在指向的版本目录（软链不存在、不是指向版本目录时为 None）。
fn linked(home: &Path) -> Option<PathBuf> {
    let t = std::fs::read_link(bin_dir(home).join(EXE)).ok()?;
    let dir = bin_dir(home).join(t).parent()?.to_path_buf();
    let name = dir.file_name()?.to_str()?.to_string();
    parse(&name)?;
    Some(versions_dir(home).join(name))
}

/// 程序所在的安装目录（资源和主程序同目录时）的内容：复制成版本目录用。
fn package_items(exe_dir: &Path) -> Vec<PathBuf> {
    const ITEMS: [&str; 8] = ["looklook", "looklook-term", "looklook-mux", "trzsz", "fonts", "LICENSES", "stop.sh", "uninstall.sh"];
    ITEMS.iter().map(|n| exe_dir.join(n)).filter(|p| p.exists()).collect()
}

/// `run` 启动时：自己不在 `versions/` 里（从 `/opt`、镜像里的程序启动，而且没有更高的能用版本）时把自己复制进去，
/// 并在稳定入口还没有、或者指向的版本已经不在时指向自己。这样只映射了数据目录的容器重建后，
/// 主机服务用 `{home}/bin/looklook` 也能启动；镜像里的版本比数据目录里的新时，稳定入口也会换成它（见 [`mark_ok`]）。
pub fn seed(home: &Path) {
    if !enabled() {
        return;
    }
    let Some(exe) = crate::updater::exe_path() else { return };
    let Some(exe_dir) = exe.parent() else { return };
    let vdir = versions_dir(home);
    let dest = vdir.join(VERSION);
    let inside = vdir.canonicalize().is_ok_and(|v| exe.starts_with(v));
    if !inside && !dest.join(EXE).is_file() {
        // 只复制完整安装的程序（同目录里有随包的终端服务）
        if !exe_dir.join(crate::paths::TERM_EXE).is_file() {
            return;
        }
        let items = package_items(exe_dir);
        let copy = |_: &Path, to: &Path| -> std::io::Result<()> {
            std::fs::create_dir_all(to)?;
            for item in &items {
                copy_tree(item, &to.join(item.file_name().unwrap_or_default()))?;
            }
            Ok(())
        };
        match place_with(exe_dir, home, VERSION, false, copy) {
            Ok(d) => tracing::info!(dir = %d.display(), "已把当前版本放进数据目录"),
            Err(e) => {
                tracing::warn!(error = %e, "无法把当前版本放进数据目录");
                return;
            }
        }
    }
    if linked(home).is_none_or(|d| !d.join(EXE).is_file()) {
        if let Err(e) = link(home, &dest) {
            tracing::warn!(error = %e, "无法更新稳定入口 bin/looklook");
        }
    }
}

/// `run` 正常工作了 [`OK_AFTER`]：记下这个版本能用，稳定入口指向它（它就是现在能用的最高版本）。
pub fn mark_ok(home: &Path) {
    if !enabled() {
        return;
    }
    let dir = versions_dir(home).join(VERSION);
    if !dir.join(EXE).is_file() {
        return;
    }
    let _ = std::fs::write(dir.join(OK_FILE), b"");
    let _ = std::fs::remove_file(dir.join(TRIES_FILE));
    if linked(home).as_deref() != Some(dir.as_path()) {
        match link(home, &dir) {
            Ok(()) => tracing::info!(version = VERSION, "稳定入口 bin/looklook 已指向当前版本"),
            Err(e) => tracing::warn!(error = %e, "无法更新稳定入口 bin/looklook"),
        }
    }
}

/// 用户主动安装了 `version`（安装脚本、`looklook install-version`）：放进 `versions/`，删掉比它高的版本
/// （否则启动时又会跳过去；正在运行的程序在 Linux 上删掉也不受影响），稳定入口指向它。
pub fn install_explicit(home: &Path, from: &Path) -> Result<PathBuf, String> {
    let exe = from.join(EXE);
    if !exe.is_file() || !from.join(crate::paths::TERM_EXE).is_file() {
        return Err(format!("{} 不是完整的安装包 / not a complete package", from.display()));
    }
    let items = package_items(from);
    let copy = |_: &Path, to: &Path| -> std::io::Result<()> {
        std::fs::create_dir_all(to)?;
        for item in &items {
            copy_tree(item, &to.join(item.file_name().unwrap_or_default()))?;
        }
        Ok(())
    };
    let dir = place_with(from, home, VERSION, false, copy).map_err(|e| e.to_string())?;
    for e in scan(home).into_iter().filter(|e| e.version > current()) {
        let _ = std::fs::remove_dir_all(&e.dir);
    }
    link(home, &dir).map_err(|e| e.to_string())?;
    Ok(dir)
}

/// 自动更新前检查：这个版本已经装过、试过 [`MAX_TRIES`] 次都没能正常运行。
pub fn is_bad(home: &Path, version: &str) -> bool {
    parse(version).is_some_and(|v| scan(home).iter().any(|e| e.version == v && e.bad()))
}

/// 清理的结果（给日志和测试用）。
#[derive(Debug, Default, PartialEq)]
pub struct Cleaned {
    pub removed: Vec<String>,
    /// 删掉的坏版本（调用者记进数据库，自动更新不再装它们）
    pub bad: Vec<String>,
    /// 有进程在用、这次没删的
    pub busy: Vec<String>,
}

/// 有进程的程序文件在这个目录下（`/proc/*/exe`）。读不了的进程（别的用户的）忽略：数据目录只有自己能访问。
pub fn in_use(dir: &Path) -> bool {
    let Ok(rd) = std::fs::read_dir("/proc") else { return false };
    let dir = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    rd.filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_str().is_some_and(|n| n.bytes().all(|b| b.is_ascii_digit())))
        .filter_map(|e| std::fs::read_link(e.path().join("exe")).ok())
        // 程序文件被删掉后显示为 “… (deleted)”，`starts_with` 按路径分段比较，照样能认出来
        .any(|exe| exe.starts_with(&dir))
}

/// 清理 `versions/`：留下正在运行的版本、它之前最近的 [`KEEP_VERSIONS`]` - 1` 个能用的、比它高但还没判定为坏的；
/// 有进程在用的目录（旧版本的会话保持程序、终端服务还在运行）跳过，下次再试。也清理残留的临时目录、不合法的目录。
/// `bin/looklook` 指向的版本要被删时，先改指到留下来的版本。
pub fn cleanup(home: &Path, busy: impl Fn(&Path) -> bool) -> Cleaned {
    let mut out = Cleaned::default();
    let vdir = versions_dir(home);
    let Ok(rd) = std::fs::read_dir(&vdir) else { return out };
    let me = current();
    let mut valid = vec![];
    let mut junk = vec![];
    for e in rd.filter_map(|e| e.ok()) {
        let (path, name) = (e.path(), e.file_name().to_string_lossy().into_owned());
        if name.starts_with('.') {
            let old = e.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|age| age > TMP_MAX_AGE);
            if name.contains(".tmp-") && old {
                junk.push((name, path));
            }
            continue;
        }
        match parse(&name) {
            Some(v) if path.join(EXE).is_file() => valid.push(Entry::read(path, v)),
            _ => junk.push((name, path)),
        }
    }
    valid.sort_by(|a, b| b.version.cmp(&a.version));
    let mut previous = 0;
    let mut remove: Vec<(Entry, bool)> = vec![];
    for e in valid.iter().cloned() {
        let keep = if e.version == me {
            true
        } else if e.bad() {
            false
        } else if e.version > me {
            true
        } else {
            previous += 1;
            previous < KEEP_VERSIONS
        };
        if !keep {
            let bad = e.bad();
            remove.push((e, bad));
        }
    }
    // 稳定入口要指向留下来的版本：当前版本优先，其次是留下的最高的能用版本
    if let Some(linked) = linked(home) {
        if remove.iter().any(|(e, _)| e.dir == linked) || !linked.join(EXE).is_file() {
            let kept = valid.iter().filter(|e| !e.bad() && !remove.iter().any(|(r, _)| r.dir == e.dir));
            let to = kept.clone().find(|e| e.version == me).or_else(|| kept.filter(|e| e.version < me || e.ok).max_by(|a, b| a.version.cmp(&b.version)));
            match to {
                Some(to) => {
                    let _ = link(home, &to.dir);
                }
                // 没有能指向的版本：留着它指向的那个（下次再清理）
                None => remove.retain(|(e, _)| e.dir != linked),
            }
        }
    }
    for (name, path) in junk {
        if busy(&path) {
            out.busy.push(name);
        } else if std::fs::remove_dir_all(&path).or_else(|_| std::fs::remove_file(&path)).is_ok() {
            out.removed.push(name);
        }
    }
    for (e, bad) in remove {
        let name = e.version.to_string();
        if busy(&e.dir) {
            out.busy.push(name);
        } else if std::fs::remove_dir_all(&e.dir).is_ok() {
            if bad {
                out.bad.push(name.clone());
            }
            out.removed.push(name);
        }
    }
    out
}

/// `{home}/versions` 可不可以写（自动更新要往里面放新版本）。
pub fn writable(home: &Path) -> bool {
    let vdir = versions_dir(home);
    let probe = vdir.join(".looklook-write-test");
    let ok = std::fs::create_dir_all(&vdir).is_ok() && std::fs::write(&probe, b"").is_ok();
    let _ = std::fs::remove_file(&probe);
    ok
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(v: &[&str]) -> Vec<OsString> {
        v.iter().map(OsString::from).collect()
    }

    /// 造一个版本目录（`exe` 为 false 时不放主程序）
    fn put(home: &Path, v: &str, ok: bool, tries: u32) -> PathBuf {
        let d = versions_dir(home).join(v);
        std::fs::create_dir_all(d.join("trzsz")).unwrap();
        std::fs::write(d.join(EXE), "bin").unwrap();
        std::fs::write(d.join("stop.sh"), "sh").unwrap();
        if ok {
            std::fs::write(d.join(OK_FILE), "").unwrap();
        }
        if tries > 0 {
            std::fs::write(d.join(TRIES_FILE), tries.to_string()).unwrap();
        }
        d
    }

    fn names(home: &Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(versions_dir(home)).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        v.sort();
        v
    }

    fn ver(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    #[test]
    fn compares_versions() {
        assert!(ver("1.10.0") > ver("1.9.9"));
        assert!(ver("1.2.0") > ver("1.2.0-beta.2") && ver("1.2.0-beta.10") > ver("1.2.0-beta.2"));
        assert!(ver("1.2.0-rc.1") > ver("1.2.0-beta.1"));
        assert!(parse("1.2.0-beta.1").is_some());
        assert!(parse("v1.2.0").is_none() && parse("1.2").is_none() && parse("01.2.0").is_none() && parse(".1.2.0.tmp-x").is_none());
    }

    #[test]
    fn picks_highest_usable_newer_version() {
        let tmp = tempfile::tempdir().unwrap();
        let h = tmp.path();
        put(h, "1.0.0", true, 0);
        put(h, "2.0.0", true, 0);
        put(h, "2.1.0", false, 1); // 没验证过，还能试
        put(h, "3.0.0", false, MAX_TRIES); // 坏版本
        std::fs::create_dir_all(versions_dir(h).join("9.0.0")).unwrap(); // 没有主程序
        std::fs::create_dir_all(versions_dir(h).join("junk")).unwrap();
        let e = scan(h);
        assert_eq!(e.iter().map(|e| e.version.to_string()).collect::<Vec<_>>(), ["3.0.0", "2.1.0", "2.0.0", "1.0.0"]);
        assert_eq!(pick(&e, &ver("1.5.0"), true).unwrap().version, ver("2.1.0"));
        assert_eq!(pick(&e, &ver("1.5.0"), false).unwrap().version, ver("2.0.0"), "别的子命令只用验证过的");
        assert_eq!(pick(&e, &ver("2.1.0"), true), None, "只跳到严格更高的版本");
        assert_eq!(pick(&e, &ver("4.0.0"), true), None, "自己比数据目录里的都新");
        std::fs::write(versions_dir(h).join("2.1.0").join(TRIES_FILE), MAX_TRIES.to_string()).unwrap();
        assert_eq!(pick(&scan(h), &ver("1.5.0"), true).unwrap().version, ver("2.0.0"), "坏版本跳过，用次高的");
        assert!(is_bad(h, "3.0.0") && is_bad(h, "2.1.0") && !is_bad(h, "2.0.0") && !is_bad(h, "5.0.0"));
    }

    impl PartialEq for Entry {
        fn eq(&self, o: &Self) -> bool {
            self.dir == o.dir
        }
    }

    #[test]
    fn parses_home_and_run_from_args() {
        assert_eq!(home_from_args(&os(&["ll", "--home", "/x", "run"])), Some(PathBuf::from("/x")));
        assert_eq!(home_from_args(&os(&["ll", "status", "--home=/y"])), Some(PathBuf::from("/y")));
        assert_ne!(home_from_args(&os(&["ll", "ssh", "h", "--", "--home", "/z"])), Some(PathBuf::from("/z")), "-- 之后是给远端的命令");
        assert!(is_run(&os(&["ll"])) && is_run(&os(&["ll", "run", "--no-browser"])) && is_run(&os(&["ll", "--home", "status", "run"])));
        assert!(is_run(&os(&["ll", "--listen", "0.0.0.0:1", "--no-browser"])));
        assert!(!is_run(&os(&["ll", "status"])) && !is_run(&os(&["ll", "hook", "Stop"])) && !is_run(&os(&["ll", "--version"])));
    }

    #[test]
    fn place_is_atomic() {
        let tmp = tempfile::tempdir().unwrap();
        let (h, src) = (tmp.path().join("home"), tmp.path().join("src"));
        std::fs::create_dir_all(src.join("fonts")).unwrap();
        std::fs::write(src.join(EXE), "new").unwrap();
        std::fs::write(src.join("fonts/a.css"), "x").unwrap();
        // 复制到一半失败：不留下 versions/<v>，也不留下临时目录
        let failing = |from: &Path, to: &Path| -> std::io::Result<()> {
            std::fs::create_dir_all(to)?;
            std::fs::copy(from.join(EXE), to.join(EXE))?;
            Err(std::io::Error::other("disk full"))
        };
        assert!(place_with(&src, &h, "2.0.0", false, failing).is_err());
        assert!(names(&h).is_empty(), "{:?}", names(&h));
        // 成功：完整的目录一次到位；已有的同一个版本（带着旧的状态文件）被替换
        put(&h, "2.0.0", true, 2);
        std::fs::write(src.join(OK_FILE), "").unwrap();
        let d = place_with(&src, &h, "2.0.0", false, copy_tree).unwrap();
        assert_eq!(std::fs::read_to_string(d.join(EXE)).unwrap(), "new");
        assert!(d.join("fonts/a.css").is_file() && !d.join(OK_FILE).exists() && !d.join(TRIES_FILE).exists());
        assert_eq!(names(&h), ["2.0.0"]);
        // 挪过去（同一个文件系统）：源目录没了
        let d = place_with(&src, &h, "2.1.0", true, copy_tree).unwrap();
        assert!(d.join(EXE).is_file() && !src.exists());
    }

    #[test]
    fn links_switch_atomically() {
        let tmp = tempfile::tempdir().unwrap();
        let h = tmp.path();
        let a = put(h, "1.0.0", true, 0);
        let b = put(h, "2.0.0", true, 0);
        link(h, &a).unwrap();
        assert_eq!(linked(h), Some(a.clone()));
        assert_eq!(std::fs::read_link(bin_dir(h).join(EXE)).unwrap(), Path::new("../versions/1.0.0/looklook"), "相对路径：数据目录换了位置（映射进别的容器）也能用");
        link(h, &b).unwrap();
        assert_eq!(linked(h), Some(b));
        assert!(bin_dir(h).join("trzsz").is_dir() && bin_dir(h).join("stop.sh").is_file());
        let left: Vec<String> = std::fs::read_dir(bin_dir(h)).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        assert!(!left.iter().any(|n| n.contains(".tmp-")), "{left:?}");
    }

    #[test]
    fn cleanup_keeps_current_previous_and_untested() {
        let tmp = tempfile::tempdir().unwrap();
        let h = tmp.path();
        let me = VERSION;
        let cur = current();
        // 比当前低的三个（0.x 一定比正式版本低）、当前、比当前高的一个没验证的和一个坏的
        let (l1, l2, l3) = ("0.1.0".to_string(), "0.2.0".to_string(), "0.3.0".to_string());
        for v in [&l1, &l2, &l3] {
            put(h, v, true, 0);
        }
        put(h, me, true, 0);
        let up = Version::new(cur.major + 1, 0, 0).to_string();
        let bad = Version::new(cur.major + 2, 0, 0).to_string();
        put(h, &up, false, 1);
        put(h, &bad, false, MAX_TRIES);
        std::fs::create_dir_all(versions_dir(h).join("not-a-version")).unwrap();
        std::fs::create_dir_all(versions_dir(h).join(".9.9.9.tmp-fresh")).unwrap(); // 新的临时目录：可能正在安装
        link(h, &versions_dir(h).join(&l1)).unwrap(); // 稳定入口指向要被删的旧版本
        // l2 有进程在用（旧版本的会话保持程序还在运行）
        let busy_dir = versions_dir(h).join(&l2);
        let r = cleanup(h, |d| d == busy_dir);
        assert_eq!(r.busy, std::slice::from_ref(&l2));
        assert_eq!(r.bad, std::slice::from_ref(&bad));
        let mut want = vec![l2.clone(), l3.clone(), me.to_string(), up.clone(), ".9.9.9.tmp-fresh".into()];
        want.sort();
        assert_eq!(names(h), want, "留下当前、前一个（{l3}）、在用的、没验证的更高版本");
        assert_eq!(linked(h), Some(versions_dir(h).join(me)), "稳定入口改指到当前版本");
        // 下次清理时 l2 不再被占用
        let r = cleanup(h, |_| false);
        assert_eq!(r.removed, [l2]);
    }

    #[test]
    fn detects_running_programs() {
        let exe = std::env::current_exe().unwrap();
        assert!(in_use(exe.parent().unwrap()));
        let tmp = tempfile::tempdir().unwrap();
        assert!(!in_use(tmp.path()));
    }
}
