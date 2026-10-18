//! 对话模式的 Codex 怎么启动（CODEX_UI.md §4）。
//!
//! - hooks 写进看看自己的 profile：`$CODEX_HOME/looklook-chat.config.toml`，启动时 `-p looklook-chat` 选用它。
//!   只有带这个参数的 Codex 进程会用到这些 hooks，用户自己的 `config.toml` 不动；命令行里也没有带引号的 TOML，
//!   不会碰到 PowerShell / cmd / psmux 吃引号的问题。
//! - hook 要“信任”过才会运行。profile 里同时写上这几条 hook 的 `hooks.state.'<key>'.trusted_hash`，
//!   只信任看看自己的 hook（别处来的照样要用户确认）。哈希是 Codex 内部算的：在看看自己的空 `CODEX_HOME` 里起一个
//!   `codex app-server`，用 `-c` 带上同样的 hooks，调 `hooks/list` 取 `currentHash`（只跟命令和 timeout 有关）。
//!   结果按 “Codex 程序 + hook 命令” 缓存在 `run/codex-trust.json`。算不出来时 profile 里不写信任，
//!   Codex 会在终端里弹出确认（对话界面显示“到终端里操作”），不会自动放行。
//! - Windows 上直接调用 `codex.exe`：npm 的 `codex.cmd` 会把参数里的 `<` `>` 当成重定向，`codex.ps1` 会被执行策略拦住。

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::paths::Paths;

/// `-p` 用的 profile 名（文件是 `$CODEX_HOME/<名字>.config.toml`）
pub const PROFILE: &str = "looklook-chat";

/// 要的事件（Codex 0.159 的二进制里有这些名字；没有 Notification、StopFailure，有 Interrupt）
pub const EVENTS: [&str; 12] = [
    "SessionStart",
    "SessionEnd",
    "UserPromptSubmit",
    "Stop",
    "Interrupt",
    "PreToolUse",
    "PostToolUse",
    "PermissionRequest",
    "PreCompact",
    "PostCompact",
    "SubagentStart",
    "SubagentStop",
];

fn timeout(ev: &str) -> u64 {
    if ev == "PermissionRequest" {
        600
    } else {
        10
    }
}

