//! 终端文件传输（docs/FILE_TRANSFER.md §4）：分块上传、下载、rz / sz 检测与收尾、附件目录清理。
//!
//! - ZMODEM 过不了 tmux（控制字节被吞），所以 rz / sz 只靠管理台检测横幅，文件走这里的 HTTP 接口；
//!   本机的 rz / sz 进程只用来确定目录、文件列表，传完由这里结束它。
//! - 远程链路单请求有时间与大小限制，上传按块（每块最多 8 MiB）进行，块失败可从已收字节数续传。
//! - 先写同目录的 `.{name}.looklook-part`，收齐后改名；一小时没有动静的上传连同临时文件一起清掉。
//! - 附件目录 `{LOOKLOOK_HOME}/uploads/{实例 id}/`（粘贴的截图、“发给 AI”的文件）：超过 7 天的文件定期删除。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use axum::body::Body;
use axum::http::header;
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use serde::Serialize;

use crate::error::LocalError;
use crate::instances::{Instances, Pane};
use crate::paths::Paths;
use crate::util::random_id;

/// 每块最多 8 MiB（中转直连等响应头最多 30 秒、Cloudflare 单请求 100 MB，见 §2）
pub const CHUNK_SIZE: u64 = 8 << 20;
/// 上传块路由的请求体上限：一块再加一点余量。其他路由仍是 axum 默认的 2 MB。
pub const BODY_LIMIT: usize = 9 << 20;
/// 同时进行的上传最多这么多个
const MAX_ACTIVE: usize = 8;
/// 多久没有动静的上传自动清理
const IDLE: Duration = Duration::from_secs(3600);
/// 附件目录里的文件保留多久
const ATTACH_TTL: Duration = Duration::from_secs(7 * 24 * 3600);
/// 附件目录多久清理一次
const SWEEP_EVERY: Duration = Duration::from_secs(6 * 3600);
const PART_SUFFIX: &str = ".looklook-part";
/// 文件名最多这么多字节（临时文件名还要加上 `.` 与 `.looklook-part`，不超过常见文件系统的 255 字节）
const MAX_NAME_BYTES: usize = 200;

type R<T> = Result<T, LocalError>;

// ---------------- 当前目录与 rz / sz 检测 ----------------

/// `GET /api/instances/{id}/files` 的结果（§4.1）
#[derive(Debug, Serialize)]
pub struct FilesInfo {
    /// 终端当前目录：tmux 面板当前路径，拿不到则用实例工作目录
    pub cwd: String,
    /// `pane` | `workdir`
    pub cwd_source: &'static str,
    pub attach_dir: String,
    /// 目标系统的路径分隔符
    pub sep: &'static str,
    pub os: &'static str,
    pub transfer: Option<Transfer>,
}

/// 面板里的 rz / sz（§4.1 `transfer`）
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Transfer {
    /// `rz` | `sz` | `unknown`
    pub kind: &'static str,
    /// 是本机进程（不是 ssh 等里面跑的）
    pub local: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// sz 要发送的文件
    #[serde(skip_serializing_if = "Option::is_none")]
    pub files: Option<Vec<SzFile>>,
    /// 不是本机 rz / sz 时，面板前台的命令（例如 `ssh`）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SzFile {
    pub name: String,
    pub path: String,
    pub size: u64,
}

pub async fn files_info(instances: &Instances, paths: &Paths, id: &str) -> R<FilesInfo> {
    let row = instances.get(id).await?.row;
    let pane = instances.pane(id).await;
    let (cwd, cwd_source) = match pane.as_ref().filter(|p| !p.cwd.is_empty() && Path::new(&p.cwd).is_dir()) {
        Some(p) => (p.cwd.clone(), "pane"),
        None => (row.workdir.clone(), "workdir"),
    };
    let transfer = match pane {
        Some(p) => tokio::task::spawn_blocking(move || detect(&p)).await.ok().flatten(),
        None => None,
    };
    Ok(FilesInfo {
        cwd,
        cwd_source,
        attach_dir: paths.attach_dir(id).display().to_string(),
        sep: std::path::MAIN_SEPARATOR_STR,
        os: std::env::consts::OS,
        transfer,
    })
}

/// 本机 rz / sz（重新查一次，不信任界面传来的进程号）。
async fn local_transfer(instances: &Instances, id: &str) -> R<Option<Transfer>> {
    instances.get(id).await?;
    let Some(pane) = instances.pane(id).await else { return Ok(None) };
    let t = tokio::task::spawn_blocking(move || detect(&pane)).await.ok().flatten();
    Ok(t.filter(|t| t.local))
}

/// rz 的工作目录（上传 `dest=rz`）。rz 已经不在 → `TRANSFER_GONE`。
async fn rz_dir(instances: &Instances, id: &str) -> R<PathBuf> {
    match local_transfer(instances, id).await? {
        Some(Transfer { kind: "rz", cwd: Some(cwd), .. }) => Ok(PathBuf::from(cwd)),
        _ => Err(LocalError::new("TRANSFER_GONE")),
    }
}

/// `POST /api/instances/{id}/transfer/finish|cancel`：结束本机的 rz / sz（SIGINT，2 秒后仍在则 SIGTERM）。没有也算成功。
pub async fn end_transfer(instances: &Instances, id: &str) -> R<()> {
    let Some(t) = local_transfer(instances, id).await? else { return Ok(()) };
    let Some(pid) = t.pid else { return Ok(()) };
    tracing::info!(id, pid, kind = t.kind, "结束文件传输进程");
    #[cfg(unix)]
    {
        signal(pid, libc::SIGINT);
        let kind = t.kind;
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(2)).await;
            // 进程号可能已被复用：还是同一种程序才补一刀
            let still = tokio::task::spawn_blocking(move || list_procs().into_iter().any(|p| p.pid == pid && kind_of(&p.name) == Some(kind))).await.unwrap_or(false);
            if still {
                signal(pid, libc::SIGTERM);
            }
        });
    }
    Ok(())
}

