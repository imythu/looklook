//! SSH 主机管理：保存常用的服务器，一键在看看里打开 SSH 终端；允许的话，AI 助手也能经 MCP 在上面运行命令，
//! 但始终拿不到密码、私钥。
//!
//! - 主机列表存在本机数据库（`ssh_hosts`）；密码 / 私钥口令用 ChaCha20-Poly1305 加密后单独存（`ssh_secret:{id}`），
//!   密钥文件 `{LOOKLOOK_HOME}/ssh/secret.key`（0600）。私钥两种来源：这台电脑上的文件只记路径（一般是 `~/.ssh/id_ed25519`），
//!   不复制；从浏览器上传的私钥同样加密存进数据库（`ssh_key:{id}`），连接时写成 0600 的临时文件交给 ssh，用完即删；
//! - 跳板机就是另一台保存的主机（`jump` 记它的 id，可以层层嵌套）。ssh 的 ProxyCommand 是 `looklook ssh-proxy <跳板机> %h %p`：
//!   每一跳由看看自己按那台主机的登录方式连接（`ssh -W`），各自的密码、私钥互不相混；
//!   登录方式选“跳板机代登”（`jump`）时不走 ProxyCommand：先登录跳板机，在跳板机上运行 `ssh` 连这台，
//!   用的是跳板机上已经配置好的密钥（跳板机的 `~/.ssh`、ssh-agent），看看不为这台保存任何秘密；
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
const KEY_PREFIX: &str = "ssh_key:";
/// 上传的私钥最大字节数（RSA 8192 位的 OpenSSH 私钥也只有 6 KB 多）
const KEY_MAX: usize = 32 * 1024;
/// 跳板机最多嵌套几层（防止数据被改坏后无限套娃）
const JUMP_DEPTH_MAX: u32 = 5;
const JUMP_DEPTH_ENV: &str = "LOOKLOOK_SSH_DEPTH";
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
    /// `agent`：ssh 默认的密钥 / ssh-agent；`key`：指定私钥文件（口令可选，存为秘密）；`password`：密码（存为秘密）；
    /// `jump`：跳板机代登——先登录跳板机，再在跳板机上运行 `ssh` 连到这台，用的是跳板机上配置好的密钥（必须有跳板机）
    pub auth: String,
    #[serde(default)]
    pub key_path: String,
    /// `auth = key` 时私钥的来源：false = 这台电脑上的文件（`key_path`）；true = 上传的私钥，加密存在数据库里
    #[serde(default)]
    pub key_stored: bool,
    /// 跳板机：另一台已保存主机的 id；空 = 直接连接
    #[serde(default)]
    pub jump: String,
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
    pub key_stored: bool,
    /// 上传的私钥内容（`key_stored`）；None / 空 = 不改
    #[serde(default)]
    pub key_text: Option<String>,
    /// 已保存主机的 id 作为跳板机；空 = 直接连接
    #[serde(default)]
    pub jump: String,
    /// 新建主机时顺带新建跳板机：先保存它，再把这台主机的 `jump` 指向它（界面上会明确告诉用户）
    #[serde(default)]
    pub jump_new: Option<Box<HostInput>>,
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
    /// 已存了上传的私钥
    pub has_key: bool,
}

/// 保存的结果：顺带新建了跳板机时一并返回，界面据此提示。
#[derive(Debug, Serialize)]
pub struct Saved {
    #[serde(flatten)]
    pub host: SshHost,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_jump: Option<SshHost>,
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
    list(store).into_iter().map(|h| HostView { has_secret: has_secret(store, &h.id), has_key: has_key(store, &h.id), host: h }).collect()
}

fn has_secret(store: &Store, id: &str) -> bool {
    store.get::<String>(&format!("{SECRET_PREFIX}{id}")).ok().flatten().is_some()
}

fn has_key(store: &Store, id: &str) -> bool {
    store.get::<String>(&format!("{KEY_PREFIX}{id}")).ok().flatten().is_some()
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

fn stored_key(store: &Store, home: &Path, id: &str) -> Result<Option<String>> {
    match store.get::<String>(&format!("{KEY_PREFIX}{id}"))? {
        Some(b) => Ok(Some(decrypt(home, &b)?)),
        None => Ok(None),
    }
}

/// 上传的私钥：只收 OpenSSH / PEM 格式，统一成 LF 换行并以换行结尾（ssh 对此很挑剔）。
fn normalize_key(field: &str, text: &str) -> Result<String, LocalError> {
    let t = text.replace("\r\n", "\n").replace('\r', "\n");
    let t = t.trim();
    if t.contains("PuTTY-User-Key-File") {
        return Err(LocalError::invalid(field, "ppk"));
    }
    let first = t.lines().next().unwrap_or("");
    if t.len() > KEY_MAX || !first.starts_with("-----BEGIN ") || !first.contains("PRIVATE KEY") || !t.contains("-----END ") {
        return Err(LocalError::invalid(field, "private_key"));
    }
    Ok(format!("{t}\n"))
}

/// 这台电脑 `~/.ssh` 里的私钥文件（只看文件开头，不读内容），给界面快捷选择。
pub fn local_keys() -> Vec<String> {
    let Some(dir) = dirs::home_dir().map(|h| h.join(".ssh")) else { return Vec::new() };
    let Ok(rd) = std::fs::read_dir(&dir) else { return Vec::new() };
    let mut out: Vec<String> = rd
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.ends_with(".pub") || name.starts_with("known_hosts") || name == "config" || name == "authorized_keys" {
                return None;
            }
            let mut head = [0u8; 64];
            let n = std::fs::File::open(e.path()).and_then(|mut f| f.read(&mut head)).ok()?;
            let head = String::from_utf8_lossy(&head[..n]);
            (head.starts_with("-----BEGIN ") && head.contains("PRIVATE KEY")).then(|| format!("~/.ssh/{name}"))
        })
        .collect();
    out.sort();
    out
}

