//! Codex 的“托管工作树”（0.164 起，TUI 里 `/new`、`/fork`、`/worktree` 问“新对话在哪运行”时选“新建 Git 工作树”）。
//!
//! app-server 没有建工作树的接口（TUI 自己建，再用新目录开 / 复制会话），所以这里照 Codex 源码
//! `codex-rs/worktree`（和 Codex 桌面版共用的约定）自己建，建出来的 TUI 的工作树浏览器也认得：
//! - 位置：`$CODEX_HOME/worktrees`，`config.toml` 的 `[desktop] git-worktree-root` 可以改（必须是绝对路径）；
//!   每个工作树一个 4 位十六进制的桶：`<根>/<桶>/<仓库目录名>`；macOS 上根目录放 `.metadata_never_index`；
//! - 建法：`git worktree add --detach --no-checkout <位置> <HEAD 的提交>`，只给新工作树写
//!   `core.worktree`（`config.worktree`），再 `reset --hard` 检出文件；会话目录是原目录在仓库里的相对位置；
//! - 归属：新工作树的 git 目录里写 `codex-thread.json`（`{"version":1,"ownerThreadId":…}`），不覆盖别的会话的。

use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use serde::Serialize;

use crate::error::LocalError;

type R<T> = Result<T, LocalError>;

const TIMEOUT: Duration = Duration::from_secs(120);
const OWNER_FILE: &str = "codex-thread.json";

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Checkout {
    /// 工作树根目录
    pub root: PathBuf,
    /// 会话在里面的目录（原目录在仓库里的相对位置）
    pub cwd: PathBuf,
    /// 这个工作树属于哪段对话（`codex-thread.json`）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head: Option<String>,
}

fn fail(what: &str, detail: impl std::fmt::Display) -> LocalError {
    LocalError::new("CODEX_WORKTREE_FAILED").with("detail", format!("{what}: {detail}"))
}

fn git(dir: &Path, args: &[&str]) -> R<String> {
    let out = crate::git::run_env(dir, args, &[], TIMEOUT)?;
    if !out.status.success() {
        return Err(crate::git::failed(&out));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim_end_matches(['\n', '\r']).to_string())
}

fn canon(p: &Path) -> PathBuf {
    dunce_canon(p).unwrap_or_else(|| p.to_path_buf())
}

/// 和 Codex 一样去掉 Windows 的 `\\?\` 前缀
fn dunce_canon(p: &Path) -> Option<PathBuf> {
    let c = std::fs::canonicalize(p).ok()?;
    #[cfg(windows)]
    {
        let s = c.to_string_lossy();
        if let Some(rest) = s.strip_prefix(r"\\?\") {
            if !rest.starts_with("UNC\\") {
                return Some(PathBuf::from(rest));
            }
        }
    }
    Some(c)
}

/// 托管工作树放在哪：`[desktop] git-worktree-root`，没有时 `$CODEX_HOME/worktrees`。
pub fn managed_root(codex_home: &Path) -> PathBuf {
    let configured = std::fs::read_to_string(codex_home.join("config.toml"))
        .ok()
        .and_then(|raw| raw.parse::<toml_edit::DocumentMut>().ok())
        .and_then(|doc| doc.get("desktop")?.get("git-worktree-root")?.as_str().map(|s| s.trim().to_string()))
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .filter(|p| p.is_absolute());
    configured.unwrap_or_else(|| codex_home.join("worktrees"))
}

/// 工作树里对应的目录。`rel` 为空时 `join` 会多出结尾的 `/`，和 Codex 记下的 cwd 对不上
fn under(root: &Path, rel: &Path) -> PathBuf {
    if rel.as_os_str().is_empty() {
        root.to_path_buf()
    } else {
        root.join(rel)
    }
}

/// `cwd` 所在仓库的根目录；不是仓库（或没装 git）时 `None`。
pub fn repo_root(cwd: &Path) -> Option<PathBuf> {
    crate::git::toplevel(cwd).ok().flatten().map(|p| canon(&p))
}

/// `<根>/<4 位十六进制>/<名字>`
fn managed_layout(root: &Path, checkout: &Path) -> bool {
    let Ok(rel) = checkout.strip_prefix(root) else { return false };
    let mut c = rel.components();
    let bucket_ok = matches!(c.next(), Some(Component::Normal(b)) if b.to_str().is_some_and(|b| b.len() == 4 && b.bytes().all(|x| x.is_ascii_hexdigit())));
    bucket_ok && matches!(c.next(), Some(Component::Normal(_))) && c.next().is_none()
}

