//! 第三方模型接口的公用部分：接口地址、模型名的校验，免责声明，启动环境（[`LaunchEnv`]）。
//! 配置本身在 claude_profiles.rs（Claude Code）和 codex_profiles.rs（Codex）；常见服务商的快速配置在 model_presets.rs。
//!
//! - API Key 只写在本机文件 `<数据目录>/model/*.key`（仅当前用户可读），不上传平台，
//!   也不出现在任何进程的命令行参数里：启动命令让 shell 自己读这个文件。
//! - **Claude Code**：`ANTHROPIC_BASE_URL` + `ANTHROPIC_AUTH_TOKEN`（或 `ANTHROPIC_API_KEY`），另一个清空
//!   （否则官方 Key 会一起发给第三方）。
//! - **Codex**：用 `-c` 临时定义服务商，不改用户的 `~/.codex/config.toml`。
//! - 用户必须先同意免责声明（`model_provider.disclaimer`）。

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::LocalError;
use crate::shells::Family;
use crate::store::Store;

const DISCLAIMER_KEY: &str = "model_provider.disclaimer";

/// 以前“模型服务商”里存的选择（kv `model_provider.{claude,codex}`），只用来升级（claude_profiles / codex_profiles 的 `migrate`）。
/// `provider_id` 为 None 表示自填的接口地址。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Choice {
    pub provider_id: Option<String>,
    pub name: String,
    pub base_url: String,
    /// 指定的模型（Codex 的 `model`）；不填用 Codex 的默认模型，由接口自行对应
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub selected_at: String,
}

/// 模型名：去掉首尾空白，空的算没填。会原样拼进启动命令，只收模型名里常见的字符。
pub fn clean_model(v: Option<&str>) -> Result<Option<String>, LocalError> {
    match v.map(str::trim).filter(|s| !s.is_empty()) {
        None => Ok(None),
        Some(s) if s.len() <= 100 && s.chars().all(|c| c.is_ascii_alphanumeric() || "-._:/[]@".contains(c)) => Ok(Some(s.into())),
        Some(_) => Err(LocalError::invalid("model", "invalid")),
    }
}