// ---------------- 私钥概况（脱敏） ----------------

/// 私钥 / 公钥脱敏后的样子：界面上让用户确认选对了钥匙，但看不到能拿去用的内容。
/// 正文只留开头 8 个和结尾 4 个字符，中间的用 `hidden` 个字符数代替。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Masked {
    /// 首行：`-----BEGIN … PRIVATE KEY-----`，公钥是算法名
    pub head: String,
    pub start: String,
    pub end: String,
    pub hidden: usize,
    /// 末行：`-----END … PRIVATE KEY-----`，公钥是注释
    pub foot: String,
}

/// 一把钥匙的概况。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct KeyInfo {
    /// 这台电脑上的文件路径（`~/` 开头的保持原样）；上传 / 粘贴的、ssh-agent 里的为空
    #[serde(skip_serializing_if = "String::is_empty")]
    pub path: String,
    /// `openssh` / `pem` / `pkcs8` / `agent`
    pub format: String,
    /// `ssh-ed25519`、`ssh-rsa`、`ecdsa-sha2-nistp256` …；读不出来时为空
    pub algo: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bits: Option<u32>,
    /// `SHA256:…`，和 `ssh-keygen -l` 一致；PEM 私钥旁边没有 .pub 时读不出来
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub comment: String,
    /// 有口令保护
    pub encrypted: bool,
    pub masked: Masked,
}

/// “系统密钥”登录时 ssh 会试的钥匙：ssh-agent 里的，以及 `~/.ssh` 里的默认私钥。
#[derive(Debug, Serialize)]
pub struct SystemKeys {
    /// None = 连不上 ssh-agent（没运行或没装 ssh-add）
    pub agent: Option<Vec<KeyInfo>>,
    pub files: Vec<KeyInfo>,
}

/// ssh 默认会试的私钥文件（`ssh_config` 的 IdentityFile 默认值，按 ssh 的顺序）
const DEFAULT_IDENTITIES: [&str; 6] = ["id_rsa", "id_ecdsa", "id_ecdsa_sk", "id_ed25519", "id_ed25519_sk", "id_dsa"];

fn mask(head: &str, body: &str, foot: &str) -> Masked {
    let chars: Vec<char> = body.chars().collect();
    let (s, e) = if chars.len() > 24 { (8, 4) } else { (0, 0) };
    Masked {
        head: head.to_string(),
        start: chars[..s].iter().collect(),
        end: chars[chars.len() - e..].iter().collect(),
        hidden: chars.len() - s - e,
        foot: foot.to_string(),
    }
}

/// SSH 线格式里的一段 `uint32 长度 + 内容`
fn take_str<'a>(b: &mut &'a [u8]) -> Option<&'a [u8]> {
    let n = u32::from_be_bytes(b.get(..4)?.try_into().ok()?) as usize;
    let s = b.get(4..4 + n)?;
    *b = &b[4 + n..];
    Some(s)
}

/// 公钥 blob 的算法、位数（RSA 按模数算，其他按算法名）和 SHA256 指纹。
fn blob_info(blob: &[u8]) -> Option<(String, Option<u32>, String)> {
    use sha2::Digest;
    let mut b = blob;
    let algo = String::from_utf8(take_str(&mut b)?.to_vec()).ok()?;
    let bits = match algo.as_str() {
        "ssh-rsa" => {
            take_str(&mut b)?;
            let n = take_str(&mut b)?;
            let n = &n[n.iter().position(|&x| x != 0).unwrap_or(n.len())..];
            n.first().map(|&x| (n.len() as u32 - 1) * 8 + (8 - x.leading_zeros()))
        }
        a if a.contains("nistp256") => Some(256),
        a if a.contains("nistp384") => Some(384),
        a if a.contains("nistp521") => Some(521),
        _ => None,
    };
    let fp = format!("SHA256:{}", base64::engine::general_purpose::STANDARD_NO_PAD.encode(sha2::Sha256::digest(blob)));
    Some((algo, bits, fp))
}

/// 一行公钥（`算法 base64 注释`，authorized_keys / .pub / `ssh-add -L` 的格式）。
fn pub_info(line: &str) -> Option<KeyInfo> {
    let mut parts = line.trim().splitn(3, char::is_whitespace);
    let (algo, b64) = (parts.next()?, parts.next()?);
    let comment = parts.next().unwrap_or("").trim().to_string();
    let blob = base64::engine::general_purpose::STANDARD.decode(b64).ok()?;
    let (algo2, bits, fp) = blob_info(&blob)?;
    (algo2 == algo).then(|| KeyInfo {
        path: String::new(),
        format: "agent".into(),
        algo: algo2,
        bits,
        fingerprint: Some(fp),
        masked: mask(algo, b64, &comment),
        comment,
        encrypted: false,
    })
}

