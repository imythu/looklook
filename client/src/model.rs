//! 第三方模型服务商（中转站）：平台下发推荐列表（服务端详细设计 §4.11.1），用户给 Claude Code / Codex
//! 各选一个服务商（或自填接口地址）、填 API Key；之后新建或重启的对应终端走这个接口启动。
//!
//! - 选择存在 kv `model_provider.{agent}`；API Key 只写在本机文件 `<数据目录>/model/{agent}.key`
//!   （仅当前用户可读），不上传平台，也不出现在任何进程的命令行参数里：启动命令让 shell 自己读这个文件。
//! - **Claude Code**：`ANTHROPIC_BASE_URL` + `ANTHROPIC_AUTH_TOKEN`，并清空 `ANTHROPIC_API_KEY`
//!   （否则官方 Key 会一起发给第三方）。
//! - **Codex**：用 `-c` 临时定义一个 `looklook` 服务商（`env_key` 指向我们设置的环境变量），
//!   不改用户的 `~/.codex/config.toml`，也不会把 ChatGPT 登录凭据发给第三方。
//! - 用户必须先同意免责声明（`model_provider.disclaimer`）。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::LocalError;
use crate::shells::Family;
use crate::store::Store;

const DISCLAIMER_KEY: &str = "model_provider.disclaimer";
/// Codex 读 Key 的环境变量（`model_providers.looklook.env_key`）
const CODEX_KEY_ENV: &str = "LOOKLOOK_MODEL_API_KEY";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelAgent {
    Claude,
    Codex,
}

impl ModelAgent {
    pub const ALL: [ModelAgent; 2] = [ModelAgent::Claude, ModelAgent::Codex];

    /// 终端启动类型（`instances.launch`）
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "claude" => Some(Self::Claude),
            "codex" => Some(Self::Codex),
            _ => None,
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }

    /// 平台上的客户端名（`model_provider::CLIENTS`）
    pub fn platform_client(self) -> &'static str {
        match self {
            Self::Claude => "claude_code",
            Self::Codex => "codex",
        }
    }

    fn kv_key(self) -> String {
        format!("model_provider.{}", self.id())
    }
}

/// 用户的选择。`provider_id` 为 None 表示自填的接口地址。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Choice {
    pub provider_id: Option<String>,
    pub name: String,
    pub base_url: String,
    pub selected_at: String,
}

pub fn key_path(home: &Path, agent: ModelAgent) -> PathBuf {
    home.join("model").join(format!("{}.key", agent.id()))
}

pub fn choice(store: &Store, agent: ModelAgent) -> Option<Choice> {
    store.get::<Choice>(&agent.kv_key()).ok().flatten()
}

pub fn key_set(home: &Path, agent: ModelAgent) -> bool {
    std::fs::metadata(key_path(home, agent)).is_ok_and(|m| m.len() > 0)
}

pub fn disclaimer_accepted(store: &Store) -> Option<String> {
    store.get::<String>(DISCLAIMER_KEY).ok().flatten()
}

pub fn accept_disclaimer(store: &Store) -> Result<(), LocalError> {
    if disclaimer_accepted(store).is_none() {
        store.set(DISCLAIMER_KEY, &crate::util::now_rfc3339())?;
    }
    Ok(())
}

/// 保存选择；`api_key` 为 None 时沿用已保存的 Key（没有则报错）。
pub fn save(store: &Store, home: &Path, agent: ModelAgent, choice: &Choice, api_key: Option<&str>) -> Result<(), LocalError> {
    check_base_url(&choice.base_url)?;
    match api_key.map(str::trim) {
        Some(k) if !k.is_empty() => {
            if k.len() > 4096 || k.chars().any(|c| c.is_whitespace() || c.is_control()) {
                return Err(LocalError::invalid("api_key", "invalid"));
            }
            let path = key_path(home, agent);
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            crate::util::write_private(&path, k.as_bytes())?;
        }
        _ if key_set(home, agent) => {}
        _ => return Err(LocalError::new("MODEL_KEY_REQUIRED")),
    }
    store.set(&agent.kv_key(), choice)?;
    Ok(())
}

/// 恢复官方：删掉选择和本机保存的 Key。
pub fn clear(store: &Store, home: &Path, agent: ModelAgent) -> Result<(), LocalError> {
    store.remove(&agent.kv_key())?;
    match std::fs::remove_file(key_path(home, agent)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
        _ => Ok(()),
    }
}

