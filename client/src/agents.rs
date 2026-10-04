//! 一键把看看的 MCP 接口和配套技能装进 Codex / Claude Code。
//!
//! - **Claude Code**：用它自己的命令 `claude mcp add --scope user`（`~/.claude.json` 由它自己维护，
//!   别的会话随时在改，不直接写）；状态从 `~/.claude.json` 里读。设置了 `CLAUDE_CONFIG_DIR` 时跟着它走。
//! - **Codex**：直接改 `$CODEX_HOME/config.toml`（默认 `~/.codex`）的 `[mcp_servers.looklook]`，
//!   用 toml_edit 保留用户原来的格式与注释。
//! - **技能**：写到 `<配置目录>/skills/<名字>/SKILL.md`。“自动”和“先问”两种只能装一种，装一种时删掉另一种。
//!
//! 状态里的 `mcp`：`none` 没装；`ok` 地址和令牌都对；`stale` 装过但地址或令牌已经变了（换了令牌、端口），要重装。

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;
use serde_json::Value;
use tokio::process::Command;

use crate::error::LocalError;
use crate::gateway::mcp::SERVER_NAME;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Agent {
    Claude,
    Codex,
}

impl Agent {
    pub const ALL: [Agent; 2] = [Agent::Claude, Agent::Codex];

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "claude" => Some(Agent::Claude),
            "codex" => Some(Agent::Codex),
            _ => None,
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Agent::Claude => "claude",
            Agent::Codex => "codex",
        }
    }

    /// 配置目录：`~/.claude` / `~/.codex`
    fn dir(self) -> PathBuf {
        let (env, default) = match self {
            Agent::Claude => ("CLAUDE_CONFIG_DIR", ".claude"),
            Agent::Codex => ("CODEX_HOME", ".codex"),
        };
        match std::env::var_os(env).filter(|v| !v.is_empty()) {
            Some(v) => PathBuf::from(v),
            None => home().join(default),
        }
    }

    fn skills_dir(self) -> PathBuf {
        self.dir().join("skills")
    }
}

fn home() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))
}

/// Claude Code 的用户级配置文件：默认 `~/.claude.json`；设置了 `CLAUDE_CONFIG_DIR` 时在那个目录里。
fn claude_json() -> PathBuf {
    match std::env::var_os("CLAUDE_CONFIG_DIR").filter(|v| !v.is_empty()) {
        Some(v) => PathBuf::from(v).join(".claude.json"),
        None => home().join(".claude.json"),
    }
}

/// 找 `claude` 命令：先在登录 shell 的 PATH 里找；找不到再看官方安装脚本的固定位置
/// （`~/.local/bin`，旧版的 `~/.claude/local`）——安装脚本只把它加进交互 shell 的配置，登录 shell 不一定有。
async fn claude_cli(shell: &str) -> Option<String> {
    crate::tools::detect(shell).await.claude.or_else(claude_fallback)
}

fn claude_fallback() -> Option<String> {
    let exe = if cfg!(windows) { "claude.exe" } else { "claude" };
    [home().join(".local").join("bin").join(exe), Agent::Claude.dir().join("local").join(exe)]
        .into_iter()
        .find(|p| p.is_file())
        .map(|p| p.display().to_string())
}

fn codex_toml() -> PathBuf {
    Agent::Codex.dir().join("config.toml")
}

// ---------------- 技能 ----------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Skill {
    /// 发现 HTTP 端口就自动创建/复用并给出地址
    Auto,
    /// 有就给地址，没有先问用户
    Ask,
}

impl Skill {
    const ALL: [Skill; 2] = [Skill::Auto, Skill::Ask];

    pub fn parse(s: &str) -> Option<Option<Self>> {
        match s {
            "auto" => Some(Some(Skill::Auto)),
            "ask" => Some(Some(Skill::Ask)),
            "none" | "" => Some(None),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Skill::Auto => "looklook-pages-auto",
            Skill::Ask => "looklook-pages-ask",
        }
    }