/// 私钥内容的概况。OpenSSH 格式的公钥部分不加密，即使有口令也能读出算法和指纹；PEM 只看得出算法。
pub fn key_info(text: &str) -> Result<KeyInfo, LocalError> {
    let t = normalize_key("key_text", text)?;
    let lines: Vec<&str> = t.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    let head = lines[0];
    let foot = lines.iter().rev().find(|l| l.starts_with("-----END ")).copied().unwrap_or("");
    // PEM 加密私钥的 `Proc-Type:`、`DEK-Info:` 头不算正文
    let body: String = lines[1..].iter().take_while(|l| !l.starts_with("-----END ")).filter(|l| !l.contains(':')).copied().collect();
    let label = head.trim_start_matches("-----BEGIN ").trim_end_matches('-').trim();
    let mut info = KeyInfo {
        path: String::new(),
        format: "pem".into(),
        algo: String::new(),
        bits: None,
        fingerprint: None,
        comment: String::new(),
        encrypted: t.contains("Proc-Type: 4,ENCRYPTED"),
        masked: mask(head, &body, foot),
    };
    match label {
        "OPENSSH PRIVATE KEY" => {
            info.format = "openssh".into();
            let raw = base64::engine::general_purpose::STANDARD.decode(&body).map_err(|_| LocalError::invalid("key_text", "private_key"))?;
            let parsed = (|| {
                let mut b = raw.strip_prefix(b"openssh-key-v1\0")?;
                let cipher = take_str(&mut b)?;
                take_str(&mut b)?; // kdf
                take_str(&mut b)?; // kdf 参数
                b = b.get(4..)?; // 钥匙个数
                Some((cipher != b"none", blob_info(take_str(&mut b)?)?))
            })();
            let (encrypted, (algo, bits, fp)) = parsed.ok_or_else(|| LocalError::invalid("key_text", "private_key"))?;
            info.encrypted = encrypted;
            info.algo = algo;
            info.bits = bits;
            info.fingerprint = Some(fp);
        }
        "RSA PRIVATE KEY" => info.algo = "ssh-rsa".into(),
        "EC PRIVATE KEY" => info.algo = "ecdsa".into(),
        "DSA PRIVATE KEY" => info.algo = "ssh-dss".into(),
        "ENCRYPTED PRIVATE KEY" => {
            info.format = "pkcs8".into();
            info.encrypted = true;
        }
        _ => info.format = "pkcs8".into(),
    }
    Ok(info)
}

/// 这台电脑上的私钥文件的概况：旁边有 `.pub` 的话，用它补上注释和（PEM 私钥的）指纹。
pub fn key_info_at(path: &str) -> Result<KeyInfo, LocalError> {
    let p = path.trim();
    if p.is_empty() {
        return Err(LocalError::invalid("key_path", "required"));
    }
    let full = PathBuf::from(expand_home(p));
    if !full.is_absolute() {
        return Err(LocalError::invalid("key_path", "absolute"));
    }
    let meta = std::fs::metadata(&full).map_err(|e| match e.kind() {
        std::io::ErrorKind::PermissionDenied => LocalError::invalid("key_path", "denied"),
        _ => LocalError::invalid("key_path", "missing"),
    })?;
    if !meta.is_file() || meta.len() > KEY_MAX as u64 {
        return Err(LocalError::invalid("key_path", "private_key"));
    }
    let bytes = std::fs::read(&full).map_err(|_| LocalError::invalid("key_path", "denied"))?;
    let text = String::from_utf8(bytes).map_err(|_| LocalError::invalid("key_path", "private_key"))?;
    let mut info = key_info(&text).map_err(|mut e| {
        e.params.insert("field".into(), "key_path".into());
        e
    })?;
    info.path = p.to_string();
    let mut pub_path = full.into_os_string();
    pub_path.push(".pub");
    if let Some(pk) = std::fs::read_to_string(pub_path).ok().and_then(|s| pub_info(s.lines().next().unwrap_or(""))) {
        if info.fingerprint.is_none() || info.fingerprint == pk.fingerprint {
            info.comment = pk.comment;
            info.fingerprint = pk.fingerprint;
            info.algo = pk.algo;
            info.bits = pk.bits;
        }
    }
    Ok(info)
}

/// 已保存主机上传的私钥的概况（编辑主机时显示）。
pub fn stored_key_info(store: &Store, home: &Path, id: &str) -> Result<KeyInfo, LocalError> {
    let text = stored_key(store, home, id)?.ok_or_else(LocalError::not_found)?;
    key_info(&text)
}

/// 跑一个很快就结束的命令，最多等 `limit`；超时就杀掉。返回退出码和标准输出。
fn run_quick(cmd: &mut Command, limit: Duration) -> Option<(i32, String)> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let mut child = cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().ok()?;
    // 输出放到线程里读，免得管道写满卡住子进程
    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut out = String::new();
        stdout.read_to_string(&mut out).map(|_| out)
    });
    let deadline = Instant::now() + limit;
    let code = loop {
        match child.try_wait().ok()? {
            Some(st) => break st.code().unwrap_or(-1),
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    };
    Some((code, reader.join().ok()?.ok()?))
}

