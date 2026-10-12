//! SSH 主机管理：保存常用的服务器，一键在看看里打开 SSH 终端；允许的话，AI 助手也能经 MCP 在上面运行命令，
//! 但始终拿不到密码、私钥。
//!
//! - 主机列表存在本机数据库（`ssh_hosts`）；密码 / 私钥口令用 ChaCha20-Poly1305 加密后单独存（`ssh_secret:{id}`），
//!   密钥文件 `{LOOKLOOK_HOME}/ssh/secret.key`（0600）。私钥只记路径（一般是 `~/.ssh/id_ed25519`），不复制；
//! - 连接用系统自带的 `ssh`。需要密码时，ssh 通过 `SSH_ASKPASS` 回头找启动它的 `looklook ssh` 进程要：
//!   环境变量里只有一个本机端口和一次性的随机口令，密码不出现在命令行参数、环境变量、文件或终端输出里；
//! - MCP（gateway/mcp.rs）只能用标了“允许 AI 使用”的主机，输出里出现的已存密码、私钥块、常见令牌都换成 `[redacted]`。
//!
//! 局限：AI 助手和看看以同一个系统用户运行，真想偷的程序可以读这台电脑上用户能读的任何东西（包括 `~/.ssh`）。
//! 这里防的是“顺手”泄露——密码被打进对话、日志、截图，或被 AI 从配置文件里读出来带进上下文。

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use base64::Engine;
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use serde::{Deserialize, Serialize};

use crate::error::LocalError;
use crate::store::Store;

const HOSTS_KEY: &str = "ssh_hosts";
const SECRET_PREFIX: &str = "ssh_secret:";
/// ssh 调用 askpass 时，看看用这个环境变量找到发起连接的进程：`端口:口令`
pub const ASKPASS_ENV: &str = "LOOKLOOK_ASKPASS";
/// MCP 运行命令的输出上限（每路）
pub const OUTPUT_MAX: usize = 64 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SshHost {
    pub id: String,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub user: String,
    /// `agent`：ssh 默认的密钥 / ssh-agent；`key`：指定私钥文件（口令可选，存为秘密）；`password`：密码（存为秘密）
    pub auth: String,
    #[serde(default)]
    pub key_path: String,
    /// 允许 AI 助手经 MCP 在这台主机上运行命令
    #[serde(default)]
    pub allow_ai: bool,
}

/// 新建 / 修改主机的请求
#[derive(Debug, Clone, Deserialize)]
pub struct HostInput {
    pub name: String,
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    pub user: String,
    pub auth: String,
    #[serde(default)]
    pub key_path: String,
    #[serde(default)]
    pub allow_ai: bool,
    /// 密码或私钥口令；None / 空 = 不改，`clear_secret` 为 true 时删掉已存的
    #[serde(default)]
    pub secret: Option<String>,
    #[serde(default)]
    pub clear_secret: bool,
}

fn default_port() -> u16 {
    22
}

/// 列表里给界面的样子：不含秘密，只说有没有。
#[derive(Debug, Serialize)]
pub struct HostView {
    #[serde(flatten)]
    pub host: SshHost,
    pub has_secret: bool,
}

// ---------------- 加密 ----------------

fn key_file(home: &Path) -> PathBuf {
    home.join("ssh").join("secret.key")
}

fn cipher(home: &Path) -> Result<ChaCha20Poly1305> {
    let path = key_file(home);
    let key: [u8; 32] = match std::fs::read(&path) {
        Ok(b) if b.len() == 32 => b.try_into().expect("32 bytes"),
        Ok(_) => bail!("{} 已损坏", path.display()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let dir = path.parent().expect("ssh dir");
            std::fs::create_dir_all(dir)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
            }
            let k: [u8; 32] = rand::random();
            crate::util::write_private(&path, &k)?;
            k
        }
        Err(e) => return Err(e.into()),
    };
    Ok(ChaCha20Poly1305::new(&key.into()))
}

fn encrypt(home: &Path, plain: &str) -> Result<String> {
    let nonce: [u8; 12] = rand::random();
    let ct = cipher(home)?.encrypt(Nonce::from_slice(&nonce), plain.as_bytes()).map_err(|_| anyhow::anyhow!("加密失败"))?;
    let mut blob = nonce.to_vec();
    blob.extend(ct);
    Ok(base64::engine::general_purpose::STANDARD.encode(blob))
}