fn allocate(root: &Path, repo_name: &std::ffi::OsStr) -> R<PathBuf> {
    std::fs::create_dir_all(root).map_err(|e| fail("cannot create worktree root", e))?;
    if cfg!(target_os = "macos") {
        let _ = std::fs::write(root.join(".metadata_never_index"), b"");
    }
    for _ in 0..4096 {
        let id = crate::util::random_id(8).to_ascii_lowercase();
        let hex: String = id.bytes().map(|b| char::from_digit(u32::from(b) % 16, 16).unwrap_or('0')).take(4).collect();
        let bucket = root.join(&hex);
        match std::fs::create_dir(&bucket) {
            Ok(()) => return Ok(bucket.join(repo_name)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(fail("cannot create worktree bucket", e)),
        }
    }
    Err(fail("cannot create worktree bucket", "all identifiers are in use"))
}

/// 照 Codex 的做法新建一个托管工作树（从 `source_cwd` 所在仓库的 HEAD）。
pub fn create(root: &Path, source_cwd: &Path) -> R<Checkout> {
    if !root.is_absolute() {
        return Err(fail("managed worktree root must be an absolute path", root.display()));
    }
    let source_cwd = canon(source_cwd);
    let source_root = repo_root(&source_cwd).ok_or_else(|| LocalError::new("CODEX_WORKTREE_NO_REPO"))?;
    let rel = source_cwd.strip_prefix(&source_root).map_err(|_| fail("working directory is outside the repository root", source_cwd.display()))?.to_path_buf();
    let name = source_root.file_name().ok_or_else(|| fail("repository root has no directory name", source_root.display()))?.to_os_string();
    let head = git(&source_root, &["rev-parse", "--verify", "--end-of-options", "HEAD^{commit}"])?;
    let checkout = allocate(root, &name)?;
    let checkout_s = checkout.to_string_lossy().into_owned();
    if let Err(e) = git(&source_root, &["worktree", "add", "--detach", "--no-checkout", &checkout_s, &head]) {
        let _ = checkout.parent().map(std::fs::remove_dir);
        return Err(e);
    }
    let populate = (|| -> R<()> {
        let config = git(&checkout, &["rev-parse", "--path-format=absolute", "--git-path", "config.worktree"])?;
        git(&checkout, &["config", "--file", &config, "core.worktree", &checkout_s])?;
        git(&checkout, &["--work-tree=.", "reset", "--hard", "--no-recurse-submodules", &head])?;
        Ok(())
    })();
    if let Err(e) = populate {
        let _ = git(&source_root, &["worktree", "remove", "--force", &checkout_s]);
        let _ = checkout.parent().map(std::fs::remove_dir);
        return Err(e);
    }
    let cwd = under(&checkout, &rel);
    if !cwd.is_dir() {
        let _ = git(&source_root, &["worktree", "remove", "--force", &checkout_s]);
        let _ = checkout.parent().map(std::fs::remove_dir);
        return Err(fail("the new worktree has no such directory", rel.display()));
    }
    Ok(Checkout { root: checkout, cwd, owner: None, head: Some(head) })
}

fn owner_path(checkout: &Path) -> R<PathBuf> {
    let p = git(checkout, &["rev-parse", "--git-path", OWNER_FILE])?;
    Ok(checkout.join(p))
}

/// 这个工作树属于哪段对话。
pub fn owner(checkout: &Path) -> Option<String> {
    let raw = std::fs::read(owner_path(checkout).ok()?).ok()?;
    let v: serde_json::Value = serde_json::from_slice(&raw).ok()?;
    (v["version"] == 1).then(|| v["ownerThreadId"].as_str().filter(|s| !s.is_empty()).map(String::from)).flatten()
}

/// 记下工作树属于哪段对话；已经属于别的对话时不改。
pub fn bind(checkout: &Path, thread: &str) -> R<()> {
    if let Some(cur) = owner(checkout) {
        return if cur == thread { Ok(()) } else { Err(fail("worktree already belongs to thread", cur)) };
    }
    let path = owner_path(checkout)?;
    let body = serde_json::json!({ "version": 1, "ownerThreadId": thread }).to_string();
    // 不覆盖：同时有人写时以先写的为准
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).open(&path).map_err(|e| fail("cannot write worktree owner", e))?;
    f.write_all(body.as_bytes()).map_err(|e| fail("cannot write worktree owner", e))
}