/// “系统密钥”登录时 ssh 会用到的钥匙（阻塞调用）。`ssh-add -L` 退出码 1 = agent 里没有钥匙，2 = 连不上 agent。
pub fn system_keys() -> SystemKeys {
    let agent = crate::shells::which("ssh-add").and_then(|bin| match run_quick(Command::new(bin).arg("-L"), Duration::from_secs(3)) {
        Some((0, out)) => Some(out.lines().filter_map(pub_info).collect()),
        Some((1, _)) => Some(Vec::new()),
        _ => None,
    });
    let files = DEFAULT_IDENTITIES.iter().filter_map(|n| key_info_at(&format!("~/.ssh/{n}")).ok()).collect();
    SystemKeys { agent, files }
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
        "agent" | "password" | "jump" => {}
        "key" if i.key_stored => {
            if let Some(k) = i.key_text.as_deref().filter(|k| !k.trim().is_empty()) {
                normalize_key("key_text", k)?;
            }
        }
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

/// 跳板机 `jump` 能不能给主机 `id`（新建时为 None）用：要存在，而且顺着跳板机往上走不能绕回自己。
fn check_jump(hosts: &[SshHost], id: Option<&str>, jump: &str) -> Result<(), LocalError> {
    if jump.is_empty() {
        return Ok(());
    }
    let mut cur = jump;
    for _ in 0..=JUMP_DEPTH_MAX {
        if Some(cur) == id {
            return Err(LocalError::invalid("jump", "jump_loop"));
        }
        let h = hosts.iter().find(|h| h.id == cur).ok_or_else(|| LocalError::invalid("jump", "jump_missing"))?;
        if h.jump.is_empty() {
            return Ok(());
        }
        cur = &h.jump;
    }
    Err(LocalError::invalid("jump", "jump_depth"))
}

/// 把跳板机表单里的校验错误标成“跳板机的某某”，界面才知道是哪一块填错了。
fn as_jump_error(mut e: LocalError) -> LocalError {
    if let Some(serde_json::Value::String(f)) = e.params.get("field").cloned() {
        e.params.insert("field".into(), serde_json::Value::String(format!("jump_{f}")));
    }
    e
}

/// 新建（`id` 为 None）或修改主机。新建时可以顺带新建跳板机（`jump_new`）：两台都校验通过才保存，
/// 先存跳板机再存这台；这台没存成就把刚建的跳板机删掉，不留半截。
pub fn save(store: &Store, home: &Path, id: Option<&str>, mut i: HostInput) -> Result<Saved, LocalError> {
    let Some(j) = i.jump_new.take() else {
        return save_one(store, home, id, i).map(|host| Saved { host, created_jump: None });
    };
    let mut j = *j;
    j.jump_new = None;
    validate(&i)?;
    validate(&j).map_err(as_jump_error)?;
    if j.name.trim().eq_ignore_ascii_case(i.name.trim()) {
        return Err(LocalError::new("SSH_NAME_TAKEN").with("name", j.name.trim()));
    }
    let jump = save_one(store, home, None, j).map_err(as_jump_error)?;
    i.jump = jump.id.clone();
    match save_one(store, home, id, i) {
        Ok(host) => Ok(Saved { host, created_jump: Some(jump) }),
        Err(e) => {
            let _ = delete(store, &jump.id);
            Err(e)
        }
    }
}

fn save_one(store: &Store, home: &Path, id: Option<&str>, i: HostInput) -> Result<SshHost, LocalError> {
    validate(&i)?;
    let mut hosts = list(store);
    let name = i.name.trim().to_string();
    if hosts.iter().any(|h| Some(h.id.as_str()) != id && h.name.eq_ignore_ascii_case(&name)) {
        return Err(LocalError::new("SSH_NAME_TAKEN").with("name", name));
    }
    let jump = i.jump.trim().to_string();
    check_jump(&hosts, id, &jump)?;
    if i.auth == "jump" && jump.is_empty() {
        return Err(LocalError::invalid("auth", "jump_required"));
    }
    let key_stored = i.auth == "key" && i.key_stored;
    let key_text = match i.key_text.as_deref().filter(|k| !k.trim().is_empty()) {
        Some(k) if key_stored => Some(normalize_key("key_text", k)?),
        _ => None,
    };
    // 选了“上传私钥”却既没传、也没存过
    if key_stored && key_text.is_none() && !id.is_some_and(|id| has_key(store, id)) {
        return Err(LocalError::invalid("key_text", "required"));
    }
    let key_path = if i.auth == "key" && !key_stored { i.key_path.trim().to_string() } else { String::new() };
    let host = SshHost {
        id: id.map(str::to_string).unwrap_or_else(|| crate::util::random_id(8)),
        name,
        host: i.host.trim().trim_start_matches('[').trim_end_matches(']').to_string(),
        port: i.port,
        user: i.user.trim().to_string(),
        auth: i.auth.clone(),
        key_path,
        key_stored,
        jump,
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
    let no_secret = matches!(i.auth.as_str(), "agent" | "jump");
    if no_secret || i.clear_secret {
        store.remove(&skey)?;
    }
    if let Some(s) = i.secret.as_deref().filter(|s| !s.is_empty()) {
        if !no_secret {
            store.set(&skey, &encrypt(home, s)?)?;
        }
    }
    let kkey = format!("{KEY_PREFIX}{}", host.id);
    match key_text {
        Some(k) => store.set(&kkey, &encrypt(home, &k)?)?,
        None if !key_stored => store.remove(&kkey)?,
        None => {}
    }
    store.set(HOSTS_KEY, &hosts)?;
    Ok(host)
}

pub fn delete(store: &Store, id: &str) -> Result<(), LocalError> {
    let mut hosts = list(store);
    // 还有主机拿它当跳板机：先改那些主机，不然它们就连不上了
    let users: Vec<&str> = hosts.iter().filter(|h| h.jump == id).map(|h| h.name.as_str()).collect();
    if !users.is_empty() {
        return Err(LocalError::new("SSH_JUMP_IN_USE").with("names", users.join("、")));
    }
    let before = hosts.len();
    hosts.retain(|h| h.id != id);
    if hosts.len() == before {
        return Err(LocalError::not_found());
    }
    store.set(HOSTS_KEY, &hosts)?;
    store.remove(&format!("{SECRET_PREFIX}{id}"))?;
    store.remove(&format!("{KEY_PREFIX}{id}"))?;
    Ok(())
}

// ---------------- 连接 ----------------

fn ssh_bin() -> String {
    crate::shells::which("ssh").unwrap_or_else(|| "ssh".into())
}

/// 一台主机的 ssh 参数之外，连接时才准备好的东西。
#[derive(Default)]
pub struct Route {
    /// 上传的私钥写成的临时文件
    pub key_file: Option<String>,
    /// 经跳板机时的 ProxyCommand
    pub proxy: Option<String>,
    /// 作为别的主机的跳板：`-W 主机:端口`，把连接转给下一跳
    pub forward: Option<String>,
    /// 带着远程命令也要分配终端（`-t`）：跳板机代登时，交互式连接要在跳板机上再跑一个交互的 ssh
    pub tty: bool,
}

/// ssh 的参数（不含远程命令）。
pub fn ssh_args(h: &SshHost, batch: bool, route: &Route) -> Vec<String> {
    let mut a: Vec<String> = vec!["-p".into(), h.port.to_string(), "-o".into(), "StrictHostKeyChecking=accept-new".into(), "-o".into(), "ServerAliveInterval=30".into()];
    match h.auth.as_str() {
        "key" => {
            let key = route.key_file.clone().unwrap_or_else(|| expand_home(&h.key_path));
            a.extend(["-i".into(), key, "-o".into(), "IdentitiesOnly=yes".into()]);
        }
        "password" => a.extend(["-o".into(), "PreferredAuthentications=password,keyboard-interactive".into(), "-o".into(), "PubkeyAuthentication=no".into()]),
        _ => {}
    }
    if let Some(p) = &route.proxy {
        a.extend(["-o".into(), format!("ProxyCommand={p}")]);
    }
    if let Some(f) = &route.forward {
        a.extend(["-W".into(), f.clone(), "-o".into(), "ConnectTimeout=15".into()]);
    } else if batch {
        a.extend(["-T".into(), "-o".into(), "ConnectTimeout=15".into(), "-o".into(), "NumberOfPasswordPrompts=1".into()]);
    } else if route.tty {
        a.push("-t".into());
    }
    a.push("-l".into());
    a.push(h.user.clone());
    a.push("--".into());
    a.push(h.host.clone());
    a
}

/// ProxyCommand：`<看看程序> ssh-proxy <跳板机 id> %h %p`。ssh 用 shell 运行它（Windows 上直接拆参数），
/// 程序路径要加引号；`%` 是 ssh 的占位符前缀，路径里的要写成 `%%`。
fn proxy_command(exe: &Path, jump: &str) -> String {
    let exe = exe.display().to_string().replace('%', "%%");
    let quoted = if cfg!(windows) { format!("\"{exe}\"") } else { format!("'{}'", exe.replace('\'', "'\\''")) };
    format!("{quoted} ssh-proxy {jump} %h %p")
}

/// 上传的私钥：连接时写成只有自己能读的临时文件。ssh 只在登录时读它，两分钟后（或连接结束时）删掉；
/// 程序被强行结束留下的旧文件，下次连接时清理。
struct TempKey {
    path: PathBuf,
}

impl TempKey {
    fn write(store: &Store, home: &Path, id: &str) -> Result<Self> {
        let text = stored_key(store, home, id)?.context("上传的私钥不见了，请在看看的“SSH 主机”里重新上传")?;
        let dir = home.join("ssh").join("keys");
        std::fs::create_dir_all(&dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
        }
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for e in rd.flatten() {
                let old = e.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|d| d > Duration::from_secs(600));
                if old {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
        let path = dir.join(format!("{id}-{}", crate::util::random_id(12)));
        crate::util::write_private(&path, text.as_bytes())?;
        let p = path.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(120));
            let _ = std::fs::remove_file(p);
        });
        Ok(Self { path })
    }
}

