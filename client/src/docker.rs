//! Docker 容器里的会话：`docker ps` 列出运行中的容器，探测容器里的用户、默认目录和装了哪些 AI 工具，
//! 拼出 `docker exec -it …` 启动命令。tmux（会话保持）仍在这台电脑上，容器里只运行工具本身。
//!
//! 容器里的会话没有对话界面：hooks 回调的本机地址、对话记录（`~/.claude/projects` 等）都在容器里，客户端拿不到。

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::process::Command;

use crate::error::LocalError;

const TIMEOUT: Duration = Duration::from_secs(8);
const AGENTS: [&str; 3] = ["claude", "codex", "opencode"];

/// 会话所在的容器（存在 kv：`instance_container.{id}`）。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Container {
    /// 容器名（或 ID）
    pub name: String,
    /// 容器里的工作目录
    pub dir: String,
    /// 容器里默认用户是 root（新建时探测）
    #[serde(default)]
    pub root: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Listed {
    pub id: String,
    pub name: String,
    pub image: String,
    pub status: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Probe {
    pub root: bool,
    pub user: String,
    pub workdir: String,
    pub tools: Vec<String>,
}

/// docker 命令：PATH 里的，再看 Docker Desktop / OrbStack 的常见位置（macOS 的 App 从访达启动时 PATH 很短）。
pub fn binary() -> Option<String> {
    if let Some(p) = crate::shells::which("docker") {
        return Some(p);
    }
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from).unwrap_or_default();
    [
        Path::new("/Applications/Docker.app/Contents/Resources/bin/docker").to_path_buf(),
        home.join(".docker/bin/docker"),
        home.join(".orbstack/bin/docker"),
    ]
    .into_iter()
    .find(|p| p.is_file())
    .map(|p| p.display().to_string())
}

async fn run(args: &[&str]) -> Result<String, LocalError> {
    let bin = binary().ok_or_else(|| LocalError::new("DOCKER_NOT_FOUND"))?;
    let mut cmd = Command::new(bin);
    cmd.args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000);
    let out = tokio::time::timeout(TIMEOUT, cmd.output())
        .await
        .map_err(|_| LocalError::new("DOCKER_FAILED").with("detail", "timeout"))?
        .map_err(|e| LocalError::new("DOCKER_FAILED").with("detail", e.to_string()))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().chars().take(300).collect::<String>();
        return Err(LocalError::new("DOCKER_FAILED").with("detail", err));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// 运行中的容器（`docker ps`）。
pub async fn list() -> Result<Vec<Listed>, LocalError> {
    let out = run(&["ps", "--format", "{{json .}}"]).await?;
    Ok(parse_ps(&out))
}

fn parse_ps(out: &str) -> Vec<Listed> {
    out.lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l.trim()).ok())
        .map(|v| {
            let s = |k: &str| v[k].as_str().unwrap_or("").to_string();
            Listed { id: s("ID"), name: s("Names").split(',').next().unwrap_or("").to_string(), image: s("Image"), status: s("Status") }
        })
        .filter(|c| !c.name.is_empty())
        .collect()
}

/// 容器名 / ID 只允许 Docker 自己允许的字符（要拼进命令行）。
pub fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 128 && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
}

/// 容器里的目录：绝对路径、不含控制字符和引号。
pub fn valid_dir(dir: &str) -> bool {
    dir.starts_with('/') && dir.len() <= 1000 && !dir.chars().any(|c| c.is_control() || c == '\'' || c == '"')
}

/// 容器里的默认用户、默认目录、装了哪些 AI 工具（用登录 shell 找，PATH 与在容器里打开终端时一致）。
pub async fn probe(name: &str) -> Result<Probe, LocalError> {
    if !valid_name(name) {
        return Err(LocalError::invalid("container", "invalid"));
    }
    let script = format!(
        "id -u; id -un 2>/dev/null || echo; pwd; for t in {}; do command -v $t >/dev/null 2>&1 && echo tool:$t; done; true",
        AGENTS.join(" ")
    );
    let out = run(&["exec", name, "sh", "-lc", &script]).await?;
    Ok(parse_probe(&out))
}

