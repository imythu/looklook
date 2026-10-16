//! 停止 / 删除终端时结束它启动的所有程序。
//!
//! tmux 的 `kill-session` 只给面板里的进程组发 SIGHUP：忽略 SIGHUP 的程序、AI 编程助手开的 MCP 服务、
//! 后台跑着的开发服务器（`npm run dev &`、`nohup …`、自己 setsid 的守护进程）都会留下来，
//! 变成没人管的孤儿进程。直接运行（没有 tmux）时 ttyd 被强制结束，它的子进程也一样。
//!
//! 做法：动手之前先拍下进程表，找出要结束的进程——
//! - 面板（或 ttyd）进程和它的所有子孙；
//! - Linux 上再加上环境里带着这个 tmux 会话标记（`TMUX=<socket>,<服务进程号>,<会话号>`）的进程：
//!   它们是从这个会话里启动、后来脱离了父子关系（父进程已退出，被 init 收养）的。
//!
//! 然后先礼后兵：SIGHUP + SIGTERM（再补一个 SIGCONT，被 Ctrl+Z 挂起的也能收到），等一会儿还在的 SIGKILL。
//! 客户端自己和它的上级进程永远不动（客户端可能就是从某个终端里启动的）。
//! Windows 上用 `taskkill /T /F` 结束整棵进程树。

use std::collections::{HashMap, HashSet};
use std::time::Duration;

/// 先发 SIGTERM，最多等这么久再 SIGKILL。
const GRACE: Duration = Duration::from_secs(2);

/// 要结束的进程：动手前拍下来的，免得父进程先退出后子孙被 init 收养、再也找不到。
#[derive(Debug, Default)]
pub struct Doomed {
    pids: Vec<u32>,
}

impl Doomed {
    pub fn is_empty(&self) -> bool {
        self.pids.is_empty()
    }

    pub fn exclude(&mut self, pid: u32) {
        self.pids.retain(|&p| p != pid);
    }
}

/// 找出 `roots` 和它们的所有子孙；`tmux_marker` 为 `Some((服务进程号, 会话号))` 时（仅 Linux）
/// 再加上环境里带着这个会话标记的进程。
pub fn collect(roots: &[u32], tmux_marker: Option<(u32, u32)>) -> Doomed {
    let table = table();
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    for &(pid, ppid) in &table {
        children.entry(ppid).or_default().push(pid);
    }
    let mut seen: HashSet<u32> = HashSet::new();
    let mut stack: Vec<u32> = roots.to_vec();
    if let Some((server, session)) = tmux_marker {
        stack.extend(with_tmux_marker(&table, server, session));
    }
    while let Some(pid) = stack.pop() {
        if seen.insert(pid) {
            if let Some(c) = children.get(&pid) {
                stack.extend(c);
            }
        }
    }
    // 永远不碰：init、客户端自己和它的上级（客户端可能就是在要停的终端里启动的）、tmux 服务进程。
    let mut keep: HashSet<u32> = HashSet::from([0, 1]);
    let parent: HashMap<u32, u32> = table.iter().copied().collect();
    let mut p = std::process::id();
    while keep.insert(p) {
        match parent.get(&p) {
            Some(&pp) => p = pp,
            None => break,
        }
    }
    if let Some((server, _)) = tmux_marker {
        keep.insert(server);
    }
    let mut pids: Vec<u32> = seen.into_iter().filter(|p| !keep.contains(p)).collect();
    pids.sort_unstable();
    Doomed { pids }
}

/// 发出结束信号（不等待）。在 `kill-session` / 结束 ttyd 之前调用，程序能先收到 SIGTERM 自己收尾。
pub fn signal(d: &Doomed) {
    #[cfg(unix)]
    for &pid in &d.pids {
        unsafe {
            libc::kill(pid as i32, libc::SIGHUP);
            libc::kill(pid as i32, libc::SIGTERM);
            libc::kill(pid as i32, libc::SIGCONT);
        }
    }
    #[cfg(windows)]
    for &pid in &d.pids {
        use std::os::windows::process::CommandExt;
        let _ = std::process::Command::new("taskkill")
            .args(["/T", "/F", "/PID", &pid.to_string()])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .creation_flags(0x0800_0000)
            .status();
    }
}

