//! 这台电脑上正在监听的 TCP 端口（docs/PLAN_2026-10-27.md §1）：“本机网页”页列出来，点一下就转成网页。
//!
//! - Linux 读 `/proc/net/tcp{,6}` 拿到监听套接字的 inode，再扫 `/proc/<pid>/fd` 对上进程（只看得到当前用户的）；
//!   macOS 用 `lsof`，Windows 用 `netstat -ano`。进程名、命令行、工作目录用 `sysinfo`。
//! - 去掉看看自己的端口（本进程、ttyd 的端口段、管理台和中转端口）。
//! - 认得的程序（OpenClaw、Hermes、开发服务器……）给个名字和一句提示；只按进程名 / 命令行认，端口只作辅助。
//! - 命令行里像口令的参数换成 `***`：这份列表会显示在远程打开的管理台上。

use std::collections::{BTreeMap, HashMap};
use std::net::IpAddr;

use serde::Serialize;

/// 命令行最多给多少个字符
const CMD_MAX: usize = 200;

/// 一个监听端口（同一端口的 IPv4 / IPv6 合成一条）
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Listener {
    pub port: u16,
    /// `local`：只在 127.0.0.1 / ::1 上；`network`：0.0.0.0 / :: 或局域网地址，同一网络的人也能连
    pub scope: &'static str,
    pub pid: Option<u32>,
    /// 进程名；看不到（别的用户的进程）时为 None
    pub process: Option<String>,
    pub cmd: Option<String>,
    pub cwd: Option<String>,
    /// 认得的程序：`openclaw`、`hermes`、`vite`……
    pub app: Option<&'static str>,
    /// 认得的程序是网页还是接口（`web` / `api`）
    pub kind: Option<&'static str>,
}

/// 一条原始记录：地址、端口、进程号
#[derive(Debug, Clone, PartialEq)]
struct Raw {
    addr: IpAddr,
    port: u16,
    pid: Option<u32>,
}

/// 列出监听端口，去掉 `skip` 里的和看看自己的进程。认得的网页排在前面，其余按端口排序。
pub fn list(skip: impl Fn(u16) -> bool) -> Vec<Listener> {
    let raw = raw();
    let me = std::process::id();
    let mut by_port: BTreeMap<u16, Vec<Raw>> = BTreeMap::new();
    for r in raw.into_iter().filter(|r| !skip(r.port) && r.pid != Some(me)) {
        by_port.entry(r.port).or_default().push(r);
    }
    let mut pids: Vec<u32> = by_port.values().flatten().filter_map(|r| r.pid).collect();
    // 同一个进程常常开好几个端口：重复的进程号会让 sysinfo 一个都取不到
    pids.sort_unstable();
    pids.dedup();
    let procs = processes(&pids);
    let mut out: Vec<Listener> = by_port
        .into_iter()
        .filter(|(_, rs)| !rs.iter().filter_map(|r| r.pid).any(|p| procs.get(&p).is_some_and(|p| is_ours(&p.name))))
        .map(|(port, rs)| {
            let scope = if rs.iter().all(|r| r.addr.is_loopback()) { "local" } else { "network" };
            let pid = rs.iter().find_map(|r| r.pid);
            let p = pid.and_then(|p| procs.get(&p));
            let known = p.and_then(|p| recognize(&p.name, &p.cmd, port));
            Listener {
                port,
                scope,
                pid,
                process: p.map(|p| p.name.clone()),
                cmd: p.map(|p| redact(&p.cmd)),
                cwd: p.and_then(|p| p.cwd.clone()),
                app: known.map(|k| k.0),
                kind: known.map(|k| k.1),
            }
        })
        .collect();
    out.sort_by_key(|l| (l.kind != Some("web"), l.port));
    out
}

/// 看看自己的程序（另一个看看、它带的终端程序）
fn is_ours(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n == "looklook" || n == "looklook.exe" || n.starts_with("looklook-")
}

struct Proc {
    name: String,
    cmd: String,
    cwd: Option<String>,
}

fn processes(pids: &[u32]) -> HashMap<u32, Proc> {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
    if pids.is_empty() {
        return HashMap::new();
    }
    let list: Vec<Pid> = pids.iter().map(|p| Pid::from_u32(*p)).collect();
    let mut sys = System::new();
    sys.refresh_processes_specifics(ProcessesToUpdate::Some(&list), true, ProcessRefreshKind::nothing().with_cmd(UpdateKind::Always).with_cwd(UpdateKind::Always));
    pids.iter()
        .filter_map(|pid| {
            let p = sys.process(Pid::from_u32(*pid))?;
            let cmd = p.cmd().iter().map(|a| a.to_string_lossy()).collect::<Vec<_>>().join(" ");
            Some((*pid, Proc { name: p.name().to_string_lossy().into_owned(), cmd, cwd: p.cwd().map(|c| c.display().to_string()).filter(|c| !c.is_empty()) }))
        })
        .collect()
}