#[cfg(unix)]
fn signal(pid: u32, sig: i32) {
    if let Ok(pid) = i32::try_from(pid) {
        unsafe {
            libc::kill(pid, sig);
        }
    }
}

/// 面板里是不是本机的 rz / sz。Windows（psmux）没有 rz / sz，不检测。
fn detect(pane: &Pane) -> Option<Transfer> {
    if cfg!(windows) {
        return None;
    }
    let procs = list_procs();
    let Some((pid, kind)) = find_transfer(&procs, pane.pid) else {
        return Some(Transfer { kind: "unknown", local: false, pid: None, cwd: None, files: None, command: Some(pane.command.clone()) });
    };
    let cwd = proc_cwd(pid).unwrap_or_else(|| PathBuf::from(&pane.cwd));
    let files = (kind == "sz").then(|| sz_files(&proc_args(pid), &cwd));
    Some(Transfer { kind, local: true, pid: Some(pid), cwd: Some(cwd.display().to_string()), files, command: None })
}

#[derive(Debug, Clone)]
struct ProcInfo {
    pid: u32,
    ppid: u32,
    name: String,
}

fn kind_of(name: &str) -> Option<&'static str> {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    match base {
        "rz" | "lrz" => Some("rz"),
        "sz" | "lsz" => Some("sz"),
        _ => None,
    }
}

/// 从面板的 shell 往下找 rz / sz（含它自己：面板直接运行 rz 的情况）。
fn find_transfer(procs: &[ProcInfo], root: u32) -> Option<(u32, &'static str)> {
    let mut queue = vec![root];
    let mut seen = std::collections::HashSet::new();
    while let Some(pid) = queue.pop() {
        if !seen.insert(pid) {
            continue;
        }
        if let Some(p) = procs.iter().find(|p| p.pid == pid) {
            if let Some(k) = kind_of(&p.name) {
                return Some((pid, k));
            }
        }
        queue.extend(procs.iter().filter(|p| p.ppid == pid && p.pid != pid).map(|p| p.pid));
    }
    None
}

/// sz 的参数里要发送的文件：跳过选项（及带值选项的值），相对路径按 sz 的工作目录解析，只保留存在的普通文件。
fn sz_files(args: &[String], cwd: &Path) -> Vec<SzFile> {
    // 带一个值的选项（lrzsz 0.12 `sz --help`）
    const WITH_VALUE: &[&str] = &[
        "-B", "--bufsize", "-C", "--command-tries", "--delay-startup", "-L", "--packetlen", "-l", "--framelen", "-m", "--min-bps", "-M", "--min-bps-time", "-s",
        "--stop-at", "--tcp-client", "-w", "--windowsize", "-c", "--command", "-i", "--immediate-command",
    ];
    let mut out = Vec::new();
    let mut it = args.iter().skip(1);
    let mut opts = true;
    while let Some(a) = it.next() {
        if opts && a == "--" {
            opts = false;
            continue;
        }
        if opts && a.starts_with('-') && a.len() > 1 {
            if WITH_VALUE.contains(&a.as_str()) {
                it.next();
            }
            continue;
        }
        let p = cwd.join(a);
        if let Ok(m) = std::fs::metadata(&p) {
            if m.is_file() {
                let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                out.push(SzFile { name, path: p.display().to_string(), size: m.len() });
            }
        }
    }
    out
}

#[cfg(target_os = "linux")]
fn list_procs() -> Vec<ProcInfo> {
    let Ok(rd) = std::fs::read_dir("/proc") else { return vec![] };
    rd.filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().to_str()?.parse::<u32>().ok())
        .filter_map(|pid| parse_stat(pid, &std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?))
        .collect()
}

/// `/proc/<pid>/stat`：`pid (comm) state ppid …`，comm 里可能有空格和括号，以最后一个 `)` 为界。
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn parse_stat(pid: u32, s: &str) -> Option<ProcInfo> {
    let open = s.find('(')?;
    let close = s.rfind(')')?;
    let name = s.get(open + 1..close)?.to_string();
    let mut rest = s.get(close + 1..)?.split_whitespace();
    let _state = rest.next()?;
    let ppid = rest.next()?.parse().ok()?;
    Some(ProcInfo { pid, ppid, name })
}

#[cfg(target_os = "linux")]
fn proc_args(pid: u32) -> Vec<String> {
    std::fs::read(format!("/proc/{pid}/cmdline"))
        .map(|b| b.split(|c| *c == 0).filter(|s| !s.is_empty()).map(|s| String::from_utf8_lossy(s).into_owned()).collect())
        .unwrap_or_default()
}

#[cfg(target_os = "linux")]
fn proc_cwd(pid: u32) -> Option<PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/cwd")).ok()
}

/// macOS：`ps -A -o pid=,ppid=,comm=`（comm 是程序的完整路径）。
#[cfg(not(any(target_os = "linux", windows)))]
fn list_procs() -> Vec<ProcInfo> {
    let Ok(out) = std::process::Command::new("ps").args(["-A", "-o", "pid=,ppid=,comm="]).output() else { return vec![] };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let pid = it.next()?.parse().ok()?;
            let ppid = it.next()?.parse().ok()?;
            let name = it.collect::<Vec<_>>().join(" ");
            Some(ProcInfo { pid, ppid, name })
        })
        .collect()
}

