//! 输入框里打 `@` 时提示的文件和目录（Claude Code、Codex 的 TUI 都有）。
//!
//! 在 Git 仓库里用 `git ls-files`（跟着 .gitignore，含没提交的新文件）；不是仓库就自己走一遍目录，跳过隐藏目录和常见的依赖、产物目录。
//! 列表按目录缓存一小会儿：每打一个字都要查，大仓库每次重新列太慢。

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// 列表最多这么多项（文件加目录），再多的不提示
const MAX_ENTRIES: usize = 200_000;
/// 不是 Git 仓库时最多走这么深、这么多个文件
const WALK_DEPTH: usize = 12;
const WALK_FILES: usize = 50_000;
const SKIP_DIRS: &[&str] = &["node_modules", "target", "dist", "build", "__pycache__", "venv", "vendor"];
const TTL: Duration = Duration::from_secs(20);

/// 一项：相对路径（`/` 分隔），目录以 `/` 结尾
type List = Arc<Vec<String>>;

fn cache() -> &'static Mutex<HashMap<PathBuf, (Instant, List)>> {
    static C: OnceLock<Mutex<HashMap<PathBuf, (Instant, List)>>> = OnceLock::new();
    C.get_or_init(Default::default)
}

fn entries(root: &Path) -> List {
    let mut c = cache().lock().unwrap_or_else(|e| e.into_inner());
    c.retain(|_, (t, _)| t.elapsed() < TTL);
    if let Some((_, l)) = c.get(root) {
        return l.clone();
    }
    drop(c);
    let files = git_files(root).unwrap_or_else(|| walk(root));
    let list = Arc::new(with_dirs(files));
    cache().lock().unwrap_or_else(|e| e.into_inner()).insert(root.to_path_buf(), (Instant::now(), list.clone()));
    list
}

fn git_files(root: &Path) -> Option<Vec<String>> {
    let out = crate::git::run_env(root, &["ls-files", "-co", "--exclude-standard", "-z"], &[], Duration::from_secs(10)).ok()?;
    if !out.status.success() {
        return None;
    }
    Some(out.stdout.split(|b| *b == 0).filter(|s| !s.is_empty()).take(MAX_ENTRIES).map(|s| String::from_utf8_lossy(s).into_owned()).collect())
}

fn walk(root: &Path) -> Vec<String> {
    let mut out = vec![];
    let mut stack = vec![(root.to_path_buf(), String::new(), 0)];
    while let Some((dir, rel, depth)) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            let path = if rel.is_empty() { name.clone() } else { format!("{rel}/{name}") };
            match e.file_type() {
                Ok(t) if t.is_dir() => {
                    if depth < WALK_DEPTH && !SKIP_DIRS.contains(&name.as_str()) {
                        stack.push((e.path(), path, depth + 1));
                    }
                }
                Ok(_) => out.push(path),
                Err(_) => {}
            }
            if out.len() >= WALK_FILES {
                return out;
            }
        }
    }
    out
}

/// 文件加上它们所在的目录（`a/b/c.rs` → `a/`、`a/b/`）
fn with_dirs(files: Vec<String>) -> Vec<String> {
    let mut dirs = HashSet::new();
    for f in &files {
        let mut at = 0;
        while let Some(n) = f[at..].find('/') {
            at += n + 1;
            dirs.insert(f[..at].to_string());
        }
    }
    let mut all: Vec<String> = dirs.into_iter().collect();
    all.extend(files);
    all.truncate(MAX_ENTRIES);
    all
}

/// 和 `q` 的匹配分数，越大越靠前；对不上是 None。
/// 文件名开头 > 文件名里 > 路径里 > 按顺序含有这些字（像 `chv` 对上 `ChatView`），同分时路径短的在前
fn score(q: &str, path: &str) -> Option<i64> {
    let p = path.to_lowercase();
    let base = p.trim_end_matches('/').rsplit('/').next().unwrap_or("");
    let s = if base.starts_with(q) {
        4000
    } else if base.contains(q) {
        3000
    } else if p.contains(q) {
        2000
    } else {
        let mut it = p.chars();
        if !q.chars().all(|c| it.any(|x| x == c)) {
            return None;
        }
        1000
    };
    Some(s - p.len() as i64)
}

/// `root` 下和 `q` 对得上的前 `limit` 项：`[{ path, dir }]`。`q` 空的时候是第一层的目录和文件
pub fn search(root: &Path, q: &str, limit: usize) -> Vec<serde_json::Value> {
    let q = q.trim().trim_start_matches("./").to_lowercase();
    let list = entries(root);
    let mut hits: Vec<(i64, &String)> = if q.is_empty() {
        list.iter().filter(|p| !p.trim_end_matches('/').contains('/')).map(|p| (if p.ends_with('/') { 1 } else { 0 }, p)).collect()
    } else {
        list.iter().filter_map(|p| score(&q, p).map(|s| (s, p))).collect()
    };
    hits.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(b.1)));
    hits.into_iter().take(limit).map(|(_, p)| serde_json::json!({ "path": p, "dir": p.ends_with('/') })).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranks_names_before_paths() {
        assert!(score("chat", "src/chat.rs").unwrap() > score("chat", "chat/x/mod.rs").unwrap());
        assert!(score("chat", "a/xchat.rs").unwrap() > score("chat", "chat/x.rs").unwrap());
        assert!(score("cvw", "pages/ChatView.jsx").is_some());
        assert!(score("zz", "pages/ChatView.jsx").is_none());
    }

    #[test]
    fn walks_and_adds_dirs() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("src/chat")).unwrap();
        std::fs::create_dir_all(d.path().join("node_modules/x")).unwrap();
        std::fs::create_dir_all(d.path().join(".hidden")).unwrap();
        for f in ["src/chat/mod.rs", "src/main.rs", "README.md", "node_modules/x/i.js", ".hidden/a"] {
            std::fs::write(d.path().join(f), "").unwrap();
        }
        let all: Vec<String> = search(d.path(), "mod", 10).into_iter().map(|v| v["path"].as_str().unwrap().to_string()).collect();
        assert_eq!(all, ["src/chat/mod.rs"]);
        let top: Vec<String> = search(d.path(), "", 10).into_iter().map(|v| v["path"].as_str().unwrap().to_string()).collect();
        assert_eq!(top, ["src/", "README.md"]);
        assert!(search(d.path(), "chat/", 10).iter().any(|v| v["path"] == "src/chat/" && v["dir"] == true));
    }
}