/// 命令行里像口令的部分换成 `***`，截到 [`CMD_MAX`] 个字符
fn redact(cmd: &str) -> String {
    let mut out: Vec<String> = vec![];
    let mut hide_next = false;
    for word in cmd.split_whitespace() {
        if hide_next {
            out.push("***".into());
            hide_next = false;
            continue;
        }
        let lower = word.to_ascii_lowercase();
        let secretish = ["token", "password", "passwd", "secret", "api-key", "apikey", "api_key", "auth"];
        if let Some((k, _)) = word.split_once('=').filter(|(k, _)| secretish.iter().any(|s| k.to_ascii_lowercase().contains(s))) {
            // `--token=xxx`、`TOKEN=xxx`、`http://…?token=xxx`
            out.push(format!("{k}=***"));
        } else if lower.starts_with("--") && secretish.iter().any(|s| lower.contains(s)) {
            out.push(word.into());
            hide_next = true;
        } else if word.starts_with("sk-") || word.starts_with("ghp_") || word.starts_with("xox") {
            out.push("***".into());
        } else {
            out.push(word.into());
        }
    }
    let s = out.join(" ");
    if s.chars().count() > CMD_MAX {
        format!("{}…", s.chars().take(CMD_MAX).collect::<String>())
    } else {
        s
    }
}

/// 认得的程序：（名字，网页还是接口）
fn recognize(name: &str, cmd: &str, port: u16) -> Option<(&'static str, &'static str)> {
    let name = name.to_ascii_lowercase();
    let name = name.trim_end_matches(".exe");
    let cmd = cmd.to_ascii_lowercase();
    let words: Vec<&str> = cmd.split_whitespace().map(|w| w.rsplit(['/', '\\']).next().unwrap_or(w)).collect();
    let has = |w: &str| words.iter().any(|x| *x == w || x.trim_end_matches(".exe") == w || x.trim_end_matches(".js") == w || x.trim_end_matches(".mjs") == w);
    let found = if name == "openclaw" || has("openclaw") || cmd.contains("openclaw") && (cmd.contains("gateway") || port == 18789) {
        ("openclaw", "web")
    } else if (name == "hermes" || has("hermes")) && (cmd.contains("dashboard") || port == 9119) {
        ("hermes", "web")
    } else if (name == "qwen" || has("qwen")) && has("serve") {
        ("qwen", "web")
    } else if name == "ollama" || has("ollama") {
        ("ollama", "api")
    } else if has("jupyter") || has("jupyter-lab") || has("jupyter-notebook") || cmd.contains("jupyterlab") {
        ("jupyter", "web")
    } else if has("vite") {
        ("vite", "web")
    } else if has("next") && (has("dev") || has("start")) || cmd.contains("next/dist") {
        ("next", "web")
    } else if name == "code-server" || has("code-server") {
        ("code-server", "web")
    } else if cmd.contains("comfyui") || has("main.py") && port == 8188 {
        ("comfyui", "web")
    } else if cmd.contains("open-webui") || cmd.contains("open_webui") {
        ("open-webui", "web")
    } else if cmd.contains("gradio") || port == 7860 && name.starts_with("python") {
        ("gradio", "web")
    } else {
        return None;
    };
    Some(found)
}

// ---------------- 各平台的监听表 ----------------

#[cfg(target_os = "linux")]
fn raw() -> Vec<Raw> {
    let mut by_inode: HashMap<u64, Vec<(IpAddr, u16)>> = HashMap::new();
    for (file, v6) in [("/proc/net/tcp", false), ("/proc/net/tcp6", true)] {
        if let Ok(text) = std::fs::read_to_string(file) {
            for (addr, port, inode) in parse_proc_net(&text, v6) {
                by_inode.entry(inode).or_default().push((addr, port));
            }
        }
    }
    let mut owner: HashMap<u64, u32> = HashMap::new();
    if let Ok(dir) = std::fs::read_dir("/proc") {
        'procs: for e in dir.flatten() {
            let Some(pid) = e.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else { continue };
            let Ok(fds) = std::fs::read_dir(e.path().join("fd")) else { continue };
            for fd in fds.flatten() {
                let Ok(target) = std::fs::read_link(fd.path()) else { continue };
                let t = target.to_string_lossy();
                if let Some(inode) = t.strip_prefix("socket:[").and_then(|s| s.strip_suffix(']')).and_then(|s| s.parse::<u64>().ok()) {
                    if by_inode.contains_key(&inode) {
                        owner.entry(inode).or_insert(pid);
                        if owner.len() == by_inode.len() {
                            break 'procs;
                        }
                    }
                }
            }
        }
    }
    by_inode.into_iter().flat_map(|(inode, list)| list.into_iter().map(move |(addr, port)| (inode, addr, port))).map(|(inode, addr, port)| Raw { addr, port, pid: owner.get(&inode).copied() }).collect()
}