fn parse_probe(out: &str) -> Probe {
    let mut lines = out.lines().map(str::trim);
    let uid = lines.next().unwrap_or("");
    let user = lines.next().unwrap_or("").to_string();
    let workdir = lines.next().unwrap_or("/").to_string();
    let tools = lines.filter_map(|l| l.strip_prefix("tool:")).map(String::from).collect();
    Probe { root: uid == "0", user, workdir: if workdir.starts_with('/') { workdir } else { "/".into() }, tools }
}

/// 容器里的文件夹列表（开始页选“容器里的目录”），字段和 `GET /api/fs/list` 一样，界面共用文件夹选择弹窗。
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct DirList {
    pub path: String,
    pub parent: Option<String>,
    pub entries: Vec<DirEntry>,
    pub exists: bool,
    pub denied: bool,
    pub os: &'static str,
    pub roots: Vec<String>,
    pub home: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DirEntry {
    pub name: String,
    pub is_dir: bool,
}

/// 列出容器里的子文件夹（以容器默认用户身份）。路径为空或 `~` 取容器里的主目录；不存在时退到最近的已存在上级。
/// 路径和开关作为 `sh -c` 的位置参数传进去，不拼进脚本。
pub async fn list_dirs(name: &str, path: &str, hidden: bool) -> Result<DirList, LocalError> {
    if !valid_name(name) {
        return Err(LocalError::invalid("container", "invalid"));
    }
    let path = path.trim();
    if !(path.is_empty() || path == "~" || path.starts_with("~/") || path.starts_with('/')) || path.chars().any(char::is_control) {
        return Err(LocalError::new("WORKDIR_NOT_FOUND").with("path", path));
    }
    const SCRIPT: &str = r#"p=$1; h=${HOME:-/}
case $p in ''|'~') p=$h ;; '~/'*) p=$h/${p#'~/'} ;; esac
want=$p
while [ ! -d "$p" ]; do
  case $p in /|'') p=/; break ;; esac
  p=$(dirname "$p")
done
echo "home:$h"
echo "path:$p"
[ "$p" = "$want" ] && echo exists
cd "$p" 2>/dev/null && [ -r . ] || { echo denied; exit 0; }
for f in .* *; do
  case $f in .|..) continue ;; .*) [ "$2" = 1 ] || continue ;; esac
  [ -d "$f" ] && printf 'd:%s
' "$f"
done
exit 0"#;
    let out = run(&["exec", name, "sh", "-c", SCRIPT, "sh", path, if hidden { "1" } else { "0" }]).await?;
    Ok(parse_dirs(&out))
}

fn parse_dirs(out: &str) -> DirList {
    let mut l = DirList { os: "linux", roots: vec!["/".into()], path: "/".into(), ..Default::default() };
    for line in out.lines() {
        if let Some(h) = line.strip_prefix("home:") {
            l.home = Some(h.to_string()).filter(|h| h.starts_with('/'));
        } else if let Some(p) = line.strip_prefix("path:") {
            // `dirname` 可能留下 `//`、结尾的 `/`
            let p = p.trim_end_matches('/');
            l.path = if p.is_empty() { "/".into() } else { p.replace("//", "/") };
        } else if line == "exists" {
            l.exists = true;
        } else if line == "denied" {
            l.denied = true;
        } else if let Some(n) = line.strip_prefix("d:") {
            l.entries.push(DirEntry { name: n.to_string(), is_dir: true });
        }
    }
    l.entries.sort_by_key(|e| e.name.to_lowercase());
    l.entries.truncate(2000);
    l.parent = (l.path != "/").then(|| match l.path.rfind('/') {
        Some(0) | None => "/".to_string(),
        Some(i) => l.path[..i].to_string(),
    });
    l
}