/// macOS 拿不到原始的参数数组，`ps -o args=` 按空白分开（文件名里有空格的会认不出，界面上就少列这个文件）。
#[cfg(not(any(target_os = "linux", windows)))]
fn proc_args(pid: u32) -> Vec<String> {
    let Ok(out) = std::process::Command::new("ps").args(["-o", "args=", "-p", &pid.to_string()]).output() else { return vec![] };
    String::from_utf8_lossy(&out.stdout).split_whitespace().map(String::from).collect()
}

#[cfg(not(any(target_os = "linux", windows)))]
fn proc_cwd(pid: u32) -> Option<PathBuf> {
    let out = std::process::Command::new("lsof").args(["-a", "-p", &pid.to_string(), "-d", "cwd", "-Fn"]).output().ok()?;
    String::from_utf8_lossy(&out.stdout).lines().find_map(|l| l.strip_prefix('n')).map(PathBuf::from)
}

#[cfg(windows)]
fn list_procs() -> Vec<ProcInfo> {
    vec![]
}

#[cfg(windows)]
fn proc_args(_pid: u32) -> Vec<String> {
    vec![]
}

#[cfg(windows)]
fn proc_cwd(_pid: u32) -> Option<PathBuf> {
    None
}

// ---------------- 文件名 ----------------

/// Windows 不允许的文件名（不区分大小写，带扩展名也不行）
fn is_reserved(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or("").trim_end().to_ascii_uppercase();
    matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ((stem.starts_with("COM") || stem.starts_with("LPT")) && stem.len() == 4 && matches!(stem.as_bytes()[3], b'1'..=b'9'))
}

/// 上传文件名：只取最后一段，去掉控制字符与路径分隔符；`.`、`..` 视为没有名字；
/// Windows 保留名前面加 `_`。各系统都去掉 Windows 不允许的字符（文件可能经同步盘到 Windows）。
/// 返回空串表示没有可用的名字（调用方用 `paste-…png`）。
pub fn sanitize_name(raw: &str) -> String {
    let last = raw.rsplit(['/', '\\']).next().unwrap_or("");
    let mut name: String = last.chars().filter(|c| !c.is_control() && !matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*')).collect();
    // Windows 会悄悄去掉结尾的点和空格
    name = name.trim().trim_end_matches(['.', ' ']).to_string();
    if name.is_empty() || name.chars().all(|c| c == '.') {
        return String::new();
    }
    if is_reserved(&name) {
        name.insert(0, '_');
    }
    truncate_name(&name, MAX_NAME_BYTES)
}

/// 主名与扩展名（`.tar.gz` 这类双扩展名一起算扩展名；以点开头的 `.bashrc` 没有扩展名）。
fn split_ext(name: &str) -> (&str, &str) {
    let Some(i) = name.rfind('.').filter(|i| *i > 0) else { return (name, "") };
    let (stem, ext) = name.split_at(i);
    if let Some(j) = stem.rfind(".tar").filter(|j| *j > 0 && stem.len() - j == 4) {
        return name.split_at(j);
    }
    (stem, ext)
}

/// 超长的名字截断主名，保留扩展名。
fn truncate_name(name: &str, max: usize) -> String {
    if name.len() <= max {
        return name.to_string();
    }
    let (stem, ext) = split_ext(name);
    let ext = if ext.len() > max / 2 { "" } else { ext };
    let mut s = String::new();
    for c in stem.chars() {
        if s.len() + c.len_utf8() + ext.len() > max {
            break;
        }
        s.push(c);
    }
    s + ext
}

/// 第 n 个候选名：`a.png`、`a (1).png`、`a (2).png` …
fn numbered(name: &str, n: u32) -> String {
    if n == 0 {
        return name.to_string();
    }
    let (stem, ext) = split_ext(name);
    format!("{stem} ({n}){ext}")
}

fn part_path(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!(".{name}{PART_SUFFIX}"))
}

/// 本机时间 `YYYYMMDD-HHMMSS`（文件名用）
fn stamp() -> String {
    let (y, mo, d, h, mi, s) = local_now();
    format!("{y:04}{mo:02}{d:02}-{h:02}{mi:02}{s:02}")
}

#[cfg(unix)]
fn local_now() -> (i32, u32, u32, u32, u32, u32) {
    unsafe {
        let t = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&t, &mut tm).is_null() {
            return utc_now();
        }
        (tm.tm_year + 1900, tm.tm_mon as u32 + 1, tm.tm_mday as u32, tm.tm_hour as u32, tm.tm_min as u32, tm.tm_sec as u32)
    }
}

#[cfg(windows)]
fn local_now() -> (i32, u32, u32, u32, u32, u32) {
    #[repr(C)]
    #[derive(Default)]
    struct SystemTime {
        year: u16,
        month: u16,
        day_of_week: u16,
        day: u16,
        hour: u16,
        minute: u16,
        second: u16,
        ms: u16,
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GetLocalTime(t: *mut SystemTime);
    }
    let mut t = SystemTime::default();
    unsafe { GetLocalTime(&mut t) };
    (t.year as i32, t.month as u32, t.day as u32, t.hour as u32, t.minute as u32, t.second as u32)
}

#[cfg_attr(windows, allow(dead_code))]
fn utc_now() -> (i32, u32, u32, u32, u32, u32) {
    let t = time::OffsetDateTime::now_utc();
    (t.year(), t.month() as u32, t.day() as u32, t.hour() as u32, t.minute() as u32, t.second() as u32)
}

// ---------------- 分块上传 ----------------

struct Upload {
    dir: PathBuf,
    /// 去重后的文件名（收齐时目标又被占用会再换一个）
    name: String,
    part: PathBuf,
    size: u64,
    received: u64,
    last: Instant,
}