/// `/proc/net/tcp{,6}` 里 LISTEN（状态 0A）的行：（地址，端口，inode）
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn parse_proc_net(text: &str, v6: bool) -> Vec<(IpAddr, u16, u64)> {
    text.lines()
        .skip(1)
        .filter_map(|line| {
            let f: Vec<&str> = line.split_whitespace().collect();
            if f.len() < 10 || f[3] != "0A" {
                return None;
            }
            let (hex_addr, hex_port) = f[1].split_once(':')?;
            let port = u16::from_str_radix(hex_port, 16).ok()?;
            let inode = f[9].parse::<u64>().ok()?;
            let addr = if v6 {
                // 4 个 32 位字，每个按本机字节序（小端）存
                if hex_addr.len() != 32 {
                    return None;
                }
                let mut b = [0u8; 16];
                for i in 0..4 {
                    let w = u32::from_str_radix(&hex_addr[i * 8..i * 8 + 8], 16).ok()?;
                    b[i * 4..i * 4 + 4].copy_from_slice(&w.to_le_bytes());
                }
                let a = std::net::Ipv6Addr::from(b);
                a.to_ipv4_mapped().map_or(IpAddr::V6(a), IpAddr::V4)
            } else {
                IpAddr::V4(std::net::Ipv4Addr::from(u32::from_str_radix(hex_addr, 16).ok()?.to_le_bytes()))
            };
            Some((addr, port, inode))
        })
        .collect()
}

#[cfg(target_os = "macos")]
fn raw() -> Vec<Raw> {
    let Ok(out) = std::process::Command::new("lsof").args(["-nP", "-iTCP", "-sTCP:LISTEN", "-F", "pn"]).output() else { return vec![] };
    parse_lsof(&String::from_utf8_lossy(&out.stdout))
}

/// `lsof -F pn` 的输出：`p<pid>` 一行，后面若干 `n<地址>:<端口>`
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn parse_lsof(text: &str) -> Vec<Raw> {
    let mut pid = None;
    let mut out = vec![];
    for line in text.lines() {
        if let Some(p) = line.strip_prefix('p') {
            pid = p.parse::<u32>().ok();
        } else if let Some(n) = line.strip_prefix('n') {
            if let Some((addr, port)) = split_host_port(n) {
                out.push(Raw { addr, port, pid });
            }
        }
    }
    out
}

#[cfg(windows)]
fn raw() -> Vec<Raw> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let mut out = vec![];
    for proto in ["TCP", "TCPv6"] {
        if let Ok(o) = std::process::Command::new("netstat").args(["-ano", "-p", proto]).creation_flags(CREATE_NO_WINDOW).output() {
            out.extend(parse_netstat(&String::from_utf8_lossy(&o.stdout)));
        }
    }
    out
}

/// `netstat -ano` 里 LISTENING 的行：`TCP  0.0.0.0:135  0.0.0.0:0  LISTENING  1234`
#[cfg_attr(not(windows), allow(dead_code))]
fn parse_netstat(text: &str) -> Vec<Raw> {
    text.lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            if f.len() < 5 || !f[0].eq_ignore_ascii_case("tcp") || f[3] != "LISTENING" {
                return None;
            }
            let (addr, port) = split_host_port(f[1])?;
            Some(Raw { addr, port, pid: f[4].parse().ok().filter(|p| *p != 0) })
        })
        .collect()
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn raw() -> Vec<Raw> {
    vec![]
}

