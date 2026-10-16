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
    pub(crate) fn set(&mut self, name: &str, path: String) {
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

// ---------------- 手动指定的程序位置 ----------------
//
// 找不到（没在 PATH 里，或者装在别的地方）时，用户可以在新建会话时指定程序文件或它所在的文件夹。
// 记在 kv `tool_paths`（工具 → 程序文件）；检测结果以它为准，新开的终端用完整路径启动它（Windows 上 psmux
// 按系统环境启动 shell，改 PATH 传不进去），tmux 后端还会把它所在的文件夹加到 PATH 最前面。

const PATHS_KEY: &str = "tool_paths";
/// 可以手动指定位置的工具（AI 编程助手）。
pub const AI_TOOLS: &[&str] = &["codex", "claude", "opencode", "dsh"];

pub fn configured(store: &crate::store::Store) -> std::collections::BTreeMap<String, String> {
    store.get(PATHS_KEY).ok().flatten().unwrap_or_default()
}

fn candidates(tool: &str) -> Vec<String> {
    if cfg!(windows) {
        vec![format!("{tool}.exe"), format!("{tool}.cmd"), format!("{tool}.bat"), tool.to_string()]
    } else {
        vec![tool.to_string()]
    }
}

/// 用户填的位置 → 程序文件：可以直接是程序文件，也可以是它所在的文件夹（或文件夹下的 `bin`、`node_modules/.bin`）。
pub fn resolve_program(tool: &str, input: &str) -> Option<std::path::PathBuf> {
    let p = std::path::PathBuf::from(input.trim().trim_matches('"'));
    if !p.is_absolute() {
        return None;
    }
    if p.is_file() {
        return Some(p);
    }
    if !p.is_dir() {
        return None;
    }
    for dir in [p.clone(), p.join("bin"), p.join("node_modules").join(".bin")] {
        for c in candidates(tool) {
            let f = dir.join(&c);
            if f.is_file() {
                return Some(f);
            }
        }
    }
    None
}

/// 保存手动指定的位置。错误是（字段，原因）。
pub fn set_path(store: &crate::store::Store, tool: &str, input: &str) -> Result<std::path::PathBuf, (&'static str, &'static str)> {
    if !AI_TOOLS.contains(&tool) {
        return Err(("tool", "invalid"));
    }
    let program = resolve_program(tool, input).ok_or(("path", "not_found"))?;
    let mut all = configured(store);
    all.insert(tool.to_string(), program.display().to_string());
    store.set(PATHS_KEY, &all).map_err(|_| ("path", "save_failed"))?;
    Ok(program)
}

pub fn clear_path(store: &crate::store::Store, tool: &str) {
    let mut all = configured(store);
    if all.remove(tool).is_some() {
        let _ = store.set(PATHS_KEY, &all);
    }
}

/// 手动指定、而且文件还在的那些：(工具, 程序文件)。
fn existing(store: &crate::store::Store) -> Vec<(String, std::path::PathBuf)> {
    configured(store).into_iter().map(|(k, v)| (k, std::path::PathBuf::from(v))).filter(|(_, p)| p.is_file()).collect()
}

/// 某个工具手动指定、而且文件还在的程序位置。
pub fn configured_program(store: &crate::store::Store, tool: &str) -> Option<String> {
    existing(store).into_iter().find(|(k, _)| k == tool).map(|(_, p)| p.display().to_string())
}

/// 版本号目录名（`2.1.295`）→ 可比较的数字；不是版本号的排在最后。
fn version_key(name: &str) -> Vec<u64> {
    name.split('.').map(|p| p.parse::<u64>().unwrap_or(0)).collect()
}

/// 不在 PATH 里、但装在常见位置的程序：
/// - Claude Code 独立安装：`~/.local/bin/claude`、`~/.claude/local/claude`；
/// - Claude 桌面版自带的：`<配置目录>/Claude/claude-code/<版本>/<…>/claude(.exe)`（取最新版本）。
pub fn known_location(tool: &str) -> Option<std::path::PathBuf> {
    if tool == "dsh" {
        return dsh_bundled();
    }
    if tool != "claude" {
        return None;
    }
    let exe = if cfg!(windows) { "claude.exe" } else { "claude" };
    let home = dirs::home_dir()?;
    let claude_home = std::env::var_os("CLAUDE_CONFIG_DIR").filter(|v| !v.is_empty()).map(std::path::PathBuf::from).unwrap_or_else(|| home.join(".claude"));
    if let Some(p) = [home.join(".local").join("bin").join(exe), claude_home.join("local").join(exe)].into_iter().find(|p| p.is_file()) {
        return Some(p);
    }
    let bundled = dirs::config_dir()?.join("Claude").join("claude-code");
    let mut versions: Vec<std::path::PathBuf> = std::fs::read_dir(&bundled).ok()?.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
    versions.sort_by_key(|p| std::cmp::Reverse(version_key(&p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default())));
    for v in versions {
        if v.join(exe).is_file() {
            return Some(v.join(exe));
        }
        let Ok(rd) = std::fs::read_dir(&v) else { continue };
        if let Some(p) = rd.flatten().map(|e| e.path().join(exe)).find(|p| p.is_file()) {
            return Some(p);
        }
    }
    None
}

/// DeepSeek Harness 桌面版自带的命令行：`<安装目录>/resources/runtime/cli/bin/dsh(.cmd)`。
/// 安装目录：Windows 上看“卸载程序”登记里的 InstallLocation，再试默认的安装位置；macOS 看 /Applications。
fn dsh_bundled() -> Option<std::path::PathBuf> {
    let cli = |dir: &std::path::Path| {
        let bin = dir.join("resources").join("runtime").join("cli").join("bin");
        [bin.join(if cfg!(windows) { "dsh.cmd" } else { "dsh" })].into_iter().find(|p| p.is_file())
    };
    let mut dirs_: Vec<std::path::PathBuf> = Vec::new();
    #[cfg(windows)]
    dirs_.extend(windows_install_locations("DeepSeek Harness"));
    if let Some(l) = dirs::data_local_dir() {
        dirs_.push(l.join("Programs").join("DeepSeek Harness"));
    }
    if cfg!(target_os = "macos") {
        dirs_.push(std::path::PathBuf::from("/Applications/DeepSeek Harness.app/Contents"));
    }
    dirs_.iter().find_map(|d| cli(d))
}

/// Windows“卸载程序”登记里显示名以 `name` 开头的程序的安装目录（HKCU、HKLM、32 位）。结果缓存一分钟。
#[cfg(windows)]
fn windows_install_locations(name: &str) -> Vec<std::path::PathBuf> {
    use std::sync::Mutex;
    use std::time::Instant;
    static CACHE: Mutex<Option<(Instant, String, Vec<std::path::PathBuf>)>> = Mutex::new(None);
    if let Some((at, n, v)) = CACHE.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
        if n == name && at.elapsed() < Duration::from_secs(60) {
            return v.clone();
        }
    }
    let mut out = Vec::new();
    for key in [
        r"HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall",
        r"HKLM\Software\Microsoft\Windows\CurrentVersion\Uninstall",
        r"HKLM\Software\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall",
    ] {
        let mut cmd = std::process::Command::new("reg.exe");
        cmd.args(["query", key, "/s"]).stdin(Stdio::null()).stderr(Stdio::null());
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000);
        }
        let Ok(o) = cmd.output() else { continue };
        let text = String::from_utf8_lossy(&o.stdout);
        // 每个子键一段：先是 HKEY_… 这一行，然后是“名称  类型  值”
        let (mut display, mut location): (Option<String>, Option<String>) = (None, None);
        let mut flush = |d: &mut Option<String>, l: &mut Option<String>| {
            if let (Some(d), Some(l)) = (d.take(), l.take()) {
                if d.starts_with(name) && !l.trim().is_empty() {
                    out.push(std::path::PathBuf::from(l.trim().trim_matches('"')));
                }
            }
            *d = None;
            *l = None;
        };
        for line in text.lines() {
            if line.starts_with("HKEY_") {
                flush(&mut display, &mut location);
                continue;
            }
            let t = line.trim_start();
            let value = |prefix: &str| t.strip_prefix(prefix).and_then(|r| r.trim_start().strip_prefix("REG_SZ")).map(|v| v.trim().to_string());
            if let Some(v) = value("DisplayName") {
                display = Some(v);
            } else if let Some(v) = value("InstallLocation") {
                location = Some(v);
            }
        }
        flush(&mut display, &mut location);
    }
    *CACHE.lock().unwrap_or_else(|e| e.into_inner()) = Some((Instant::now(), name.to_string(), out.clone()));
    out
}