/// POSIX sh 的单引号写法（容器里一定是 sh）。
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// 把启动命令放进容器：`docker exec -it [-e NAME=值…] <容器> sh -lc 'mkdir -p <目录>…; cd <目录> && exec <命令>'`。
/// 目录不存在时先建好（和普通文件夹一样，选择文件夹时可以新建）。
/// `inner` 是容器里要运行的命令（POSIX 写法，开头可以带 `NAME=值` 环境变量，例如 root + 全部权限时的
/// `IS_SANDBOX=1 claude …`，这里改成 `-e`）；None 时打开容器里的 shell（有 bash 用 bash）。
/// 返回的命令行按这台电脑的 shell（`family`）加引号。
pub fn exec_command(docker: &str, c: &Container, inner: Option<&str>, family: crate::shells::Family) -> String {
    let q = |s: &str| crate::shells::quote_arg(s, family);
    let mut env = vec![];
    let script = match inner.map(str::trim).filter(|s| !s.is_empty()) {
        Some(cmd) => {
            let mut rest = cmd;
            while let Some((head, tail)) = rest.split_once(' ') {
                let is_env = head.split_once('=').is_some_and(|(k, _)| !k.is_empty() && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'));
                if !is_env {
                    break;
                }
                env.push(head.to_string());
                rest = tail.trim_start();
            }
            format!("exec {rest}")
        }
        None => "if command -v bash >/dev/null 2>&1; then exec bash -l; else exec sh -l; fi".into(),
    };
    let dir = sh_quote(&c.dir);
    let script = format!("mkdir -p {dir} 2>/dev/null; cd {dir} && {script}");
    // PowerShell 里带引号的路径是字符串，要用 `&` 才会当命令运行
    let program = if family == crate::shells::Family::Pwsh { format!("& {}", q(docker)) } else { q(docker) };
    let mut parts = vec![program, "exec".into(), "-it".into()];
    for e in env {
        parts.push("-e".into());
        parts.push(q(&e));
    }
    parts.extend([q(&c.name), "sh".into(), "-lc".into(), q(&script)]);
    parts.join(" ")
}

/// 第一句话、普通终端里的命令接在容器里的命令后面时用的引号（容器里是 sh）。
pub fn quote_inner(s: &str) -> String {
    sh_quote(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shells::Family;

    #[test]
    fn parses_ps_and_probe() {
        let ps = "{\"ID\":\"ab12\",\"Names\":\"dev\",\"Image\":\"node:22\",\"Status\":\"Up 2 hours\"}\n\n";
        let l = parse_ps(ps);
        assert_eq!((l[0].id.as_str(), l[0].name.as_str(), l[0].image.as_str()), ("ab12", "dev", "node:22"));
        let p = parse_probe("0\nroot\n/app\ntool:claude\ntool:codex\n");
        assert!(p.root);
        assert_eq!((p.user.as_str(), p.workdir.as_str()), ("root", "/app"));
        assert_eq!(p.tools, vec!["claude", "codex"]);
        assert!(!parse_probe("1000\nnode\n/home/node\n").root);
    }

    #[test]
    fn parses_dirs() {
        let l = parse_dirs("home:/root\npath:/app/src\nexists\nd:lib\nd:Bin\n");
        assert_eq!((l.path.as_str(), l.parent.as_deref(), l.exists, l.home.as_deref()), ("/app/src", Some("/app"), true, Some("/root")));
        assert_eq!(l.entries.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(), ["Bin", "lib"]);
        let r = parse_dirs("home:/\npath:/\ndenied\n");
        assert_eq!((r.path.as_str(), r.parent, r.exists, r.denied), ("/", None, false, true));
        assert_eq!(parse_dirs("path:/home\n").parent.as_deref(), Some("/"));
    }

    #[test]
    fn builds_exec_command() {
        let c = Container { name: "dev".into(), dir: "/app".into(), root: true };
        assert_eq!(
            exec_command("docker", &c, Some("IS_SANDBOX=1 claude --dangerously-skip-permissions 'hi'"), Family::Posix),
            "'docker' exec -it -e 'IS_SANDBOX=1' 'dev' sh -lc 'mkdir -p '\\''/app'\\'' 2>/dev/null; cd '\\''/app'\\'' && exec claude --dangerously-skip-permissions '\\''hi'\\'''"
        );
        assert!(exec_command("docker", &c, None, Family::Posix).ends_with("'dev' sh -lc 'mkdir -p '\\''/app'\\'' 2>/dev/null; cd '\\''/app'\\'' && if command -v bash >/dev/null 2>&1; then exec bash -l; else exec sh -l; fi'"));
        assert!(valid_name("my_app-1.2") && !valid_name("a b") && !valid_name("x;rm"));
        assert!(valid_dir("/app/src") && !valid_dir("app") && !valid_dir("/a'b"));
    }
}
