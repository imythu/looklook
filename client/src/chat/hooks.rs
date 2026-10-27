//! 注入给 Claude Code 的 hooks（docs/CLAUDE_UI.md §4.1、§4.2）。
//!
//! - `run/claude-hooks.json`：启动对话模式的 Claude Code 时用 `--settings` 传进去，不改用户自己的 settings。
//!   每个事件挂一条 command hook：`<looklook> hook <run/hook.json>`；另外带上 `skipDangerousModePermissionPrompt`，
//!   以全部权限启动时不弹 Bypass Permissions 警告（2.1.310 判断要不要弹时也看 `--settings` 里的值）。
//! - `run/claude-terminal.json`：不带对话界面的 Claude Code 终端用的 `--settings`，只有 `skipDangerousModePermissionPrompt`。
//! - `run/hook.json`：客户端每次启动写入（0600）：管理台的本机地址和这次运行的随机令牌。
//! - `looklook hook`：Claude Code 调用的子命令。读 stdin 的 hook 输入，转给客户端的 `POST /hook`，
//!   把客户端的答复原样写到 stdout。**出什么错都放行**：连不上、超时、出错一律退出 0、不输出，Claude Code 照常运行。
//!   只用标准库（不起 tokio、不初始化日志和数据库），启动要快。

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Value};

use crate::paths::Paths;

pub const SUBCOMMAND: &str = "hook";
/// 客户端收到 hook 用的路径（只接受本机、要令牌，见 gateway/mod.rs）
pub const PATH: &str = "/hook";
pub const TOKEN_HEADER: &str = "x-looklook-hook";
pub const INSTANCE_HEADER: &str = "x-looklook-instance";
/// 对话模式的 Claude Code 带着这个环境变量运行，hook 据此知道是哪个终端
pub const INSTANCE_ENV: &str = "LOOKLOOK_INSTANCE";

/// 需要的事件。全部同步：状态要按事件顺序变化，异步 hook 可能乱序（例如 Stop 之后才到的 PostToolUse）。
/// 客户端对除 PermissionRequest 以外的事件都立即答复，hook 程序本身启动很快，开销很小。
const EVENTS: [&str; 15] = [
    "SessionStart",
    "SessionEnd",
    "UserPromptSubmit",
    "Stop",
    "StopFailure",
    "Notification",
    "PreToolUse",
    "PostToolUse",
    "PostToolUseFailure",
    "PermissionRequest",
    "PermissionDenied",
    "PreCompact",
    "PostCompact",
    "SubagentStart",
    "SubagentStop",
];
/// 权限审批最多等多久（秒）。客户端自己最多挂起 590 秒，到时间放行给 TUI（§4.4）。
/// 命令（Bash）的 PreToolUse 也这么久：命中命令规则时要等人确认，客户端 590 秒内一定答复（没人确认就拒绝），
/// 不能让 hook 超时（PreToolUse 超时 Claude Code 照常执行）。没命中时立即答复。
const PERMISSION_TIMEOUT: u64 = 600;

pub fn settings_path(paths: &Paths) -> PathBuf {
    paths.home.join("run").join("claude-hooks.json")
}

pub fn terminal_settings_path(paths: &Paths) -> PathBuf {
    paths.home.join("run").join("claude-terminal.json")
}

pub fn endpoint_path(paths: &Paths) -> PathBuf {
    paths.home.join("run").join("hook.json")
}

/// 命令行里的路径，双引号括起来。Windows 上 Claude Code 用 Git Bash 运行 hook，所以：
/// - 去掉 `canonicalize()` 留下的 `\\?\` 前缀（bash 会把开头的 `\\` 变成 `\`，结果是 `\?\G:\…`，找不到程序）；
/// - 反斜杠换成斜杠（`G:/Program Files/…`，Git Bash 和 Windows 都认）；
/// - 双引号里还会展开的 `"`、`$`、`` ` ``、`\` 加反斜杠。
fn quote(p: &Path) -> String {
    format!("\"{}\"", shell_path(&p.display().to_string(), cfg!(windows)))
}

