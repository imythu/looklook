//! AI 命令行工具：检测 Codex / Claude Code / OpenCode / DSH（DeepSeek Harness）/ Node.js 是否已安装，并生成安装命令。
//! 安装是一个临时的后台任务（不进 tmux、不建终端）：输出留在内存里给界面看进度，客户端重启就没了；
//! npm 镜像可选淘宝（npmmirror）、腾讯或官方。

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use tokio::io::AsyncReadExt;
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

// ---------------- 安装任务 ----------------

/// 输出最多留这么多字节（只留最后的部分）。
const OUTPUT_CAP: usize = 64 * 1024;
/// 装得再慢也不该超过这么久。
const INSTALL_TIMEOUT: Duration = Duration::from_secs(20 * 60);

#[derive(Debug, Clone, Serialize)]
pub struct Job {
    pub tool: String,
    pub command: String,
    /// `running` | `ok` | `failed`
    pub state: &'static str,
    pub exit_code: Option<i32>,
    pub output: String,
    pub started_at: String,
}

/// 每个工具最多一个任务（重新安装替换上一个结束了的）。
static JOBS: Mutex<Option<HashMap<String, Arc<Mutex<Job>>>>> = Mutex::new(None);

fn jobs<R>(f: impl FnOnce(&mut HashMap<String, Arc<Mutex<Job>>>) -> R) -> R {
    let mut g = JOBS.lock().unwrap_or_else(|e| e.into_inner());
    f(g.get_or_insert_with(HashMap::new))
}

pub fn job(tool: &str) -> Option<Job> {
    jobs(|m| m.get(tool).map(|j| j.lock().unwrap_or_else(|e| e.into_inner()).clone()))
}

fn push_output(job: &Mutex<Job>, chunk: &str) {
    let mut j = job.lock().unwrap_or_else(|e| e.into_inner());
    j.output.push_str(chunk);
    if j.output.len() > OUTPUT_CAP {
        let mut cut = j.output.len() - OUTPUT_CAP;
        while !j.output.is_char_boundary(cut) {
            cut += 1;
        }
        j.output.drain(..cut);
    }
}

/// 开始安装（同一个工具已经在装时返回那个任务）。stdout、stderr 合在一起；`--loglevel=http` 让 npm 在没有终端时也逐个打印下载，看得到进度。
pub fn start_install(shell: &str, tool: &str, mirror: &str) -> Option<Job> {
    let command = install_command(tool, mirror)?;
    let job = jobs(|m| {
        if let Some(j) = m.get(tool) {
            if j.lock().unwrap_or_else(|e| e.into_inner()).state == "running" {
                return Err(j.clone());
            }
        }
        let j = Arc::new(Mutex::new(Job {
            tool: tool.to_string(),
            command: command.clone(),
            state: "running",
            exit_code: None,
            output: String::new(),
            started_at: crate::util::now_rfc3339(),
        }));
        m.insert(tool.to_string(), j.clone());
        Ok(j)
    });
    let job = match job {
        Ok(j) => j,
        Err(running) => return Some(running.lock().unwrap_or_else(|e| e.into_inner()).clone()),
    };
    let line = format!("{command} --loglevel=http --no-fund --no-audit");
    let mut cmd = if cfg!(windows) {
        let mut c = Command::new("cmd.exe");
        c.args(["/D", "/S", "/C", &format!("{line} 2>&1")]);
        #[cfg(windows)]
        c.creation_flags(0x0800_0000);
        c
    } else {
        let mut c = Command::new(shell);
        c.args(["-lc", &format!("exec {line} 2>&1")]);
        c
    };
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).env("NO_COLOR", "1").kill_on_drop(true);
    push_output(&job, &format!("$ {command}\n"));
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            push_output(&job, &format!("{e}\n"));
            job.lock().unwrap_or_else(|e| e.into_inner()).state = "failed";
            return job_view(&job);
        }
    };
    let task = job.clone();
    tokio::spawn(async move {
        let mut out = child.stdout.take().expect("piped");
        let run = async {
            let mut buf = vec![0u8; 8192];
            let mut pending = Vec::new();
            loop {
                match out.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        pending.extend_from_slice(&buf[..n]);
                        // 只交出完整的 UTF-8（Windows 上也可能是本地编码，那就按有损转换）
                        let valid = match std::str::from_utf8(&pending) {
                            Ok(_) => pending.len(),
                            Err(e) if e.error_len().is_none() => e.valid_up_to(),
                            Err(_) => pending.len(),
                        };
                        let text = String::from_utf8_lossy(&pending[..valid]).replace("\r\n", "\n");
                        pending.drain(..valid);
                        push_output(&task, &text);
                    }
                }
            }
            child.wait().await
        };
        let (state, code, note) = match tokio::time::timeout(INSTALL_TIMEOUT, run).await {
            Ok(Ok(st)) if st.success() => ("ok", st.code(), None),
            Ok(Ok(st)) => ("failed", st.code(), None),
            Ok(Err(e)) => ("failed", None, Some(e.to_string())),
            Err(_) => ("failed", None, Some("timed out".to_string())),
        };
        if let Some(n) = note {
            push_output(&task, &format!("\n{n}\n"));
        }
        let mut j = task.lock().unwrap_or_else(|e| e.into_inner());
        j.state = state;
        j.exit_code = code;
        tracing::info!(tool = %j.tool, state, code, "安装 AI 工具结束");
    });
    job_view(&job)
}

fn job_view(job: &Mutex<Job>) -> Option<Job> {
    Some(job.lock().unwrap_or_else(|e| e.into_inner()).clone())
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

    #[test]
    fn output_keeps_the_tail() {
        let j = Mutex::new(Job { tool: "x".into(), command: String::new(), state: "running", exit_code: None, output: String::new(), started_at: String::new() });
        push_output(&j, &"安".repeat(OUTPUT_CAP));
        push_output(&j, "end");
        let out = j.lock().unwrap().output.clone();
        assert!(out.len() <= OUTPUT_CAP && out.ends_with("end"));
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