/// `PreToolUse` → `pre_tool_use`（信任状态的 key 里用这种写法）
fn snake(ev: &str) -> String {
    let mut out = String::new();
    for (i, c) in ev.chars().enumerate() {
        if c.is_ascii_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// TOML 字符串：能用字面量（单引号，反斜杠不用转义）就用，否则用基本字符串
fn toml_str(s: &str) -> String {
    if !s.contains('\'') && !s.contains('\n') {
        format!("'{s}'")
    } else {
        Value::String(s.to_string()).to_string()
    }
}

fn hook_entry(ev: &str, command: &str) -> String {
    format!("[{{ matcher = \"*\", hooks = [{{ type = \"command\", command = {}, timeout = {} }}] }}]", toml_str(command), timeout(ev))
}

pub fn profile_path() -> PathBuf {
    crate::agents::codex_home().join(format!("{PROFILE}.config.toml"))
}

/// profile 的内容。`hashes`：事件 → 信任哈希（算不出来时为空）
pub fn profile_text(path: &Path, command: &str, hashes: &[(String, String)]) -> String {
    let mut out = String::from("# 看看（looklook）对话界面用的 Codex 配置，只在看看用 `codex -p looklook-chat` 启动时生效。\n# 看看会自动重写这个文件，请不要手动修改。\n\n[hooks]\n");
    for ev in EVENTS {
        out.push_str(&format!("{ev} = {}\n", hook_entry(ev, command)));
    }
    for (ev, hash) in hashes {
        let key = format!("{}:{}:0:0", path.display(), snake(ev));
        out.push_str(&format!("\n[hooks.state.{}]\ntrusted_hash = {}\n", toml_str(&key), toml_str(hash)));
    }
    out
}

/// 只换掉 `[hooks]`，文件里别的内容原样保留：Codex 会把用户在这个 profile 下做的决定写回这个文件
/// （例如信任文件夹的 `[projects.'<目录>']`），整个重写会让它每次都重新问。原文件解析不了时整个换掉。
fn merge_profile(old: Option<&str>, fresh: &str) -> String {
    let Some(mut doc) = old.and_then(|o| o.parse::<toml_edit::DocumentMut>().ok()) else { return fresh.to_string() };
    let Ok(new) = fresh.parse::<toml_edit::DocumentMut>() else { return fresh.to_string() };
    doc["hooks"] = new["hooks"].clone();
    doc.to_string()
}

/// 写好 profile（内容没变就不动）。启动对话模式的 Codex 之前调用；会阻塞（第一次要起 app-server 算哈希，几秒钟）。
pub fn ensure_profile(paths: &Paths) -> std::io::Result<PathBuf> {
    let command = hook_command(&super::hooks::hook_exe(), &super::hooks::endpoint_path(paths), cfg!(windows));
    let path = profile_path();
    let hashes = trust_hashes(paths, &command).unwrap_or_else(|e| {
        tracing::warn!(error = %e, "算不出 Codex hook 的信任哈希，第一次运行时要在终端里确认");
        vec![]
    });
    let old = std::fs::read_to_string(&path).ok();
    let text = merge_profile(old.as_deref(), &profile_text(&path, &command, &hashes));
    if old.as_deref() != Some(text.as_str()) {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&path, text)?;
    }
    Ok(path)
}

/// Codex 运行 hook 的方式和 Claude Code 不同：Windows 上用 PowerShell 跑命令（实测，`$PSVersionTable` 会被展开），
/// 所以要写成 `& '<程序>' hook '<hook.json>'`（单引号里不展开 `$`、反引号）；其他平台和 Claude 一样是 sh 的写法。
fn hook_command(exe: &Path, endpoint: &Path, windows: bool) -> String {
    if !windows {
        return format!("{} {} {}", sh_quote(exe), super::hooks::SUBCOMMAND, sh_quote(endpoint));
    }
    let ps = |p: &Path| {
        let s = p.display().to_string();
        let s = s.strip_prefix(r"\\?\").unwrap_or(&s).to_string();
        format!("'{}'", s.replace('\'', "''"))
    };
    format!("& {} {} {}", ps(exe), super::hooks::SUBCOMMAND, ps(endpoint))
}

fn sh_quote(p: &Path) -> String {
    format!("'{}'", p.display().to_string().replace('\'', r"'\''"))
}

/// 启动 Codex 用的程序：Windows 上是 `codex.exe` 的完整路径（找不到时 None，照常用名字）；其他平台 None。
pub fn program() -> Option<PathBuf> {
    if !cfg!(windows) {
        return None;
    }
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let exe = dir.join("codex.exe");
        if exe.is_file() {
            return Some(exe);
        }
        if dir.join("codex.cmd").is_file() {
            if let Some(exe) = npm_vendor_exe(&dir) {
                return Some(exe);
            }
        }
    }
    None
}

/// npm 全局安装：`<目录>/node_modules/@openai/codex/node_modules/@openai/codex-win32-*/vendor/<三元组>/bin/codex.exe`
fn npm_vendor_exe(npm_dir: &Path) -> Option<PathBuf> {
    let base = npm_dir.join("node_modules").join("@openai").join("codex");
    let scopes = [base.join("node_modules").join("@openai"), npm_dir.join("node_modules").join("@openai")];
    for scope in scopes {
        let Ok(rd) = std::fs::read_dir(&scope) else { continue };
        for pkg in rd.flatten().map(|e| e.path()).filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("codex-win32-"))) {
            let Ok(vendor) = std::fs::read_dir(pkg.join("vendor")) else { continue };
            if let Some(exe) = vendor.flatten().map(|e| e.path().join("bin").join("codex.exe")).find(|p| p.is_file()) {
                return Some(exe);
            }
        }
    }
    None
}

/// 每个事件的信任哈希（有缓存）
fn trust_hashes(paths: &Paths, command: &str) -> std::io::Result<Vec<(String, String)>> {
    let exe = program().unwrap_or_else(|| PathBuf::from("codex"));
    let stamp = std::fs::metadata(&exe).map(|m| format!("{}-{:?}", m.len(), m.modified().ok())).unwrap_or_default();
    let cache_path = paths.home.join("run").join("codex-trust.json");
    let cache: Value = std::fs::read_to_string(&cache_path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    let wanted = json!({ "exe": exe.display().to_string(), "stamp": stamp, "command": command });
    if cache["key"] == wanted {
        if let Some(h) = cache["hashes"].as_object() {
            let v: Vec<(String, String)> = h.iter().filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string()))).collect();
            if v.len() == EVENTS.len() {
                return Ok(v);
            }
        }
    }
    let hashes = ask_app_server(paths, &exe, command)?;
    let map: serde_json::Map<String, Value> = hashes.iter().map(|(k, v)| (k.clone(), json!(v))).collect();
    let _ = std::fs::write(&cache_path, json!({ "key": wanted, "hashes": map }).to_string());
    Ok(hashes)
}

