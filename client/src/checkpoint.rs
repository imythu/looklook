//! 每轮检查点（docs/PLAN_2026-10-27.md §3.3）：会话的文件夹是 git 仓库时，每一轮开始给工作区拍一个快照，
//! 之后可以对比、回到那一轮之前。
//!
//! - 快照：复制仓库的 index 到临时文件，`GIT_INDEX_FILE=临时 git add -A`（沿用 `.gitignore`，未跟踪的新文件也算），
//!   `write-tree`，`commit-tree`（父提交是 HEAD）。**不动用户的 index、HEAD、分支、stash，也不建 ref**：
//!   ref 会出现在 `git log --all` 里、被 `push --mirror` 推走。提交只记在看看自己的数据库里（每个会话最近 20 个），
//!   git 清理没人引用的对象至少等两周（`gc.pruneExpire`），对“回到前几轮”够用；真被清掉了就提示没法恢复。
//! - 复制 index 是为了用上已有的文件状态缓存，大仓库也快（连修改时间一起保留，git 才会照常复查刚改过的文件）；
//!   超过 10 秒就放弃这一次。
//! - 恢复：先给现在拍一个“恢复前”的检查点（恢复本身也能撤销），快照里有的文件用
//!   `git restore --source=<快照> --worktree` 恢复（只动工作区），之后新增的文件删掉。

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::error::LocalError;
use crate::git::{cap, failed, run_env, toplevel};
use crate::store::Store;

type R<T> = Result<T, LocalError>;

const KEEP: usize = 20;
const TAKE_TIMEOUT: Duration = Duration::from_secs(10);
const GIT_TIMEOUT: Duration = Duration::from_secs(30);
const LABEL_MAX: usize = 120;
/// 同一时间只拍一个（同一个会话连着两轮、两个会话同一个仓库）
static BUSY: Mutex<()> = Mutex::new(());

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cp {
    /// 拍的时间（毫秒），也是检查点的 id
    pub id: i64,
    pub commit: String,
    pub tree: String,
    /// 那一轮的第一句话
    pub label: String,
    pub root: String,
    /// 恢复之前自动拍的
    #[serde(default)]
    pub before_restore: bool,
}

fn key(inst: &str) -> String {
    format!("checkpoints:{inst}")
}

fn saved(store: &Store, inst: &str) -> Vec<Cp> {
    store.get(&key(inst)).ok().flatten().unwrap_or_default()
}

/// 删会话时一起删（提交对象留给 git 自己清理）
pub fn forget(store: &Store, inst: &str) {
    let _ = store.remove(&key(inst));
}

fn git(root: &Path, args: &[&str], env: &[(&str, &std::ffi::OsStr)], timeout: Duration) -> R<String> {
    let out = run_env(root, args, env, timeout)?;
    if !out.status.success() {
        return Err(failed(&out));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim_end().to_string())
}

/// 工作区现在的样子（含未跟踪的文件，按 `.gitignore`）写成一棵目录树，返回它的 id。不动用户的 index
fn snapshot_tree(root: &Path, timeout: Duration) -> R<String> {
    let index = git(root, &["rev-parse", "--git-path", "index"], &[], timeout)?;
    let index = root.join(index);
    let tmp = std::env::temp_dir().join(format!("looklook-cp-{}-{:x}.idx", std::process::id(), rand::random::<u64>()));
    let attempt = |copy: bool| -> R<String> {
        let _ = std::fs::remove_file(&tmp);
        if copy && index.is_file() {
            let io = |e: std::io::Error| LocalError::new("GIT_FAILED").with("detail", e.to_string());
            std::fs::copy(&index, &tmp).map_err(io)?;
            // 保留原来的修改时间：git 靠“文件不比 index 新”判断缓存的文件状态可不可信（racy git），
            // 复制出来的 index 时间是现在，刚改过、大小没变的文件会被当成没改
            let at = std::fs::metadata(&index).and_then(|m| m.modified()).map_err(io)?;
            std::fs::File::options().write(true).open(&tmp).and_then(|f| f.set_modified(at)).map_err(io)?;
        }
        let env = [("GIT_INDEX_FILE", tmp.as_os_str())];
        git(root, &["add", "-A", "--", ":/"], &env, timeout)?;
        git(root, &["write-tree"], &env, timeout)
    };
    // 复制来的 index 用不了（例如拆分的 index）：从空的开始
    let r = attempt(true).or_else(|e| if e.code == "GIT_TIMEOUT" { Err(e) } else { attempt(false) });
    let _ = std::fs::remove_file(&tmp);
    let _ = std::fs::remove_file(tmp.with_extension("idx.lock"));
    r
}