impl Drop for TempKey {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
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

/// 准备好的 ssh 命令（需要密码时带着 askpass 服务、上传的私钥带着临时文件，命令结束前要一直持有）。
pub struct Prepared {
    pub cmd: Command,
    _askpass: Option<Askpass>,
    _key: Option<TempKey>,
    pub secret: Option<String>,
}

pub fn prepare(store: &Store, home: &Path, h: &SshHost, remote: &[String], batch: bool) -> Result<Prepared> {
    prepare_hop(store, home, h, remote, batch, None)
}

/// 跳板机代登时在跳板机上运行的命令：`ssh … -l 用户 -- 主机 [远程命令…]`，每个参数都按 POSIX shell 加引号
/// （跳板机上的登录 shell 会再解析一遍；远程命令各参数原样传到 ssh，由它照常拼接）。
fn hop_command(h: &SshHost, remote: &[String], batch: bool, forward: Option<&str>) -> String {
    let mut a: Vec<String> = vec!["ssh".into(), "-p".into(), h.port.to_string(), "-o".into(), "StrictHostKeyChecking=accept-new".into(), "-o".into(), "ServerAliveInterval=30".into()];
    if let Some(f) = forward {
        a.extend(["-W".into(), f.to_string(), "-o".into(), "ConnectTimeout=15".into(), "-o".into(), "BatchMode=yes".into()]);
    } else if batch {
        // 跳板机上没有人能回答提问：要密码就直接失败
        a.extend(["-T".into(), "-o".into(), "ConnectTimeout=15".into(), "-o".into(), "BatchMode=yes".into()]);
    } else {
        a.push("-t".into());
    }
    a.extend(["-l".into(), h.user.clone(), "--".into(), h.host.clone()]);
    a.extend(remote.iter().cloned());
    let quote = |s: &String| if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "-_./=:@,%+".contains(c)) { s.clone() } else { format!("'{}'", s.replace('\'', "'\\''")) };
    a.iter().map(quote).collect::<Vec<_>>().join(" ")
}