#[derive(Debug, Serialize)]
pub struct Created {
    pub id: String,
    pub path: String,
    pub chunk_size: u64,
}

/// 上传去向（§4.2 `dest`）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dest {
    Cwd,
    Attach,
    Rz,
}

impl Dest {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "cwd" => Some(Dest::Cwd),
            "attach" => Some(Dest::Attach),
            "rz" => Some(Dest::Rz),
            _ => None,
        }
    }
}

pub struct Transfers {
    paths: Paths,
    uploads: Mutex<HashMap<String, Arc<tokio::sync::Mutex<Upload>>>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn unwritable(dir: &Path) -> LocalError {
    LocalError::new("UPLOAD_DIR_UNWRITABLE").with("path", dir.display().to_string())
}

impl Transfers {
    pub fn new(paths: Paths) -> Arc<Self> {
        Arc::new(Self { paths, uploads: Mutex::new(HashMap::new()) })
    }

    /// `POST /api/instances/{id}/uploads`：定目录与最终文件名，占好临时文件。
    pub async fn create(&self, instances: &Instances, id: &str, name: &str, size: u64, dest: Dest) -> R<Created> {
        let dir = match dest {
            Dest::Cwd => PathBuf::from(files_info(instances, &self.paths, id).await?.cwd),
            Dest::Rz => rz_dir(instances, id).await?,
            Dest::Attach => {
                instances.get(id).await?;
                let d = self.paths.attach_dir(id);
                std::fs::create_dir_all(&d).map_err(|_| unwritable(&d))?;
                d
            }
        };
        let clean = sanitize_name(name);
        let name = match (clean.is_empty(), dest) {
            (true, _) => format!("paste-{}.png", stamp()),
            (false, Dest::Attach) => truncate_name(&format!("{}-{clean}", stamp()), MAX_NAME_BYTES),
            (false, _) => clean,
        };
        self.reserve(dir, &name, size)
    }