fn label_of(text: &str) -> String {
    let line = text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    line.chars().take(LABEL_MAX).collect()
}

/// 拍一个检查点。不是仓库、和上一个一样时为 None
pub fn take(store: &Store, inst: &str, workdir: &Path, label: &str, before_restore: bool) -> R<Option<Cp>> {
    let _busy = BUSY.lock().unwrap_or_else(|e| e.into_inner());
    let Some(root) = toplevel(workdir)? else { return Ok(None) };
    let tree = snapshot_tree(&root, TAKE_TIMEOUT)?;
    let mut list = saved(store, inst);
    let root_s = root.display().to_string();
    if list.last().is_some_and(|c| c.tree == tree && c.root == root_s) {
        return Ok(None);
    }
    let label = label_of(label);
    let head = git(&root, &["rev-parse", "--verify", "-q", "HEAD"], &[], TAKE_TIMEOUT).ok();
    let mut args = vec!["commit-tree", tree.as_str()];
    if let Some(h) = head.as_deref() {
        args.extend(["-p", h]);
    }
    let msg = if label.is_empty() { "Looklook checkpoint".to_string() } else { format!("Looklook checkpoint: {label}") };
    args.extend(["-m", msg.as_str()]);
    // 用户可能没设置名字和邮箱
    let who = std::ffi::OsStr::new("Looklook");
    let mail = std::ffi::OsStr::new("checkpoint@looklook.local");
    let env = [("GIT_AUTHOR_NAME", who), ("GIT_AUTHOR_EMAIL", mail), ("GIT_COMMITTER_NAME", who), ("GIT_COMMITTER_EMAIL", mail)];
    let commit = git(&root, &args, &env, TAKE_TIMEOUT)?;
    let now = crate::util::system_ms();
    let id = list.last().map_or(now, |c| now.max(c.id + 1));
    let cp = Cp { id, commit, tree, label, root: root_s, before_restore };
    list.push(cp.clone());
    if list.len() > KEEP {
        list.drain(..list.len() - KEEP);
    }
    store.set(&key(inst), &list).map_err(LocalError::from)?;
    Ok(Some(cp))
}

/// 两棵树之间改了哪些文件：`(状态字母, 路径)`，不认改名（恢复时按删除 + 新增处理）
fn changes(root: &Path, from: &str, to: &str) -> R<Vec<(char, String)>> {
    let raw = git(root, &["diff", "--name-status", "-z", "--no-renames", from, to], &[], GIT_TIMEOUT)?;
    let mut it = raw.split('\0').filter(|s| !s.is_empty());
    let mut out = vec![];
    while let (Some(st), Some(path)) = (it.next(), it.next()) {
        out.push((st.chars().next().unwrap_or('M'), path.to_string()));
    }
    Ok(out)
}

fn find(store: &Store, inst: &str, id: i64) -> R<Cp> {
    saved(store, inst).into_iter().find(|c| c.id == id).ok_or_else(LocalError::not_found)
}