/// 起一个 app-server（看看自己的空 CODEX_HOME，不读用户配置、不用登录），带上同样的 hooks，问 `hooks/list`。
fn ask_app_server(paths: &Paths, exe: &Path, command: &str) -> std::io::Result<Vec<(String, String)>> {
    let home = paths.home.join("run").join("codex-trust-home");
    std::fs::create_dir_all(&home)?;
    let mut cmd = Command::new(exe);
    cmd.arg("app-server");
    for ev in EVENTS {
        cmd.arg("-c").arg(format!("hooks.{ev}={}", hook_entry(ev, command)));
    }
    cmd.env("CODEX_HOME", &home).current_dir(&home).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let mut child = cmd.spawn()?;
    let mut stdin = child.stdin.take().expect("stdin");
    let stdout = child.stdout.take().expect("stdout");
    let (tx, rx) = std::sync::mpsc::channel::<Value>();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if let Ok(v) = serde_json::from_str::<Value>(&line) {
                if v["id"] == 2 {
                    let _ = tx.send(v);
                    return;
                }
            }
        }
    });
    let cwd = home.display().to_string();
    for msg in [
        json!({ "id": 1, "method": "initialize", "params": { "clientInfo": { "name": "looklook", "version": env!("CARGO_PKG_VERSION") } } }),
        json!({ "method": "initialized" }),
        json!({ "id": 2, "method": "hooks/list", "params": { "cwds": [cwd] } }),
    ] {
        writeln!(stdin, "{msg}")?;
    }
    stdin.flush()?;
    let deadline = Instant::now() + Duration::from_secs(20);
    let reply = rx.recv_timeout(deadline.saturating_duration_since(Instant::now()));
    let _ = child.kill();
    let _ = child.wait();
    let reply = reply.map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "codex app-server 没有回答 hooks/list"))?;
    parse_hooks_list(&reply, command).ok_or_else(|| std::io::Error::other(format!("hooks/list 的结果对不上：{}", reply.to_string().chars().take(300).collect::<String>())))
}

/// `hooks/list` 的结果 → 每个事件的哈希。只取命令行带进去的（`sessionFlags`）、命令和我们的一样的那几条，要每个事件都有。
fn parse_hooks_list(reply: &Value, command: &str) -> Option<Vec<(String, String)>> {
    let hooks = reply["result"]["data"].as_array()?.iter().flat_map(|d| d["hooks"].as_array().into_iter().flatten());
    let mut out = vec![];
    for h in hooks.filter(|h| h["source"] == "sessionFlags" && h["command"] == command) {
        let event = h["eventName"].as_str()?;
        let name = EVENTS.iter().find(|e| e.eq_ignore_ascii_case(event))?;
        let hash = h["currentHash"].as_str()?;
        if !out.iter().any(|(e, _)| e == name) {
            out.push((name.to_string(), hash.to_string()));
        }
    }
    (out.len() == EVENTS.len()).then_some(out)
}

