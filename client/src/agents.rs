//! 一键把看看的 MCP 接口和配套技能装进 Codex / Claude Code / OpenCode / DSH（DeepSeek Harness）。
//!
//! - **Claude Code**：用它自己的命令 `claude mcp add --scope user`（`~/.claude.json` 由它自己维护，
//!   别的会话随时在改，不直接写）；状态从 `~/.claude.json` 里读。设置了 `CLAUDE_CONFIG_DIR` 时跟着它走。
//! - **Codex**：直接改 `$CODEX_HOME/config.toml`（默认 `~/.codex`）的 `[mcp_servers.looklook]`，
//!   用 toml_edit 保留用户原来的格式与注释。
//! - **OpenCode**：改全局配置 `opencode.json` 的 `mcp.looklook`（`$OPENCODE_CONFIG_DIR`，
//!   否则 `$XDG_CONFIG_HOME/opencode`，默认 `~/.config/opencode`，Windows 上也是这个位置）。保留其他键的顺序；
//!   文件里有注释（JSONC）解析不了时报错，不覆盖。
//! - **DSH**：MCP 服务是 home 层补丁 `$DSH_HOME/cordis.patch.yml`（默认 `~/.dsh`）里的一行
//!   `@deepseek-ai/dsh-mcp-client`，所有 profile（网页、桌面、命令行）都会读到。
//!   我们只管自己写的那一段（用注释标记首尾），文件其余部分原样保留。
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
    OpenCode,
    Dsh,
}