/// 这个 dsh 能不能开终端界面（`dsh tui`）。DSH 桌面版自带的命令行只有 web / headless 两个 profile，
/// 命令行版（npm 的 @deepseek-ai/dsh）才有 tui。跑一次 `dsh tui --help` 看看，按程序路径缓存。
pub async fn dsh_has_tui(program: &str) -> bool {
    use std::sync::Mutex;
    static CACHE: Mutex<Option<HashMap<String, bool>>> = Mutex::new(None);
    if let Some(v) = CACHE.lock().unwrap_or_else(|e| e.into_inner()).as_ref().and_then(|m| m.get(program).copied()) {
        return v;
    }
    let mut cmd = Command::new(program);
    cmd.args(["tui", "--help"]).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000);
    let ok = match tokio::time::timeout(Duration::from_secs(30), cmd.output()).await {
        Ok(Ok(o)) => {
            let text = format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
            o.status.success() && !text.contains("does not exist")
        }
        // 超时、启动不了：不记，下次再试
        _ => return false,
    };
    CACHE.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(program.to_string(), ok);
    ok
}

/// 在 PATH 里能不能直接找到这个程序。
fn on_path(tool: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else { return false };
    std::env::split_paths(&path).any(|d| candidates(tool).iter().any(|c| d.join(c).is_file()))
}

/// 新开终端时用什么启动这个工具：手动指定的位置；没指定、PATH 里又找不到时用常见位置里找到的。
/// 返回 None 就照常用工具名。
pub fn launch_program(store: &crate::store::Store, tool: &str) -> Option<String> {
    configured_program(store, tool).or_else(|| if on_path(tool) { None } else { known_location(tool).map(|p| p.display().to_string()) })
}

/// 新开终端时加到 PATH 最前面的文件夹。
pub fn extra_dirs(store: &crate::store::Store) -> Vec<std::path::PathBuf> {
    let mut out: Vec<std::path::PathBuf> = Vec::new();
    for (_, p) in existing(store) {
        if let Some(d) = p.parent().map(std::path::Path::to_path_buf) {
            if !out.contains(&d) {
                out.push(d);
            }
        }
    }
    out
}

/// 检测，加上手动指定的位置（手动指定的为准）。
pub async fn detect_for(shell: &str, store: &crate::store::Store) -> Detected {
    let mut d = detect(shell).await;
    for (tool, p) in existing(store) {
        d.set(&tool, p.display().to_string());
    }
    if d.claude.is_none() {
        d.claude = known_location("claude").map(|p| p.display().to_string());
    }
    if d.dsh.is_none() {
        d.dsh = known_location("dsh").map(|p| p.display().to_string());
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