fn decrypt(home: &Path, b64: &str) -> Result<String> {
    let blob = base64::engine::general_purpose::STANDARD.decode(b64)?;
    if blob.len() < 12 + 16 {
        bail!("已存的密码已损坏");
    }
    let (nonce, ct) = blob.split_at(12);
    let plain = cipher(home)?.decrypt(Nonce::from_slice(nonce), ct).map_err(|_| anyhow::anyhow!("已存的密码解不开（{} 被换过？）", key_file(home).display()))?;
    Ok(String::from_utf8(plain)?)
}

// ---------------- 存取 ----------------

pub fn list(store: &Store) -> Vec<SshHost> {
    store.get::<Vec<SshHost>>(HOSTS_KEY).ok().flatten().unwrap_or_default()
}

pub fn views(store: &Store) -> Vec<HostView> {
    list(store).into_iter().map(|h| HostView { has_secret: has_secret(store, &h.id), host: h }).collect()
}

fn has_secret(store: &Store, id: &str) -> bool {
    store.get::<String>(&format!("{SECRET_PREFIX}{id}")).ok().flatten().is_some()
}

/// 按 id 或名字（不分大小写）找主机。
pub fn find(store: &Store, key: &str) -> Option<SshHost> {
    let key = key.trim();
    let hosts = list(store);
    hosts.iter().find(|h| h.id == key).or_else(|| hosts.iter().find(|h| h.name.eq_ignore_ascii_case(key))).cloned()
}

pub fn secret(store: &Store, home: &Path, id: &str) -> Result<Option<String>> {
    match store.get::<String>(&format!("{SECRET_PREFIX}{id}"))? {
        Some(b) => Ok(Some(decrypt(home, &b)?)),
        None => Ok(None),
    }
}

fn validate(i: &HostInput) -> Result<(), LocalError> {
    let name = i.name.trim();
    if name.is_empty() || name.chars().count() > 40 || name.chars().any(char::is_control) {
        return Err(LocalError::invalid("name", "length"));
    }
    let host = i.host.trim();
    // 主机名、IPv4、IPv6；不能以 - 开头（否则会被 ssh 当成选项）
    if host.is_empty() || host.len() > 253 || host.starts_with('-') || !host.chars().all(|c| c.is_ascii_alphanumeric() || ".-:_[]%".contains(c)) {
        return Err(LocalError::invalid("host", "host_name"));
    }
    let user = i.user.trim();
    if user.is_empty() || user.len() > 64 || user.starts_with('-') || !user.chars().all(|c| c.is_ascii_alphanumeric() || "._-$\\".contains(c)) {
        return Err(LocalError::invalid("user", "charset"));
    }
    if i.port == 0 {
        return Err(LocalError::invalid("port", "port"));
    }
    match i.auth.as_str() {
        "agent" | "password" => {}
        "key" => {
            let p = i.key_path.trim();
            if p.is_empty() || p.starts_with('-') {
                return Err(LocalError::invalid("key_path", "required"));
            }
        }
        _ => return Err(LocalError::invalid("auth", "enum")),
    }
    Ok(())
}

/// 新建（`id` 为 None）或修改主机，返回保存后的主机。
pub fn save(store: &Store, home: &Path, id: Option<&str>, i: HostInput) -> Result<SshHost, LocalError> {
    validate(&i)?;
    let mut hosts = list(store);
    let name = i.name.trim().to_string();
    if hosts.iter().any(|h| Some(h.id.as_str()) != id && h.name.eq_ignore_ascii_case(&name)) {
        return Err(LocalError::new("SSH_NAME_TAKEN").with("name", name));
    }
    let key_path = if i.auth == "key" { i.key_path.trim().to_string() } else { String::new() };
    let host = SshHost {
        id: id.map(str::to_string).unwrap_or_else(|| crate::util::random_id(8)),
        name,
        host: i.host.trim().trim_start_matches('[').trim_end_matches(']').to_string(),
        port: i.port,
        user: i.user.trim().to_string(),
        auth: i.auth.clone(),
        key_path,
        allow_ai: i.allow_ai,
    };
    match id {
        Some(id) => {
            let slot = hosts.iter_mut().find(|h| h.id == id).ok_or_else(LocalError::not_found)?;
            *slot = host.clone();
        }
        None => {
            if hosts.len() >= 200 {
                return Err(LocalError::invalid("hosts", "too_many"));
            }
            hosts.push(host.clone());
        }
    }
    let skey = format!("{SECRET_PREFIX}{}", host.id);
    if i.auth == "agent" || i.clear_secret {
        store.remove(&skey)?;
    }
    if let Some(s) = i.secret.as_deref().filter(|s| !s.is_empty()) {
        if i.auth != "agent" {
            store.set(&skey, &encrypt(home, s)?)?;
        }
    }
    store.set(HOSTS_KEY, &hosts)?;
    Ok(host)
}