/// `127.0.0.1:5173`、`[::1]:5173`、`*:5173`、`[::]:5173`
#[cfg_attr(target_os = "linux", allow(dead_code))]
fn split_host_port(s: &str) -> Option<(IpAddr, u16)> {
    let (host, port) = s.rsplit_once(':')?;
    let port = port.parse::<u16>().ok()?;
    let host = host.trim_start_matches('[').trim_end_matches(']');
    let host = host.split('%').next().unwrap_or(host);
    let addr = if host == "*" { IpAddr::from([0, 0, 0, 0]) } else { host.parse().ok()? };
    Some((addr, port))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proc_net_listen_rows() {
        let v4 = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n   0: 0100007F:4965 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 12345 1 0000000000000000 100 0 0 10 0\n   1: 00000000:1F90 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 222 1\n   2: 0100007F:4965 0100007F:D2F0 01 00000000:00000000 00:00000000 00000000  1000        0 333 1\n";
        let rows = parse_proc_net(v4, false);
        assert_eq!(rows, vec![(IpAddr::from([127, 0, 0, 1]), 18789, 12345), (IpAddr::from([0, 0, 0, 0]), 8080, 222)]);
        // ::1 和 ::
        let v6 = "  sl  local_address                         remote_address                        st\n   0: 00000000000000000000000001000000:1435 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 444 1\n   1: 00000000000000000000000000000000:23AF 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 555 1\n";
        let rows = parse_proc_net(v6, true);
        assert_eq!(rows[0], ("::1".parse().unwrap(), 5173, 444));
        assert_eq!(rows[1], ("::".parse().unwrap(), 9135, 555));
    }

    #[test]
    fn lsof_and_netstat() {
        let l = parse_lsof("p501\nn127.0.0.1:18789\nn[::1]:18789\np77\nn*:5173\n");
        assert_eq!(l.len(), 3);
        assert_eq!(l[2], Raw { addr: IpAddr::from([0, 0, 0, 0]), port: 5173, pid: Some(77) });
        let n = parse_netstat("\nActive Connections\n\n  Proto  Local Address          Foreign Address        State           PID\n  TCP    0.0.0.0:135            0.0.0.0:0              LISTENING       1012\n  TCP    127.0.0.1:9119         0.0.0.0:0              LISTENING       4321\n  TCP    [::1]:5173             [::]:0                 LISTENING       99\n  TCP    10.0.0.2:50000         1.2.3.4:443            ESTABLISHED     55\n");
        assert_eq!(n.len(), 3);
        assert_eq!(n[1], Raw { addr: IpAddr::from([127, 0, 0, 1]), port: 9119, pid: Some(4321) });
        assert_eq!(n[2].addr, "::1".parse::<IpAddr>().unwrap());
    }

    #[test]
    fn known_apps() {
        assert_eq!(recognize("node", "node /usr/lib/node_modules/openclaw/dist/index.js gateway --port 18789", 18789), Some(("openclaw", "web")));
        assert_eq!(recognize("openclaw", "openclaw gateway", 18789), Some(("openclaw", "web")));
        assert_eq!(recognize("python3", "/home/a/.local/bin/hermes dashboard --no-open", 9119), Some(("hermes", "web")));
        assert_eq!(recognize("python3", "/home/a/.local/bin/hermes chat", 9120), None);
        assert_eq!(recognize("node", "node /p/node_modules/.bin/vite --host", 5173), Some(("vite", "web")));
        assert_eq!(recognize("node", "node /p/node_modules/next/dist/bin/next dev", 3000), Some(("next", "web")));
        assert_eq!(recognize("ollama", "/usr/local/bin/ollama serve", 11434), Some(("ollama", "api")));
        assert_eq!(recognize("node", "qwen serve --local-control", 8765), Some(("qwen", "web")));
        assert_eq!(recognize("python", "python -m jupyter lab", 8888), Some(("jupyter", "web")));
        assert_eq!(recognize("nginx", "nginx: master process", 80), None);
    }

    #[test]
    fn secrets_are_hidden() {
        assert_eq!(redact("node app.js --token=abc123 --port 3000"), "node app.js --token=*** --port 3000");
        assert_eq!(redact("server --password hunter2 --x"), "server --password *** --x");
        assert_eq!(redact("API_KEY=sk-123 run"), "API_KEY=*** run");
        assert_eq!(redact("curl http://127.0.0.1:8888/?token=deadbeef"), "curl http://127.0.0.1:8888/?token=***");
        assert_eq!(redact("tool sk-abcdef"), "tool ***");
        let long = "x ".repeat(300);
        assert!(redact(&long).ends_with('…'));
    }

    #[test]
    fn host_port() {
        assert_eq!(split_host_port("[fe80::1%en0]:8080"), Some(("fe80::1".parse().unwrap(), 8080)));
        assert_eq!(split_host_port("*:22"), Some((IpAddr::from([0, 0, 0, 0]), 22)));
        assert_eq!(split_host_port("nonsense"), None);
    }

    /// 真的开一个端口，能在列表里看到（并且是本进程的会被去掉）
    #[cfg(target_os = "linux")]
    #[test]
    fn sees_a_real_listener_but_not_its_own() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        assert!(raw().iter().any(|r| r.port == port && r.pid == Some(std::process::id())));
        assert!(!list(|_| false).iter().any(|x| x.port == port));
    }
}