    pub fn content(self) -> &'static str {
        match self {
            Skill::Auto => include_str!("skills/looklook-pages-auto/SKILL.md"),
            Skill::Ask => include_str!("skills/looklook-pages-ask/SKILL.md"),
        }
    }
}

fn installed_skill(agent: Agent) -> Option<Skill> {
    let root = agent.skills_dir();
    Skill::ALL.into_iter().find(|s| root.join(s.name()).join("SKILL.md").is_file())
}

/// 装上选中的技能、删掉另一种；`None` 两种都删。只动我们自己的两个目录。
fn set_skill(agent: Agent, skill: Option<Skill>) -> std::io::Result<()> {
    let root = agent.skills_dir();
    for s in Skill::ALL {
        let dir = root.join(s.name());
        if Some(s) == skill {
            std::fs::create_dir_all(&dir)?;
            write_atomic(&dir.join("SKILL.md"), s.content().as_bytes())?;
        } else if dir.exists() {
            std::fs::remove_dir_all(&dir)?;
        }
    }
    Ok(())
}

// ---------------- 状态 ----------------

#[derive(Debug, Clone, Serialize)]
pub struct Status {
    pub agent: &'static str,
    /// 这台机器上找到了这个助手（命令或配置目录）
    pub available: bool,
    /// `none` | `ok` | `stale`
    pub mcp: &'static str,
    pub skill: Option<Skill>,
}

/// `url`、`token` 是现在应该写在配置里的值，据此判断装好的配置是不是旧的。
pub async fn status(shell: &str, url: &str, token: &str) -> Vec<Status> {
    let mut found = crate::tools::detect(shell).await;
    if found.claude.is_none() {
        found.claude = claude_fallback();
    }
    let tok = token.to_string();
    let url = url.to_string();
    tokio::task::spawn_blocking(move || {
        Agent::ALL
            .into_iter()
            .map(|a| {
                let cli = match a {
                    Agent::Claude => found.claude.is_some(),
                    Agent::Codex => found.codex.is_some(),
                };
                let configured = match a {
                    Agent::Claude => claude_mcp(&url, &tok),
                    Agent::Codex => codex_mcp(&url, &tok),
                };
                Status { agent: a.id(), available: cli || a.dir().is_dir(), mcp: configured, skill: installed_skill(a) }
            })
            .collect()
    })
    .await
    .unwrap_or_default()
}

fn judge(entry_url: Option<&str>, auth: Option<&str>, url: &str, token: &str) -> &'static str {
    match entry_url {
        None => "none",
        Some(u) if u == url && auth == Some(format!("Bearer {token}").as_str()) => "ok",
        Some(_) => "stale",
    }
}

fn claude_mcp(url: &str, token: &str) -> &'static str {
    let Ok(raw) = std::fs::read_to_string(claude_json()) else { return "none" };
    let Ok(v) = serde_json::from_str::<Value>(&raw) else { return "none" };
    let e = &v["mcpServers"][SERVER_NAME];
    if e.is_null() {
        return "none";
    }
    judge(Some(e["url"].as_str().unwrap_or("")), e["headers"]["Authorization"].as_str(), url, token)
}

fn codex_mcp(url: &str, token: &str) -> &'static str {
    let Ok(raw) = std::fs::read_to_string(codex_toml()) else { return "none" };
    let Ok(doc) = raw.parse::<toml_edit::DocumentMut>() else { return "none" };
    let Some(e) = doc.get("mcp_servers").and_then(|t| t.get(SERVER_NAME)) else { return "none" };
    let auth = e.get("http_headers").and_then(|h| h.get("Authorization")).and_then(|v| v.as_str());
    judge(Some(e.get("url").and_then(|v| v.as_str()).unwrap_or("")), auth, url, token)
}

// ---------------- 安装 / 卸载 ----------------

fn failed(agent: Agent, detail: impl std::fmt::Display) -> LocalError {
    tracing::warn!(agent = agent.id(), %detail, "安装 AI 助手配置失败");
    LocalError::new("AGENT_INSTALL_FAILED").with("agent", agent.id()).with("detail", detail.to_string())
}