/// 这个仓库的托管工作树（`git worktree list` 里在托管目录下、是同一个仓库的），带上归属和会话目录。
pub fn list(root: &Path, source_cwd: &Path) -> R<Vec<Checkout>> {
    let source_cwd = canon(source_cwd);
    let Some(source_root) = repo_root(&source_cwd) else { return Ok(vec![]) };
    let rel = source_cwd.strip_prefix(&source_root).map(Path::to_path_buf).unwrap_or_default();
    let managed = canon(root);
    let raw = git(&source_root, &["worktree", "list", "--porcelain"])?;
    let mut out = vec![];
    for entry in raw.split("\n\n") {
        let mut path = None;
        let mut head = None;
        for line in entry.lines() {
            if let Some(p) = line.strip_prefix("worktree ") {
                path = Some(PathBuf::from(p));
            } else if let Some(h) = line.strip_prefix("HEAD ") {
                head = Some(h.to_string());
            }
        }
        let Some(path) = path else { continue };
        let checkout = canon(&path);
        if !managed_layout(&managed, &checkout) || !checkout.join(".git").is_file() {
            continue;
        }
        let cwd = under(&path, &rel);
        if !cwd.is_dir() {
            continue;
        }
        out.push(Checkout { owner: owner(&checkout), root: path, cwd, head });
    }
    out.sort_by(|a, b| a.root.cmp(&b.root));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(dir: &Path, args: &[&str]) {
        let ok = std::process::Command::new("git").args(args).current_dir(dir).env("GIT_AUTHOR_NAME", "t").env("GIT_AUTHOR_EMAIL", "t@t").env("GIT_COMMITTER_NAME", "t").env("GIT_COMMITTER_EMAIL", "t@t").status().unwrap().success();
        assert!(ok, "git {args:?}");
    }

    #[test]
    fn layout() {
        let r = Path::new("/h/.codex/worktrees");
        assert!(managed_layout(r, Path::new("/h/.codex/worktrees/a1b2/repo")));
        assert!(!managed_layout(r, Path::new("/h/.codex/worktrees/a1b2")));
        assert!(!managed_layout(r, Path::new("/h/.codex/worktrees/xyz1/repo")));
        assert!(!managed_layout(r, Path::new("/h/.codex/worktrees/a1b2/repo/sub")));
        assert!(!managed_layout(r, Path::new("/elsewhere/a1b2/repo")));
    }

    #[test]
    fn root_from_config() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(managed_root(tmp.path()), tmp.path().join("worktrees"));
        std::fs::write(tmp.path().join("config.toml"), "model = \"x\"\n[desktop]\ngit-worktree-root = \"/srv/wt\"\n").unwrap();
        assert_eq!(managed_root(tmp.path()), PathBuf::from("/srv/wt"));
        std::fs::write(tmp.path().join("config.toml"), "[desktop]\ngit-worktree-root = \"relative\"\n").unwrap();
        assert_eq!(managed_root(tmp.path()), tmp.path().join("worktrees"), "相对路径不算");
    }

    #[test]
    fn create_bind_list() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("my-repo");
        std::fs::create_dir_all(repo.join("app")).unwrap();
        sh(&repo, &["init", "-q"]);
        std::fs::write(repo.join("app/a.txt"), "hi").unwrap();
        sh(&repo, &["add", "."]);
        sh(&repo, &["commit", "-qm", "x"]);
        let root = tmp.path().join("codex/worktrees");
        let c = create(&root, &repo.join("app")).unwrap();
        assert!(managed_layout(&canon(&root), &canon(&c.root)));
        assert_eq!(c.root.file_name().unwrap(), "my-repo");
        assert!(c.cwd.ends_with("my-repo/app"));
        assert_eq!(std::fs::read_to_string(c.cwd.join("a.txt")).unwrap(), "hi");
        assert_eq!(owner(&c.root), None);
        bind(&c.root, "t-1").unwrap();
        bind(&c.root, "t-1").unwrap();
        assert!(bind(&c.root, "t-2").is_err(), "不抢别的对话的工作树");
        assert_eq!(owner(&c.root).as_deref(), Some("t-1"));
        let l = list(&root, &repo.join("app")).unwrap();
        assert_eq!(l.len(), 1);
        assert_eq!(l[0].owner.as_deref(), Some("t-1"));
        assert!(l[0].cwd.ends_with("app"));
        // 不是仓库
        let plain = tmp.path().join("plain");
        std::fs::create_dir_all(&plain).unwrap();
        assert!(repo_root(&plain).is_none());
        assert!(list(&root, &plain).unwrap().is_empty());
    }

    #[test]
    fn cwd_at_root_has_no_trailing_slash() {
        let root = Path::new("/w/e910/repo");
        assert_eq!(under(root, Path::new("")).to_string_lossy(), "/w/e910/repo");
        assert_eq!(under(root, Path::new("app")), root.join("app"));
    }
}
