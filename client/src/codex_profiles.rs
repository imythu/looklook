//! Codex 配置：本机默认（Codex 自己的登录和 `~/.codex/config.toml`，一直在、不能删）和多个第三方接口，
//! 选一个当默认。新建会话时记下用哪个（kv `instance_codex_profile.{实例ID}`），之后切换默认不影响已有的会话。
//! 和 Claude Code 的账号切换（claude_profiles.rs）是同一套规则。
//!
//! - **第三方接口**：OpenAI Responses 兼容的地址、模型名、用户自己的 Key。Key 只写在本机文件
//!   `<数据目录>/model/codex-<ID>.key`（只有当前用户可读），不出现在任何进程的命令行参数里。
//!   用 `-c` 临时定义服务商 `looklook-<ID>`，不改用户的 `~/.codex/config.toml`，也不会把 ChatGPT 登录凭据发给第三方。
//! - **终端里直接起的 Codex**：这个配置的服务商定义 + `model_provider` + `model`，Key 由 shell 读进 `LOOKLOOK_MODEL_API_KEY`。
//! - **对话模式**（看看起的一个 app-server 给所有会话用）：app-server 启动时把所有第三方配置都定义上，
//!   每个的 Key 在自己的环境变量 `LOOKLOOK_CODEX_KEY_<ID>` 里；每个会话 `thread/start`、`thread/resume`、`thread/fork`
//!   时传 `modelProvider`、`model`（app-server 协议支持按会话选，2026-10 用 0.164 实测）。
//!   终端里接到 app-server 上的 TUI 也要定义同一个服务商并选上（TUI 自己先解析配置，不认识的服务商会报错）。
//! - **升级**：以前“模型服务商”里给 Codex 选的接口（kv `model_provider.codex` + `model/codex.key`）变成一个配置，设为默认。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::error::LocalError;
use crate::model::LaunchEnv;
use crate::store::Store;

pub const OFFICIAL: &str = "official";
/// app-server、TUI 里第三方配置的服务商名：`looklook-<配置 ID>`（用量统计据此认出走的哪家）
pub const PROVIDER_PREFIX: &str = "looklook-";
const LIST_KEY: &str = "codex_profiles";
const DEFAULT_KEY: &str = "codex_profiles.default";
const MAX_PROFILES: usize = 20;
const NAME_MAX: usize = 40;
/// 终端里直接起的 Codex 读 Key 的环境变量（只有一个配置）
const TERM_KEY_ENV: &str = "LOOKLOOK_MODEL_API_KEY";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Profile {
    pub id: String,
    /// 用户起的名字；本机默认为空（界面上显示“本机默认”）
    pub name: String,
    /// 常见服务商的快速配置（model_presets.rs）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset: Option<String>,
    /// 平台推荐列表里的服务商
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_id: Option<String>,
    /// 本机默认为 None
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default)]
    pub created_at: String,
}

impl Profile {
    pub fn is_api(&self) -> bool {
        self.base_url.is_some()
    }

    /// app-server、TUI 里的服务商名
    pub fn provider_name(&self) -> String {
        format!("{PROVIDER_PREFIX}{}", self.id)
    }

    fn key_env(&self) -> String {
        format!("LOOKLOOK_CODEX_KEY_{}", self.id.to_ascii_uppercase())
    }

    /// 定义这个服务商的 `-c`（`env_key` 由调用方给）
    fn define(&self, env: LaunchEnv, key_env: &str) -> LaunchEnv {
        let n = self.provider_name();
        let url = self.base_url.as_deref().unwrap_or_default();
        env.config(format!("model_providers.{n}.name={n}"))
            .config(format!("model_providers.{n}.base_url={url}"))
            .config(format!("model_providers.{n}.env_key={key_env}"))
            .config(format!("model_providers.{n}.wire_api=responses"))
    }

    /// 选上这个服务商和模型的 `-c`
    fn select(&self, env: LaunchEnv) -> LaunchEnv {
        let env = env.config(format!("model_provider={}", self.provider_name()));
        match self.model.as_deref().filter(|m| crate::model::codex_model_ok(m)) {
            Some(m) => env.config(format!("model={m}")),
            None => env,
        }
    }
}

fn builtin() -> Profile {
    Profile { id: OFFICIAL.into(), name: String::new(), preset: None, provider_id: None, base_url: None, model: None, created_at: String::new() }
}

fn stored(store: &Store) -> Vec<Profile> {
    store.get::<Vec<Profile>>(LIST_KEY).ok().flatten().unwrap_or_default()
}

/// 本机默认在最前面
pub fn list(store: &Store) -> Vec<Profile> {
    std::iter::once(builtin()).chain(stored(store)).collect()
}