fn shell_path(p: &str, windows: bool) -> String {
    let p = match p.strip_prefix(r"\\?\UNC\") {
        Some(rest) => format!(r"\\{rest}"),
        None => p.strip_prefix(r"\\?\").unwrap_or(p).to_string(),
    };
    let p = if windows { p.replace('\\', "/") } else { p };
    let mut out = String::with_capacity(p.len());
    for c in p.chars() {
        if matches!(c, '"' | '$' | '`' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

pub fn settings_json(exe: &Path, endpoint: &Path) -> Value {
    let command = format!("{} {SUBCOMMAND} {}", quote(exe), quote(endpoint));
    let hooks: serde_json::Map<String, Value> = EVENTS
        .iter()
        .map(|ev| {
            let timeout = if matches!(*ev, "PermissionRequest" | "PreToolUse") { PERMISSION_TIMEOUT } else { 10 };
            (ev.to_string(), json!([{ "matcher": "*", "hooks": [{ "type": "command", "command": command, "timeout": timeout }] }]))
        })
        .collect();
    json!({ "hooks": hooks, "skipDangerousModePermissionPrompt": true })
}

/// hook 用的程序：看看自己。开发、测试时可以用 `LOOKLOOK_HOOK_EXE` 指定（`cargo test` 里当前程序是测试程序）。
/// Linux 上用稳定入口 `bin/looklook`：运行中的 Claude Code 一直用启动时读到的路径，版本目录被清理后也要能调用。
pub fn hook_exe(paths: &Paths) -> PathBuf {
    std::env::var_os("LOOKLOOK_HOOK_EXE")
        .map(PathBuf::from)
        .or_else(|| crate::versions::stable_exe(&paths.home))
        .unwrap_or_else(|| std::env::current_exe().unwrap_or_else(|_| PathBuf::from("looklook")))
}

/// 写好 `claude-hooks.json`（内容没变就不动）。启动对话模式的 Claude Code 之前必须调用：文件不在时 claude 会直接报错退出。
pub fn ensure_settings(paths: &Paths) -> std::io::Result<PathBuf> {
    let exe = hook_exe(paths);
    let path = settings_path(paths);
    let text = serde_json::to_string_pretty(&settings_json(&exe, &endpoint_path(paths)))?;
    write_changed(path, text)
}

/// 写好 `claude-terminal.json`（内容没变就不动）：不带对话界面的 Claude Code 以全部权限启动时也不弹 Bypass 警告，不碰用户设置。
pub fn ensure_terminal_settings(paths: &Paths) -> std::io::Result<PathBuf> {
    let text = serde_json::to_string_pretty(&json!({ "skipDangerousModePermissionPrompt": true }))?;
    write_changed(terminal_settings_path(paths), text)
}

fn write_changed(path: PathBuf, text: String) -> std::io::Result<PathBuf> {
    if std::fs::read_to_string(&path).ok().as_deref() != Some(text.as_str()) {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&path, text)?;
    }
    Ok(path)
}

/// 客户端启动时写下本机地址和令牌，只有当前用户能读。
pub fn write_endpoint(paths: &Paths, addr: SocketAddr, token: &str) -> std::io::Result<()> {
    let path = endpoint_path(paths);
    let text = json!({ "addr": addr.to_string(), "token": token }).to_string();
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&path)?;
        f.write_all(text.as_bytes())
    }
    #[cfg(not(unix))]
    {
        std::fs::write(&path, text)
    }
}

/// `looklook hook <hook.json>`：返回进程退出码，总是 0。
pub fn main(endpoint_file: &str) -> i32 {
    let mut input = String::new();
    if std::io::stdin().read_to_string(&mut input).is_err() {
        return 0;
    }
    let Ok(instance) = std::env::var(INSTANCE_ENV) else { return 0 };
    if let Some(out) = forward(endpoint_file, &instance, &input) {
        if !out.is_empty() {
            let mut stdout = std::io::stdout();
            let _ = stdout.write_all(out.as_bytes());
            let _ = stdout.flush();
        }
    }
    0
}

fn forward(endpoint_file: &str, instance: &str, input: &str) -> Option<String> {
    let cfg: Value = serde_json::from_str(&std::fs::read_to_string(endpoint_file).ok()?).ok()?;
    let addr: SocketAddr = cfg["addr"].as_str()?.parse().ok()?;
    let token = cfg["token"].as_str()?;
    if !instance.chars().all(|c| c.is_ascii_alphanumeric()) || token.contains(['\r', '\n']) {
        return None;
    }
    let event: Value = serde_json::from_str(input).ok()?;
    let waits = event["hook_event_name"] == "PermissionRequest" || event["hook_event_name"] == "PreToolUse" && event["tool_name"] == "Bash";
    let mut s = TcpStream::connect_timeout(&addr, Duration::from_millis(300)).ok()?;
    let _ = s.set_nodelay(true);
    s.set_write_timeout(Some(Duration::from_secs(2))).ok()?;
    s.set_read_timeout(Some(Duration::from_secs(if waits { PERMISSION_TIMEOUT } else { 3 }))).ok()?;
    let head = format!(
        "POST {PATH} HTTP/1.1\r\nHost: {addr}\r\n{TOKEN_HEADER}: {token}\r\n{INSTANCE_HEADER}: {instance}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        input.len()
    );
    s.write_all(head.as_bytes()).ok()?;
    s.write_all(input.as_bytes()).ok()?;
    let mut resp = Vec::new();
    s.read_to_end(&mut resp).ok()?;
    parse_response(&resp)
}

/// 只认 200 加 `Content-Length` 的响应体（客户端总是这样答复）。
fn parse_response(resp: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(resp).ok()?;
    let (head, body) = text.split_once("\r\n\r\n")?;
    let status = head.lines().next()?.split_whitespace().nth(1)?;
    if status != "200" {
        return None;
    }
    let len = head.lines().find_map(|l| l.split_once(':').filter(|(k, _)| k.eq_ignore_ascii_case("content-length")).and_then(|(_, v)| v.trim().parse::<usize>().ok()));
    Some(match len {
        Some(n) => body.get(..n).unwrap_or(body).to_string(),
        None => body.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_have_every_event_with_quoted_command() {
        let v = settings_json(Path::new("/opt/look look/looklook"), Path::new("/home/a/.local/share/looklook/run/hook.json"));
        for ev in EVENTS {
            let h = &v["hooks"][ev][0]["hooks"][0];
            assert_eq!(h["command"], "\"/opt/look look/looklook\" hook \"/home/a/.local/share/looklook/run/hook.json\"");
            assert!(h.get("async").is_none(), "{ev} 必须同步");
        }
        assert_eq!(v["hooks"]["PermissionRequest"][0]["hooks"][0]["timeout"], 600);
        assert_eq!(v["hooks"]["PreToolUse"][0]["hooks"][0]["timeout"], 600);
        assert_eq!(v["hooks"]["Stop"][0]["hooks"][0]["timeout"], 10);
        assert_eq!(v["skipDangerousModePermissionPrompt"], true);
    }

    #[test]
    fn windows_paths_for_git_bash() {
        assert_eq!(shell_path(r"\\?\G:\Program Files\Looklook\looklook.exe", true), "G:/Program Files/Looklook/looklook.exe");
        assert_eq!(shell_path(r"C:\Users\a\AppData\Roaming\looklook\run\hook.json", true), "C:/Users/a/AppData/Roaming/looklook/run/hook.json");
        assert_eq!(shell_path(r"\\?\UNC\nas\share\looklook.exe", true), "//nas/share/looklook.exe");
        assert_eq!(shell_path("/opt/a$b/look`x\"y", false), "/opt/a\\$b/look\\`x\\\"y");
    }

    #[test]
    fn responses() {
        assert_eq!(parse_response(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\n{}").as_deref(), Some("{}"));
        assert_eq!(parse_response(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n").as_deref(), Some(""));
        assert_eq!(parse_response(b"HTTP/1.1 401 Unauthorized\r\ncontent-length: 2\r\n\r\nno"), None);
        assert_eq!(parse_response(b"garbage"), None);
    }

    /// 客户端没在运行：立刻放弃，不输出
    #[test]
    fn unreachable_client_gives_up_fast() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("hook.json");
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        std::fs::write(&f, json!({ "addr": format!("127.0.0.1:{port}"), "token": "t" }).to_string()).unwrap();
        let t = std::time::Instant::now();
        assert_eq!(forward(f.to_str().unwrap(), "abcd2345", r#"{"hook_event_name":"Stop"}"#), None);
        assert!(t.elapsed() < Duration::from_millis(500));
        assert_eq!(forward("/nonexistent/hook.json", "abcd2345", "{}"), None);
    }
}