/// 写入（或更新）MCP 配置，并装上选中的技能。
pub async fn install(agent: Agent, shell: &str, url: &str, token: &str, skill: Option<Skill>) -> Result<(), LocalError> {
    match agent {
        Agent::Claude => {
            let cli = claude_cli(shell).await.ok_or_else(|| LocalError::new("AGENT_NOT_FOUND").with("agent", agent.id()))?;
            // 先删掉旧的（不存在时会报错，忽略），再加
            let _ = run_cli(shell, &cli, &["mcp", "remove", "--scope", "user", SERVER_NAME]).await;
            let header = format!("Authorization: Bearer {token}");
            run_cli(shell, &cli, &["mcp", "add", "--transport", "http", "--scope", "user", SERVER_NAME, url, "--header", &header])
                .await
                .map_err(|e| failed(agent, e))?;
        }
        Agent::Codex => {
            let (url, token) = (url.to_string(), token.to_string());
            tokio::task::spawn_blocking(move || codex_write(Some((&url, &token))))
                .await
                .map_err(|e| failed(agent, e))?
                .map_err(|e| failed(agent, e))?;
        }
    }
    tokio::task::spawn_blocking(move || set_skill(agent, skill)).await.map_err(|e| failed(agent, e))?.map_err(|e| failed(agent, e))
}

/// 换了令牌：只更新已经装过的配置（技能不动）。
pub async fn refresh(agent: Agent, shell: &str, url: &str, token: &str) -> Result<(), LocalError> {
    let skill = tokio::task::spawn_blocking(move || installed_skill(agent)).await.ok().flatten();
    install(agent, shell, url, token, skill).await
}

/// 删掉 MCP 配置和技能。
pub async fn uninstall(agent: Agent, shell: &str) -> Result<(), LocalError> {
    match agent {
        Agent::Claude => {
            if let Some(cli) = claude_cli(shell).await {
                let _ = run_cli(shell, &cli, &["mcp", "remove", "--scope", "user", SERVER_NAME]).await;
            }
        }
        Agent::Codex => {
            tokio::task::spawn_blocking(|| codex_write(None)).await.map_err(|e| failed(agent, e))?.map_err(|e| failed(agent, e))?;
        }
    }
    tokio::task::spawn_blocking(move || set_skill(agent, None)).await.map_err(|e| failed(agent, e))?.map_err(|e| failed(agent, e))
}

/// 运行助手自己的命令行。Unix 上经过登录 shell，PATH 与用户打开终端时一致（`claude` 通常要 node）；
/// 参数作为位置参数传给 `exec "$0" "$@"`，不经过 shell 解析。
async fn run_cli(shell: &str, cli: &str, args: &[&str]) -> Result<(), String> {
    let mut cmd = if cfg!(windows) {
        // where.exe 可能先列出 npm 给 Git Bash 用的无扩展名脚本，Windows 运行不了，换成旁边的 .cmd
        let p = Path::new(cli);
        let cli = match p.with_extension("cmd") {
            c if p.extension().is_none() && c.is_file() => c,
            _ => p.to_path_buf(),
        };
        let mut c = Command::new(cli);
        c.args(args);
        #[cfg(windows)]
        c.creation_flags(0x0800_0000);
        c
    } else {
        let mut c = Command::new(shell);
        c.args(["-lc", "exec \"$0\" \"$@\"", cli]).args(args);
        c
    };
    cmd.stdin(std::process::Stdio::null()).kill_on_drop(true);
    let out = tokio::time::timeout(Duration::from_secs(60), cmd.output()).await.map_err(|_| "timed out".to_string())?.map_err(|e| e.to_string())?;
    if out.status.success() {
        return Ok(());
    }
    let mut msg = String::from_utf8_lossy(&out.stderr).trim().to_string();
    if msg.is_empty() {
        msg = String::from_utf8_lossy(&out.stdout).trim().to_string();
    }
    Err(msg.chars().take(300).collect())
}