/// 会话的检查点（新的在前），每个带“之后改了几个文件”（相对现在）
pub fn list(store: &Store, inst: &str, workdir: &Path) -> R<Value> {
    let list = saved(store, inst);
    let Some(root) = toplevel(workdir)? else { return Ok(json!({ "repo": false, "items": [] })) };
    let root_s = root.display().to_string();
    let now = if list.is_empty() { None } else { Some(snapshot_tree(&root, GIT_TIMEOUT)?) };
    let items: Vec<Value> = list
        .iter()
        .rev()
        .filter(|c| c.root == root_s)
        .map(|c| {
            let changed = now.as_deref().map(|n| changes(&root, &c.tree, n).map(|v| v.len()));
            json!({ "id": c.id, "label": c.label, "before_restore": c.before_restore, "changed": changed.as_ref().and_then(|r| r.as_ref().ok()), "missing": changed.is_some_and(|r| r.is_err()) })
        })
        .collect();
    Ok(json!({ "repo": true, "items": items }))
}

/// 从检查点到现在改了什么：文件列表和对比
pub fn diff(store: &Store, inst: &str, workdir: &Path, id: i64) -> R<Value> {
    let cp = find(store, inst, id)?;
    let root = PathBuf::from(&cp.root);
    if toplevel(workdir)?.as_deref() != Some(root.as_path()) {
        return Err(LocalError::new("CHECKPOINT_MOVED"));
    }
    let now = snapshot_tree(&root, GIT_TIMEOUT)?;
    let files = changes(&root, &cp.tree, &now).map_err(|_| LocalError::new("CHECKPOINT_GONE"))?;
    let out = run_env(&root, &["diff", "--no-ext-diff", "--no-renames", &cp.tree, &now], &[], GIT_TIMEOUT)?;
    let d = cap(&out.stdout);
    Ok(json!({
        "files": files.iter().map(|(s, p)| json!({ "status": s.to_string(), "path": p })).collect::<Vec<_>>(),
        "text": d.text,
        "truncated": d.truncated,
        "binary": d.binary,
    }))
}