pub fn get(store: &Store, id: &str) -> Option<Profile> {
    list(store).into_iter().find(|p| p.id == id)
}

pub fn default_id(store: &Store) -> String {
    store.get::<String>(DEFAULT_KEY).ok().flatten().filter(|id| get(store, id).is_some()).unwrap_or_else(|| OFFICIAL.into())
}

pub fn set_default(store: &Store, id: &str) -> Result<(), LocalError> {
    get(store, id).ok_or_else(LocalError::not_found)?;
    store.set(DEFAULT_KEY, &id.to_string())?;
    Ok(())
}

pub fn key_path(home: &Path, id: &str) -> PathBuf {
    home.join("model").join(format!("codex-{id}.key"))
}

pub fn key_set(home: &Path, id: &str) -> bool {
    std::fs::metadata(key_path(home, id)).is_ok_and(|m| m.len() > 0)
}

/// 新建或修改时填的
#[derive(Debug, Default, Deserialize)]
pub struct Input {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub preset: Option<String>,
    #[serde(default)]
    pub provider_id: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
    /// 空字符串表示不指定
    #[serde(default)]
    pub model: Option<String>,
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

fn clean_model(m: Option<&str>) -> Result<Option<String>, LocalError> {
    let m = crate::model::clean_model(m)?;
    if m.as_deref().is_some_and(|m| !crate::model::codex_model_ok(m)) {
        return Err(LocalError::invalid("model", "invalid"));
    }
    Ok(m)
}

fn clean_url(u: &str) -> Result<String, LocalError> {
    let url = u.trim().trim_end_matches('/').to_string();
    crate::model::check_base_url(&url)?;
    Ok(url)
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

/// 新建第三方接口。`base_url` 由调用方确定（平台推荐的以平台下发的为准，快速配置的以预设为准）。
pub fn add(store: &Store, home: &Path, input: Input) -> Result<Profile, LocalError> {
    let mut all = stored(store);
    if all.len() >= MAX_PROFILES {
        return Err(LocalError::invalid("profiles", "too_many"));
    }
    let id = loop {
        // 小写字母和数字：要拼进服务商名和环境变量名
        let id = crate::util::random_id(8).to_ascii_lowercase();
        if all.iter().all(|p| p.id != id) && id != OFFICIAL && id.chars().all(|c| c.is_ascii_alphanumeric()) {
            break id;
        }
    };
    let p = Profile {
        id,
        name: clean_name(input.name.as_deref())?,
        preset: input.preset.filter(|s| crate::model_presets::get(s).is_some()),
        provider_id: input.provider_id.filter(|s| !s.is_empty()),
        base_url: Some(clean_url(input.base_url.as_deref().unwrap_or(""))?),
        model: clean_model(input.model.as_deref())?,
        created_at: crate::util::now_rfc3339(),
    };
    save_key(home, &p.id, input.api_key.as_deref())?;
    all.push(p.clone());
    store.set(LIST_KEY, &all)?;
    Ok(p)
}

/// 修改：名字、地址（连同来源）、模型、Key。本机默认不能改。
pub fn update(store: &Store, home: &Path, id: &str, input: Input) -> Result<Profile, LocalError> {
    let mut all = stored(store);
    let p = all.iter_mut().find(|p| p.id == id).ok_or_else(LocalError::not_found)?;
    if let Some(name) = input.name.as_deref() {
        p.name = clean_name(Some(name))?;
    }
    if let Some(url) = input.base_url.as_deref() {
        p.base_url = Some(clean_url(url)?);
        p.provider_id = input.provider_id.filter(|s| !s.is_empty());
        p.preset = input.preset.filter(|s| crate::model_presets::get(s).is_some());
    }
    if input.model.is_some() {
        p.model = clean_model(input.model.as_deref())?;
    }
    save_key(home, id, input.api_key.as_deref())?;
    let out = p.clone();
    store.set(LIST_KEY, &all)?;
    Ok(out)
}

pub fn remove(store: &Store, home: &Path, id: &str) -> Result<(), LocalError> {
    if id == OFFICIAL {
        return Err(LocalError::invalid("id", "builtin"));
    }
    let mut all = stored(store);
    let before = all.len();
    all.retain(|p| p.id != id);
    if all.len() == before {
        return Err(LocalError::not_found());
    }
    store.set(LIST_KEY, &all)?;
    if store.get::<String>(DEFAULT_KEY).ok().flatten().as_deref() == Some(id) {
        store.remove(DEFAULT_KEY)?;
    }
    match std::fs::remove_file(key_path(home, id)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
        _ => Ok(()),
    }
}

fn pin_key(instance: &str) -> String {
    format!("instance_codex_profile.{instance}")
}

/// 新建会话时记下用哪个；不指定用当时的默认
pub fn pin(store: &Store, instance: &str, profile: Option<&str>) -> Result<(), LocalError> {
    let id = match profile {
        Some(id) => get(store, id).ok_or_else(|| LocalError::invalid("codex_profile", "not_found"))?.id,
        None => default_id(store),
    };
    store.set(&pin_key(instance), &id)?;
    Ok(())
}

pub fn unpin(store: &Store, instance: &str) {
    let _ = store.remove(&pin_key(instance));
}

pub fn pinned(store: &Store, instance: &str) -> Option<String> {
    store.get::<String>(&pin_key(instance)).ok().flatten()
}

/// 这个会话用的配置：记下的（被删了就用默认），没记下的（容器里的、升级前的）用默认
pub fn for_instance(store: &Store, instance: &str) -> Profile {
    pinned(store, instance).and_then(|id| get(store, &id)).or_else(|| get(store, &default_id(store))).unwrap_or_else(builtin)
}

/// 第三方接口而且 Key 在：能用
fn usable(home: &Path, p: &Profile) -> bool {
    p.base_url.as_deref().is_some_and(|u| crate::model::check_base_url(u).is_ok()) && key_set(home, &p.id)
}

/// 终端里直接起的 Codex 用这个配置时的启动环境；本机默认、Key 不在时 None（按官方方式启动）
pub fn launch_env(home: &Path, p: &Profile) -> Option<LaunchEnv> {
    if !usable(home, p) {
        return None;
    }
    let env = LaunchEnv::new().file(TERM_KEY_ENV, key_path(home, &p.id));
    Some(p.select(p.define(env, TERM_KEY_ENV)))
}

/// app-server 的启动环境：所有能用的第三方配置都定义上，各自的 Key 在各自的环境变量里。不选默认服务商：
/// 没指定服务商的会话（本机默认）照常用用户自己的配置。没有第三方配置时 None。
pub fn server_env(store: &Store, home: &Path) -> Option<LaunchEnv> {
    let api: Vec<Profile> = stored(store).into_iter().filter(|p| usable(home, p)).collect();
    if api.is_empty() {
        return None;
    }
    Some(api.iter().fold(LaunchEnv::new(), |env, p| {
        let k = p.key_env();
        p.define(env.file(k.clone(), key_path(home, &p.id)), &k)
    }))
}

/// 对话模式下终端里的 TUI（接到 app-server）：定义这个配置的服务商并选上。Key 不用给：请求由 app-server 发
pub fn tui_env(home: &Path, p: &Profile) -> Option<LaunchEnv> {
    usable(home, p).then(|| p.select(p.define(LaunchEnv::new(), &p.key_env())))
}

/// `thread/start`、`thread/resume`、`thread/fork` 要加的参数；本机默认不加（用用户自己的配置）
pub fn thread_params(home: &Path, p: &Profile) -> Value {
    if !usable(home, p) {
        return json!({});
    }
    let mut v = json!({ "modelProvider": p.provider_name() });
    if let Some(m) = p.model.as_deref().filter(|m| crate::model::codex_model_ok(m)) {
        v["model"] = json!(m);
    }
    v
}

/// 升级：以前“模型服务商”里给 Codex 选的接口变成一个配置，设为默认；Key 文件挪过去
pub fn migrate(store: &Store, home: &Path) {
    const OLD: &str = "model_provider.codex";
    let Ok(Some(old)) = store.get::<crate::model::Choice>(OLD) else { return };
    let old_key = home.join("model").join("codex.key");
    let key = std::fs::read_to_string(&old_key).ok().map(|k| k.trim().to_string()).filter(|k| !k.is_empty());
    if let Some(key) = key {
        let input = Input { name: Some(old.name.clone()), preset: None, provider_id: old.provider_id.clone(), base_url: Some(old.base_url.clone()), model: old.model.clone(), api_key: Some(key) };
        match add(store, home, input) {
            Ok(p) => {
                let _ = set_default(store, &p.id);
            }
            Err(e) => {
                tracing::warn!(error = ?e, "以前的 Codex 服务商迁移失败");
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
    use crate::shells::Family;

    fn tmp_store(tag: &str) -> (PathBuf, Store) {
        let dir = std::env::temp_dir().join(format!("ll-codex-prof-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::open(&dir.join("t.db")).unwrap();
        (dir, store)
    }

    fn api(store: &Store, dir: &Path, name: &str, model: Option<&str>) -> Profile {
        add(store, dir, Input { name: Some(name.into()), base_url: Some(format!("https://{name}.example.com/v1/")), model: model.map(String::from), api_key: Some("sk-1".into()), ..Default::default() }).unwrap()
    }

    #[test]
    fn profiles_default_and_pin() {
        let (dir, store) = tmp_store("pin");
        assert_eq!(default_id(&store), OFFICIAL);
        assert!(add(&store, &dir, Input { name: Some("x".into()), base_url: Some("https://x.example.com".into()), ..Default::default() }).is_err(), "要 Key");
        assert!(add(&store, &dir, Input { name: Some("x".into()), base_url: Some("https://x.example.com".into()), model: Some("1.5".into()), api_key: Some("k".into()), ..Default::default() }).is_err(), "模型名要有字母");
        let a = api(&store, &dir, "a", Some("glm-5.3"));
        assert_eq!(a.base_url.as_deref(), Some("https://a.example.com/v1"));
        pin(&store, "i1", None).unwrap();
        set_default(&store, &a.id).unwrap();
        pin(&store, "i2", None).unwrap();
        assert_eq!(for_instance(&store, "i1").id, OFFICIAL);
        assert_eq!(for_instance(&store, "i2").id, a.id);
        assert_eq!(for_instance(&store, "nope").id, a.id);
        let a = update(&store, &dir, &a.id, Input { model: Some(String::new()), ..Default::default() }).unwrap();
        assert_eq!(a.model, None);
        remove(&store, &dir, &a.id).unwrap();
        assert_eq!(default_id(&store), OFFICIAL);
        assert_eq!(for_instance(&store, "i2").id, OFFICIAL);
        assert!(remove(&store, &dir, OFFICIAL).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn launch_and_server_env() {
        let (dir, store) = tmp_store("env");
        assert!(server_env(&store, &dir).is_none());
        assert!(launch_env(&dir, &builtin()).is_none());
        assert_eq!(thread_params(&dir, &builtin()), json!({}));
        let a = api(&store, &dir, "a", Some("glm-5.3"));
        let b = api(&store, &dir, "b", None);
        let n = a.provider_name();
        let cmd = launch_env(&dir, &a).unwrap().wrap("codex", Family::Posix);
        assert!(cmd.starts_with("LOOKLOOK_MODEL_API_KEY=\"$(cat "), "{cmd}");
        assert!(cmd.ends_with(&format!(" codex -c model_providers.{n}.name={n} -c model_providers.{n}.base_url=https://a.example.com/v1 -c model_providers.{n}.env_key=LOOKLOOK_MODEL_API_KEY -c model_providers.{n}.wire_api=responses -c model_provider={n} -c model=glm-5.3")), "{cmd}");
        let server = server_env(&store, &dir).unwrap().wrap("codex app-server", Family::Posix);
        for p in [&a, &b] {
            assert!(server.contains(&format!("{}=\"$(cat ", p.key_env())), "{server}");
            assert!(server.contains(&format!("-c model_providers.{}.env_key={}", p.provider_name(), p.key_env())), "{server}");
        }
        assert!(!server.contains("-c model_provider="), "app-server 不选默认服务商");
        let tui = tui_env(&dir, &b).unwrap().wrap("codex", Family::Posix);
        assert!(tui.ends_with(&format!("-c model_provider={}", b.provider_name())) && !tui.contains("$(cat"), "{tui}");
        assert_eq!(thread_params(&dir, &a), json!({ "modelProvider": n, "model": "glm-5.3" }));
        std::fs::remove_file(key_path(&dir, &b.id)).unwrap();
        assert!(launch_env(&dir, &b).is_none(), "Key 不在时按官方方式启动");
        assert!(!server_env(&store, &dir).unwrap().wrap("x", Family::Posix).contains(&b.provider_name()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn migrate_old_choice() {
        let (dir, store) = tmp_store("mig");
        let c = crate::model::Choice { provider_id: None, name: "Old".into(), base_url: "https://old.example.com/v1".into(), model: Some("kimi-k3".into()), selected_at: String::new() };
        store.set("model_provider.codex", &c).unwrap();
        std::fs::create_dir_all(dir.join("model")).unwrap();
        std::fs::write(dir.join("model/codex.key"), "sk-old\n").unwrap();
        migrate(&store, &dir);
        let p = get(&store, &default_id(&store)).unwrap();
        assert_eq!((p.name.as_str(), p.model.as_deref()), ("Old", Some("kimi-k3")));
        assert_eq!(std::fs::read_to_string(key_path(&dir, &p.id)).unwrap(), "sk-old");
        assert!(store.get::<crate::model::Choice>("model_provider.codex").unwrap().is_none());
        assert!(!dir.join("model/codex.key").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