/// 改 Codex 的 config.toml：`Some((url, token))` 写入，`None` 删除。
fn codex_write(entry: Option<(&str, &str)>) -> anyhow::Result<()> {
    let path = codex_toml();
    let raw = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e.into()),
    };
    let Some(out) = codex_edit(&raw, entry)? else { return Ok(()) };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    write_atomic(&path, out.as_bytes())?;
    Ok(())
}

/// 返回新的文件内容；没有要改的返回 `None`。
fn codex_edit(raw: &str, entry: Option<(&str, &str)>) -> anyhow::Result<Option<String>> {
    use toml_edit::{value, DocumentMut, InlineTable, Item, Table};
    let mut doc: DocumentMut = raw.parse().map_err(|e| anyhow::anyhow!("config.toml: {e}"))?;
    match entry {
        Some((url, token)) => {
            let servers = doc.entry("mcp_servers").or_insert_with(|| {
                let mut t = Table::new();
                t.set_implicit(true);
                Item::Table(t)
            });
            let servers = servers.as_table_mut().ok_or_else(|| anyhow::anyhow!("config.toml: mcp_servers is not a table"))?;
            let mut t = Table::new();
            t["url"] = value(url);
            let mut headers = InlineTable::new();
            headers.insert("Authorization", format!("Bearer {token}").into());
            t["http_headers"] = value(headers);
            servers.insert(SERVER_NAME, Item::Table(t));
        }
        None => {
            let Some(servers) = doc.get_mut("mcp_servers").and_then(|i| i.as_table_mut()) else { return Ok(None) };
            if servers.remove(SERVER_NAME).is_none() {
                return Ok(None);
            }
        }
    }
    Ok(Some(doc.to_string()))
}

/// 先写临时文件再改名，写到一半退出也不会留下半个配置文件。
fn write_atomic(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension(format!("looklook-{}.tmp", std::process::id()));
    std::fs::write(&tmp, data)?;
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_config_roundtrip() {
        let raw = "# mine\nmodel = \"o3\"\n\n[mcp_servers.other]\ncommand = \"x\"\n";
        let out = codex_edit(raw, Some(("http://127.0.0.1:1234/mcp", "llmcp_a"))).unwrap().unwrap();
        assert!(out.starts_with("# mine\nmodel = \"o3\""));
        assert!(out.contains("[mcp_servers.other]"));
        assert!(out.contains("[mcp_servers.looklook]\nurl = \"http://127.0.0.1:1234/mcp\"\nhttp_headers = { Authorization = \"Bearer llmcp_a\" }"));
        // 再写一次是替换不是追加
        let again = codex_edit(&out, Some(("http://127.0.0.1:1244/mcp", "llmcp_b"))).unwrap().unwrap();
        assert_eq!(again.matches("[mcp_servers.looklook]").count(), 1);
        assert!(again.contains("Bearer llmcp_b"));
        let removed = codex_edit(&again, None).unwrap().unwrap();
        assert!(!removed.contains("looklook") && removed.contains("[mcp_servers.other]"));
        assert!(codex_edit(&removed, None).unwrap().is_none());
        // 空文件不会多出一个空的 [mcp_servers]
        let fresh = codex_edit("", Some(("u", "t"))).unwrap().unwrap();
        assert!(!fresh.contains("[mcp_servers]\n"));
    }

    #[test]
    fn judging() {
        assert_eq!(judge(None, None, "u", "t"), "none");
        assert_eq!(judge(Some("u"), Some("Bearer t"), "u", "t"), "ok");
        assert_eq!(judge(Some("u"), Some("Bearer old"), "u", "t"), "stale");
        assert_eq!(judge(Some("http://127.0.0.1:1244/mcp"), Some("Bearer t"), "u", "t"), "stale");
    }

    #[test]
    fn skills_have_frontmatter() {
        for s in Skill::ALL {
            let c = s.content();
            assert!(c.starts_with(&format!("---\nname: {}\ndescription: ", s.name())), "{}", s.name());
        }
    }
}