/// 接口地址只接受 `https://主机[:端口][/路径]`，路径里只能有字母、数字和 `-._~/`：
/// 地址会原样拼进启动命令（Codex 的 `-c` 参数），不能有任何 shell 会解释的字符。
pub fn check_base_url(url: &str) -> Result<(), LocalError> {
    let bad = || LocalError::new("MODEL_URL_UNSUPPORTED");
    let rest = url.strip_prefix("https://").ok_or_else(bad)?;
    if url.len() > 512 {
        return Err(bad());
    }
    let (host_port, path) = rest.split_once('/').map_or((rest, ""), |(h, p)| (h, p));
    let (host, port) = host_port.split_once(':').map_or((host_port, None), |(h, p)| (h, Some(p)));
    let host_ok = !host.is_empty()
        && host.contains('.')
        && host.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.')
        && !host.starts_with(['-', '.'])
        && !host.ends_with(['-', '.']);
    let port_ok = port.is_none_or(|p| !p.is_empty() && p.len() <= 5 && p.chars().all(|c| c.is_ascii_digit()));
    let path_ok = path.chars().all(|c| c.is_ascii_alphanumeric() || "-._~/".contains(c)) && !path.contains("..");
    if host_ok && port_ok && path_ok {
        Ok(())
    } else {
        Err(bad())
    }
}

