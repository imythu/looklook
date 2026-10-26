//! 终端页的 “Git” 标签（只读）：终端当前目录所在仓库的状态、改动对比、提交历史。
//! 暂存、提交、拉取、推送都交给终端（AI 编程助手或 git 命令），这里不改动仓库。
//!
//! - 直接调用这台电脑上的 `git`（参数列表，不经过 shell）；没装 git → `GIT_MISSING`，不是仓库 → `{repo: false}`；
//! - 读操作加 `--no-optional-locks`，不和终端里正在跑的 git 抢锁。

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::error::LocalError;

type R<T> = Result<T, LocalError>;

/// 一次对比最多返回这么多字节（再多界面也看不过来，提示到终端里看）。
pub const DIFF_MAX: usize = 512 * 1024;
const LOCAL_TIMEOUT: Duration = Duration::from_secs(15);
const LOG_LIMIT: usize = 50;

fn git_bin() -> R<String> {
    crate::shells::which("git").ok_or_else(|| LocalError::new("GIT_MISSING"))
}

/// 在 `dir` 里运行 git，超时就结束它。返回完整输出（不检查退出码）。
fn run(dir: &Path, args: &[&str], timeout: Duration) -> R<Output> {
    let mut cmd = Command::new(git_bin()?);
    cmd.arg("-C").arg(dir).args(["-c", "core.quotepath=false", "-c", "color.ui=false"]).args(args);
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    cmd.env("GIT_TERMINAL_PROMPT", "0").env("LC_ALL", "C").env("GIT_OPTIONAL_LOCKS", "0");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let mut child = cmd.spawn().map_err(|e| LocalError::new("GIT_FAILED").with("detail", e.to_string()))?;
    // 输出可能很大：读取放到线程里，避免管道写满卡住子进程
    let mut out = child.stdout.take();
    let mut err = child.stderr.take();
    let read_out = std::thread::spawn(move || {
        let mut b = Vec::new();
        if let Some(o) = out.as_mut() {
            let _ = std::io::Read::read_to_end(o, &mut b);
        }
        b
    });
    let read_err = std::thread::spawn(move || {
        let mut b = Vec::new();
        if let Some(e) = err.as_mut() {
            let _ = std::io::Read::read_to_end(e, &mut b);
        }
        b
    });
    let start = Instant::now();
    let status = loop {
        if let Some(s) = child.try_wait()? {
            break s;
        }
        if start.elapsed() > timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Err(LocalError::new("GIT_TIMEOUT").with("seconds", timeout.as_secs()));
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    Ok(Output { status, stdout: read_out.join().unwrap_or_default(), stderr: read_err.join().unwrap_or_default() })
}

/// 运行并要求成功；失败时带上 git 的错误信息（最后几行）。
fn run_ok(dir: &Path, args: &[&str], timeout: Duration) -> R<String> {
    let out = run(dir, args, timeout)?;
    if !out.status.success() {
        return Err(failed(&out));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn failed(out: &Output) -> LocalError {
    let text = String::from_utf8_lossy(if out.stderr.is_empty() { &out.stdout } else { &out.stderr }).trim().to_string();
    let tail: Vec<&str> = text.lines().rev().take(12).collect();
    let detail: String = tail.into_iter().rev().collect::<Vec<_>>().join("\n").chars().take(2000).collect();
    let auth = ["Authentication failed", "could not read Username", "Permission denied (publickey", "terminal prompts disabled", "Host key verification failed"];
    if auth.iter().any(|a| detail.contains(a)) {
        return LocalError::new("GIT_AUTH").with("detail", detail);
    }
    LocalError::new("GIT_FAILED").with("detail", detail)
}

/// 仓库根目录；不是仓库时为 None。
fn toplevel(dir: &Path) -> R<Option<PathBuf>> {
    if !dir.is_dir() {
        return Err(LocalError::not_found().with("path", dir.display().to_string()));
    }
    let out = run(dir, &["rev-parse", "--show-toplevel"], LOCAL_TIMEOUT)?;
    if !out.status.success() {
        return Ok(None);
    }
    let p = String::from_utf8_lossy(&out.stdout).trim().to_string();
    Ok((!p.is_empty()).then(|| PathBuf::from(p)))
}

fn repo(dir: &Path) -> R<PathBuf> {
    toplevel(dir)?.ok_or_else(|| LocalError::new("GIT_NOT_REPO").with("path", dir.display().to_string()))
}

// ---------------- 状态 ----------------

#[derive(Debug, Default, Serialize, PartialEq, Eq)]
pub struct Status {
    pub repo: bool,
    /// 仓库根目录
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
    /// 当前分支；分离头指针时为 None
    pub branch: Option<String>,
    /// 当前提交（短）
    pub head: Option<String>,
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    pub files: Vec<FileChange>,
    pub commits: Vec<Commit>,
    /// 改动太多时只列前这么多个
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct FileChange {
    /// 相对仓库根目录，`/` 分隔
    pub path: String,
    /// 改名 / 复制前的路径
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    /// 暂存区状态：`M` 修改、`A` 新增、`D` 删除、`R` 改名、`C` 复制、`T` 类型变化、`.` 没变
    pub index: char,
    /// 工作区状态，同上；未跟踪的文件两者都是 `?`
    pub worktree: char,
    /// 有冲突
    pub conflict: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Commit {
    pub hash: String,
    pub short: String,
    pub author: String,
    /// Unix 秒
    pub time: i64,
    pub subject: String,
    /// 指向它的分支、标签（`HEAD -> main, origin/main`）
    #[serde(skip_serializing_if = "String::is_empty")]
    pub refs: String,
}

const MAX_FILES: usize = 2000;

/// 解析 `git status --porcelain=v2 --branch -z`。
fn parse_status(raw: &str) -> Status {
    let mut s = Status { repo: true, ..Default::default() };
    let mut it = raw.split('\0').peekable();
    while let Some(rec) = it.next() {
        if rec.is_empty() {
            continue;
        }
        if let Some(h) = rec.strip_prefix("# ") {
            let (k, v) = h.split_once(' ').unwrap_or((h, ""));
            match k {
                "branch.oid" if v != "(initial)" => s.head = Some(v.chars().take(8).collect()),
                "branch.head" if v != "(detached)" => s.branch = Some(v.to_string()),
                "branch.upstream" => s.upstream = Some(v.to_string()),
                "branch.ab" => {
                    for part in v.split(' ') {
                        if let Some(n) = part.strip_prefix('+') {
                            s.ahead = n.parse().unwrap_or(0);
                        } else if let Some(n) = part.strip_prefix('-') {
                            s.behind = n.parse().unwrap_or(0);
                        }
                    }
                }
                _ => {}
            }
            continue;
        }
        if s.files.len() >= MAX_FILES {
            s.truncated = true;
            continue;
        }
        let xy = |f: &str| {
            let mut c = f.chars();
            (c.next().unwrap_or('.'), c.next().unwrap_or('.'))
        };
        match rec.as_bytes()[0] {
            // 1 XY sub mH mI mW hH hI path
            b'1' => {
                let f: Vec<&str> = rec.splitn(9, ' ').collect();
                if f.len() == 9 {
                    let (i, w) = xy(f[1]);
                    s.files.push(FileChange { path: f[8].into(), from: None, index: i, worktree: w, conflict: false });
                }
            }
            // 2 XY sub mH mI mW hH hI Xscore path \0 origPath
            b'2' => {
                let f: Vec<&str> = rec.splitn(10, ' ').collect();
                let from = it.next().map(str::to_string);
                if f.len() == 10 {
                    let (i, w) = xy(f[1]);
                    s.files.push(FileChange { path: f[9].into(), from, index: i, worktree: w, conflict: false });
                }
            }
            // u XY sub m1 m2 m3 mW h1 h2 h3 path
            b'u' => {
                let f: Vec<&str> = rec.splitn(11, ' ').collect();
                if f.len() == 11 {
                    let (i, w) = xy(f[1]);
                    s.files.push(FileChange { path: f[10].into(), from: None, index: i, worktree: w, conflict: true });
                }
            }
            b'?' => s.files.push(FileChange { path: rec[2..].into(), from: None, index: '?', worktree: '?', conflict: false }),
            _ => {}
        }
    }
    s
}

fn parse_log(raw: &str) -> Vec<Commit> {
    raw.split('\x1e')
        .filter_map(|rec| {
            let f: Vec<&str> = rec.trim_start_matches('\n').split('\x1f').collect();
            (f.len() == 6).then(|| Commit {
                hash: f[0].into(),
                short: f[1].into(),
                author: f[2].into(),
                time: f[3].parse().unwrap_or(0),
                subject: f[4].into(),
                refs: f[5].trim().into(),
            })
        })
        .collect()
}

/// `GET /api/instances/{id}/git?path=`：`dir` 所在仓库的状态与最近的提交。
pub fn status(dir: &Path) -> R<Status> {
    let Some(root) = toplevel(dir)? else { return Ok(Status::default()) };
    let raw = run_ok(&root, &["status", "--porcelain=v2", "--branch", "-z", "--untracked-files=all"], LOCAL_TIMEOUT)?;
    let mut s = parse_status(&raw);
    s.root = Some(root.display().to_string());
    // 还没有任何提交时 log 会失败，当作没有历史
    if s.head.is_some() {
        let n = format!("-n{LOG_LIMIT}");
        let log = run_ok(&root, &["log", &n, "--format=%H%x1f%h%x1f%an%x1f%at%x1f%s%x1f%D%x1e"], LOCAL_TIMEOUT)?;
        s.commits = parse_log(&log);
    }
    Ok(s)
}

// ---------------- 对比 ----------------

#[derive(Debug, Serialize)]
pub struct Diff {
    pub text: String,
    pub truncated: bool,
    /// 二进制文件
    pub binary: bool,
}

fn cap(bytes: &[u8]) -> Diff {
    let truncated = bytes.len() > DIFF_MAX;
    let mut text = String::from_utf8_lossy(&bytes[..bytes.len().min(DIFF_MAX)]).into_owned();
    if truncated {
        // 不要停在半行
        if let Some(i) = text.rfind('\n') {
            text.truncate(i + 1);
        }
    }
    let binary = text.lines().any(|l| l.starts_with("Binary files ") && l.ends_with(" differ"));
    Diff { text, truncated, binary }
}

/// 文件路径：相对仓库根目录、不能跑到仓库外面。
fn repo_path(file: &str) -> R<&str> {
    let bad = file.is_empty() || file.starts_with('/') || file.starts_with('\\') || file.contains(':') || file.split(['/', '\\']).any(|s| s == "..");
    if bad {
        return Err(LocalError::invalid("file", "relative"));
    }
    Ok(file)
}

/// 一个文件相对上次提交（HEAD）的全部改动（暂存与否都算）；未跟踪的文件显示为整份新增。
pub fn diff_file(dir: &Path, file: &str, untracked: bool) -> R<Diff> {
    let root = repo(dir)?;
    let file = repo_path(file)?;
    let null = if cfg!(windows) { "NUL" } else { "/dev/null" };
    let out = if untracked {
        // --no-index 有差异时退出码是 1
        let out = run(&root, &["diff", "--no-index", "--no-ext-diff", "--", null, file], LOCAL_TIMEOUT)?;
        if !matches!(out.status.code(), Some(0 | 1)) {
            return Err(failed(&out));
        }
        out.stdout
    } else {
        // 还没有任何提交：和空目录树比
        let base = if run(&root, &["rev-parse", "--verify", "-q", "HEAD"], LOCAL_TIMEOUT)?.status.success() {
            "HEAD".to_string()
        } else {
            run_ok(&root, &["hash-object", "-t", "tree", null], LOCAL_TIMEOUT)?.trim().to_string()
        };
        run_ok(&root, &["diff", "--no-ext-diff", "-M", &base, "--", file], LOCAL_TIMEOUT)?.into_bytes()
    };
    Ok(cap(&out))
}

/// 一个提交的说明与改动。
pub fn show(dir: &Path, hash: &str) -> R<Diff> {
    if !(4..=64).contains(&hash.len()) || !hash.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(LocalError::invalid("commit", "hash"));
    }
    let root = repo(dir)?;
    let out = run(&root, &["show", "--no-ext-diff", "-M", "--stat", "--patch", "--format=commit %H%nAuthor: %an <%ae>%nDate:   %ad%n%n%B", hash], LOCAL_TIMEOUT)?;
    if !out.status.success() {
        return Err(failed(&out));
    }
    Ok(cap(&out.stdout))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_porcelain_v2() {
        let raw = "# branch.oid 0123456789abcdef\0# branch.head main\0# branch.upstream origin/main\0# branch.ab +2 -1\0\
                   1 .M N... 100644 100644 100644 aaa bbb src/main.rs\0\
                   1 A. N... 000000 100644 100644 000 ccc 新文件 带空格.md\0\
                   2 R. N... 100644 100644 100644 ddd eee R100 docs/new.md\0docs/old.md\0\
                   u UU N... 100644 100644 100644 100644 f1 f2 f3 conflict.txt\0\
                   ? notes/todo.txt\0";
        let s = parse_status(raw);
        assert_eq!(s.branch.as_deref(), Some("main"));
        assert_eq!(s.head.as_deref(), Some("01234567"));
        assert_eq!(s.upstream.as_deref(), Some("origin/main"));
        assert_eq!((s.ahead, s.behind), (2, 1));
        assert_eq!(s.files.len(), 5);
        assert_eq!((s.files[0].index, s.files[0].worktree), ('.', 'M'));
        assert_eq!(s.files[1].path, "新文件 带空格.md");
        assert_eq!(s.files[2].from.as_deref(), Some("docs/old.md"));
        assert_eq!(s.files[2].index, 'R');
        assert!(s.files[3].conflict);
        assert_eq!((s.files[4].index, s.files[4].path.as_str()), ('?', "notes/todo.txt"));
        let d = parse_status("# branch.oid (initial)\0# branch.head (detached)\0");
        assert_eq!((d.head, d.branch), (None, None));
    }

    #[test]
    fn rejects_paths_outside_the_repo() {
        assert!(repo_path("src/a.rs").is_ok());
        for bad in ["", "/etc/passwd", "../x", "a/../../x", "C:\\x", "\\\\server\\share"] {
            assert!(repo_path(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn real_repository_round_trip() {
        if crate::shells::which("git").is_none() {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        assert_eq!(status(dir).unwrap(), Status::default(), "不是仓库");
        let git = |args: &[&str]| run_ok(dir, args, LOCAL_TIMEOUT).unwrap();
        git(&["init", "-q", "-b", "main"]);
        git(&["config", "user.email", "t@example.com"]);
        git(&["config", "user.name", "Tester"]);
        std::fs::create_dir(dir.join("sub")).unwrap();
        std::fs::write(dir.join("sub/a.txt"), "one\n").unwrap();
        // 子目录里打开也能找到仓库
        let s = status(&dir.join("sub")).unwrap();
        assert!(s.repo && s.head.is_none() && s.commits.is_empty());
        assert_eq!(s.files[0].index, '?');
        let d = diff_file(dir, "sub/a.txt", true).unwrap();
        assert!(d.text.contains("+one"), "{}", d.text);
        git(&["add", "-A"]);
        assert_eq!(status(dir).unwrap().files[0].index, 'A');
        assert!(diff_file(dir, "sub/a.txt", false).unwrap().text.contains("+one"), "还没有提交时和空目录树比");
        git(&["commit", "-q", "-m", "first commit"]);
        std::fs::write(dir.join("sub/a.txt"), "one\ntwo\n").unwrap();
        let s = status(dir).unwrap();
        assert_eq!(s.branch.as_deref(), Some("main"));
        assert_eq!(s.commits.len(), 1);
        assert_eq!(s.commits[0].subject, "first commit");
        assert_eq!((s.files[0].index, s.files[0].worktree), ('.', 'M'));
        assert!(diff_file(dir, "sub/a.txt", false).unwrap().text.contains("+two"));
        git(&["add", "-A"]);
        assert!(diff_file(dir, "sub/a.txt", false).unwrap().text.contains("+two"), "暂存了也照样显示");
        assert!(show(dir, &s.commits[0].hash).unwrap().text.contains("first commit"));
        assert_eq!(show(dir, "zz; rm -rf /").unwrap_err().code, "VALIDATION_FAILED");
    }
}
