//! AI 命令行工具：检测 Codex / Claude Code / OpenCode / DSH（DeepSeek Harness）/ Node.js 是否已安装，并生成安装命令。
//! 安装在一个普通终端里进行，用户能看到进度与报错；npm 镜像可选淘宝（npmmirror）、腾讯或官方。

use std::time::Duration;

use serde::Serialize;
use tokio::process::Command;

pub const TOOLS: &[&str] = &["codex", "claude", "opencode", "dsh", "node", "npm"];

#[derive(Debug, Clone, Serialize, Default)]
pub struct Detected {
    pub codex: Option<String>,
    pub claude: Option<String>,
    pub opencode: Option<String>,
    pub dsh: Option<String>,
    pub node: Option<String>,
    pub npm: Option<String>,
}

impl Detected {
    fn set(&mut self, name: &str, path: String) {
        let slot = match name {
            "codex" => &mut self.codex,
            "claude" => &mut self.claude,
            "opencode" => &mut self.opencode,
            "dsh" => &mut self.dsh,
            "node" => &mut self.node,
            "npm" => &mut self.npm,
            _ => return,
        };
        *slot = Some(path);
    }
}

/// 在用户的登录 shell 里查找命令（PATH 与用户平时打开终端时一致）。
pub async fn detect(shell: &str) -> Detected {
    let mut d = Detected::default();
    let out = if cfg!(windows) {
        let mut cmd = Command::new("where.exe");
        cmd.args(TOOLS);
        #[cfg(windows)]
        cmd.creation_flags(0x0800_0000);
        tokio::time::timeout(Duration::from_secs(8), cmd.output()).await
    } else {
        let script = TOOLS.iter().map(|t| format!("printf '%s=%s\\n' {t} \"$(command -v {t} 2>/dev/null)\";")).collect::<String>();
        let mut cmd = Command::new(shell);
        cmd.args(["-lic", &script]).stdin(std::process::Stdio::null());
        tokio::time::timeout(Duration::from_secs(8), cmd.output()).await
    };
    let Ok(Ok(out)) = out else { return d };
    let text = String::from_utf8_lossy(&out.stdout);
    for line in text.lines() {
        let line = line.trim();
        if cfg!(windows) {
            // where.exe 每行一个完整路径
            let lower = line.to_ascii_lowercase();
            for t in TOOLS {
                let stem = std::path::Path::new(&lower).file_stem().and_then(|s| s.to_str()).unwrap_or("");
                if stem == *t {
                    d.set(t, line.to_string());
                }
            }
        } else if let Some((name, path)) = line.split_once('=') {
            if TOOLS.contains(&name) && path.starts_with('/') {
                d.set(name, path.to_string());
            }
        }
    }
    d
}

pub fn registry(mirror: &str) -> &'static str {
    match mirror {
        "tencent" => "https://mirrors.cloud.tencent.com/npm/",
        "official" => "https://registry.npmjs.org/",
        _ => "https://registry.npmmirror.com/",
    }
}

pub fn package(tool: &str) -> Option<&'static str> {
    match tool {
        "codex" => Some("@openai/codex"),
        "claude" => Some("@anthropic-ai/claude-code"),
        "opencode" => Some("opencode-ai"),
        "dsh" => Some("@deepseek-ai/dsh"),
        _ => None,
    }
}

pub fn install_command(tool: &str, mirror: &str) -> Option<String> {
    Some(format!("npm install -g {} --registry={}", package(tool)?, registry(mirror)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands() {
        assert_eq!(install_command("codex", "npmmirror").unwrap(), "npm install -g @openai/codex --registry=https://registry.npmmirror.com/");
        assert!(install_command("claude", "tencent").unwrap().contains("mirrors.cloud.tencent.com"));
        assert_eq!(install_command("opencode", "official").unwrap(), "npm install -g opencode-ai --registry=https://registry.npmjs.org/");
        assert_eq!(install_command("dsh", "npmmirror").unwrap(), "npm install -g @deepseek-ai/dsh --registry=https://registry.npmmirror.com/");
        assert!(install_command("vim", "official").is_none());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn detects_shell_builtins_path() {
        let d = detect("/bin/sh").await;
        // 测试环境不一定装了这些工具；只要求解析不出错，且找到的都是绝对路径。
        for p in [d.codex, d.claude, d.opencode, d.dsh, d.node, d.npm].into_iter().flatten() {
            assert!(p.starts_with('/'));
        }
    }
}