    fn reserve(&self, dir: PathBuf, name: &str, size: u64) -> R<Created> {
        if !dir.is_dir() {
            return Err(unwritable(&dir));
        }
        let mut map = lock(&self.uploads);
        if map.len() >= MAX_ACTIVE {
            return Err(LocalError::new("UPLOAD_BUSY").with("max", MAX_ACTIVE));
        }
        // 最终文件与临时文件都没被占用的第一个名字；临时文件用 create_new 占位，同时进行的同名上传不会撞车
        let mut chosen = None;
        for n in 0..1000 {
            let cand = numbered(name, n);
            if dir.join(&cand).symlink_metadata().is_ok() {
                continue;
            }
            let part = part_path(&dir, &cand);
            match std::fs::OpenOptions::new().write(true).create_new(true).open(&part) {
                Ok(_) => {
                    chosen = Some((cand, part));
                    break;
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => {
                    tracing::debug!(dir = %dir.display(), error = %e, "上传目录不可写");
                    return Err(unwritable(&dir));
                }
            }
        }
        let (name, part) = chosen.ok_or_else(|| unwritable(&dir))?;
        let uid = format!("u_{}", random_id(16));
        let path = dir.join(&name).display().to_string();
        map.insert(uid.clone(), Arc::new(tokio::sync::Mutex::new(Upload { dir, name, part, size, received: 0, last: Instant::now() })));
        Ok(Created { id: uid, path, chunk_size: CHUNK_SIZE })
    }

    fn get(&self, uid: &str) -> R<Arc<tokio::sync::Mutex<Upload>>> {
        lock(&self.uploads).get(uid).cloned().ok_or_else(LocalError::not_found)
    }

    /// `PUT /api/uploads/{uid}?offset=N`：写一块，返回已收字节数。
    pub async fn write(&self, uid: &str, offset: u64, data: Bytes) -> R<u64> {
        use tokio::io::{AsyncSeekExt, AsyncWriteExt};
        let u = self.get(uid)?;
        let mut u = u.lock().await;
        u.last = Instant::now();
        if offset != u.received {
            return Err(LocalError::new("UPLOAD_OFFSET").with("received", u.received));
        }
        let len = data.len() as u64;
        if len > CHUNK_SIZE {
            return Err(LocalError::invalid("body", "too_large").with("max", CHUNK_SIZE));
        }
        if offset + len > u.size {
            return Err(LocalError::invalid("body", "exceeds_size").with("size", u.size));
        }
        if len > 0 {
            let mut f = tokio::fs::OpenOptions::new().write(true).open(&u.part).await.map_err(|e| {
                tracing::warn!(part = %u.part.display(), error = %e, "上传的临时文件打不开");
                LocalError::new("UPLOAD_DIR_UNWRITABLE").with("path", u.dir.display().to_string())
            })?;
            f.seek(std::io::SeekFrom::Start(offset)).await?;
            f.write_all(&data).await?;
            f.flush().await?;
        }
        u.received += len;
        u.last = Instant::now();
        Ok(u.received)
    }

    /// `POST /api/uploads/{uid}/finish`：字节数对上后改名为最终文件，返回最终路径。
    pub async fn finish(&self, uid: &str) -> R<String> {
        let arc = self.get(uid)?;
        let mut u = arc.lock().await;
        u.last = Instant::now();
        if u.received != u.size {
            return Err(LocalError::new("UPLOAD_INCOMPLETE").with("received", u.received).with("size", u.size));
        }
        // 写失败重试可能在后面留下多余字节
        if let Ok(f) = std::fs::OpenOptions::new().write(true).open(&u.part) {
            let _ = f.set_len(u.size);
        }
        // 上传期间同名文件被别人建了出来：不覆盖，再换一个名字（Windows 与 Unix 的 rename 都会覆盖已有文件）
        let mut target = u.dir.join(&u.name);
        let mut n = 1;
        while target.symlink_metadata().is_ok() && n < 1000 {
            target = u.dir.join(numbered(&u.name, n));
            n += 1;
        }
        std::fs::rename(&u.part, &target).map_err(|e| {
            tracing::warn!(target = %target.display(), error = %e, "上传完成后改名失败");
            unwritable(&u.dir)
        })?;
        u.name = target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        drop(u);
        lock(&self.uploads).remove(uid);
        Ok(target.display().to_string())
    }

    /// `DELETE /api/uploads/{uid}`：取消并删掉临时文件（不存在也算成功）。
    pub async fn cancel(&self, uid: &str) {
        let u = lock(&self.uploads).remove(uid);
        if let Some(u) = u {
            let u = u.lock().await;
            let _ = std::fs::remove_file(&u.part);
        }
    }

    /// 清掉一小时没有动静的上传（正在写的跳过）。
    fn cleanup_idle(&self, idle: Duration) {
        let mut map = lock(&self.uploads);
        map.retain(|uid, u| {
            let Ok(u) = u.try_lock() else { return true };
            if u.last.elapsed() < idle {
                return true;
            }
            tracing::info!(uid, part = %u.part.display(), "清理长时间没有动静的上传");
            let _ = std::fs::remove_file(&u.part);
            false
        });
    }

    /// 后台：每分钟清理闲置上传；启动时和之后每 6 小时清理附件目录。
    pub async fn run(self: Arc<Self>) {
        let mut last_sweep: Option<Instant> = None;
        loop {
            self.cleanup_idle(IDLE);
            if last_sweep.is_none_or(|t| t.elapsed() >= SWEEP_EVERY) {
                last_sweep = Some(Instant::now());
                let root = self.paths.uploads();
                let removed = tokio::task::spawn_blocking(move || sweep(&root, ATTACH_TTL, SystemTime::now())).await.unwrap_or(0);
                if removed > 0 {
                    tracing::info!(removed, "已清理过期的附件");
                }
            }
            tokio::time::sleep(Duration::from_secs(60)).await;
        }
    }
}

/// 删掉 `root` 下修改时间超过 `ttl` 的文件和清空后的目录（`root` 本身保留），返回删掉的文件数。
fn sweep(root: &Path, ttl: Duration, now: SystemTime) -> usize {
    fn walk(dir: &Path, ttl: Duration, now: SystemTime, removed: &mut usize) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for e in rd.filter_map(|e| e.ok()) {
            let p = e.path();
            let Ok(m) = std::fs::symlink_metadata(&p) else { continue };
            if m.is_dir() {
                walk(&p, ttl, now, removed);
                // 只删空目录；刚删空的目录也一并删掉
                let _ = std::fs::remove_dir(&p);
            } else {
                let old = m.modified().ok().and_then(|t| now.duration_since(t).ok()).is_some_and(|age| age > ttl);
                if old && std::fs::remove_file(&p).is_ok() {
                    *removed += 1;
                }
            }
        }
    }
    let mut removed = 0;
    walk(root, ttl, now, &mut removed);
    removed
}

// ---------------- 下载 ----------------

/// `GET /api/instances/{id}/download?path=`：流式返回一个普通文件。
pub async fn download(path: &str) -> R<Response> {
    let p = PathBuf::from(path);
    if !p.is_absolute() {
        return Err(LocalError::invalid("path", "absolute"));
    }
    let m = match tokio::fs::metadata(&p).await {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(LocalError::not_found().with("path", path)),
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => return Err(LocalError::new("FORBIDDEN").with("path", path)),
        Err(e) => return Err(e.into()),
    };
    if !m.is_file() {
        return Err(LocalError::new("NOT_A_FILE").with("path", path));
    }
    let f = match tokio::fs::File::open(&p).await {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => return Err(LocalError::new("FORBIDDEN").with("path", path)),
        Err(e) => return Err(e.into()),
    };
    let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "download".into());
    Ok((
        [
            (header::CONTENT_TYPE, "application/octet-stream".to_string()),
            (header::CONTENT_LENGTH, m.len().to_string()),
            (header::CONTENT_DISPOSITION, content_disposition(&name)),
        ],
        Body::from_stream(tokio_util::io::ReaderStream::with_capacity(f, 64 * 1024)),
    )
        .into_response())
}

