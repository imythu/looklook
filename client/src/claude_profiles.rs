//! Claude Code 账号切换：本机默认账号、另外登录的官方账号、第三方接口（Anthropic 官方格式），选一个当默认，
//! 新建的会话用默认的，也可以在新建时另选。会话用哪个在新建时定下（kv `instance_claude_profile.{实例ID}`），
//! 之后切换默认不影响已有的会话（升级前就有的会话在升级时记下当时的默认）。
//!
//! 按 Anthropic 的规则来（docs/CLAUDE_ACCOUNTS.md）：
//! - **官方账号**：每个账号一个 `CLAUDE_CONFIG_DIR`（官方文档“Log in with multiple accounts”的做法）。
//!   登录、退出都由 Claude Code 自己完成（`claude auth login` / `claude auth logout`），看看不读、不存、不转发
//!   claude.ai 的令牌；账号状态只看 `claude auth status` 的输出（邮箱、套餐）。
//!   本机默认账号就是 `~/.claude`（或用户自己设的 `CLAUDE_CONFIG_DIR`），启动时什么都不加。
//!   另外的账号在 `<数据目录>/claude-accounts/<ID>/`：设置、CLAUDE.md、技能、插件、对话记录等用软链接共用默认目录的
//!   （见 [`SHARED`]），只有登录状态和 `.claude.json` 是各自的。
//! - **第三方接口**：用户自己的 Key，`ANTHROPIC_BASE_URL` + `ANTHROPIC_AUTH_TOKEN`（见 model.rs），
//!   可选指定模型（`ANTHROPIC_MODEL`、`ANTHROPIC_DEFAULT_{OPUS,SONNET,HAIKU}_MODEL`）。只支持官方格式的接口，不做格式转换。
//!   Key 只写在本机文件 `<数据目录>/model/claude-<ID>.key`，启动命令让 shell 自己读。

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::error::LocalError;
use crate::model::LaunchEnv;
use crate::store::Store;

/// 本机默认账号（`~/.claude`）的 ID：一直在，不能删
pub const OFFICIAL: &str = "official";
const LIST_KEY: &str = "claude_profiles";
const DEFAULT_KEY: &str = "claude_profiles.default";
const NAME_MAX: usize = 40;
const MAX_PROFILES: usize = 30;

/// 另外的官方账号和默认目录共用的东西（默认目录里有的才链）。`.credentials.json`、`.claude.json` 不共用。
const SHARED: &[&str] = &["settings.json", "CLAUDE.md", "keybindings.json", "agents", "commands", "skills", "plugins", "output-styles", "hooks", "projects"];
/// 默认目录里没有时也先建好再链的：对话记录要在同一个地方，看看的对话界面、历史、接着以前的对话都靠它
const SHARED_CREATE: &[&str] = &["projects"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Official,
    Api,
}

/// 第三方接口指定的模型；都不填时用 Claude Code 自己的默认（接口那边映射）
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Models {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub main: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opus: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sonnet: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub haiku: Option<String>,
}