/// 启动时要设置的环境变量。
#[derive(Debug, Clone, PartialEq)]
enum Val {
    Lit(String),
    /// 由 shell 读文件内容（API Key 不出现在命令行里）
    File(PathBuf),
    Unset,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LaunchEnv {
    vars: Vec<(&'static str, Val)>,
    /// 追加在启动命令后面的参数（不含机密）
    args: String,
}

/// 该终端要走第三方接口时的启动环境；没有选择、或 Key 文件不在时为 None（按官方方式启动）。
pub fn launch_env(store: &Store, home: &Path, launch: &str) -> Option<LaunchEnv> {
    let agent = ModelAgent::parse(launch)?;
    let c = choice(store, agent)?;
    if !key_set(home, agent) || check_base_url(&c.base_url).is_err() {
        return None;
    }
    Some(env_for(agent, &c.base_url, key_path(home, agent)))
}

fn env_for(agent: ModelAgent, base_url: &str, key: PathBuf) -> LaunchEnv {
    match agent {
        ModelAgent::Claude => LaunchEnv {
            vars: vec![
                ("ANTHROPIC_BASE_URL", Val::Lit(base_url.into())),
                ("ANTHROPIC_AUTH_TOKEN", Val::File(key)),
                ("ANTHROPIC_API_KEY", Val::Unset),
            ],
            args: String::new(),
        },
        ModelAgent::Codex => LaunchEnv {
            vars: vec![(CODEX_KEY_ENV, Val::File(key))],
            args: [
                "model_provider=looklook".to_string(),
                "model_providers.looklook.name=looklook".into(),
                format!("model_providers.looklook.base_url={base_url}"),
                format!("model_providers.looklook.env_key={CODEX_KEY_ENV}"),
                "model_providers.looklook.wire_api=responses".into(),
            ]
            .iter()
            .map(|a| format!(" -c {a}"))
            .collect(),
        },
    }
}

impl LaunchEnv {
    /// 按终端所用 shell 的写法，把环境变量加在启动命令前面。认不出的 shell（命令是“打”进去的）按 POSIX 写。
    pub fn wrap(&self, cmd: &str, family: Family) -> String {
        let cmd = format!("{cmd}{}", self.args);
        match family {
            Family::Pwsh => {
                let set: Vec<String> = self
                    .vars
                    .iter()
                    .map(|(k, v)| match v {
                        Val::Lit(s) => format!("$env:{k}={};", sq(s, "''")),
                        Val::File(p) => format!("$env:{k}=(Get-Content -Raw -ErrorAction Stop -LiteralPath {}).Trim();", sq(&p.display().to_string(), "''")),
                        Val::Unset => format!("$env:{k}=$null;"),
                    })
                    .collect();
                format!("{} {cmd}", set.join(" "))
            }
            Family::Cmd => {
                let set: Vec<String> = self
                    .vars
                    .iter()
                    .map(|(k, v)| match v {
                        Val::Lit(s) => format!("set \"{k}={s}\" &&"),
                        Val::File(p) => format!("set /p {k}=<\"{}\" &&", p.display()),
                        Val::Unset => format!("set \"{k}=\" &&"),
                    })
                    .collect();
                format!("{} {cmd}", set.join(" "))
            }
            Family::Nu => {
                let set: Vec<String> = self
                    .vars
                    .iter()
                    .map(|(k, v)| match v {
                        Val::Lit(s) => format!("$env.{k} = r#'{s}'#;"),
                        Val::File(p) => format!("$env.{k} = (open --raw r#'{}'# | str trim);", p.display()),
                        Val::Unset => format!("hide-env -i {k};"),
                    })
                    .collect();
                format!("{} {cmd}", set.join(" "))
            }
            Family::Posix | Family::Other => {
                let set: Vec<String> = self
                    .vars
                    .iter()
                    .map(|(k, v)| match v {
                        Val::Lit(s) => format!("{k}={}", sq(s, "'\\''")),
                        Val::File(p) => format!("{k}=\"$(cat {})\"", sq(&p.display().to_string(), "'\\''")),
                        Val::Unset => format!("{k}="),
                    })
                    .collect();
                format!("{} {cmd}", set.join(" "))
            }
        }
    }
}

/// 单引号字符串；`esc` 是该 shell 里单引号本身的写法。
fn sq(s: &str, esc: &str) -> String {
    format!("'{}'", s.replace('\'', esc))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_url_rules() {
        for ok in ["https://api.example.com", "https://api.example.com/", "https://a.b.co:8443/v1", "https://x.y/anthropic/v1_beta-2"] {
            assert!(check_base_url(ok).is_ok(), "{ok}");
        }
        for bad in [
            "http://api.example.com",
            "https://",
            "https://localhost",
            "https://a.com/v1?x=1",
            "https://a.com/v1&rm -rf",
            "https://a.com/$(id)",
            "https://a.com/v1;x",
            "https://a.com:/v1",
            "https://a.com:99999x",
            "https://a.com/../etc",
            "https://-a.com",
            "https://a .com",
        ] {
            assert!(check_base_url(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn claude_posix() {
        let e = env_for(ModelAgent::Claude, "https://r.example.com", PathBuf::from("/home/u/it's/claude.key"));
        assert_eq!(
            e.wrap("claude --dangerously-skip-permissions", Family::Posix),
            "ANTHROPIC_BASE_URL='https://r.example.com' ANTHROPIC_AUTH_TOKEN=\"$(cat '/home/u/it'\\''s/claude.key')\" ANTHROPIC_API_KEY= claude --dangerously-skip-permissions"
        );
    }

    #[test]
    fn codex_shells() {
        let e = env_for(ModelAgent::Codex, "https://r.example.com/v1", PathBuf::from(r"C:\Users\A B\looklook\model\codex.key"));
        let args = " -c model_provider=looklook -c model_providers.looklook.name=looklook -c model_providers.looklook.base_url=https://r.example.com/v1 -c model_providers.looklook.env_key=LOOKLOOK_MODEL_API_KEY -c model_providers.looklook.wire_api=responses";
        assert_eq!(
            e.wrap("codex --yolo", Family::Pwsh),
            format!(r"$env:LOOKLOOK_MODEL_API_KEY=(Get-Content -Raw -ErrorAction Stop -LiteralPath 'C:\Users\A B\looklook\model\codex.key').Trim(); codex --yolo{args}")
        );
        assert_eq!(e.wrap("codex", Family::Cmd), format!(r#"set /p LOOKLOOK_MODEL_API_KEY=<"C:\Users\A B\looklook\model\codex.key" && codex{args}"#));
        assert!(e.wrap("codex", Family::Nu).starts_with("$env.LOOKLOOK_MODEL_API_KEY = (open --raw r#'C:\\Users\\A B"));
    }

    #[test]
    fn no_choice_means_official() {
        let dir = std::env::temp_dir().join(format!("ll-model-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::open(&dir.join("t.db")).unwrap();
        assert!(launch_env(&store, &dir, "claude").is_none());
        let c = Choice { provider_id: None, name: "x".into(), base_url: "https://r.example.com".into(), selected_at: String::new() };
        assert!(save(&store, &dir, ModelAgent::Claude, &c, None).is_err(), "第一次保存必须带 Key");
        save(&store, &dir, ModelAgent::Claude, &c, Some(" sk-test ")).unwrap();
        assert_eq!(std::fs::read_to_string(key_path(&dir, ModelAgent::Claude)).unwrap(), "sk-test");
        assert!(launch_env(&store, &dir, "claude").is_some());
        assert!(launch_env(&store, &dir, "codex").is_none());
        assert!(launch_env(&store, &dir, "shell").is_none());
        save(&store, &dir, ModelAgent::Claude, &c, None).unwrap();
        clear(&store, &dir, ModelAgent::Claude).unwrap();
        assert!(launch_env(&store, &dir, "claude").is_none());
        assert!(!key_set(&dir, ModelAgent::Claude));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