/// Codex 的 `-c model=…` 不加引号拼进命令（codex_profiles.rs）：不能有 shell 的通配符 `[]`，也要有字母（否则 `-c` 会按 TOML 读成数字）
pub fn codex_model_ok(m: &str) -> bool {
    !m.contains(['[', ']']) && m.chars().any(|c| c.is_ascii_alphabetic())
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

/// 接口地址只接受 `https://主机[:端口][/路径]`，路径里只能有字母、数字和 `-._~/`：
/// 地址会原样拼进启动命令（Codex 的 `-c` 参数），不能有任何 shell 会解释的字符。
/// 本机的接口（Ollama 等，docs/PLAN_2026-10-27.md §4）可以用 `http://localhost` / `http://127.0.0.1`：Key 不出这台电脑。
pub fn check_base_url(url: &str) -> Result<(), LocalError> {
    let bad = || LocalError::new("MODEL_URL_UNSUPPORTED");
    let (rest, local) = match url.strip_prefix("https://") {
        Some(r) => (r, false),
        None => (url.strip_prefix("http://").ok_or_else(bad)?, true),
    };
    if url.len() > 512 {
        return Err(bad());
    }
    let (host_port, path) = rest.split_once('/').map_or((rest, ""), |(h, p)| (h, p));
    let (host, port) = host_port.split_once(':').map_or((host_port, None), |(h, p)| (h, Some(p)));
    if local && !matches!(host, "localhost" | "127.0.0.1") {
        return Err(bad());
    }
    let host_ok = !host.is_empty()
        && (host.contains('.') || local)
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
    vars: Vec<(String, Val)>,
    /// 追加在启动命令后面的参数（不含机密）
    args: String,
}

/// Claude Code 走第三方接口（官方格式）：`ANTHROPIC_BASE_URL` + `ANTHROPIC_AUTH_TOKEN`，并清空 `ANTHROPIC_API_KEY`
/// （否则官方 Key 会一起发给第三方）。
pub fn claude_api_env(base_url: &str, key: PathBuf) -> LaunchEnv {
    LaunchEnv::new().lit("ANTHROPIC_BASE_URL", base_url).file("ANTHROPIC_AUTH_TOKEN", key).unset("ANTHROPIC_API_KEY")
}

/// 要求 `x-api-key` 头的接口（Anthropic 自己的 API、部分服务商）：Key 放 `ANTHROPIC_API_KEY`，清空 `ANTHROPIC_AUTH_TOKEN`
/// （`ANTHROPIC_AUTH_TOKEN` 发的是 `Authorization: Bearer`，Anthropic 的 API Key 不认）。
pub fn claude_api_key_env(base_url: &str, key: PathBuf) -> LaunchEnv {
    LaunchEnv::new().lit("ANTHROPIC_BASE_URL", base_url).file("ANTHROPIC_API_KEY", key).unset("ANTHROPIC_AUTH_TOKEN")
}

impl LaunchEnv {
    pub fn new() -> Self {
        LaunchEnv { vars: vec![], args: String::new() }
    }

    pub fn lit(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.vars.push((key.into(), Val::Lit(value.into())));
        self
    }

    pub fn file(mut self, key: impl Into<String>, path: PathBuf) -> Self {
        self.vars.push((key.into(), Val::File(path)));
        self
    }

    pub fn unset(mut self, key: impl Into<String>) -> Self {
        self.vars.push((key.into(), Val::Unset));
        self
    }

    /// 只设一个环境变量，值由 shell 从文件里读（口令、Key 不出现在命令行里）
    pub fn file_var(key: &'static str, path: PathBuf) -> Self {
        LaunchEnv { vars: vec![(key.into(), Val::File(path))], args: String::new() }
    }

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

    /// 追加一个 Codex 的 `-c 键=值`（不含机密，原样拼进命令，[`Self::apply`] 也按 `-c` 拆开）
    pub fn config(mut self, kv: impl AsRef<str>) -> Self {
        self.args.push_str(" -c ");
        self.args.push_str(kv.as_ref());
        self
    }

    /// 合并另一份（变量、参数都接在后面）
    pub fn merge(mut self, other: LaunchEnv) -> Self {
        self.vars.extend(other.vars);
        self.args.push_str(&other.args);
        self
    }

    /// 直接起进程（Codex 的 app-server）时用：环境变量设在进程上，Key 文件由这里读；`-c` 参数逐个加上。
    pub fn apply(&self, cmd: &mut tokio::process::Command) {
        for (k, v) in &self.vars {
            match v {
                Val::Lit(s) => {
                    cmd.env(k, s);
                }
                Val::File(p) => {
                    if let Ok(s) = std::fs::read_to_string(p) {
                        cmd.env(k, s.trim());
                    }
                }
                Val::Unset => {
                    cmd.env_remove(k);
                }
            }
        }
        for a in self.args.split(" -c ").map(str::trim).filter(|a| !a.is_empty()) {
            cmd.arg("-c").arg(a);
        }
    }

    /// 区分配置是否变了（不含 Key 内容）
    pub fn signature(&self) -> String {
        format!("{:?}{}", self.vars, self.args)
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
        for ok in ["https://api.example.com", "https://api.example.com/", "https://a.b.co:8443/v1", "https://x.y/anthropic/v1_beta-2", "http://localhost:11434", "http://127.0.0.1:11434/v1"] {
            assert!(check_base_url(ok).is_ok(), "{ok}");
        }
        for bad in [
            "http://api.example.com",
            "http://localhost.example.com",
            "http://127.0.0.2:11434",
            "http://localhost:11434/$(id)",
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
        let e = claude_api_env("https://r.example.com", PathBuf::from("/home/u/it's/claude.key"));
        assert_eq!(
            e.wrap("claude --dangerously-skip-permissions", Family::Posix),
            "ANTHROPIC_BASE_URL='https://r.example.com' ANTHROPIC_AUTH_TOKEN=\"$(cat '/home/u/it'\\''s/claude.key')\" ANTHROPIC_API_KEY= claude --dangerously-skip-permissions"
        );
    }

    #[test]
    fn codex_shells() {
        let e = LaunchEnv::new().file("LOOKLOOK_MODEL_API_KEY", PathBuf::from(r"C:\Users\A B\looklook\model\codex.key")).config("model_provider=x").config("model=glm-5.3");
        let args = " -c model_provider=x -c model=glm-5.3";
        assert_eq!(
            e.wrap("codex --yolo", Family::Pwsh),
            format!(r"$env:LOOKLOOK_MODEL_API_KEY=(Get-Content -Raw -ErrorAction Stop -LiteralPath 'C:\Users\A B\looklook\model\codex.key').Trim(); codex --yolo{args}")
        );
        assert_eq!(e.wrap("codex", Family::Cmd), format!(r#"set /p LOOKLOOK_MODEL_API_KEY=<"C:\Users\A B\looklook\model\codex.key" && codex{args}"#));
        assert!(e.wrap("codex", Family::Nu).starts_with("$env.LOOKLOOK_MODEL_API_KEY = (open --raw r#'C:\\Users\\A B"));
        let mut cmd = tokio::process::Command::new("codex");
        e.apply(&mut cmd);
        let args: Vec<String> = cmd.as_std().get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        assert_eq!(args, ["-c", "model_provider=x", "-c", "model=glm-5.3"]);
    }

    #[test]
    fn codex_model_rules() {
        assert_eq!(clean_model(Some("  ")).unwrap(), None);
        assert_eq!(clean_model(Some(" kimi-k2:0905 ")).unwrap().as_deref(), Some("kimi-k2:0905"));
        assert!(clean_model(Some("a b")).is_err());
        assert!(clean_model(Some("$(id)")).is_err());
        for (m, ok) in [("glm-4.6", true), ("qwen/qwen3-coder", true), ("x[1m]", false), ("1.5", false)] {
            assert_eq!(codex_model_ok(m), ok, "{m}");
        }
    }
}