fn prepare_hop(store: &Store, home: &Path, h: &SshHost, remote: &[String], batch: bool, forward: Option<String>) -> Result<Prepared> {
    let depth: u32 = std::env::var(JUMP_DEPTH_ENV).ok().and_then(|v| v.parse().ok()).unwrap_or(0);
    prepare_at(store, home, h, remote, batch, forward, false, depth)
}

/// tty：带着远程命令也要 `-t`（跳板机代登的交互连接）；depth：已经套了几层跳板机。
#[allow(clippy::too_many_arguments)]
fn prepare_at(store: &Store, home: &Path, h: &SshHost, remote: &[String], batch: bool, forward: Option<String>, tty: bool, depth: u32) -> Result<Prepared> {
    if h.auth == "jump" {
        // 跳板机代登：连接跳板机本身（按它自己的登录方式和跳板机），远程命令是在跳板机上 ssh 到这台
        if depth >= JUMP_DEPTH_MAX {
            bail!("跳板机嵌套太多层了（最多 {JUMP_DEPTH_MAX} 层）");
        }
        let jump = list(store).into_iter().find(|j| j.id == h.jump).with_context(|| format!("“{}”的跳板机已被删除，请在看看里重新选择", h.name))?;
        let inner = hop_command(h, remote, batch, forward.as_deref());
        return prepare_at(store, home, &jump, &[inner], batch, None, forward.is_none(), depth + 1);
    }
    let mut cmd = Command::new(ssh_bin());
    let key = if h.auth == "key" && h.key_stored { Some(TempKey::write(store, home, &h.id)?) } else { None };
    let proxy = if h.jump.is_empty() {
        None
    } else {
        if depth >= JUMP_DEPTH_MAX {
            bail!("跳板机嵌套太多层了（最多 {JUMP_DEPTH_MAX} 层）");
        }
        let jump = list(store).into_iter().find(|j| j.id == h.jump).with_context(|| format!("“{}”的跳板机已被删除，请在看看里重新选择", h.name))?;
        let exe = std::env::current_exe().context("找不到看看程序的路径")?;
        // 下一跳由 `looklook ssh-proxy` 连接：让它找到同一个数据目录，并记着已经套了几层
        cmd.env("LOOKLOOK_HOME", home).env(JUMP_DEPTH_ENV, (depth + 1).to_string());
        Some(proxy_command(&exe, &jump.id))
    };
    let route = Route { key_file: key.as_ref().map(|k| k.path.display().to_string()), proxy, forward, tty };
    cmd.args(ssh_args(h, batch, &route));
    cmd.args(remote);
    let secret = if matches!(h.auth.as_str(), "agent" | "jump") { None } else { secret(store, home, &h.id)? };
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
        None => {
            // 作为跳板时环境里还带着上一跳的 askpass：这一跳没有存密码，不能把上一跳的密码交出去，让 ssh 照常在终端里问
            if std::env::var_os(ASKPASS_ENV).is_some() {
                cmd.env_remove(ASKPASS_ENV).env_remove("SSH_ASKPASS").env_remove("SSH_ASKPASS_REQUIRE");
            }
            None
        }
    };
    Ok(Prepared { cmd, _askpass: askpass, _key: key, secret })
}

/// 等 ssh 结束期间收到 SIGHUP / SIGTERM 时不立刻退出：外层 ssh 结束时会给 ProxyCommand 发 SIGHUP，
/// 终端标签页关掉时 shell 里的 `looklook ssh` 也会收到 SIGHUP。直接退出的话，上传私钥的临时文件来不及删。
/// 装一个什么也不做的处理函数（exec 后自动恢复默认，ssh 子进程不受影响）：ssh 随后自己结束（输入断了 / 它也收到了信号），
/// 等待返回后照常清理。
fn outlive_hangup() {
    #[cfg(unix)]
    {
        extern "C" fn ignore(_: libc::c_int) {}
        let handler = ignore as extern "C" fn(libc::c_int) as libc::sighandler_t;
        // SAFETY: 只登记一个不做任何事的处理函数
        unsafe {
            libc::signal(libc::SIGHUP, handler);
            libc::signal(libc::SIGTERM, handler);
        }
    }
}

/// `looklook ssh-proxy <跳板机> <主机> <端口>`：ssh 的 ProxyCommand。用跳板机自己的登录方式连上它，
/// 把标准输入输出转到下一跳（`ssh -W`）。这里不能往标准输出打印任何东西，那是 ssh 的数据通道。
pub fn proxy_cli(store: &Store, home: &Path, jump: &str, host: &str, port: u16) -> Result<i32> {
    let h = list(store).into_iter().find(|h| h.id == jump).with_context(|| format!("没有 id 为 {jump:?} 的 SSH 主机"))?;
    if host.is_empty() || host.starts_with('-') || !host.chars().all(|c| c.is_ascii_alphanumeric() || ".-:_%".contains(c)) {
        bail!("主机地址不正确：{host:?}");
    }
    let target = if host.contains(':') { format!("[{host}]:{port}") } else { format!("{host}:{port}") };
    let mut p = prepare_hop(store, home, &h, &[], false, Some(target))?;
    outlive_hangup();
    let status = p.cmd.status().context("运行 ssh 失败：这台电脑上没有 OpenSSH 客户端？")?;
    Ok(status.code().unwrap_or(255))
}