/// 启动命令：`codex [--yolo]` → `LOOKLOOK_INSTANCE=<id> codex [resume] --no-daemon -p looklook-chat [--yolo] [<会话>]`。
/// 环境变量用 POSIX 写法，PowerShell / cmd 由 shells.rs 改成各自的写法。
pub fn with_chat(cmd: &str, id: &str, resume: Option<&str>) -> String {
    let rest = cmd.strip_prefix("codex").unwrap_or(cmd);
    let resume = resume.filter(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'));
    let (sub, tail) = match resume {
        Some(s) => ("resume ", format!(" {s}")),
        None => ("", String::new()),
    };
    format!("{}={id} codex {sub}--no-daemon -p {PROFILE}{rest}{tail}", super::hooks::INSTANCE_ENV)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hook_commands() {
        assert_eq!(
            hook_command(Path::new(r"\\?\C:\Program Files\it's\looklook.exe"), Path::new(r"C:\Users\a\run\hook.json"), true),
            r"& 'C:\Program Files\it''s\looklook.exe' hook 'C:\Users\a\run\hook.json'"
        );
        assert_eq!(hook_command(Path::new("/opt/look look/looklook"), Path::new("/home/a/run/hook.json"), false), "'/opt/look look/looklook' hook '/home/a/run/hook.json'");
    }

    #[test]
    fn command_lines() {
        assert_eq!(with_chat("codex", "abcd2345", None), "LOOKLOOK_INSTANCE=abcd2345 codex --no-daemon -p looklook-chat");
        assert_eq!(with_chat("codex --yolo", "x", Some("01a14d4b-27db-7853")), "LOOKLOOK_INSTANCE=x codex resume --no-daemon -p looklook-chat --yolo 01a14d4b-27db-7853");
        assert!(!with_chat("codex", "x", Some("x; rm -rf /")).contains("rm"));
    }

    #[test]
    fn profile_has_hooks_and_trust() {
        let path = PathBuf::from(r"C:\Users\a\.codex\looklook-chat.config.toml");
        let cmd = r#""C:/x/looklook.exe" hook "C:/x/hook.json""#;
        let text = profile_text(&path, cmd, &[("PreToolUse".into(), "sha256:ab".into())]);
        let v: toml_edit::DocumentMut = text.parse().unwrap();
        let entry = &v["hooks"]["PermissionRequest"][0]["hooks"][0];
        assert_eq!(entry["command"].as_str(), Some(cmd));
        assert_eq!(entry["timeout"].as_integer(), Some(600));
        assert_eq!(v["hooks"].as_table().unwrap().len(), EVENTS.len() + 1, "12 个事件加 state");
        let state = &v["hooks"]["state"][r"C:\Users\a\.codex\looklook-chat.config.toml:pre_tool_use:0:0"];
        assert_eq!(state["trusted_hash"].as_str(), Some("sha256:ab"));
        // 命令里有单引号：换成基本字符串
        let odd = profile_text(&path, "/it's/looklook hook /x", &[]);
        let v: toml_edit::DocumentMut = odd.parse().unwrap();
        assert_eq!(v["hooks"]["Stop"][0]["hooks"][0]["command"].as_str(), Some("/it's/looklook hook /x"));
    }

    #[test]
    fn rewriting_keeps_what_codex_saved() {
        let path = PathBuf::from(r"C:\Users\a\.codex\looklook-chat.config.toml");
        let first = profile_text(&path, "old-cmd", &[]);
        // Codex 在这个 profile 下信任了一个文件夹
        let saved = format!("{first}\n[projects.'c:\\users\\a\\work']\ntrust_level = \"trusted\"\n");
        let next = merge_profile(Some(&saved), &profile_text(&path, "new-cmd", &[("Stop".into(), "sha256:x".into())]));
        let v: toml_edit::DocumentMut = next.parse().unwrap();
        assert_eq!(v["projects"][r"c:\users\a\work"]["trust_level"].as_str(), Some("trusted"));
        assert_eq!(v["hooks"]["Stop"][0]["hooks"][0]["command"].as_str(), Some("new-cmd"));
        assert!(!next.contains("old-cmd"));
        assert_eq!(merge_profile(None, &first), first);
        assert_eq!(merge_profile(Some("not = [toml"), &first), first);
        // 没变时内容一样（不会每次都写文件）
        assert_eq!(merge_profile(Some(&next), &profile_text(&path, "new-cmd", &[("Stop".into(), "sha256:x".into())])), next);
    }

    #[test]
    fn hooks_list_reply() {
        let cmd = "\"C:/x/looklook.exe\" hook \"C:/x/hook.json\"";
        let hooks: Vec<Value> = EVENTS
            .iter()
            .map(|e| {
                let camel = format!("{}{}", e[..1].to_lowercase(), &e[1..]);
                json!({ "eventName": camel, "source": "sessionFlags", "command": cmd, "currentHash": format!("sha256:{e}") })
            })
            .chain([json!({ "eventName": "stop", "source": "user", "command": "other", "currentHash": "sha256:user" })])
            .collect();
        let reply = json!({ "id": 2, "result": { "data": [{ "cwd": "x", "hooks": hooks }] } });
        let h = parse_hooks_list(&reply, cmd).unwrap();
        assert_eq!(h.len(), EVENTS.len());
        assert!(h.contains(&("Stop".into(), "sha256:Stop".into())), "用户自己的 hook 不算");
        assert!(parse_hooks_list(&reply, "different").is_none());
        assert_eq!(snake("PreToolUse"), "pre_tool_use");
        assert_eq!(snake("Interrupt"), "interrupt");
    }

    /// 真的起 Codex app-server 算哈希（要装好 Codex）：`cargo test codex_trust_live -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn codex_trust_live() {
        let home = tempfile::tempdir().unwrap();
        let paths = Paths::discover(Some(home.path().to_path_buf())).unwrap();
        let h = trust_hashes(&paths, "\"C:/x/looklook.exe\" hook \"C:/x/hook.json\"").unwrap();
        println!("{h:?}");
        assert_eq!(h.len(), EVENTS.len());
    }
}