/// `attachment; filename="<ASCII 近似>"; filename*=UTF-8''<百分号编码>`（RFC 6266 / RFC 5987）
fn content_disposition(name: &str) -> String {
    let ascii: String = name.chars().map(|c| if c.is_ascii_graphic() && c != '"' && c != '\\' || c == ' ' { c } else { '_' }).collect();
    let mut enc = String::new();
    for b in name.bytes() {
        if b.is_ascii_alphanumeric() || b"!#$&+-.^_`|~".contains(&b) {
            enc.push(b as char);
        } else {
            enc.push_str(&format!("%{b:02X}"));
        }
    }
    format!("attachment; filename=\"{ascii}\"; filename*=UTF-8''{enc}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(home: &Path) -> Paths {
        Paths { home: home.to_path_buf(), ttyd: None, mux: None, fonts: None, trzsz: None }
    }

    #[test]
    fn names_are_sanitized() {
        assert_eq!(sanitize_name("a.png"), "a.png");
        assert_eq!(sanitize_name("../../etc/passwd"), "passwd");
        assert_eq!(sanitize_name("C:\\Users\\me\\报告 v2.pdf"), "报告 v2.pdf");
        assert_eq!(sanitize_name("a\u{0}b\nc\u{7f}.txt"), "abc.txt");
        assert_eq!(sanitize_name(".."), "");
        assert_eq!(sanitize_name("dir/.."), "");
        assert_eq!(sanitize_name("."), "");
        assert_eq!(sanitize_name("   "), "");
        assert_eq!(sanitize_name(""), "");
        assert_eq!(sanitize_name("x/"), "");
        assert_eq!(sanitize_name("CON"), "_CON");
        assert_eq!(sanitize_name("nul.txt"), "_nul.txt");
        assert_eq!(sanitize_name("com1.log"), "_com1.log");
        assert_eq!(sanitize_name("console.log"), "console.log");
        assert_eq!(sanitize_name("a<b>:c?.txt"), "abc.txt");
        assert_eq!(sanitize_name("trail. . "), "trail");
        assert_eq!(sanitize_name(".bashrc"), ".bashrc");
        let long = format!("{}.png", "长".repeat(100));
        let s = sanitize_name(&long);
        assert!(s.len() <= MAX_NAME_BYTES && s.ends_with(".png"), "{s}");
    }

    #[test]
    fn numbered_names_keep_extension() {
        assert_eq!(numbered("a.png", 0), "a.png");
        assert_eq!(numbered("a.png", 1), "a (1).png");
        assert_eq!(numbered("Makefile", 2), "Makefile (2)");
        assert_eq!(numbered(".bashrc", 1), ".bashrc (1)");
        assert_eq!(numbered("x.tar.gz", 1), "x (1).tar.gz");
        assert_eq!(numbered("a.b.c", 1), "a.b (1).c");
    }

    #[test]
    fn content_disposition_is_rfc5987() {
        assert_eq!(content_disposition("a b.txt"), "attachment; filename=\"a b.txt\"; filename*=UTF-8''a%20b.txt");
        assert_eq!(content_disposition("报告\".pdf"), "attachment; filename=\"___.pdf\"; filename*=UTF-8''%E6%8A%A5%E5%91%8A%22.pdf");
    }

    #[tokio::test]
    async fn upload_dedupes_resumes_and_checks_size() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        std::fs::write(dir.join("a.png"), b"old").unwrap();
        let t = Transfers::new(paths(tmp.path()));
        let c = t.reserve(dir.clone(), "a.png", 10).unwrap();
        assert_eq!(c.path, dir.join("a (1).png").display().to_string());
        assert_eq!(c.chunk_size, CHUNK_SIZE);
        assert!(dir.join(".a (1).png.looklook-part").is_file());
        // 同名的第二个上传同时进行：拿到下一个名字
        let c2 = t.reserve(dir.clone(), "a.png", 1).unwrap();
        assert_eq!(c2.path, dir.join("a (2).png").display().to_string());

        assert_eq!(t.write(&c.id, 0, Bytes::from_static(b"hello")).await.unwrap(), 5);
        // 偏移不对：告诉前端从哪里续传
        let e = t.write(&c.id, 0, Bytes::from_static(b"hello")).await.unwrap_err();
        assert_eq!(e.code, "UPLOAD_OFFSET");
        assert_eq!(e.status, axum::http::StatusCode::CONFLICT);
        assert_eq!(e.params["received"], 5);
        // 没收齐不能完成
        let e = t.finish(&c.id).await.unwrap_err();
        assert_eq!(e.code, "UPLOAD_INCOMPLETE");
        assert_eq!(e.params["received"], 5);
        // 超出声明的大小
        assert!(t.write(&c.id, 5, Bytes::from_static(b"world!")).await.is_err());
        assert_eq!(t.write(&c.id, 5, Bytes::from_static(b"world")).await.unwrap(), 10);
        let p = t.finish(&c.id).await.unwrap();
        assert_eq!(p, c.path);
        assert_eq!(std::fs::read(&p).unwrap(), b"helloworld");
        assert!(!dir.join(".a (1).png.looklook-part").exists());
        assert_eq!(std::fs::read(dir.join("a.png")).unwrap(), b"old", "不覆盖已有文件");
        assert_eq!(t.finish(&c.id).await.unwrap_err().code, "NOT_FOUND");

        // 取消：临时文件删掉
        t.cancel(&c2.id).await;
        assert!(!dir.join(".a (2).png.looklook-part").exists());
        assert_eq!(t.write(&c2.id, 0, Bytes::from_static(b"x")).await.unwrap_err().code, "NOT_FOUND");
    }

    #[tokio::test]
    async fn finish_does_not_overwrite_file_created_meanwhile() {
        let tmp = tempfile::tempdir().unwrap();
        let t = Transfers::new(paths(tmp.path()));
        let c = t.reserve(tmp.path().to_path_buf(), "b.txt", 0).unwrap();
        std::fs::write(tmp.path().join("b.txt"), b"mine").unwrap();
        let p = t.finish(&c.id).await.unwrap();
        assert_eq!(p, tmp.path().join("b (1).txt").display().to_string());
        assert_eq!(std::fs::read(tmp.path().join("b.txt")).unwrap(), b"mine");
    }

    #[tokio::test]
    async fn upload_limits_and_idle_cleanup() {
        let tmp = tempfile::tempdir().unwrap();
        let t = Transfers::new(paths(tmp.path()));
        let missing = tmp.path().join("nope");
        assert_eq!(t.reserve(missing, "a", 1).unwrap_err().code, "UPLOAD_DIR_UNWRITABLE");
        let ids: Vec<_> = (0..MAX_ACTIVE).map(|i| t.reserve(tmp.path().to_path_buf(), &format!("f{i}"), 1).unwrap().id).collect();
        let e = t.reserve(tmp.path().to_path_buf(), "g", 1).unwrap_err();
        assert_eq!(e.code, "UPLOAD_BUSY");
        assert_eq!(e.status, axum::http::StatusCode::TOO_MANY_REQUESTS);
        t.cleanup_idle(IDLE);
        assert_eq!(lock(&t.uploads).len(), MAX_ACTIVE, "没到时间不清理");
        t.cleanup_idle(Duration::ZERO);
        assert!(lock(&t.uploads).is_empty());
        assert!(!tmp.path().join(".f0.looklook-part").exists());
        assert_eq!(t.write(&ids[0], 0, Bytes::new()).await.unwrap_err().code, "NOT_FOUND");
    }

    #[test]
    fn sweep_removes_old_files_and_empty_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("uploads");
        std::fs::create_dir_all(root.join("inst1")).unwrap();
        std::fs::create_dir_all(root.join("inst2/sub")).unwrap();
        std::fs::write(root.join("inst1/old.png"), b"x").unwrap();
        std::fs::write(root.join("inst2/sub/old.png"), b"x").unwrap();
        // 以“8 天后”为当前时间：现有文件都算过期；以“1 天后”为当前时间：都不过期
        let day = Duration::from_secs(86400);
        assert_eq!(sweep(&root, ATTACH_TTL, SystemTime::now() + day), 0);
        assert!(root.join("inst1/old.png").exists());
        std::fs::create_dir_all(root.join("inst3")).unwrap();
        std::fs::write(root.join("inst3/new.png"), b"x").unwrap();
        let f = std::fs::File::options().write(true).open(root.join("inst3/new.png")).unwrap();
        f.set_modified(SystemTime::now() + 7 * day).unwrap();
        assert_eq!(sweep(&root, ATTACH_TTL, SystemTime::now() + 8 * day), 2);
        assert!(!root.join("inst1").exists() && !root.join("inst2").exists());
        assert!(root.join("inst3/new.png").exists());
        assert!(root.is_dir(), "上级目录保留");
    }

    #[test]
    fn finds_transfer_among_descendants() {
        let p = |pid, ppid, name: &str| ProcInfo { pid, ppid, name: name.into() };
        let procs = vec![p(1, 0, "init"), p(10, 1, "bash"), p(11, 10, "sh"), p(12, 11, "lrz"), p(20, 1, "sz"), p(30, 10, "ssh")];
        assert_eq!(find_transfer(&procs, 10), Some((12, "rz")));
        assert_eq!(find_transfer(&procs, 30), None, "其他面板的 sz 不算");
        assert_eq!(find_transfer(&procs, 20), Some((20, "sz")));
        assert_eq!(kind_of("/usr/local/bin/sz"), Some("sz"));
        assert_eq!(kind_of("rzsz"), None);
    }

    #[test]
    fn parses_proc_stat_with_odd_names() {
        let s = "4242 (my (weird) prog) S 4000 4242 4000 0 -1";
        let p = parse_stat(4242, s).unwrap();
        assert_eq!((p.pid, p.ppid, p.name.as_str()), (4242, 4000, "my (weird) prog"));
    }

    #[test]
    fn sz_args_list_existing_files() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a.log"), b"12345").unwrap();
        std::fs::write(tmp.path().join("-b.log"), b"1").unwrap();
        std::fs::create_dir(tmp.path().join("d")).unwrap();
        let args: Vec<String> = ["sz", "-w", "1024", "-e", "a.log", "d", "missing", "--", "-b.log"].iter().map(|s| s.to_string()).collect();
        let files = sz_files(&args, tmp.path());
        assert_eq!(files.iter().map(|f| (f.name.as_str(), f.size)).collect::<Vec<_>>(), [("a.log", 5), ("-b.log", 1)]);
        assert_eq!(files[0].path, tmp.path().join("a.log").display().to_string());
    }

    /// 端到端：随包 looklook-mux 里运行 rz / sz，检测、上传到 rz 目录、结束 rz、列出 sz 的文件并下载。
    /// 需要 vendor/ 里的 ttyd、mux 和系统里的 lrzsz，默认不跑：`cargo test -- --ignored rz_sz_end_to_end`。
    #[tokio::test]
    #[ignore]
    async fn rz_sz_end_to_end() {
        use crate::instances::Input;
        use crate::store::Store;
        let home = tempfile::tempdir().unwrap();
        // 独立的 tmux socket 目录，不碰这台机器上真在用的 looklook 会话
        std::env::set_var("TMUX_TMPDIR", home.path());
        let work = home.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        let paths = Paths::discover(Some(home.path().join("data"))).unwrap();
        let mux = paths.mux.clone().expect("vendor/ 里没有 mux");
        let conf = paths.tmux_conf();
        let inst = Instances::new(Arc::new(Store::memory().unwrap()), paths.clone());
        let id = inst.create(Input { name: Some("xfer".into()), workdir: Some(work.display().to_string()), ..Default::default() }).await.unwrap().row.id;
        inst.start(&id).await.unwrap();
        let target = format!("={}:", Instances::session_name(&id));
        let keys = |cmd: &str| {
            let st = std::process::Command::new(&mux).args(["-L", "looklook", "-f"]).arg(&conf).args(["send-keys", "-t", &target, "-l", cmd]).status().unwrap();
            assert!(st.success());
            std::process::Command::new(&mux).args(["-L", "looklook", "-f"]).arg(&conf).args(["send-keys", "-t", &target, "Enter"]).status().unwrap();
        };
        let capture = || String::from_utf8_lossy(&std::process::Command::new(&mux).args(["-L", "looklook", "-f"]).arg(&conf).args(["capture-pane", "-p", "-t", &target]).output().unwrap().stdout).into_owned();
        tokio::time::sleep(Duration::from_millis(1500)).await;

        // 随包 trz / tsz 在会话的 PATH 里
        keys("command -v trz tsz > which.txt; echo PATH=$PATH > path.txt");
        tokio::time::sleep(Duration::from_millis(800)).await;
        let which = std::fs::read_to_string(work.join("which.txt")).unwrap_or_default();
        eprintln!("which: {which}\n{}", std::fs::read_to_string(work.join("path.txt")).unwrap_or_default());

        let pane_pid = inst.pane(&id).await.unwrap().pid;
        let pane_env = std::fs::read(format!("/proc/{pane_pid}/environ")).unwrap();
        // 当前目录跟着 cd 走
        std::fs::create_dir_all(work.join("sub")).unwrap();
        keys("cd sub");
        tokio::time::sleep(Duration::from_millis(500)).await;
        let info = files_info(&inst, &paths, &id).await.unwrap();
        assert_eq!((info.cwd.as_str(), info.cwd_source), (work.join("sub").to_str().unwrap(), "pane"));
        assert_eq!(info.transfer.as_ref().map(|t| (t.kind, t.local)), Some(("unknown", false)));

        // rz：检测到本机进程，上传到它的目录，结束它，shell 回到提示符
        keys("rz");
        tokio::time::sleep(Duration::from_millis(800)).await;
        let t = files_info(&inst, &paths, &id).await.unwrap().transfer.unwrap();
        assert_eq!((t.kind, t.local, t.cwd.as_deref()), ("rz", true, work.join("sub").to_str()));
        let tr = Transfers::new(paths.clone());
        for (name, body) in [("one.txt", "hello"), ("two.txt", "world!")] {
            let c = tr.create(&inst, &id, name, body.len() as u64, Dest::Rz).await.unwrap();
            assert_eq!(tr.write(&c.id, 0, Bytes::from(body)).await.unwrap(), body.len() as u64);
            let p = tr.finish(&c.id).await.unwrap();
            assert_eq!(std::fs::read_to_string(&p).unwrap(), body);
        }
        end_transfer(&inst, &id).await.unwrap();
        tokio::time::sleep(Duration::from_millis(2500)).await;
        assert_eq!(files_info(&inst, &paths, &id).await.unwrap().transfer.map(|t| t.kind), Some("unknown"), "rz 应该已经结束");
        assert_eq!(tr.create(&inst, &id, "x", 1, Dest::Rz).await.unwrap_err().code, "TRANSFER_GONE");
        keys("echo back-at-$((40+2))");
        tokio::time::sleep(Duration::from_millis(500)).await;
        let screen = capture();
        eprintln!("--- 屏幕 ---\n{screen}");
        assert!(screen.contains("back-at-42"), "shell 没回到提示符");

        // sz：列出要发送的文件，可以下载
        keys("sz one.txt two.txt");
        tokio::time::sleep(Duration::from_millis(800)).await;
        let t = files_info(&inst, &paths, &id).await.unwrap().transfer.unwrap();
        assert_eq!((t.kind, t.local), ("sz", true));
        let files = t.files.unwrap();
        assert_eq!(files.iter().map(|f| (f.name.as_str(), f.size)).collect::<Vec<_>>(), [("one.txt", 5), ("two.txt", 6)]);
        let r = download(&files[1].path).await.unwrap();
        assert_eq!(r.headers()[header::CONTENT_DISPOSITION], "attachment; filename=\"two.txt\"; filename*=UTF-8''two.txt");
        let body = axum::body::to_bytes(r.into_body(), 1 << 20).await.unwrap();
        assert_eq!(&body[..], b"world!");
        assert_eq!(download(work.to_str().unwrap()).await.unwrap_err().code, "NOT_A_FILE");
        end_transfer(&inst, &id).await.unwrap();
        tokio::time::sleep(Duration::from_millis(2500)).await;
        assert_eq!(files_info(&inst, &paths, &id).await.unwrap().transfer.map(|t| t.kind), Some("unknown"), "sz 应该已经结束");
        keys("echo again-$((40+3))");
        tokio::time::sleep(Duration::from_millis(500)).await;
        let screen = capture();
        eprintln!("--- 屏幕 ---\n{screen}");
        assert!(screen.contains("again-43"));

        // 附件目录随终端删除
        let c = tr.create(&inst, &id, "", 3, Dest::Attach).await.unwrap();
        assert!(c.path.starts_with(paths.attach_dir(&id).to_str().unwrap()) && c.path.contains("paste-") && c.path.ends_with(".png"), "{}", c.path);
        tr.write(&c.id, 0, Bytes::from_static(b"png")).await.unwrap();
        tr.finish(&c.id).await.unwrap();
        inst.stop(&id).await.unwrap();
        inst.delete(&id).await.unwrap();
        assert!(!paths.attach_dir(&id).exists());
        let _ = std::process::Command::new(&mux).args(["-L", "looklook", "kill-server"]).status();
        // 会话的 shell 启动时 PATH 最前面是随包 trzsz 的目录（Debian 的 /etc/profile 会给登录 shell 重设 PATH，
        // 那种系统上 shell 里最后看不到，见 which.txt）
        let path = pane_env.split(|c| *c == 0).find_map(|kv| kv.strip_prefix(b"PATH=")).map(|v| String::from_utf8_lossy(v).into_owned()).unwrap();
        assert!(path.starts_with(paths.trzsz.as_ref().unwrap().to_str().unwrap()), "{path}");
        if !which.contains("trzsz/trz") {
            eprintln!("注意：shell 的启动脚本重设了 PATH，trz / tsz 不在 PATH 里");
        }
    }

    #[test]
    fn stamp_format() {
        let s = stamp();
        assert_eq!(s.len(), 15);
        assert_eq!(&s[8..9], "-");
    }
}