impl Agent {
    pub const ALL: [Agent; 4] = [Agent::Claude, Agent::Codex, Agent::OpenCode, Agent::Dsh];

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "claude" => Some(Agent::Claude),
            "codex" => Some(Agent::Codex),
            "opencode" => Some(Agent::OpenCode),
            "dsh" => Some(Agent::Dsh),
            _ => None,
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Agent::Claude => "claude",
            Agent::Codex => "codex",
            Agent::OpenCode => "opencode",
            Agent::Dsh => "dsh",
        }
    }

    /// 配置目录：`~/.claude` / `~/.codex` / `~/.config/opencode` / `~/.dsh`
    fn dir(self) -> PathBuf {
        let env = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
        match self {
            Agent::Claude => env("CLAUDE_CONFIG_DIR").unwrap_or_else(|| home().join(".claude")),
            Agent::Codex => env("CODEX_HOME").unwrap_or_else(|| home().join(".codex")),
            Agent::OpenCode => env("OPENCODE_CONFIG_DIR")
                .unwrap_or_else(|| env("XDG_CONFIG_HOME").unwrap_or_else(|| home().join(".config")).join("opencode")),
            Agent::Dsh => env("DSH_HOME").unwrap_or_else(|| home().join(".dsh")),
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

fn opencode_json() -> PathBuf {
    Agent::OpenCode.dir().join("opencode.json")
}

fn dsh_patch() -> PathBuf {
    Agent::Dsh.dir().join("cordis.patch.yml")
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
                    Agent::OpenCode => found.opencode.is_some(),
                    Agent::Dsh => found.dsh.is_some(),
                };
                let available = cli || a.dir().is_dir();
                let configured = match a {
                    Agent::Claude => claude_mcp(&url, &tok),
                    Agent::Codex => codex_mcp(&url, &tok),
                    Agent::OpenCode => opencode_mcp(&url, &tok),
                    Agent::Dsh => dsh_mcp(&url, &tok),
                };
                Status { agent: a.id(), available, mcp: configured, skill: installed_skill(a) }
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

fn opencode_mcp(url: &str, token: &str) -> &'static str {
    let Ok(raw) = std::fs::read_to_string(opencode_json()) else { return "none" };
    let Ok(v) = serde_json::from_str::<Value>(&raw) else { return "none" };
    let e = &v["mcp"][SERVER_NAME];
    if e.is_null() {
        return "none";
    }
    judge(Some(e["url"].as_str().unwrap_or("")), e["headers"]["Authorization"].as_str(), url, token)
}

fn dsh_mcp(url: &str, token: &str) -> &'static str {
    let Ok(raw) = std::fs::read_to_string(dsh_patch()) else { return "none" };
    let Some(block) = dsh_block(&raw) else { return "none" };
    // 我们自己写的段落，每个值都是一行 `key: "json 字符串"`
    let field = |key: &str| {
        block.lines().find_map(|l| l.trim().strip_prefix(key)?.strip_prefix(": ").and_then(|v| serde_json::from_str::<String>(v).ok()))
    };
    judge(Some(field("url").as_deref().unwrap_or("")), field("Authorization").as_deref(), url, token)
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
        Agent::Codex | Agent::OpenCode | Agent::Dsh => {
            let (url, token) = (url.to_string(), token.to_string());
            tokio::task::spawn_blocking(move || write_config(agent, Some((&url, &token))))
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
        Agent::Codex | Agent::OpenCode | Agent::Dsh => {
            tokio::task::spawn_blocking(move || write_config(agent, None)).await.map_err(|e| failed(agent, e))?.map_err(|e| failed(agent, e))?;
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

/// 改配置文件内容：拿到原文件和要写的 `(url, token)`（`None` 删除），返回新内容；没有要改的返回 `None`。
type Edit = fn(&str, Option<(&str, &str)>) -> anyhow::Result<Option<String>>;

/// 改直接写文件的那几个助手的配置：`Some((url, token))` 写入，`None` 删除。
fn write_config(agent: Agent, entry: Option<(&str, &str)>) -> anyhow::Result<()> {
    let (path, edit): (PathBuf, Edit) = match agent {
        Agent::Codex => (codex_toml(), codex_edit),
        Agent::OpenCode => (opencode_json(), opencode_edit),
        Agent::Dsh => (dsh_patch(), dsh_edit),
        Agent::Claude => unreachable!("Claude Code 用它自己的命令改配置"),
    };
    let raw = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e.into()),
    };
    let Some(out) = edit(&raw, entry)? else { return Ok(()) };
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

/// 改 OpenCode 的 opencode.json；返回新的文件内容，没有要改的返回 `None`。
fn opencode_edit(raw: &str, entry: Option<(&str, &str)>) -> anyhow::Result<Option<String>> {
    let mut doc: Value = if raw.trim().is_empty() {
        serde_json::json!({ "$schema": "https://opencode.ai/config.json" })
    } else {
        serde_json::from_str(raw).map_err(|e| anyhow::anyhow!("opencode.json: {e}"))?
    };
    let root = doc.as_object_mut().ok_or_else(|| anyhow::anyhow!("opencode.json: not an object"))?;
    match entry {
        Some((url, token)) => {
            let mcp = root.entry("mcp").or_insert_with(|| Value::Object(Default::default()));
            let mcp = mcp.as_object_mut().ok_or_else(|| anyhow::anyhow!("opencode.json: mcp is not an object"))?;
            mcp.insert(
                SERVER_NAME.into(),
                serde_json::json!({ "type": "remote", "url": url, "enabled": true, "headers": { "Authorization": format!("Bearer {token}") } }),
            );
        }
        None => {
            let Some(mcp) = root.get_mut("mcp").and_then(|m| m.as_object_mut()) else { return Ok(None) };
            if mcp.shift_remove(SERVER_NAME).is_none() {
                return Ok(None);
            }
        }
    }
    Ok(Some(serde_json::to_string_pretty(&doc)? + "\n"))
}

const DSH_BEGIN: &str = "# >>> looklook (managed by Looklook; changes inside are overwritten) >>>";
const DSH_END: &str = "# <<< looklook <<<";

/// 找到我们写的那一段（含首尾标记行）。
fn dsh_block_range(raw: &str) -> Option<std::ops::Range<usize>> {
    let start = raw.find(DSH_BEGIN)?;
    let end = start + raw[start..].find(DSH_END)? + DSH_END.len();
    let end = if raw[end..].starts_with("\r\n") { end + 2 } else if raw[end..].starts_with('\n') { end + 1 } else { end };
    Some(start..end)
}

fn dsh_block(raw: &str) -> Option<&str> {
    dsh_block_range(raw).map(|r| &raw[r])
}

/// 改 DSH 的 profile 补丁文件（YAML 列表）：只增删我们自己那一段；返回新的文件内容，没有要改的返回 `None`。
fn dsh_edit(raw: &str, entry: Option<(&str, &str)>) -> anyhow::Result<Option<String>> {
    let mut out = match dsh_block_range(raw) {
        Some(r) => format!("{}{}", &raw[..r.start], &raw[r.end..]),
        None if entry.is_none() => return Ok(None),
        None => raw.to_string(),
    };
    if let Some((url, token)) = entry {
        // 空列表 `[]` 不能再接 `- ` 行
        if out.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).eq(["[]"]) {
            out = out.lines().filter(|l| l.trim() != "[]").map(|l| format!("{l}\n")).collect();
        }
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        // JSON 字符串同时是合法的 YAML 双引号字符串
        let q = |s: &str| serde_json::to_string(s).expect("string");
        out.push_str(&format!(
            "{DSH_BEGIN}\n- insert:\n    - id: mcp-{SERVER_NAME}\n      name: '@deepseek-ai/dsh-mcp-client'\n      config:\n        serverName: {SERVER_NAME}\n        transport: streamable-http\n        url: {}\n        headers:\n          Authorization: {}\n{DSH_END}\n",
            q(url),
            q(&format!("Bearer {token}")),
        ));
    }
    Ok(Some(out))
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
    fn opencode_config_roundtrip() {
        let raw = "{\n  \"model\": \"x\",\n  \"$schema\": \"s\",\n  \"mcp\": { \"other\": { \"type\": \"local\" } }\n}";
        let out = opencode_edit(raw, Some(("http://127.0.0.1:1234/mcp", "llmcp_a"))).unwrap().unwrap();
        // 保留原来的键顺序
        assert!(out.find("\"model\"").unwrap() < out.find("\"$schema\"").unwrap());
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["mcp"]["looklook"]["type"], "remote");
        assert_eq!(v["mcp"]["looklook"]["headers"]["Authorization"], "Bearer llmcp_a");
        assert_eq!(v["mcp"]["other"]["type"], "local");
        let removed = opencode_edit(&out, None).unwrap().unwrap();
        assert!(!removed.contains("looklook") && removed.contains("\"other\""));
        assert!(opencode_edit(&removed, None).unwrap().is_none());
        let fresh: Value = serde_json::from_str(&opencode_edit("", Some(("u", "t"))).unwrap().unwrap()).unwrap();
        assert_eq!(fresh["mcp"]["looklook"]["url"], "u");
        assert!(opencode_edit("{ // jsonc\n}", Some(("u", "t"))).is_err(), "带注释的不覆盖");
    }

    #[test]
    fn dsh_config_roundtrip() {
        let raw = "# mine\n- id: tools\n  disabled: true";
        let out = dsh_edit(raw, Some(("http://127.0.0.1:1234/mcp", "llmcp_a"))).unwrap().unwrap();
        assert!(out.starts_with("# mine\n- id: tools\n  disabled: true\n# >>> looklook"));
        assert!(out.contains("        url: \"http://127.0.0.1:1234/mcp\"\n        headers:\n          Authorization: \"Bearer llmcp_a\"\n"));
        let block = dsh_block(&out).unwrap();
        assert!(block.ends_with("# <<< looklook <<<\n"));
        // 再写一次是替换不是追加
        let again = dsh_edit(&out, Some(("http://127.0.0.1:1244/mcp", "llmcp_b"))).unwrap().unwrap();
        assert_eq!(again.matches(DSH_BEGIN).count(), 1);
        assert!(again.contains("Bearer llmcp_b") && !again.contains("llmcp_a"));
        let removed = dsh_edit(&again, None).unwrap().unwrap();
        assert_eq!(removed, "# mine\n- id: tools\n  disabled: true\n");
        assert!(dsh_edit(&removed, None).unwrap().is_none());
        let empty = dsh_edit("# x\n[]\n", Some(("u", "t"))).unwrap().unwrap();
        assert!(empty.starts_with("# x\n# >>> looklook"));
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