impl Models {
    fn vars(&self) -> [(&'static str, &Option<String>); 4] {
        [("ANTHROPIC_MODEL", &self.main), ("ANTHROPIC_DEFAULT_OPUS_MODEL", &self.opus), ("ANTHROPIC_DEFAULT_SONNET_MODEL", &self.sonnet), ("ANTHROPIC_DEFAULT_HAIKU_MODEL", &self.haiku)]
    }

    fn clean(self) -> Result<Self, LocalError> {
        let f = |v: Option<String>| -> Result<Option<String>, LocalError> {
            match v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()) {
                None => Ok(None),
                // 会原样拼进启动命令：只收模型名里常见的字符
                Some(s) if s.len() <= 100 && s.chars().all(|c| c.is_ascii_alphanumeric() || "-._:/[]@".contains(c)) => Ok(Some(s)),
                Some(_) => Err(LocalError::invalid("models", "invalid")),
            }
        };
        Ok(Models { main: f(self.main)?, opus: f(self.opus)?, sonnet: f(self.sonnet)?, haiku: f(self.haiku)? })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Profile {
    pub id: String,
    pub kind: Kind,
    /// 用户起的名字；本机默认账号为空（界面上显示“本机默认账号”）
    pub name: String,
    /// 平台推荐列表里的服务商（第三方接口）；自填地址时为 None
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(default)]
    pub models: Models,
    #[serde(default)]
    pub created_at: String,
}

fn builtin() -> Profile {
    Profile { id: OFFICIAL.into(), kind: Kind::Official, name: String::new(), provider_id: None, base_url: None, models: Models::default(), created_at: String::new() }
}

fn stored(store: &Store) -> Vec<Profile> {
    store.get::<Vec<Profile>>(LIST_KEY).ok().flatten().unwrap_or_default()
}

/// 全部：本机默认账号在最前面
pub fn list(store: &Store) -> Vec<Profile> {
    let mut v = vec![builtin()];
    v.extend(stored(store));
    v
}

pub fn get(store: &Store, id: &str) -> Option<Profile> {
    list(store).into_iter().find(|p| p.id == id)
}

/// 默认用哪个；记下的那个被删了时回到本机默认账号
pub fn default_id(store: &Store) -> String {
    store.get::<String>(DEFAULT_KEY).ok().flatten().filter(|id| get(store, id).is_some()).unwrap_or_else(|| OFFICIAL.into())
}

pub fn set_default(store: &Store, id: &str) -> Result<(), LocalError> {
    get(store, id).ok_or_else(LocalError::not_found)?;
    store.set(DEFAULT_KEY, &id.to_string())?;
    Ok(())
}

pub fn key_path(home: &Path, id: &str) -> PathBuf {
    home.join("model").join(format!("claude-{id}.key"))
}

pub fn key_set(home: &Path, id: &str) -> bool {
    std::fs::metadata(key_path(home, id)).is_ok_and(|m| m.len() > 0)
}

/// 另外的官方账号的配置目录
pub fn account_dir(home: &Path, id: &str) -> PathBuf {
    home.join("claude-accounts").join(id)
}

/// 这个配置用的 Claude Code 配置目录和用户级状态文件（`.claude.json`）
pub fn config_paths(home: &Path, p: &Profile) -> (PathBuf, PathBuf) {
    match p.kind {
        Kind::Official if p.id != OFFICIAL => {
            let dir = account_dir(home, &p.id);
            let state = dir.join(".claude.json");
            (dir, state)
        }
        _ => (crate::chat::models::config_dir(), crate::chat::models::state_file()),
    }
}

/// 新建或修改时填的
#[derive(Debug, Default, Deserialize)]
pub struct Input {
    pub kind: Option<Kind>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub provider_id: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub models: Option<Models>,
    /// 不填沿用已保存的 Key
    #[serde(default)]
    pub api_key: Option<String>,
}

fn clean_name(s: Option<&str>) -> Result<String, LocalError> {
    let name: String = s.unwrap_or("").trim().chars().filter(|c| !c.is_control()).take(NAME_MAX).collect();
    if name.is_empty() {
        return Err(LocalError::invalid("name", "required"));
    }
    Ok(name)
}

fn save_key(home: &Path, id: &str, key: Option<&str>) -> Result<(), LocalError> {
    match key.map(str::trim) {
        Some(k) if !k.is_empty() => {
            if k.len() > 4096 || k.chars().any(|c| c.is_whitespace() || c.is_control()) {
                return Err(LocalError::invalid("api_key", "invalid"));
            }
            let path = key_path(home, id);
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            crate::util::write_private(&path, k.as_bytes())?;
            Ok(())
        }
        _ if key_set(home, id) => Ok(()),
        _ => Err(LocalError::new("MODEL_KEY_REQUIRED")),
    }
}

/// 新建。第三方接口的 `base_url` 由调用方确定（平台推荐的以平台下发的为准）。
pub fn add(store: &Store, home: &Path, input: Input) -> Result<Profile, LocalError> {
    let mut all = stored(store);
    if all.len() >= MAX_PROFILES {
        return Err(LocalError::invalid("profiles", "too_many"));
    }
    let id = loop {
        let id = crate::util::random_id(8);
        if all.iter().all(|p| p.id != id) && id != OFFICIAL {
            break id;
        }
    };
    let kind = input.kind.ok_or_else(|| LocalError::invalid("kind", "required"))?;
    let mut p = Profile { id, kind, name: clean_name(input.name.as_deref())?, provider_id: None, base_url: None, models: Models::default(), created_at: crate::util::now_rfc3339() };
    if kind == Kind::Api {
        let url = input.base_url.as_deref().unwrap_or("").trim().trim_end_matches('/').to_string();
        crate::model::check_base_url(&url)?;
        p.base_url = Some(url);
        p.provider_id = input.provider_id.filter(|s| !s.is_empty());
        p.models = input.models.unwrap_or_default().clean()?;
        save_key(home, &p.id, input.api_key.as_deref())?;
    } else {
        prepare_dir(home, &p.id)?;
    }
    all.push(p.clone());
    store.set(LIST_KEY, &all)?;
    Ok(p)
}

/// 修改：名字；第三方接口还可以改地址、模型、Key。种类不能改。
pub fn update(store: &Store, home: &Path, id: &str, input: Input) -> Result<Profile, LocalError> {
    let mut all = stored(store);
    let p = all.iter_mut().find(|p| p.id == id).ok_or_else(LocalError::not_found)?;
    if let Some(name) = input.name.as_deref() {
        p.name = clean_name(Some(name))?;
    }
    if p.kind == Kind::Api {
        if let Some(url) = input.base_url.as_deref() {
            let url = url.trim().trim_end_matches('/').to_string();
            crate::model::check_base_url(&url)?;
            p.base_url = Some(url);
            p.provider_id = input.provider_id.filter(|s| !s.is_empty());
        }
        if let Some(m) = input.models {
            p.models = m.clean()?;
        }
        save_key(home, id, input.api_key.as_deref())?;
    }
    let out = p.clone();
    store.set(LIST_KEY, &all)?;
    Ok(out)
}

/// 删掉配置和本机文件（Key、账号目录）。官方账号要先由调用方 `claude auth logout`（macOS 的钥匙串条目只有它删得掉）。
pub fn remove(store: &Store, home: &Path, id: &str) -> Result<(), LocalError> {
    let mut all = stored(store);
    let Some(i) = all.iter().position(|p| p.id == id) else { return Err(LocalError::not_found()) };
    let p = all.remove(i);
    store.set(LIST_KEY, &all)?;
    match p.kind {
        Kind::Api => match std::fs::remove_file(key_path(home, id)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
            _ => {}
        },
        Kind::Official => remove_dir(&account_dir(home, id))?,
    }
    Ok(())
}

/// 删账号目录：先拆掉共用的链接（不跟进去删默认目录里的东西），再删剩下的
fn remove_dir(dir: &Path) -> std::io::Result<()> {
    for name in SHARED {
        let p = dir.join(name);
        if is_link(&p) {
            unlink(&p)?;
        }
    }
    match std::fs::remove_dir_all(dir) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

// ---------------- 会话 ----------------

fn pin_key(instance: &str) -> String {
    format!("instance_claude_profile.{instance}")
}

/// 新建会话时记下用哪个（`None` 用当前默认）
pub fn pin(store: &Store, instance: &str, profile: Option<&str>) -> Result<(), LocalError> {
    let id = match profile.filter(|s| !s.is_empty()) {
        Some(id) => get(store, id).ok_or_else(|| LocalError::invalid("claude_profile", "not_found"))?.id,
        None => default_id(store),
    };
    store.set(&pin_key(instance), &id)?;
    Ok(())
}

pub fn unpin(store: &Store, instance: &str) {
    let _ = store.remove(&pin_key(instance));
}

/// 会话记下的那个（被删了、没记下时用当前默认）
pub fn for_instance(store: &Store, instance: &str) -> Profile {
    store.get::<String>(&pin_key(instance)).ok().flatten().and_then(|id| get(store, &id)).unwrap_or_else(|| get(store, &default_id(store)).unwrap_or_else(builtin))
}

/// 会话记下的配置 ID（只给界面显示用；没记下时 None）
pub fn pinned(store: &Store, instance: &str) -> Option<String> {
    store.get::<String>(&pin_key(instance)).ok().flatten()
}

/// 用这个配置启动 Claude Code 时要加的环境变量；本机默认账号、或第三方接口的 Key 不在时为 None（按官方方式启动）。
pub fn launch_env(home: &Path, p: &Profile) -> Option<LaunchEnv> {
    match p.kind {
        Kind::Official if p.id == OFFICIAL => None,
        Kind::Official => {
            if let Err(e) = prepare_dir(home, &p.id) {
                tracing::warn!(error = %e, profile = %p.id, "准备 Claude Code 账号目录失败");
            }
            Some(LaunchEnv::new().lit("CLAUDE_CONFIG_DIR", account_dir(home, &p.id).display().to_string()))
        }
        Kind::Api => {
            let url = p.base_url.as_deref().filter(|u| crate::model::check_base_url(u).is_ok())?;
            if !key_set(home, &p.id) {
                return None;
            }
            let mut env = crate::model::claude_api_env(url, key_path(home, &p.id));
            for (k, v) in p.models.vars() {
                if let Some(v) = v {
                    env = env.lit(k, v.clone());
                }
            }
            Some(env)
        }
    }
}

/// Claude Code 启动时的几个确认用户不用管（小白看不懂，只会多一道坎），启动前在 `.claude.json` 里写成确认过了，
/// 跟用户自己点过一样。只补缺的，已经有的不动：
/// - 信任工作目录：`projects.<工作目录>.hasTrustDialogAccepted: true`（官方文档 permissions 里手动信任的写法）。
///   哪个目录都一样：git 仓库里的子目录、家目录写目录本身 Claude Code 也认（2.1.310 实测；家目录的信任官方说它自己从不保存）。
///   值是 `false` 也改：那是 Claude Code 第一次见到这个目录时自己记的默认值，不是用户选的。以后不认了也只是照常弹窗；
/// - 首次启动的引导（选主题、安全提示）：文档里没有对应的设置，补上走完引导后 Claude Code 自己写的 `hasCompletedOnboarding`；
///   同一次把主题设成 `auto`（跟随终端）。写在这里而不是用户设置里：这里的 `theme` 只在 settings 文件都没设时生效
///   （官方文档 settings-reference `theme`），用户自己选的主题照样优先。
///
/// Bypass Permissions 警告不在这里：对话界面启动时 `--settings` 传进去（见 `chat::hooks::settings_json`），不碰用户设置。
///
/// **时机**：每次生成 Claude Code 的启动命令时调用（instances.rs 的 `model_wrap`：新建、自动启动、接着以前的对话、
/// 对话界面里重新启动、ttyd 起来时的 `new-session -A`），命令交给 tmux 之前就写完了，Claude Code 启动时读到的是写好的。
///
/// **文件同时被改**：别的 Claude Code 会话随时在写这个文件。用它自己的锁（[`ConfigLock`]）：锁里重新读、只改缺的键、
/// 写到临时文件再换上去；Claude Code 自己保存时也是锁里重新读再合并，两边都不会盖掉对方的改动，运行中的会话发现文件变了会重新读。
/// 拿不到锁、读不了（例如是个目录）、不是 JSON 对象（让 Claude Code 自己处理）时这次不写，Claude Code 照常弹窗，下次启动再试。
/// 写过之后用户或 Claude Code 再改：下次启动只补又缺了的，用户改过的主题不会被改回去。
pub fn skip_first_run(home: &Path, p: &Profile, workdir: &Path) -> std::io::Result<()> {
    let (_, state_file) = config_paths(home, p);
    let trust = trust_key(workdir);
    edit_locked(&state_file, |o| {
        let mut changed = false;
        if o.get("hasCompletedOnboarding").is_none() {
            o.insert("hasCompletedOnboarding".into(), json!(true));
            o.entry("theme").or_insert_with(|| json!("auto"));
            changed = true;
        }
        if let Some(key) = trust {
            let projects = o.entry("projects").or_insert_with(|| json!({}));
            if let Some(entry) = projects.as_object_mut().map(|m| m.entry(key).or_insert_with(|| json!({}))) {
                if let Some(e) = entry.as_object_mut().filter(|e| e.get("hasTrustDialogAccepted") != Some(&json!(true))) {
                    e.insert("hasTrustDialogAccepted".into(), json!(true));
                    changed = true;
                }
            }
        }
        changed
    })
}

/// Claude Code 改 `.claude.json` 时拿的锁：`<文件>.lock` 目录（它内置的 proper-lockfile，`mkdir` 成功就是拿到了）。
/// 持有的进程每 5 秒刷新目录的修改时间，超过 10 秒没刷新算进程已经没了，删掉重来（跟 proper-lockfile 一样）。
/// 看看只拿很短一会儿（读、改、写一次），不用刷新。
struct ConfigLock(PathBuf);

impl ConfigLock {
    const WAIT: Duration = Duration::from_secs(2);
    const STALE: Duration = Duration::from_secs(10);

    fn acquire(file: &Path) -> std::io::Result<Self> {
        let mut lock = file.as_os_str().to_owned();
        lock.push(".lock");
        let lock = PathBuf::from(lock);
        let deadline = std::time::Instant::now() + Self::WAIT;
        loop {
            match std::fs::create_dir(&lock) {
                Ok(()) => return Ok(Self(lock)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    let age = std::fs::metadata(&lock).and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok());
                    if age.is_some_and(|a| a > Self::STALE) {
                        let _ = std::fs::remove_dir(&lock);
                        continue;
                    }
                    if std::time::Instant::now() >= deadline {
                        return Err(std::io::Error::new(std::io::ErrorKind::WouldBlock, format!("{} 被别的进程拿着", lock.display())));
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(e) => return Err(e),
            }
        }
    }
}

impl Drop for ConfigLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir(&self.0);
    }
}

/// 拿着 Claude Code 的锁改 JSON 对象文件：锁里读（没有文件时是空对象），`f` 返回改了没有，改了才写回。
/// 文件不是 JSON 对象时不动。
fn edit_locked(file: &Path, f: impl FnOnce(&mut serde_json::Map<String, Value>) -> bool) -> std::io::Result<()> {
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let _lock = ConfigLock::acquire(file)?;
    let mut v = match std::fs::read(file) {
        Ok(raw) => match serde_json::from_slice::<Value>(&raw) {
            Ok(v) if v.is_object() => v,
            _ => return Ok(()),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => json!({}),
        Err(e) => return Err(e),
    };
    if !f(v.as_object_mut().expect("object")) {
        return Ok(());
    }
    replace_file(file, serde_json::to_string_pretty(&v)?.as_bytes())
}

/// 整个换掉文件内容：写到同目录的临时文件再改名（读的人不会读到写了一半的），权限沿用原来的（没有时 0600）；
/// 文件是软链接时换的是它指向的文件，链接还在。
fn replace_file(file: &Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let target = std::fs::canonicalize(file).unwrap_or_else(|_| file.to_path_buf());
    let mut tmp = target.as_os_str().to_owned();
    tmp.push(format!(".looklook-{}.tmp", std::process::id()));
    let tmp = PathBuf::from(tmp);
    let perm = std::fs::metadata(&target).ok().map(|m| m.permissions());
    let res = (|| {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
        drop(f);
        if let Some(perm) = perm {
            std::fs::set_permissions(&tmp, perm)?;
        }
        std::fs::rename(&tmp, &target)
    })();
    if res.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    res
}

/// 信任记在哪个路径下：工作目录的真实路径（Claude Code 的工作目录是解析过软链接的）。目录不存在时为 None。
fn trust_key(workdir: &Path) -> Option<String> {
    let key = std::fs::canonicalize(workdir).ok()?.to_string_lossy().into_owned();
    // Windows：去掉 canonicalize 加的 `\\?\`，Claude Code 记路径用正斜杠
    Some(if cfg!(windows) { key.trim_start_matches(r"\\?\").replace('\\', "/") } else { key })
}

// ---------------- 官方账号的配置目录 ----------------

/// 建好账号目录：链上共用的东西，第一次时从默认的 `.claude.json` 抄一些不含账号信息的设置（见 [`seed_state`]），
/// 之后每次补上默认那边新加的 MCP 服务器。
pub fn prepare_dir(home: &Path, id: &str) -> std::io::Result<()> {
    let dir = account_dir(home, id);
    std::fs::create_dir_all(&dir)?;
    let main = crate::chat::models::config_dir();
    for name in SHARED {
        let target = main.join(name);
        let link = dir.join(name);
        if std::fs::symlink_metadata(&link).is_ok() {
            // 已经链好了，或者 Claude Code 把链接换成了自己的文件：都不动
            continue;
        }
        if !target.exists() {
            if !SHARED_CREATE.contains(name) {
                continue;
            }
            std::fs::create_dir_all(&target)?;
        }
        if let Err(e) = make_link(&target, &link) {
            tracing::debug!(error = %e, item = name, "无法链接到默认的 Claude Code 配置");
        }
    }
    sync_state(&crate::chat::models::state_file(), &dir.join(".claude.json"))
}

/// 从默认的状态文件抄过来的键：不含账号（`oauthAccount`、`userID`）和用量缓存
fn seed_state(main: &Value) -> Value {
    let mut out = serde_json::Map::new();
    for k in ["hasCompletedOnboarding", "lastOnboardingVersion", "theme", "bypassPermissionsModeAccepted", "mcpServers"] {
        if let Some(v) = main.get(k) {
            out.insert(k.into(), v.clone());
        }
    }
    // 信任过的项目：不用在新账号里再点一次“信任这个文件夹”
    if let Some(projects) = main.get("projects").and_then(Value::as_object) {
        let trusted: serde_json::Map<String, Value> =
            projects.iter().filter(|(_, v)| v["hasTrustDialogAccepted"] == true).map(|(k, _)| (k.clone(), json!({ "hasTrustDialogAccepted": true }))).collect();
        out.insert("projects".into(), Value::Object(trusted));
    }
    Value::Object(out)
}

fn sync_state(main_file: &Path, file: &Path) -> std::io::Result<()> {
    let main: Value = std::fs::read(main_file).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or(Value::Null);
    // Claude Code 自己在维护这个文件：拿它的锁（见 [`edit_locked`]），第一次（还是空的）时抄过来，
    // 之后只补上没有的 MCP 服务器（例如后来装上的看看 MCP），别的不动
    edit_locked(file, |o| {
        if o.is_empty() {
            if let Value::Object(seed) = seed_state(&main) {
                *o = seed;
            }
            return true;
        }
        let Some(main_servers) = main.get("mcpServers").and_then(Value::as_object) else { return false };
        let Some(servers) = o.entry("mcpServers").or_insert_with(|| json!({})).as_object_mut() else { return false };
        let mut changed = false;
        for (k, v) in main_servers {
            if !servers.contains_key(k) {
                servers.insert(k.clone(), v.clone());
                changed = true;
            }
        }
        changed
    })
}

fn is_link(p: &Path) -> bool {
    std::fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_symlink() || (cfg!(windows) && m.is_dir() && std::fs::read_link(p).is_ok()))
}

#[cfg(unix)]
fn make_link(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

/// Windows：目录用目录联接（不需要管理员或开发者模式），文件先试符号链接，不行就复制一份
#[cfg(windows)]
fn make_link(target: &Path, link: &Path) -> std::io::Result<()> {
    if target.is_dir() {
        if std::os::windows::fs::symlink_dir(target, link).is_ok() {
            return Ok(());
        }
        use std::os::windows::process::CommandExt;
        let st = std::process::Command::new("cmd").arg("/C").arg("mklink").arg("/J").arg(link).arg(target).creation_flags(0x0800_0000).output()?;
        return if st.status.success() { Ok(()) } else { Err(std::io::Error::other(String::from_utf8_lossy(&st.stderr).trim().to_string())) };
    }
    std::os::windows::fs::symlink_file(target, link).or_else(|_| std::fs::copy(target, link).map(|_| ()))
}

#[cfg(not(any(unix, windows)))]
fn make_link(_target: &Path, _link: &Path) -> std::io::Result<()> {
    Err(std::io::ErrorKind::Unsupported.into())
}

fn unlink(p: &Path) -> std::io::Result<()> {
    // Windows 的目录联接、目录符号链接要用 remove_dir（只删链接本身）
    if cfg!(windows) && std::fs::symlink_metadata(p).is_ok_and(|m| m.is_dir()) {
        std::fs::remove_dir(p)
    } else {
        std::fs::remove_file(p)
    }
}

// ---------------- 登录状态 ----------------

/// `claude auth status` 的结果（只取不含令牌的字段）。找不到 `claude`、超时时 `error`。
pub async fn status(shell: &str, home: &Path, p: &Profile) -> Value {
    if p.kind != Kind::Official {
        return json!({ "key_set": key_set(home, &p.id) });
    }
    let Some(cli) = crate::agents::claude_cli(shell).await else { return json!({ "error": "not_installed" }) };
    let dir = account_dir(home, &p.id);
    let mut env: Vec<(&str, Option<&std::ffi::OsStr>)> = vec![];
    if p.id != OFFICIAL {
        env.push(("CLAUDE_CONFIG_DIR", Some(dir.as_os_str())));
    }
    match crate::agents::cli_output(shell, &cli, &["auth", "status"], &env, Duration::from_secs(15)).await {
        Ok(out) => parse_status(&String::from_utf8_lossy(&out.stdout)),
        Err(e) => {
            tracing::debug!(error = %e, profile = %p.id, "claude auth status 失败");
            json!({ "error": "failed" })
        }
    }
}

/// 查官方账号的订阅用量：请 Claude Code 自己去取（`claude -p /usage`，不调用模型、不消耗额度），看看不拿令牌去问 Anthropic。
/// Claude Code 不把 HTTP 状态码交出来；登录失效（401）时它取不到用量，并且会清掉这个账号的凭据，
/// 所以没取到时再问一次 `claude auth status`，以它为准：
/// - `ok`：取到了，`limits` 同对话页（5 小时、7 天）
/// - `logged_out`：登录已失效，要重新登录
/// - `no_limits`：还登录着但没有用量（用 API Key 登录的没有订阅额度，或者 Anthropic 暂时没给）
/// - `failed`：没装 Claude Code、超时等
///
/// 都带上 `status`（同 [`status`]），界面顺便刷新登录状态。
pub async fn usage(shell: &str, home: &Path, p: &Profile) -> Value {
    let (config_dir, state_file) = config_paths(home, p);
    let account = (p.id != OFFICIAL).then(|| account_dir(home, &p.id));
    let cwd = home.join("run").join("usage");
    let got = crate::chat::models::fetch_limits(shell, &config_dir, account.as_deref(), &cwd).await;
    let st = status(shell, home, p).await;
    let state = if st["logged_in"] == false {
        "logged_out"
    } else {
        match got {
            Ok(true) => "ok",
            Ok(false) => "no_limits",
            Err(()) => "failed",
        }
    };
    let limits = if state == "ok" {
        let ctx = crate::chat::models::Ctx { config_dir, state_file, workdir: &cwd, env: Default::default() };
        tokio::task::block_in_place(|| crate::chat::models::limits(&ctx))
    } else {
        Value::Null
    };
    json!({ "state": state, "limits": limits, "status": st })
}

fn parse_status(stdout: &str) -> Value {
    // 前面可能有登录 shell 打出来的东西：从第一个 `{` 开始
    let Some(v) = stdout.find('{').and_then(|i| serde_json::from_str::<Value>(&stdout[i..]).ok()) else { return json!({ "error": "failed" }) };
    json!({
        "logged_in": v["loggedIn"].as_bool().unwrap_or(false),
        "auth_method": v["authMethod"],
        "email": v["email"],
        "org_name": v["orgName"],
        "plan": v["subscriptionType"],
    })
}

// ---------------- 登录（不开终端） ----------------
//
// 在后台运行 Claude Code 自己的 `claude auth login`：它没有终端时打印授权页地址，然后从标准输入读授权码。
// 界面显示这个地址（用户自己打开、在 claude.com 上登录），把用户粘贴的授权码原样写给它；换令牌、保存令牌都是
// Claude Code 自己做的。看看不保存、不记录授权码，也碰不到令牌。看看就在这台电脑上时，Claude Code 也可能自己
// 打开浏览器、走本机回调，不用粘贴就完成。

/// 一次登录最长等多久（用户去浏览器里登录）
const LOGIN_TIMEOUT: Duration = Duration::from_secs(15 * 60);

#[derive(Debug, Clone, Default, Serialize)]
pub struct LoginView {
    /// `running` / `ok` / `failed`
    pub state: &'static str,
    /// 授权页地址（Claude Code 打印出来之前为 None）
    pub url: Option<String>,
    /// 失败时 Claude Code 最后说的话（如 `Login failed: …`）
    pub error: Option<String>,
    /// 已经把授权码交给它了，在等它换令牌
    pub code_sent: bool,
}

struct Login {
    view: std::sync::Mutex<LoginView>,
    stdin: tokio::sync::Mutex<Option<tokio::process::ChildStdin>>,
    cancel: tokio::sync::Notify,
}

static LOGINS: std::sync::Mutex<Option<std::collections::HashMap<String, std::sync::Arc<Login>>>> = std::sync::Mutex::new(None);

fn login_slot(id: &str) -> Option<std::sync::Arc<Login>> {
    LOGINS.lock().unwrap_or_else(|e| e.into_inner()).as_ref().and_then(|m| m.get(id).cloned())
}

/// 开始登录（同一个账号上一次没完成的先结束掉）；等到授权页地址出来（最多几秒）再返回。
pub async fn login_start(shell: &str, home: &Path, p: &Profile) -> Result<LoginView, LocalError> {
    if p.kind != Kind::Official {
        return Err(LocalError::invalid("id", "not_official"));
    }
    login_cancel(&p.id);
    let cli = crate::agents::claude_cli(shell).await.ok_or_else(|| LocalError::new("AGENT_NOT_FOUND").with("agent", "claude"))?;
    let dir = account_dir(home, &p.id);
    let mut env: Vec<(&str, Option<&std::ffi::OsStr>)> = vec![];
    if p.id != OFFICIAL {
        prepare_dir(home, &p.id)?;
        env.push(("CLAUDE_CONFIG_DIR", Some(dir.as_os_str())));
    }
    let mut cmd = crate::agents::cli_command(shell, &cli, &["auth", "login"], &env);
    cmd.stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).kill_on_drop(true);
    let mut child = cmd.spawn().map_err(|e| LocalError::new("AGENT_NOT_FOUND").with("agent", "claude").with("detail", e.to_string()))?;
    let login = std::sync::Arc::new(Login {
        view: std::sync::Mutex::new(LoginView { state: "running", ..Default::default() }),
        stdin: tokio::sync::Mutex::new(child.stdin.take()),
        cancel: tokio::sync::Notify::new(),
    });
    LOGINS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(Default::default).insert(p.id.clone(), login.clone());
    let out = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let readers: Vec<_> = [child.stdout.take().map(|r| Box::new(r) as Box<dyn tokio::io::AsyncRead + Unpin + Send>), child.stderr.take().map(|r| Box::new(r) as _)]
        .into_iter()
        .flatten()
        .map(|mut r| {
            let (out, login) = (out.clone(), login.clone());
            tokio::spawn(async move {
                use tokio::io::AsyncReadExt;
                let mut buf = [0u8; 4096];
                while let Ok(n) = r.read(&mut buf).await {
                    if n == 0 {
                        break;
                    }
                    let mut o = out.lock().unwrap_or_else(|e| e.into_inner());
                    o.push_str(&String::from_utf8_lossy(&buf[..n]));
                    if o.len() > 64 * 1024 {
                        let cut = o.len() - 32 * 1024;
                        let cut = (cut..o.len()).find(|i| o.is_char_boundary(*i)).unwrap_or(o.len());
                        o.drain(..cut);
                    }
                    let mut v = login.view.lock().unwrap_or_else(|e| e.into_inner());
                    if v.url.is_none() {
                        v.url = auth_url(&o);
                    }
                }
            })
        })
        .collect();
    let task_login = login.clone();
    tokio::spawn(async move {
        let status = tokio::select! {
            s = child.wait() => s.ok(),
            _ = task_login.cancel.notified() => None,
            _ = tokio::time::sleep(LOGIN_TIMEOUT) => None,
        };
        if status.is_none() {
            let _ = child.kill().await;
        }
        for r in readers {
            let _ = r.await;
        }
        task_login.stdin.lock().await.take();
        let mut v = task_login.view.lock().unwrap_or_else(|e| e.into_inner());
        if status.is_some_and(|s| s.success()) {
            v.state = "ok";
        } else {
            v.state = "failed";
            v.error = status.map(|_| last_line(&out.lock().unwrap_or_else(|e| e.into_inner())));
        }
    });
    for _ in 0..80 {
        let v = login.view.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if v.url.is_some() || v.state != "running" {
            return Ok(v);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let v = login.view.lock().unwrap_or_else(|e| e.into_inner()).clone();
    Ok(v)
}

pub fn login_state(id: &str) -> Option<LoginView> {
    login_slot(id).map(|l| l.view.lock().unwrap_or_else(|e| e.into_inner()).clone())
}

/// 把用户粘贴的授权码交给 `claude auth login`（不保存、不写日志）
pub async fn login_code(id: &str, code: &str) -> Result<LoginView, LocalError> {
    let code = code.trim();
    if code.is_empty() || code.len() > 2048 || code.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(LocalError::invalid("code", "invalid"));
    }
    let login = login_slot(id).ok_or_else(|| LocalError::invalid("code", "not_running"))?;
    {
        use tokio::io::AsyncWriteExt;
        let mut stdin = login.stdin.lock().await;
        let w = stdin.as_mut().ok_or_else(|| LocalError::invalid("code", "not_running"))?;
        w.write_all(format!("{code}\n").as_bytes()).await.map_err(|_| LocalError::invalid("code", "not_running"))?;
        let _ = w.flush().await;
    }
    let mut v = login.view.lock().unwrap_or_else(|e| e.into_inner());
    v.code_sent = true;
    Ok(v.clone())
}

pub fn login_cancel(id: &str) {
    if let Some(l) = LOGINS.lock().unwrap_or_else(|e| e.into_inner()).as_mut().and_then(|m| m.remove(id)) {
        l.cancel.notify_one();
    }
}

/// 输出里的授权页地址：`https://…/oauth/authorize?…`，可能包在终端超链接（OSC 8）里
fn auth_url(out: &str) -> Option<String> {
    let mut rest = out;
    while let Some(i) = rest.find("https://") {
        let tail = &rest[i..];
        let end = tail.find(|c: char| c.is_whitespace() || c.is_control()).unwrap_or(tail.len());
        let url = &tail[..end];
        if url.contains("/oauth/authorize?") {
            return Some(url.to_string());
        }
        rest = &tail[end..];
    }
    None
}

/// 最后一行有字的输出（去掉终端控制序列、地址），失败时给界面看
fn last_line(out: &str) -> String {
    let clean: String = strip_ansi(out);
    let line = clean.lines().map(str::trim).rfind(|l| !l.is_empty() && !l.contains("https://")).unwrap_or("");
    // 去掉前面的 “Paste code here if prompted > ” 提示
    let line = line.rfind("> ").map_or(line, |i| &line[i + 2..]);
    line.chars().take(300).collect()
}

fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c != '\u{1b}' {
            if !c.is_control() || c == '\n' {
                out.push(c);
            }
            continue;
        }
        match it.next() {
            // OSC：到 BEL 或 ESC \ 为止
            Some(']') => {
                while let Some(c) = it.next() {
                    if c == '\u{7}' || (c == '\u{1b}' && it.next_if_eq(&'\\').is_some()) {
                        break;
                    }
                }
            }
            // CSI：到字母为止
            Some('[') => {
                for c in it.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    out
}

// ---------------- 升级 ----------------

/// 升级（各只做一次）：
/// 1. 以前“模型服务商”里给 Claude Code 选的第三方接口（kv `model_provider.claude` + `model/claude.key`）
///    变成一个第三方接口配置，并设为默认（和以前一样新会话走它）；
/// 2. 已有的 Claude Code 会话记下当时的默认，之后切换默认不再影响它们（和新建的会话一样）。
pub fn migrate(store: &Store, home: &Path) {
    migrate_choice(store, home);
    const PINNED: &str = "migrate.claude_profiles_pin";
    if store.get::<bool>(PINNED).ok().flatten().is_some() {
        return;
    }
    if let Ok(rows) = store.instances() {
        for r in rows.iter().filter(|r| r.launch == "claude") {
            if pinned(store, &r.id).is_none() {
                let _ = pin(store, &r.id, None);
            }
        }
    }
    let _ = store.set(PINNED, &true);
}

fn migrate_choice(store: &Store, home: &Path) {
    const OLD: &str = "model_provider.claude";
    let Ok(Some(old)) = store.get::<crate::model::Choice>(OLD) else { return };
    let old_key = home.join("model").join("claude.key");
    let key = std::fs::read_to_string(&old_key).ok().map(|k| k.trim().to_string()).filter(|k| !k.is_empty());
    if let Some(key) = key {
        let input = Input { kind: Some(Kind::Api), name: Some(old.name.clone()), provider_id: old.provider_id.clone(), base_url: Some(old.base_url.clone()), models: None, api_key: Some(key) };
        match add(store, home, input) {
            Ok(p) => {
                let _ = set_default(store, &p.id);
            }
            Err(e) => {
                tracing::warn!(error = ?e, "迁移 Claude Code 的第三方接口失败");
                return;
            }
        }
    }
    let _ = store.remove(OLD);
    let _ = std::fs::remove_file(old_key);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_store(tag: &str) -> (PathBuf, Store) {
        let dir = std::env::temp_dir().join(format!("ll-cprof-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::open(&dir.join("t.db")).unwrap();
        (dir, store)
    }

    #[test]
    fn default_and_pin() {
        let (dir, store) = tmp_store("pin");
        assert_eq!(default_id(&store), OFFICIAL);
        let api = add(&store, &dir, Input { kind: Some(Kind::Api), name: Some("R".into()), base_url: Some("https://r.example.com/".into()), api_key: Some("sk-1".into()), ..Default::default() }).unwrap();
        assert_eq!(api.base_url.as_deref(), Some("https://r.example.com"));
        assert!(add(&store, &dir, Input { kind: Some(Kind::Api), name: Some("x".into()), base_url: Some("https://x.example.com".into()), ..Default::default() }).is_err(), "第三方接口要 Key");
        pin(&store, "i1", None).unwrap();
        set_default(&store, &api.id).unwrap();
        pin(&store, "i2", None).unwrap();
        // 切换默认不影响已经建好的会话
        assert_eq!(for_instance(&store, "i1").id, OFFICIAL);
        assert_eq!(for_instance(&store, "i2").id, api.id);
        // 没记下的（例如容器里的）跟着默认
        assert_eq!(for_instance(&store, "old").id, api.id);
        assert!(pin(&store, "i3", Some("nope")).is_err());
        remove(&store, &dir, &api.id).unwrap();
        assert!(!key_set(&dir, &api.id));
        assert_eq!(default_id(&store), OFFICIAL);
        assert_eq!(for_instance(&store, "i2").id, OFFICIAL);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn api_env() {
        let (dir, store) = tmp_store("env");
        let models = Models { main: Some("glm-4.6".into()), haiku: Some("glm-4.5-air".into()), ..Default::default() };
        let p = add(&store, &dir, Input { kind: Some(Kind::Api), name: Some("R".into()), base_url: Some("https://r.example.com/api/anthropic".into()), models: Some(models), api_key: Some("sk-1".into()), ..Default::default() }).unwrap();
        let cmd = launch_env(&dir, &p).unwrap().wrap("claude", crate::shells::Family::Posix);
        assert!(cmd.starts_with("ANTHROPIC_BASE_URL='https://r.example.com/api/anthropic' ANTHROPIC_AUTH_TOKEN=\"$(cat "), "{cmd}");
        assert!(cmd.ends_with(" ANTHROPIC_API_KEY= ANTHROPIC_MODEL='glm-4.6' ANTHROPIC_DEFAULT_HAIKU_MODEL='glm-4.5-air' claude"), "{cmd}");
        assert!(launch_env(&dir, &builtin()).is_none());
        let bad = Models { main: Some("x; rm -rf /".into()), ..Default::default() };
        assert!(update(&store, &dir, &p.id, Input { models: Some(bad), ..Default::default() }).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn account_dir_shares_but_keeps_login_apart() {
        let (dir, _store) = tmp_store("dir");
        let main = dir.join("main");
        std::fs::create_dir_all(main.join("skills")).unwrap();
        std::fs::write(main.join("settings.json"), "{}").unwrap();
        std::fs::write(main.join(".credentials.json"), "secret").unwrap();
        let main_state = dir.join("main.json");
        let state = json!({
            "hasCompletedOnboarding": true,
            "oauthAccount": { "emailAddress": "a@b.c" },
            "userID": "u",
            "mcpServers": { "looklook": { "type": "http" } },
            "projects": { "/a": { "hasTrustDialogAccepted": true, "lastCost": 3 }, "/b": { "hasTrustDialogAccepted": false } }
        });
        std::fs::write(&main_state, state.to_string()).unwrap();
        let acct = dir.join("acct");
        std::fs::create_dir_all(&acct).unwrap();
        // prepare_dir 用的是真实的默认目录；这里直接测它的两半
        for name in SHARED {
            let target = main.join(name);
            if target.exists() {
                make_link(&target, &acct.join(name)).unwrap();
            }
        }
        sync_state(&main_state, &acct.join(".claude.json")).unwrap();
        assert!(is_link(&acct.join("skills")) && is_link(&acct.join("settings.json")));
        assert!(!acct.join(".credentials.json").exists());
        let seeded: Value = serde_json::from_slice(&std::fs::read(acct.join(".claude.json")).unwrap()).unwrap();
        assert_eq!(seeded["hasCompletedOnboarding"], true);
        assert!(seeded.get("oauthAccount").is_none() && seeded.get("userID").is_none());
        assert_eq!(seeded["projects"], json!({ "/a": { "hasTrustDialogAccepted": true } }));
        // 默认那边后来加的 MCP 服务器补进来，账号自己的不动
        let mut s2 = state.clone();
        s2["mcpServers"]["other"] = json!({ "type": "stdio" });
        std::fs::write(&main_state, s2.to_string()).unwrap();
        let mut mine = seeded.clone();
        mine["oauthAccount"] = json!({ "emailAddress": "x@y.z" });
        std::fs::write(acct.join(".claude.json"), mine.to_string()).unwrap();
        sync_state(&main_state, &acct.join(".claude.json")).unwrap();
        let after: Value = serde_json::from_slice(&std::fs::read(acct.join(".claude.json")).unwrap()).unwrap();
        assert_eq!(after["mcpServers"]["other"]["type"], "stdio");
        assert_eq!(after["oauthAccount"]["emailAddress"], "x@y.z");
        // 删账号目录不会删到默认目录里的东西
        remove_dir(&acct).unwrap();
        assert!(!acct.exists() && main.join("skills").is_dir() && main.join("settings.json").is_file());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn login_output() {
        let out = "Opening browser to sign in\u{2026}\nIf the browser didn't open, visit: \u{1b}]8;;https://claude.com/cai/oauth/authorize?code=true&state=x\u{7}https://claude.com/cai/oauth/authorize?code=true&state=x\u{1b}]8;;\u{7}\nPaste code here if prompted > Login failed: Request failed with status code 400\n";
        assert_eq!(auth_url(out).as_deref(), Some("https://claude.com/cai/oauth/authorize?code=true&state=x"));
        assert_eq!(last_line(out), "Login failed: Request failed with status code 400");
        assert_eq!(auth_url("visit: https://example.com/x"), None);
    }

    /// 真的跑一次本机的 `claude -p /usage`（本地命令，不消耗额度）：没登录的账号是 `logged_out`；
    /// 本机默认账号登录着订阅时是 `ok`，有用量。`cargo test real_usage -- --ignored`
    #[tokio::test(flavor = "multi_thread")]
    #[ignore]
    async fn real_usage() {
        let (dir, store) = tmp_store("usage");
        let p = add(&store, &dir, Input { kind: Some(Kind::Official), name: Some("t".into()), ..Default::default() }).unwrap();
        let shell = crate::shells::login_shell();
        let v = usage(&shell, &dir, &p).await;
        assert_eq!(v["state"], "logged_out", "{v}");
        assert_eq!(v["status"]["logged_in"], false, "{v}");
        let v = usage(&shell, &dir, &builtin()).await;
        if v["status"]["auth_method"] == "claude.ai" {
            assert_eq!(v["state"], "ok", "{v}");
            assert!(v["limits"]["five_hour"]["used"].is_number() || v["limits"]["seven_day"]["used"].is_number(), "{v}");
        }
        remove_dir(&account_dir(&dir, &p.id)).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 真的跑一次本机的 `claude auth login`（不消耗额度）：拿到授权页地址，错的授权码报 Login failed。
    /// `cargo test real_login -- --ignored`
    #[tokio::test(flavor = "multi_thread")]
    #[ignore]
    async fn real_login_flow() {
        let (dir, store) = tmp_store("login");
        let p = add(&store, &dir, Input { kind: Some(Kind::Official), name: Some("t".into()), ..Default::default() }).unwrap();
        let shell = crate::shells::login_shell();
        let v = login_start(&shell, &dir, &p).await.unwrap();
        assert_eq!(v.state, "running");
        assert!(v.url.as_deref().is_some_and(|u| u.starts_with("https://") && u.contains("/oauth/authorize?")), "{v:?}");
        assert!(login_code(&p.id, "bad code").await.is_err(), "有空格的不收");
        login_code(&p.id, "fakecode123#fakestate").await.unwrap();
        let mut v = login_state(&p.id).unwrap();
        for _ in 0..300 {
            if v.state != "running" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
            v = login_state(&p.id).unwrap();
        }
        assert_eq!(v.state, "failed", "{v:?}");
        assert!(v.error.as_deref().is_some_and(|e| e.starts_with("Login failed")), "{v:?}");
        login_cancel(&p.id);
        remove_dir(&account_dir(&dir, &p.id)).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn status_json() {
        let v = parse_status("motd\n{\"loggedIn\":true,\"authMethod\":\"claude.ai\",\"email\":\"a@b.c\",\"subscriptionType\":\"max\",\"orgName\":null}");
        assert_eq!(v["logged_in"], true);
        assert_eq!(v["plan"], "max");
        assert_eq!(parse_status("")["error"], "failed");
    }

    #[test]
    fn migrate_old_choice() {
        let (dir, store) = tmp_store("mig");
        let c = crate::model::Choice { provider_id: None, name: "Old".into(), base_url: "https://old.example.com".into(), selected_at: String::new() };
        store.set("model_provider.claude", &c).unwrap();
        std::fs::create_dir_all(dir.join("model")).unwrap();
        std::fs::write(dir.join("model/claude.key"), "sk-old\n").unwrap();
        migrate(&store, &dir);
        let id = default_id(&store);
        let p = get(&store, &id).unwrap();
        assert_eq!((p.kind, p.name.as_str(), p.base_url.as_deref()), (Kind::Api, "Old", Some("https://old.example.com")));
        assert_eq!(std::fs::read_to_string(key_path(&dir, &id)).unwrap(), "sk-old");
        assert!(store.get::<crate::model::Choice>("model_provider.claude").unwrap().is_none());
        migrate(&store, &dir);
        assert_eq!(list(&store).len(), 2, "只迁一次");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn first_run_skipped_once() {
        let (dir, _store) = tmp_store("firstrun");
        let read = |f: &Path| -> Value { serde_json::from_slice(&std::fs::read(f).unwrap()).unwrap() };
        let p = Profile { id: "acct1".into(), kind: Kind::Official, name: "a".into(), provider_id: None, base_url: None, models: Models::default(), created_at: String::new() };
        let (cfg, state) = config_paths(&dir, &p);
        // 信任记在工作目录本身；没有文件时建一个，用户设置不碰
        let wd = dir.join("repo/src/a");
        std::fs::create_dir_all(&wd).unwrap();
        let wd_key = std::fs::canonicalize(&wd).unwrap().display().to_string();
        skip_first_run(&dir, &p, &wd).unwrap();
        assert_eq!(read(&state), json!({ "hasCompletedOnboarding": true, "theme": "auto", "projects": { wd_key: { "hasTrustDialogAccepted": true } } }));
        assert!(!cfg.join("settings.json").exists());
        assert!(!cfg.join(".claude.json.lock").exists(), "锁要放掉");
        // 写过之后被改了（Claude Code 第一次见到目录记的 false、别的会话加的键、用户换的主题）：只补缺的
        let plain = dir.join("plain");
        std::fs::create_dir_all(&plain).unwrap();
        let key = std::fs::canonicalize(&plain).unwrap().display().to_string();
        std::fs::write(&state, json!({ "hasCompletedOnboarding": true, "theme": "light", "userID": "u", "projects": { key.clone(): { "hasTrustDialogAccepted": false, "mcpServers": {} } } }).to_string()).unwrap();
        skip_first_run(&dir, &p, &plain).unwrap();
        let v = read(&state);
        assert_eq!((v["theme"].as_str(), v["userID"].as_str()), (Some("light"), Some("u")));
        assert_eq!(v["projects"][&key], json!({ "mcpServers": {}, "hasTrustDialogAccepted": true }));
        // 都确认过：不写文件
        let before = std::fs::read_to_string(&state).unwrap();
        skip_first_run(&dir, &p, &plain).unwrap();
        assert_eq!(std::fs::read_to_string(&state).unwrap(), before);
        // 不是 JSON（不是 Claude Code 写的样子）：不动
        std::fs::write(&state, "{").unwrap();
        skip_first_run(&dir, &p, &plain).unwrap();
        assert_eq!(std::fs::read_to_string(&state).unwrap(), "{");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn first_run_respects_claude_lock() {
        let (dir, _store) = tmp_store("firstlock");
        let p = Profile { id: "acct2".into(), kind: Kind::Official, name: "a".into(), provider_id: None, base_url: None, models: Models::default(), created_at: String::new() };
        let (cfg, state) = config_paths(&dir, &p);
        std::fs::create_dir_all(&cfg).unwrap();
        let lock = cfg.join(".claude.json.lock");
        // 别的 Claude Code 正拿着锁：等一会儿拿不到就不写，锁也不动
        std::fs::create_dir(&lock).unwrap();
        let err = skip_first_run(&dir, &p, &dir).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::WouldBlock);
        assert!(!state.exists() && lock.is_dir());
        // 拿着锁的进程没了（10 秒以上没刷新）：删掉重来
        let old = std::time::SystemTime::now() - Duration::from_secs(30);
        std::fs::File::open(&lock).unwrap().set_modified(old).unwrap();
        skip_first_run(&dir, &p, &dir).unwrap();
        assert!(state.exists() && !lock.exists());
        // `.claude.json` 是软链接（例如 dotfiles 管着）：改它指向的文件，链接还在
        let real = dir.join("dotfiles-claude.json");
        std::fs::write(&real, "{}").unwrap();
        std::fs::remove_file(&state).unwrap();
        std::os::unix::fs::symlink(&real, &state).unwrap();
        skip_first_run(&dir, &p, &dir).unwrap();
        assert!(std::fs::symlink_metadata(&state).unwrap().file_type().is_symlink());
        let v: Value = serde_json::from_slice(&std::fs::read(&real).unwrap()).unwrap();
        assert_eq!(v["hasCompletedOnboarding"], true);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 真的 Claude Code 开着时写：它退出时自己也写这个文件，两边写的都要在。不用登录、不发消息。
    /// `cargo test first_run_with_running_claude -- --ignored --nocapture`（要有 claude、tmux）
    #[cfg(unix)]
    #[test]
    #[ignore]
    fn first_run_with_running_claude() {
        let (dir, _store) = tmp_store("firstlive");
        let p = Profile { id: "live".into(), kind: Kind::Official, name: "a".into(), provider_id: None, base_url: None, models: Models::default(), created_at: String::new() };
        let (cfg, state) = config_paths(&dir, &p);
        let (x, y) = (dir.join("x"), dir.join("y"));
        std::fs::create_dir_all(&x).unwrap();
        std::fs::create_dir_all(&y).unwrap();
        skip_first_run(&dir, &p, &x).unwrap();
        let sock = dir.join("tmux.sock");
        let tmux = |args: &[&str]| std::process::Command::new("tmux").arg("-S").arg(&sock).args(args).output().unwrap();
        let cmd = format!("env CLAUDE_CONFIG_DIR='{}' claude --model haiku; sleep 30", cfg.display());
        tmux(&["new-session", "-d", "-s", "a", "-x", "120", "-y", "40", "-c", x.to_str().unwrap(), &cmd]);
        std::thread::sleep(Duration::from_secs(8));
        let screen = String::from_utf8_lossy(&tmux(&["capture-pane", "-p", "-t", "a"]).stdout).to_string();
        assert!(screen.contains('❯') && !screen.contains("trust this folder"), "{screen}");
        // Claude Code 开着：写另一个目录的信任
        skip_first_run(&dir, &p, &y).unwrap();
        tmux(&["send-keys", "-t", "a", "/exit", "Enter"]);
        std::thread::sleep(Duration::from_secs(4));
        tmux(&["kill-server"]);
        let v: Value = serde_json::from_slice(&std::fs::read(&state).unwrap()).unwrap();
        let key = |d: &Path| std::fs::canonicalize(d).unwrap().display().to_string();
        println!("{}", serde_json::to_string_pretty(&v["projects"]).unwrap());
        assert_eq!(v["projects"][key(&y)]["hasTrustDialogAccepted"], true, "看看写的还在");
        assert_eq!(v["projects"][key(&x)]["hasTrustDialogAccepted"], true);
        assert!(v["projects"][key(&x)].get("lastSessionId").is_some() || v.get("firstStartTime").is_some(), "Claude Code 写的也在");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