/// 等它们退出；超过 [`GRACE`] 还在的强制结束。返回被强制结束的个数。
pub async fn reap(d: Doomed) -> usize {
    #[cfg(unix)]
    {
        let deadline = std::time::Instant::now() + GRACE;
        let mut left = d.pids;
        loop {
            left.retain(|&p| alive(p));
            if left.is_empty() || std::time::Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        for &pid in &left {
            unsafe {
                libc::kill(pid as i32, libc::SIGKILL);
            }
        }
        left.len()
    }
    #[cfg(not(unix))]
    {
        let _ = d;
        0
    }
}

/// 还活着（僵尸进程不算：它已经结束了，只是等父进程回收）。
#[cfg(unix)]
fn alive(pid: u32) -> bool {
    if unsafe { libc::kill(pid as i32, 0) } != 0 {
        return false;
    }
    if cfg!(target_os = "linux") {
        if let Ok(s) = std::fs::read_to_string(format!("/proc/{pid}/stat")) {
            return stat_fields(&s).is_none_or(|(state, _)| state != 'Z');
        }
        return false;
    }
    true
}

/// `/proc/{pid}/stat` 里进程名之后的状态与父进程号。进程名可能带空格和括号，从最后一个 `)` 往后解析。
fn stat_fields(s: &str) -> Option<(char, u32)> {
    let rest = &s[s.rfind(')')? + 1..];
    let mut it = rest.split_whitespace();
    let state = it.next()?.chars().next()?;
    let ppid = it.next()?.parse().ok()?;
    Some((state, ppid))
}

/// 进程表：(进程号, 父进程号)。
fn table() -> Vec<(u32, u32)> {
    if cfg!(target_os = "linux") {
        let Ok(dir) = std::fs::read_dir("/proc") else { return vec![] };
        return dir
            .flatten()
            .filter_map(|e| {
                let pid: u32 = e.file_name().to_str()?.parse().ok()?;
                let s = std::fs::read_to_string(e.path().join("stat")).ok()?;
                Some((pid, stat_fields(&s)?.1))
            })
            .collect();
    }
    if cfg!(unix) {
        let Ok(out) = std::process::Command::new("ps").args(["-A", "-o", "pid=,ppid="]).output() else { return vec![] };
        return parse_ps(&String::from_utf8_lossy(&out.stdout));
    }
    vec![]
}

fn parse_ps(s: &str) -> Vec<(u32, u32)> {
    s.lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            Some((it.next()?.parse().ok()?, it.next()?.parse().ok()?))
        })
        .collect()
}

/// Linux：环境变量 `TMUX` 以 `,<服务进程号>,<会话号>` 结尾的进程（tmux 给会话里启动的每个程序都设了它）。
fn with_tmux_marker(table: &[(u32, u32)], server: u32, session: u32) -> Vec<u32> {
    if !cfg!(target_os = "linux") {
        return vec![];
    }
    let suffix = format!(",{server},{session}");
    table
        .iter()
        .filter(|(pid, _)| {
            std::fs::read(format!("/proc/{pid}/environ")).is_ok_and(|env| {
                env.split(|&b| b == 0)
                    .any(|kv| kv.strip_prefix(b"TMUX=").is_some_and(|v| v.ends_with(suffix.as_bytes())))
            })
        })
        .map(|(pid, _)| *pid)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stat_with_odd_process_name() {
        assert_eq!(stat_fields("123 (a) b (c)) S 45 123 123 0"), Some(('S', 45)));
        assert_eq!(stat_fields("9 (zsh) Z 1 9 9"), Some(('Z', 1)));
        assert_eq!(stat_fields("garbage"), None);
    }

    #[test]
    fn ps_output() {
        assert_eq!(parse_ps("  1     0\n 345   1\nbad\n"), vec![(1, 0), (345, 1)]);
    }

    #[test]
    fn never_includes_self_or_ancestors() {
        let me = std::process::id();
        let d = collect(&[me, 1], None);
        assert!(!d.pids.contains(&me));
        assert!(!d.pids.contains(&1));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn kills_whole_tree_including_sighup_ignorers() {
        // sh 起一个忽略 SIGHUP/SIGTERM 的孙进程，模拟“关会话也不退出”的后台程序。
        let mut child = std::process::Command::new("sh")
            .args(["-c", "sh -c 'trap \"\" HUP TERM; sleep 30' & wait"])
            .spawn()
            .unwrap();
        std::thread::sleep(Duration::from_millis(300));
        let d = collect(&[child.id()], None);
        assert!(d.pids.len() >= 3, "{:?}", d.pids); // sh、内层 sh、sleep
        let pids = d.pids.clone();
        signal(&d);
        let _ = child.wait();
        reap(d).await;
        std::thread::sleep(Duration::from_millis(100));
        assert!(pids.iter().all(|&p| !alive(p)), "{pids:?}");
    }
}