/// `looklook ssh <主机> [-- 远程命令…]`：交互式连接（看看的 SSH 终端就是在 shell 里运行这个）。
pub fn connect_cli(store: &Store, home: &Path, key: &str, remote: &[String]) -> Result<i32> {
    let h = find(store, key).with_context(|| format!("没有叫 {key:?} 的 SSH 主机（在看看的“SSH 主机”页添加）"))?;
    let via = list(store).into_iter().find(|j| j.id == h.jump).map(|j| format!(" · 经 {}", j.name)).unwrap_or_default();
    eprintln!("\x1b[2m看看 · SSH {} ({}@{}:{}){via}\x1b[0m", h.name, h.user, h.host, h.port);
    let mut p = prepare(store, home, &h, remote, false)?;
    if p._key.is_some() || !h.jump.is_empty() {
        outlive_hangup();
    }
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
        HostInput {
            name: name.into(),
            host: "example.com".into(),
            port: 22,
            user: "deploy".into(),
            auth: "password".into(),
            key_path: String::new(),
            key_stored: false,
            key_text: None,
            jump: String::new(),
            jump_new: None,
            allow_ai: true,
            secret: Some("hunter2!pw".into()),
            clear_secret: false,
        }
    }

    #[test]
    fn hosts_and_secrets_round_trip_encrypted() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::memory().unwrap();
        let h = save(&store, tmp.path(), None, input("prod")).unwrap().host;
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
    fn uploaded_keys_are_encrypted_and_written_only_while_connecting() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::memory().unwrap();
        let (kind, tail) = ("OPENSSH", "PRIVATE KEY-----");
        let pem = format!("-----BEGIN {kind} {tail}\r\nb3BlbnNzaC1rZXk\r\n-----END {kind} {tail}");
        let mut i = input("k");
        i.auth = "key".into();
        i.key_stored = true;
        i.secret = None;
        // 选了上传却没给私钥
        assert_eq!(save(&store, tmp.path(), None, i.clone()).unwrap_err().params["field"], "key_text");
        i.key_text = Some("PuTTY-User-Key-File-3: ssh-ed25519".into());
        assert_eq!(save(&store, tmp.path(), None, i.clone()).unwrap_err().params["rule"], "ppk");
        i.key_text = Some(pem.clone());
        let h = save(&store, tmp.path(), None, i.clone()).unwrap().host;
        assert!(h.key_stored && h.key_path.is_empty() && views(&store)[0].has_key);
        let raw: String = store.get(&format!("{KEY_PREFIX}{}", h.id)).unwrap().unwrap();
        assert!(!raw.contains("b3BlbnNzaC1rZXk"), "存的是密文");
        // 修改时不重新上传：保留
        i.key_text = None;
        save(&store, tmp.path(), Some(&h.id), i.clone()).unwrap();
        let p = prepare(&store, tmp.path(), &h, &[], true).unwrap();
        let args: Vec<String> = p.cmd.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        let file = &args[args.iter().position(|a| a == "-i").unwrap() + 1];
        let text = std::fs::read_to_string(file).unwrap();
        assert!(text.ends_with("-----\n") && !text.contains('\r'), "{text:?}");
        drop(p);
        assert!(!std::path::Path::new(file).exists(), "连接结束就删掉");
        // 改回这台电脑上的文件：上传的私钥删掉
        i.key_stored = false;
        i.key_path = "~/.ssh/id_ed25519".into();
        save(&store, tmp.path(), Some(&h.id), i.clone()).unwrap();
        assert!(!views(&store)[0].has_key);
    }

    #[test]
    fn jump_hosts_chain_through_looklook_and_cannot_loop() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::memory().unwrap();
        // 新建主机时一并新建跳板机
        let mut i = input("db");
        let mut j = input("bastion");
        j.auth = "agent".into();
        j.secret = None;
        i.jump_new = Some(Box::new(j.clone()));
        let saved = save(&store, tmp.path(), None, i.clone()).unwrap();
        let bastion = saved.created_jump.expect("跳板机也建好了");
        assert_eq!(saved.host.jump, bastion.id);
        assert_eq!(list(&store).len(), 2);
        // 跳板机名字和这台重了：两台都不建
        let mut dup = input("x");
        dup.jump_new = Some(Box::new(input("x")));
        assert_eq!(save(&store, tmp.path(), None, dup).unwrap_err().code, "SSH_NAME_TAKEN");
        // 跳板机表单填错：标成跳板机的字段
        let mut bad = input("y");
        let mut bj = input("z");
        bj.host = "-oProxyCommand=sh".into();
        bad.jump_new = Some(Box::new(bj));
        assert_eq!(save(&store, tmp.path(), None, bad).unwrap_err().params["field"], "jump_host");
        assert_eq!(list(&store).len(), 2);
        // 不能绕回自己
        let mut b = j.clone();
        b.jump = saved.host.id.clone();
        assert_eq!(save(&store, tmp.path(), Some(&bastion.id), b).unwrap_err().params["rule"], "jump_loop");
        // 还被用着的跳板机不能删
        assert_eq!(delete(&store, &bastion.id).unwrap_err().code, "SSH_JUMP_IN_USE");
        // 经跳板机：ProxyCommand 交给看看自己
        let p = prepare(&store, tmp.path(), &saved.host, &[], true).unwrap();
        let args: Vec<String> = p.cmd.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        let proxy = args.iter().find(|a| a.starts_with("ProxyCommand=")).unwrap();
        assert!(proxy.ends_with(&format!(" ssh-proxy {} %h %p", bastion.id)), "{proxy}");
        assert_eq!(&args[args.len() - 2..], &["--".to_string(), "example.com".to_string()]);
    }

    #[test]
    fn jump_auth_runs_ssh_on_the_jump_host_and_stores_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::memory().unwrap();
        let mut j = input("bastion");
        j.auth = "agent".into();
        j.secret = None;
        let bastion = save(&store, tmp.path(), None, j).unwrap().host;
        // 跳板机代登必须有跳板机
        let mut i = input("inner");
        i.auth = "jump".into();
        assert_eq!(save(&store, tmp.path(), None, i.clone()).unwrap_err().params["rule"], "jump_required");
        i.jump = bastion.id.clone();
        i.user = "ad$min".into();
        i.host = "10.0.0.5".into();
        i.port = 2222;
        let h = save(&store, tmp.path(), None, i).unwrap().host;
        assert!(!views(&store).iter().any(|v| v.host.id == h.id && v.has_secret), "不为这台保存密码");
        let args = |p: Prepared| p.cmd.get_args().map(|a| a.to_string_lossy().into_owned()).collect::<Vec<_>>();
        // 交互：连跳板机（-t），远程命令是在跳板机上 ssh 到这台
        let a = args(prepare(&store, tmp.path(), &h, &[], false).unwrap());
        assert!(a.contains(&"-t".to_string()) && !a.iter().any(|x| x.starts_with("ProxyCommand=")), "{a:?}");
        let n = a.len();
        assert_eq!(&a[n - 3..n - 1], &["--".to_string(), "example.com".to_string()]);
        assert_eq!(a[n - 1], "ssh -p 2222 -o StrictHostKeyChecking=accept-new -o ServerAliveInterval=30 -t -l 'ad$min' -- 10.0.0.5");
        // MCP / 测试连接：不分配终端，跳板机上的 ssh 不许提问；远程命令各参数加引号原样传过去
        let a = args(prepare(&store, tmp.path(), &h, &["echo hi; uname".into()], true).unwrap());
        assert!(a.contains(&"-T".to_string()) && !a.contains(&"-t".to_string()));
        assert!(a.last().unwrap().ends_with("-T -o ConnectTimeout=15 -o BatchMode=yes -l 'ad$min' -- 10.0.0.5 'echo hi; uname'"), "{a:?}");
        // 它再给别的主机当跳板机：跳板机上 ssh -W
        let a = args(prepare_hop(&store, tmp.path(), &h, &[], false, Some("db:5432".into())).unwrap());
        assert!(!a.contains(&"-t".to_string()) && a.last().unwrap().contains(" -W db:5432 "), "{a:?}");
    }

    #[test]
    fn key_info_masks_body_and_matches_ssh_keygen_fingerprint() {
        let dir = tempfile::tempdir().unwrap();
        for (algo, pass, want) in [("ed25519", "", "ssh-ed25519"), ("rsa", "pw-123456", "ssh-rsa"), ("ecdsa", "", "ecdsa-sha2-nistp256")] {
            let path = dir.path().join(format!("id_{algo}"));
            let Ok(st) = Command::new("ssh-keygen").args(["-q", "-t", algo, "-N", pass, "-C", "me@box", "-f"]).arg(&path).status() else { return };
            assert!(st.success());
            let fp = String::from_utf8(Command::new("ssh-keygen").arg("-lf").arg(&path).output().unwrap().stdout).unwrap();
            let text = std::fs::read_to_string(&path).unwrap();
            let i = key_info_at(path.to_str().unwrap()).unwrap();
            assert_eq!((i.format.as_str(), i.algo.as_str(), i.encrypted, i.comment.as_str()), ("openssh", want, !pass.is_empty(), "me@box"));
            assert!(fp.contains(i.fingerprint.as_deref().unwrap()), "{fp} / {:?}", i.fingerprint);
            if algo == "rsa" {
                assert_eq!(i.bits, Some(3072));
            }
            let m = &i.masked;
            assert_eq!((m.start.len(), m.end.len()), (8, 4));
            assert!(m.head.starts_with("-----BEGIN OPENSSH") && m.foot.starts_with("-----END OPENSSH"));
            let body: String = text.lines().filter(|l| !l.starts_with("-----")).collect();
            assert_eq!(m.hidden + 12, body.len());
            // 粘贴的内容同样能读，只是没有 .pub 补注释
            assert_eq!(key_info(&text).unwrap().fingerprint, i.fingerprint);
        }
        assert_eq!(key_info_at(dir.path().join("nope").to_str().unwrap()).unwrap_err().params["rule"], "missing");
        assert_eq!(key_info("hello").unwrap_err().params["rule"], "private_key");
    }

    #[test]
    fn pem_keys_take_fingerprint_from_pub_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy");
        let Ok(st) = Command::new("ssh-keygen").args(["-q", "-t", "rsa", "-b", "2048", "-m", "PEM", "-N", "", "-C", "old", "-f"]).arg(&path).status() else { return };
        assert!(st.success());
        let i = key_info_at(path.to_str().unwrap()).unwrap();
        assert_eq!((i.format.as_str(), i.algo.as_str(), i.bits, i.comment.as_str()), ("pem", "ssh-rsa", Some(2048), "old"));
        assert!(i.fingerprint.unwrap().starts_with("SHA256:"));
        std::fs::remove_file(dir.path().join("legacy.pub")).unwrap();
        assert!(key_info_at(path.to_str().unwrap()).unwrap().fingerprint.is_none());
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
        let h = SshHost { id: "a".into(), name: "n".into(), host: "h".into(), port: 22, user: "u".into(), auth: "key".into(), key_path: "~/.ssh/id_ed25519".into(), key_stored: false, jump: String::new(), allow_ai: false };
        let a = ssh_args(&h, true, &Route::default());
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
        let h = save(&store, tmp.path(), None, i).unwrap().host;
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