pub fn delete(store: &Store, id: &str) -> Result<(), LocalError> {
    let mut hosts = list(store);
    let before = hosts.len();
    hosts.retain(|h| h.id != id);
    if hosts.len() == before {
        return Err(LocalError::not_found());
    }
    store.set(HOSTS_KEY, &hosts)?;
    store.remove(&format!("{SECRET_PREFIX}{id}"))?;
    Ok(())
}

// ---------------- 连接 ----------------

fn ssh_bin() -> String {
    crate::shells::which("ssh").unwrap_or_else(|| "ssh".into())
}

/// ssh 的参数（不含远程命令）。
pub fn ssh_args(h: &SshHost, batch: bool) -> Vec<String> {
    let mut a: Vec<String> = vec!["-p".into(), h.port.to_string(), "-o".into(), "StrictHostKeyChecking=accept-new".into(), "-o".into(), "ServerAliveInterval=30".into()];
    match h.auth.as_str() {
        "key" => {
            a.extend(["-i".into(), expand_home(&h.key_path), "-o".into(), "IdentitiesOnly=yes".into()]);
        }
        "password" => a.extend(["-o".into(), "PreferredAuthentications=password,keyboard-interactive".into(), "-o".into(), "PubkeyAuthentication=no".into()]),
        _ => {}
    }
    if batch {
        a.extend(["-T".into(), "-o".into(), "ConnectTimeout=15".into(), "-o".into(), "NumberOfPasswordPrompts=1".into()]);
    }
    a.push("-l".into());
    a.push(h.user.clone());
    a.push("--".into());
    a.push(h.host.clone());
    a
}

fn expand_home(p: &str) -> String {
    match p.strip_prefix("~/").or_else(|| p.strip_prefix("~\\")) {
        Some(rest) => dirs::home_dir().map(|h| h.join(rest).display().to_string()).unwrap_or_else(|| p.to_string()),
        None => p.to_string(),
    }
}

/// 给 ssh 的 askpass 服务：本机随机端口 + 一次性口令。只回答问密码 / 口令的提示，最多 3 次（输错重试），
/// ssh 结束（`stop`）或 2 分钟后关闭。
struct Askpass {
    env: String,
    stop: Arc<AtomicBool>,
}