/// 回到检查点：先拍“恢复前”，再恢复改过、删掉的文件，删掉之后新增的文件
pub fn restore(store: &Store, inst: &str, workdir: &Path, id: i64) -> R<Value> {
    let cp = find(store, inst, id)?;
    let root = PathBuf::from(&cp.root);
    if toplevel(workdir)?.as_deref() != Some(root.as_path()) {
        return Err(LocalError::new("CHECKPOINT_MOVED"));
    }
    take(store, inst, workdir, &cp.label, true)?;
    let _busy = BUSY.lock().unwrap_or_else(|e| e.into_inner());
    let now = snapshot_tree(&root, GIT_TIMEOUT)?;
    let files = changes(&root, &cp.tree, &now).map_err(|_| LocalError::new("CHECKPOINT_GONE"))?;
    let (added, back): (Vec<_>, Vec<_>) = files.into_iter().partition(|(s, _)| *s == 'A');
    for chunk in back.chunks(200) {
        let specs: Vec<String> = chunk.iter().map(|(_, p)| format!(":(literal){p}")).collect();
        let mut args = vec!["restore", "--source", cp.commit.as_str(), "--worktree", "--"];
        args.extend(specs.iter().map(String::as_str));
        git(&root, &args, &[], GIT_TIMEOUT)?;
    }
    let mut removed = 0;
    for (_, p) in &added {
        // git 给的是仓库里的相对路径；删文件本身（是链接就删链接），再删掉因此空了的文件夹
        if p.split('/').any(|s| s == ".." || s.is_empty()) {
            continue;
        }
        let path = root.join(p);
        if std::fs::remove_file(&path).is_ok() {
            removed += 1;
            let mut dir = path.parent();
            while let Some(d) = dir.filter(|d| *d != root && d.starts_with(&root)) {
                if std::fs::remove_dir(d).is_err() {
                    break;
                }
                dir = d.parent();
            }
        }
    }
    Ok(json!({ "restored": back.len(), "removed": removed }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(dir: &Path, args: &[&str]) {
        let ok = std::process::Command::new("git").arg("-C").arg(dir).args(args).env("GIT_AUTHOR_NAME", "t").env("GIT_AUTHOR_EMAIL", "t@t").env("GIT_COMMITTER_NAME", "t").env("GIT_COMMITTER_EMAIL", "t@t").status().unwrap().success();
        assert!(ok, "git {args:?}");
    }

    fn read(p: PathBuf) -> Option<String> {
        std::fs::read_to_string(p).ok()
    }

    #[test]
    fn take_diff_restore() {
        if crate::shells::which("git").is_none() {
            return;
        }
        let home = tempfile::tempdir().unwrap();
        let store = Store::open(&home.path().join("t.db")).unwrap();
        let repo = tempfile::tempdir().unwrap();
        let root = repo.path().canonicalize().unwrap();
        sh(&root, &["init", "-q"]);
        std::fs::write(root.join("a.txt"), "one\n").unwrap();
        std::fs::write(root.join(".gitignore"), "target/\n").unwrap();
        sh(&root, &["add", "-A"]);
        sh(&root, &["commit", "-qm", "init"]);
        std::fs::write(root.join("b.txt"), "draft\n").unwrap();
        sh(&root, &["add", "b.txt"]);
        let index_before = std::fs::read(root.join(".git/index")).unwrap();

        let cp = take(&store, "i1", &root, "first line\nmore", false).unwrap().unwrap();
        assert_eq!(cp.label, "first line");
        // 没变：不再拍
        assert!(take(&store, "i1", &root, "again", false).unwrap().is_none());
        // 用户的 index、HEAD 没动，也没有建 ref
        assert_eq!(std::fs::read(root.join(".git/index")).unwrap(), index_before);
        let refs = std::process::Command::new("git").arg("-C").arg(&root).args(["for-each-ref"]).output().unwrap();
        assert!(!String::from_utf8_lossy(&refs.stdout).contains("looklook"));

        // 这一轮 AI 改了、删了、加了文件（忽略的不算）
        std::fs::write(root.join("a.txt"), "two\n").unwrap();
        std::fs::remove_file(root.join("b.txt")).unwrap();
        std::fs::create_dir_all(root.join("new/deep")).unwrap();
        std::fs::write(root.join("new/deep/c.txt"), "x\n").unwrap();
        std::fs::create_dir_all(root.join("target")).unwrap();
        std::fs::write(root.join("target/out"), "bin").unwrap();

        let l = list(&store, "i1", &root).unwrap();
        assert_eq!(l["items"][0]["changed"], 3);
        let d = diff(&store, "i1", &root, cp.id).unwrap();
        assert!(d["text"].as_str().unwrap().contains("+two"));

        let r = restore(&store, "i1", &root, cp.id).unwrap();
        assert_eq!((r["restored"].as_u64(), r["removed"].as_u64()), (Some(2), Some(1)));
        assert_eq!(read(root.join("a.txt")).as_deref(), Some("one\n"));
        assert_eq!(read(root.join("b.txt")).as_deref(), Some("draft\n"));
        assert!(!root.join("new").exists());
        assert!(root.join("target/out").exists());
        // 恢复前拍了一个，可以撤销恢复
        let all = saved(&store, "i1");
        assert_eq!(all.len(), 2);
        assert!(all[1].before_restore);
        restore(&store, "i1", &root, all[1].id).unwrap();
        assert_eq!(read(root.join("a.txt")).as_deref(), Some("two\n"));
        assert!(root.join("new/deep/c.txt").exists());
    }

    #[test]
    fn not_a_repo() {
        let home = tempfile::tempdir().unwrap();
        let store = Store::open(&home.path().join("t.db")).unwrap();
        let dir = tempfile::tempdir().unwrap();
        if crate::shells::which("git").is_some() {
            assert!(take(&store, "i1", dir.path(), "x", false).unwrap().is_none());
        }
    }
}