impl Askpass {
    fn start(secret: String) -> Result<Self> {
        let l = TcpListener::bind("127.0.0.1:0")?;
        l.set_nonblocking(true)?;
        let port = l.local_addr()?.port();
        let token = crate::util::random_id(32);
        let stop = Arc::new(AtomicBool::new(false));
        let (t, s) = (token.clone(), stop.clone());
        std::thread::spawn(move || {
            let started = Instant::now();
            let mut answered = 0;
            while !s.load(Ordering::Relaxed) && answered < 3 && started.elapsed() < Duration::from_secs(120) {
                match l.accept() {
                    Ok((c, _)) => {
                        if serve_askpass(c, &t, &secret) {
                            answered += 1;
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(50)),
                    Err(_) => break,
                }
            }
        });
        Ok(Self { env: format!("{port}:{token}"), stop })
    }
}

impl Drop for Askpass {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// 一次请求：`口令\n提示\n` → 密码。口令不对、或者不是问密码（比如问要不要信任主机）就什么也不给。
fn serve_askpass(c: TcpStream, token: &str, secret: &str) -> bool {
    let _ = c.set_nonblocking(false);
    let _ = c.set_read_timeout(Some(Duration::from_secs(5)));
    let mut r = BufReader::new(match c.try_clone() {
        Ok(c) => c,
        Err(_) => return false,
    });
    let (mut t, mut prompt) = (String::new(), String::new());
    if r.read_line(&mut t).is_err() || r.read_line(&mut prompt).is_err() {
        return false;
    }
    if !ct_eq(t.trim_end().as_bytes(), token.as_bytes()) {
        return false;
    }
    let p = prompt.to_ascii_lowercase();
    if !(p.contains("password") || p.contains("passphrase") || p.contains("verification") || p.contains("密码")) {
        return false;
    }
    let mut w = c;
    w.write_all(secret.as_bytes()).is_ok() && w.write_all(b"\n").is_ok()
}

/// askpass 模式（ssh 以 `<looklook> <提示>` 调用我们）：去找发起连接的 `looklook ssh` 进程要密码，打印给 ssh。
pub fn askpass_main(env: &str, prompt: &str) -> i32 {
    let Some((port, token)) = env.split_once(':') else { return 1 };
    let Ok(port) = port.parse::<u16>() else { return 1 };
    let Ok(mut c) = TcpStream::connect_timeout(&([127, 0, 0, 1], port).into(), Duration::from_secs(3)) else { return 1 };
    let one_line = prompt.replace(['\r', '\n'], " ");
    if c.write_all(format!("{token}\n{one_line}\n").as_bytes()).is_err() {
        return 1;
    }
    let _ = c.shutdown(std::net::Shutdown::Write);
    let mut out = String::new();
    if c.read_to_string(&mut out).is_err() || out.is_empty() {
        return 1;
    }
    print!("{out}");
    let _ = std::io::stdout().flush();
    0
}

/// 准备好的 ssh 命令（需要密码时带着 askpass 服务，命令结束前要一直持有）。
pub struct Prepared {
    pub cmd: Command,
    _askpass: Option<Askpass>,
    pub secret: Option<String>,
}

pub fn prepare(store: &Store, home: &Path, h: &SshHost, remote: &[String], batch: bool) -> Result<Prepared> {
    let mut cmd = Command::new(ssh_bin());
    cmd.args(ssh_args(h, batch));
    cmd.args(remote);
    let secret = if h.auth == "agent" { None } else { secret(store, home, &h.id)? };
    let askpass = match &secret {
        Some(s) => {
            let a = Askpass::start(s.clone())?;
            let exe = std::env::current_exe().context("找不到看看程序的路径")?;
            cmd.env("SSH_ASKPASS", exe).env("SSH_ASKPASS_REQUIRE", "force").env(ASKPASS_ENV, &a.env);
            // 老版本 OpenSSH 只在有 DISPLAY 时才用 askpass
            if std::env::var_os("DISPLAY").is_none() {
                cmd.env("DISPLAY", "looklook:0");
            }
            Some(a)
        }
        None => None,
    };
    Ok(Prepared { cmd, _askpass: askpass, secret })
}

/// `looklook ssh <主机> [-- 远程命令…]`：交互式连接（看看的 SSH 终端就是在 shell 里运行这个）。
pub fn connect_cli(store: &Store, home: &Path, key: &str, remote: &[String]) -> Result<i32> {
    let h = find(store, key).with_context(|| format!("没有叫 {key:?} 的 SSH 主机（在看看的“SSH 主机”页添加）"))?;
    eprintln!("\x1b[2m看看 · SSH {} ({}@{}:{})\x1b[0m", h.name, h.user, h.host, h.port);
    let mut p = prepare(store, home, &h, remote, false)?;
    let status = p.cmd.status().context("运行 ssh 失败：这台电脑上没有 OpenSSH 客户端？")?;
    Ok(status.code().unwrap_or(255))
}

/// 把已存的密码、私钥块和常见令牌从文本里去掉。
pub fn scrub(text: &str, secret: Option<&str>) -> String {
    let mut out = text.to_string();
    if let Some(s) = secret.filter(|s| s.len() >= 3) {
        out = out.replace(s, "[redacted]");
    }
    // PEM 私钥块（证书等公开内容不动）
    const MARK: &str = "[redacted private key]\n";
    let mut pos = 0;
    while let Some(rel) = out[pos..].find("-----BEGIN ") {
        let i = pos + rel;
        let line_end = out[i..].find('\n').map_or(out.len(), |k| i + k);
        if !out[i..line_end].contains("PRIVATE KEY") {
            pos = line_end;
            continue;
        }
        let end = match out[i..].find("-----END ") {
            Some(j) => out[i + j..].find('\n').map_or(out.len(), |k| i + j + k + 1),
            None => out.len(),
        };
        out.replace_range(i..end, MARK);
        pos = i + MARK.len();
    }
    // 常见令牌的前缀：AWS、GitHub、OpenAI / Anthropic、Slack、GitLab
    for prefix in ["AKIA", "ASIA", "ghp_", "gho_", "ghs_", "github_pat_", "sk-ant-", "sk-proj-", "sk-", "xoxb-", "xoxp-", "glpat-"] {
        let mut res = String::with_capacity(out.len());
        let mut rest = out.as_str();
        while let Some(i) = rest.find(prefix) {
            let tail = &rest[i + prefix.len()..];
            let n = tail.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-').count();
            let boundary = i == 0 || !rest[..i].chars().last().is_some_and(|c| c.is_ascii_alphanumeric());
            res.push_str(&rest[..i]);
            if n >= 16 && boundary {
                res.push_str(prefix);
                res.push_str("[redacted]");
            } else {
                res.push_str(&rest[i..i + prefix.len() + n]);
            }
            rest = &tail[n..];
        }
        res.push_str(rest);
        out = res;
    }
    out
}

fn cap(b: &[u8]) -> (String, bool) {
    let truncated = b.len() > OUTPUT_MAX;
    (String::from_utf8_lossy(&b[..b.len().min(OUTPUT_MAX)]).into_owned(), truncated)
}

#[derive(Debug, Serialize)]
pub struct RunResult {
    pub host: String,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub truncated: bool,
    pub timed_out: bool,
}

/// 不交互地在主机上运行一条命令（MCP、界面上的“测试连接”），超时就结束。输出去掉秘密。
pub async fn run(store: Arc<Store>, home: PathBuf, h: SshHost, command: String, timeout: Duration) -> Result<RunResult, LocalError> {
    tokio::task::spawn_blocking(move || -> Result<RunResult, LocalError> {
        let mut p = prepare(&store, &home, &h, &[command], true).map_err(|e| LocalError::new("SSH_FAILED").with("detail", format!("{e:#}")))?;
        p.cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            p.cmd.creation_flags(0x0800_0000);
        }
        let mut child = p.cmd.spawn().map_err(|e| LocalError::new("SSH_MISSING").with("detail", e.to_string()))?;
        let mut so = child.stdout.take();
        let mut se = child.stderr.take();
        let ro = std::thread::spawn(move || {
            let mut b = Vec::new();
            if let Some(o) = so.as_mut() {
                let _ = o.take(OUTPUT_MAX as u64 + 1).read_to_end(&mut b);
                let _ = std::io::copy(o, &mut std::io::sink());
            }
            b
        });
        let re = std::thread::spawn(move || {
            let mut b = Vec::new();
            if let Some(e) = se.as_mut() {
                let _ = e.take(OUTPUT_MAX as u64 + 1).read_to_end(&mut b);
                let _ = std::io::copy(e, &mut std::io::sink());
            }
            b
        });
        let start = Instant::now();
        let mut timed_out = false;
        let status = loop {
            if let Some(s) = child.try_wait()? {
                break Some(s);
            }
            if start.elapsed() > timeout {
                let _ = child.kill();
                let _ = child.wait();
                timed_out = true;
                break None;
            }
            std::thread::sleep(Duration::from_millis(25));
        };
        let (stdout, t1) = cap(&ro.join().unwrap_or_default());
        let (stderr, t2) = cap(&re.join().unwrap_or_default());
        let secret = p.secret.as_deref();
        Ok(RunResult {
            host: h.name.clone(),
            exit_code: status.and_then(|s| s.code()),
            stdout: scrub(&stdout, secret),
            stderr: scrub(&stderr, secret),
            truncated: t1 || t2,
            timed_out,
        })
    })
    .await
    .map_err(|e| LocalError::from(anyhow::anyhow!(e)))?
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(name: &str) -> HostInput {
        HostInput { name: name.into(), host: "example.com".into(), port: 22, user: "deploy".into(), auth: "password".into(), key_path: String::new(), allow_ai: true, secret: Some("hunter2!pw".into()), clear_secret: false }
    }

    #[test]
    fn hosts_and_secrets_round_trip_encrypted() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::memory().unwrap();
        let h = save(&store, tmp.path(), None, input("prod")).unwrap();
        assert_eq!(secret(&store, tmp.path(), &h.id).unwrap().as_deref(), Some("hunter2!pw"));
        let raw: String = store.get(&format!("{SECRET_PREFIX}{}", h.id)).unwrap().unwrap();
        assert!(!raw.contains("hunter2"), "存的是密文");
        assert_eq!(save(&store, tmp.path(), None, input("PROD")).unwrap_err().code, "SSH_NAME_TAKEN");
        // 修改时不填密码：保留原来的
        let mut i = input("prod");
        i.secret = None;
        i.port = 2222;
        save(&store, tmp.path(), Some(&h.id), i).unwrap();
        assert_eq!(find(&store, "Prod").unwrap().port, 2222);
        assert!(views(&store)[0].has_secret);
        // 改成 ssh-agent：密码删掉
        let mut i = input("prod");
        i.auth = "agent".into();
        save(&store, tmp.path(), Some(&h.id), i).unwrap();
        assert!(!views(&store)[0].has_secret);
        delete(&store, &h.id).unwrap();
        assert!(list(&store).is_empty());
        assert_eq!(delete(&store, &h.id).unwrap_err().code, "NOT_FOUND");
    }

    #[test]
    fn rejects_option_injection() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::memory().unwrap();
        for (field, bad) in [("host", "-oProxyCommand=sh"), ("host", "a b"), ("user", "-oX"), ("user", "a;b")] {
            let mut i = input("x");
            if field == "host" {
                i.host = bad.into();
            } else {
                i.user = bad.into();
            }
            assert!(save(&store, tmp.path(), None, i).is_err(), "{field}={bad}");
        }
        let h = SshHost { id: "a".into(), name: "n".into(), host: "h".into(), port: 22, user: "u".into(), auth: "key".into(), key_path: "~/.ssh/id_ed25519".into(), allow_ai: false };
        let a = ssh_args(&h, true);
        // 主机名放在 -- 之后，永远不会被当成选项
        assert_eq!(&a[a.len() - 2..], &["--".to_string(), "h".to_string()]);
        assert!(a.iter().any(|x| x.ends_with(".ssh/id_ed25519") && !x.starts_with('~')));
    }

    #[test]
    fn scrubs_secrets_keys_and_tokens() {
        // 拼出来的假私钥块：源码里不出现完整的 PEM 头（开源导出的检查会拦）
        let (kind, tail) = ("OPENSSH", "PRIVATE KEY-----");
        let text = format!("pw is hunter2!pw\n-----BEGIN {kind} {tail}\nb3BlbnNzaC1rZXk\n-----END {kind} {tail}\nkey AKIAABCDEFGHIJKLMNOP and ghp_abcdefghijklmnopqrstuvwxyz0123 skip sk-short\n");
        let text = text.as_str();
        let s = scrub(text, Some("hunter2!pw"));
        assert!(!s.contains("hunter2") && !s.contains("b3BlbnNzaC1rZXk") && !s.contains("ABCDEFGHIJKLMNOP") && !s.contains("abcdefghijklmnop"), "{s}");
        assert!(s.contains("[redacted private key]") && s.contains("AKIA[redacted]") && s.contains("sk-short"), "{s}");
        let cert = "-----BEGIN CERTIFICATE-----\nMIIB\n-----END CERTIFICATE-----\n";
        assert_eq!(scrub(cert, None), cert, "证书不是秘密");
    }

    #[tokio::test]
    async fn run_reports_connection_errors_without_hanging() {
        if crate::shells::which("ssh").is_none() {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::memory().unwrap());
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port(); // 关掉后没人监听
        let mut i = input("closed");
        i.host = "127.0.0.1".into();
        i.port = port;
        let h = save(&store, tmp.path(), None, i).unwrap();
        let started = Instant::now();
        let r = run(store, tmp.path().to_path_buf(), h, "true".into(), Duration::from_secs(20)).await.unwrap();
        assert_eq!(r.exit_code, Some(255));
        assert!(!r.timed_out && started.elapsed() < Duration::from_secs(15));
        assert!(r.stderr.to_ascii_lowercase().contains("refused"), "{}", r.stderr);
    }

    #[test]
    fn askpass_hands_the_secret_only_to_the_token_holder() {
        let a = Askpass::start("s3cret".into()).unwrap();
        let (port, token) = a.env.split_once(':').unwrap();
        let ask = |tok: &str, prompt: &str| {
            let mut c = TcpStream::connect(("127.0.0.1", port.parse::<u16>().unwrap())).unwrap();
            c.write_all(format!("{tok}\n{prompt}\n").as_bytes()).unwrap();
            c.shutdown(std::net::Shutdown::Write).unwrap();
            let mut out = String::new();
            c.read_to_string(&mut out).unwrap();
            out
        };
        assert_eq!(ask("wrong", "deploy@h's password:"), "");
        assert_eq!(ask(token, "Are you sure you want to continue connecting (yes/no)?"), "", "不替用户确认主机指纹");
        assert_eq!(ask(token, "deploy@h's password:"), "s3cret\n");
    }
}
