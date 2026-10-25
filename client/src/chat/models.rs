//! 模型菜单：和 Claude Code 自己的 `/model` 选择器用同一份数据，不打开选择器、不碰终端。
//!
//! - **模型目录**：Claude Code 从服务器取来、缓存在 `<配置目录>/cache/model-catalog/<组织>-…-cc.json` 的那份
//!   （`/model` 列出的就是它：`section: main` 是常用的几个，`overflow` 是“+N models”里的旧版本）。
//!   每个模型还带着它支持的思考强度。找不到时（第三方接口、没登录过）用几个别名兜底。
//! - **`~/.claude.json`** 里 Claude Code 缓存的额外选项（`additionalModelOptionsCache`）。
//! - **环境变量**：`ANTHROPIC_DEFAULT_{OPUS,SONNET,HAIKU,FABLE}_MODEL`（以及 `_NAME`、`_DESCRIPTION`）把别名指向别的模型，
//!   `ANTHROPIC_CUSTOM_MODEL_OPTION*` 加一项。来源：settings 里的 `env`、正在运行的 claude 进程的环境（Linux）、客户端自己的环境。
//! - settings 里的 `availableModels`：只留名单里的。
//! - **第三方接口**：用同一个地址和 Key 请求 `/v1/models`（Claude Code 自己发现模型也是这样），名单里没有的列在“服务商的其他模型”。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::Value;

/// 别名（`/model opus`）和它在目录里的家族
const FAMILIES: [&str; 4] = ["opus", "sonnet", "haiku", "fable"];
/// 服务商的模型列表缓存多久
const PROVIDER_TTL: Duration = Duration::from_secs(600);

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Opt {
    /// 选中时送给 `/model` 的值
    pub value: String,
    pub label: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// `main` 常用 / `more` 其他版本 / `provider` 服务商的其他模型
    pub section: &'static str,
    /// 实际对应的模型 ID（用来和回复里的模型对上）；别名没有指向时为空
    #[serde(skip_serializing_if = "String::is_empty")]
    pub model: String,
    /// 支持的思考强度；为空表示不支持调节
    pub efforts: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_effort: Option<String>,
}

/// 读配置用的上下文
pub struct Ctx<'a> {
    /// Claude Code 的配置目录（`~/.claude` 或 `CLAUDE_CONFIG_DIR`）
    pub config_dir: PathBuf,
    /// 用户级状态文件（`~/.claude.json`）
    pub state_file: PathBuf,
    /// 终端的工作目录（项目的 `.claude/settings*.json`）
    pub workdir: &'a Path,
    /// claude 进程的环境变量（读不到时为空）
    pub env: HashMap<String, String>,
}

impl Ctx<'_> {
    fn var(&self, k: &str) -> Option<String> {
        self.env.get(k).cloned().or_else(|| std::env::var(k).ok()).filter(|v| !v.trim().is_empty())
    }
}

pub fn config_dir() -> PathBuf {
    std::env::var_os("CLAUDE_CONFIG_DIR").filter(|v| !v.is_empty()).map(PathBuf::from).unwrap_or_else(|| home().join(".claude"))
}

pub fn state_file() -> PathBuf {
    match std::env::var_os("CLAUDE_CONFIG_DIR").filter(|v| !v.is_empty()) {
        Some(v) => PathBuf::from(v).join(".claude.json"),
        None => home().join(".claude.json"),
    }
}

fn home() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))
}

fn read_json(p: &Path) -> Option<Value> {
    serde_json::from_slice(&std::fs::read(p).ok()?).ok()
}

/// settings 文件（用户级、项目共享、项目本地），后面的覆盖前面的
fn settings(ctx: &Ctx) -> Vec<Value> {
    let proj = ctx.workdir.join(".claude");
    [ctx.config_dir.join("settings.json"), proj.join("settings.json"), proj.join("settings.local.json")].iter().filter_map(|p| read_json(p)).collect()
}

/// settings 里 `env` 的值合进来（进程自己的环境优先：Claude Code 也是进程环境优先）
pub fn merge_settings_env(ctx: &mut Ctx) {
    for s in settings(ctx) {
        if let Some(env) = s["env"].as_object() {
            for (k, v) in env {
                if let Some(v) = v.as_str() {
                    ctx.env.entry(k.clone()).or_insert_with(|| v.to_string());
                }
            }
        }
    }
}

/// 模型目录：优先当前登录的组织的那份，否则最近取的那份
fn catalog(ctx: &Ctx, state: &Value) -> Option<Value> {
    let dir = ctx.config_dir.join("cache").join("model-catalog");
    let org = state["oauthAccount"]["organizationUuid"].as_str().unwrap_or("");
    let mut best: Option<(bool, i64, Value)> = None;
    for e in std::fs::read_dir(dir).ok()?.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if !name.ends_with("-cc.json") {
            continue;
        }
        let Some(v) = read_json(&e.path()) else { continue };
        if !v["catalog"]["config"]["models"].is_array() {
            continue;
        }
        let key = (!org.is_empty() && name.starts_with(org), v["fetchedAt"].as_i64().unwrap_or(0));
        if best.as_ref().is_none_or(|(o, t, _)| key > (*o, *t)) {
            best = Some((key.0, key.1, v));
        }
    }
    best.map(|(_, _, v)| v["catalog"].clone())
}

fn efforts_of(m: &Value) -> Vec<String> {
    let opts = m["thinking"]["effort_options"].as_array().map(|a| a.iter().filter_map(|o| o["id"].as_str().map(String::from)).collect::<Vec<_>>()).unwrap_or_default();
    if opts.is_empty() {
        m["runtime"]["effort_levels"].as_array().map(|a| a.iter().filter_map(|o| o.as_str().map(String::from)).collect()).unwrap_or_default()
    } else {
        opts
    }
}

/// 模型 ID 去掉日期和上下文后缀，用来比较（`claude-haiku-4-5-20251001[1m]` → `claude-haiku-4-5`）
pub fn base_id(id: &str) -> String {
    let s = id.trim().to_ascii_lowercase();
    let s = s.split('[').next().unwrap_or("").to_string();
    match s.rsplit_once('-') {
        Some((head, tail)) if tail.len() == 8 && tail.chars().all(|c| c.is_ascii_digit()) => head.to_string(),
        _ => s,
    }
}

fn family_of(id: &str) -> Option<&'static str> {
    let low = id.to_ascii_lowercase();
    FAMILIES.into_iter().find(|f| low.contains(f))
}

/// Claude Code 的选项（不含服务商的）
pub fn options(ctx: &Ctx) -> Vec<Opt> {
    let state = read_json(&ctx.state_file).unwrap_or(Value::Null);
    let mut out: Vec<Opt> = vec![];
    let cat = catalog(ctx, &state);
    let models: Vec<Value> = cat.as_ref().and_then(|c| c["config"]["models"].as_array().cloned()).unwrap_or_default();
    let main: Vec<&Value> = models.iter().filter(|m| m["section"].as_str() != Some("overflow")).collect();
    // 默认：组织指定的默认模型，否则目录里第一个常用模型
    let default_model = state["orgModelDefaultCache"]["model"].as_str().map(String::from).or_else(|| main.first().and_then(|m| m["id"].as_str()).map(String::from)).unwrap_or_default();
    let default_row = models.iter().find(|m| m["id"].as_str() == Some(&default_model));
    out.push(Opt {
        value: "default".into(),
        label: "Default".into(),
        description: default_row.and_then(|m| m["name"].as_str()).map(String::from).unwrap_or_default(),
        section: "main",
        model: default_model.clone(),
        efforts: default_row.map(efforts_of).unwrap_or_default(),
        default_effort: default_row.and_then(|m| m["runtime"]["default_effort"].as_str()).map(String::from),
    });
    for m in &models {
        let Some(id) = m["id"].as_str() else { continue };
        out.push(Opt {
            value: id.to_string(),
            label: m["name"].as_str().unwrap_or(id).to_string(),
            description: m["description"].as_str().unwrap_or("").to_string(),
            section: if m["section"].as_str() == Some("overflow") { "more" } else { "main" },
            model: id.to_string(),
            efforts: efforts_of(m),
            default_effort: m["runtime"]["default_effort"].as_str().map(String::from),
        });
    }
    if models.is_empty() {
        // 没有目录：只给别名，名字和描述等环境变量或者回复来补
        for f in FAMILIES {
            out.push(Opt { value: f.into(), label: cap(f), description: String::new(), section: "main", model: String::new(), efforts: vec![], default_effort: None });
        }
    }
    // 额外的选项（例如 1M 上下文的版本）
    for o in state["additionalModelOptionsCache"].as_array().into_iter().flatten() {
        let (Some(v), Some(l)) = (o["value"].as_str(), o["label"].as_str()) else { continue };
        if o["disabled"].as_bool() == Some(true) || out.iter().any(|x| x.value == v) {
            continue;
        }
        let like = models.iter().find(|m| m["id"].as_str().is_some_and(|id| base_id(id) == base_id(v)));
        let label = if v.contains("[1m]") && !l.contains("1M") { format!("{l} · 1M") } else { l.to_string() };
        out.push(Opt {
            value: v.into(),
            label,
            description: o["description"].as_str().unwrap_or("").into(),
            section: "main",
            model: v.into(),
            efforts: like.map(efforts_of).unwrap_or_default(),
            default_effort: like.and_then(|m| m["runtime"]["default_effort"].as_str()).map(String::from),
        });
    }
    // 别名被环境变量指到了别的模型：常用里这个家族换成别名那一项
    for f in FAMILIES {
        let up = f.to_ascii_uppercase();
        let Some(target) = ctx.var(&format!("ANTHROPIC_DEFAULT_{up}_MODEL")) else { continue };
        out.retain(|o| !(o.section == "main" && o.value != "default" && family_of(&o.value) == Some(f)));
        let like = models.iter().find(|m| m["id"].as_str().is_some_and(|id| base_id(id) == base_id(&target)));
        out.push(Opt {
            value: f.into(),
            label: ctx.var(&format!("ANTHROPIC_DEFAULT_{up}_MODEL_NAME")).unwrap_or_else(|| target.clone()),
            description: ctx.var(&format!("ANTHROPIC_DEFAULT_{up}_MODEL_DESCRIPTION")).unwrap_or_default(),
            section: "main",
            model: target,
            efforts: like.map(efforts_of).unwrap_or_default(),
            default_effort: None,
        });
    }
    if let Some(id) = ctx.var("ANTHROPIC_CUSTOM_MODEL_OPTION") {
        if !out.iter().any(|o| o.value == id) {
            out.push(Opt {
                label: ctx.var("ANTHROPIC_CUSTOM_MODEL_OPTION_NAME").unwrap_or_else(|| id.clone()),
                description: ctx.var("ANTHROPIC_CUSTOM_MODEL_OPTION_DESCRIPTION").unwrap_or_default(),
                value: id.clone(),
                section: "main",
                model: id,
                efforts: vec![],
                default_effort: None,
            });
        }
    }
    // 管理员或用户限定的名单：别名、家族、完整 ID 都可能出现在里面
    let allow: Option<Vec<String>> = settings(ctx).iter().rev().find_map(|s| s["availableModels"].as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(|s| s.to_ascii_lowercase())).collect()));
    if let Some(allow) = allow {
        out.retain(|o| {
            o.value == "default"
                || allow.iter().any(|a| *a == o.value.to_ascii_lowercase() || (!o.model.is_empty() && base_id(a) == base_id(&o.model)) || (FAMILIES.contains(&a.as_str()) && family_of(&o.value).or(family_of(&o.model)) == Some(a.as_str())))
        });
    }
    // 常用的排前面（目录里的顺序就是 /model 里的顺序）
    out.sort_by_key(|o| match o.section {
        "main" => 0,
        "more" => 1,
        _ => 2,
    });
    out
}

/// 上下文窗口：settings 里给这个模型设的自动压缩窗口（`modelSettings.<id>.autoCompactWindow`），
/// 否则目录里这个模型的输入上限，都没有时按 20 万算。
pub fn context_window(ctx: &Ctx, model: &str) -> u64 {
    let base = base_id(model);
    for s in settings(ctx).iter().rev() {
        if let Some(w) = s["modelSettings"].as_object().and_then(|m| m.iter().find(|(k, _)| base_id(k) == base)).and_then(|(_, v)| v["autoCompactWindow"].as_u64()) {
            return w;
        }
    }
    let state = read_json(&ctx.state_file).unwrap_or(Value::Null);
    let from_catalog = catalog(ctx, &state).and_then(|c| {
        c["config"]["models"].as_array()?.iter().find(|m| m["id"].as_str().is_some_and(|id| base_id(id) == base)).and_then(|m| m["runtime"]["max_input_tokens"].as_u64())
    });
    if model.contains("[1m]") {
        return from_catalog.unwrap_or(1_000_000).max(1_000_000);
    }
    from_catalog.unwrap_or(200_000)
}

/// 订阅的用量（5 小时、7 天）：Claude Code 自己缓存在 `.claude.json` 里的（只认当前登录的账号的），
/// 和最近一次 [`refresh_limits`] 时它报出来的，哪个新用哪个（`.claude.json` 写不进去时，比如 Docker 里被挂成了目录，就只有后者）。
/// 不拿 `.credentials.json` 里的令牌去问 Anthropic：Anthropic 不允许第三方应用使用 claude.ai 的凭据（docs/CLAUDE_ACCOUNTS.md）。
pub fn limits(ctx: &Ctx) -> Value {
    let state = read_json(&ctx.state_file).unwrap_or(Value::Null);
    let c = &state["cachedUsageUtilization"];
    let cached = (!c["accountUuid"].is_null() && c["accountUuid"] == state["oauthAccount"]["accountUuid"]).then(|| {
        let u = &c["utilization"];
        let win = |k: &str| (u[k]["utilization"].clone(), u[k]["resets_at"].clone());
        (c["fetchedAtMs"].as_i64().unwrap_or(0), win("five_hour"), win("seven_day"))
    });
    let reported = REPORTED.lock().unwrap_or_else(|e| e.into_inner()).as_ref().and_then(|m| m.get(&ctx.config_dir).cloned());
    let Some((fetched, five, seven)) = cached.into_iter().chain(reported).max_by_key(|r| r.0) else { return Value::Null };
    // 可能是几天前取的：已经过了重置时间的窗口不再显示（数字早就不对了）
    let now = time::OffsetDateTime::now_utc();
    let pick = |(used, resets_at): Window| {
        let expired = resets_at.as_str().and_then(|s| time::OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339).ok()).is_some_and(|t| t <= now);
        if used.is_number() && !expired {
            serde_json::json!({ "used": used, "resets_at": resets_at })
        } else {
            Value::Null
        }
    };
    serde_json::json!({ "five_hour": pick(five), "seven_day": pick(seven), "fetched_at": fetched })
}

/// 一个窗口：用了百分之几、什么时候重置
type Window = (Value, Value);

/// [`refresh_limits`] 时 Claude Code 报出来的用量，按配置目录：（取到的时间，5 小时，7 天）
static REPORTED: Mutex<Option<HashMap<PathBuf, (i64, Window, Window)>>> = Mutex::new(None);

/// 用量缓存多旧就请 Claude Code 重新取
pub const LIMITS_STALE_MS: i64 = 2 * 60 * 1000;

/// 请 Claude Code 自己去取用量：`claude -p /usage`（本地命令，不调用模型、不消耗额度）。它会写进这个配置目录的
/// `.claude.json`，stream-json 输出里也有一份（`usage_report`），记下来，之后 [`limits`] 就能读到。
/// 不留对话记录、不启动 MCP 服务器；在 `cwd`（一个空目录）里运行。
/// 每个配置目录两分钟最多一次，同时只跑一个。`account_dir` 为 None 时是本机默认账号（不改 `CLAUDE_CONFIG_DIR`）。
pub async fn refresh_limits(shell: &str, config_dir: &Path, account_dir: Option<&Path>, cwd: &Path) {
    static LAST: Mutex<Option<HashMap<PathBuf, Instant>>> = Mutex::new(None);
    {
        let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
        let m = last.get_or_insert_with(HashMap::new);
        if m.get(config_dir).is_some_and(|t| t.elapsed() < Duration::from_millis(LIMITS_STALE_MS as u64)) {
            return;
        }
        m.insert(config_dir.to_path_buf(), Instant::now());
    }
    let Some(cli) = crate::agents::claude_cli(shell).await else { return };
    let _ = std::fs::create_dir_all(cwd);
    let env: Vec<(&str, Option<&std::ffi::OsStr>)> = account_dir.map(|d| ("CLAUDE_CONFIG_DIR", Some(d.as_os_str()))).into_iter().collect();
    let args = ["-p", "/usage", "--no-session-persistence", "--strict-mcp-config", "--output-format", "stream-json", "--verbose"];
    let mut cmd = crate::agents::cli_command(shell, &cli, &args, &env);
    cmd.current_dir(cwd).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::null()).kill_on_drop(true);
    match tokio::time::timeout(Duration::from_secs(30), cmd.output()).await {
        Ok(Ok(out)) if out.status.success() => match parse_usage_report(&String::from_utf8_lossy(&out.stdout)) {
            Some((five, seven)) => {
                REPORTED.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(config_dir.to_path_buf(), (crate::util::system_ms(), five, seven));
            }
            None => tracing::debug!("claude -p /usage 的输出里没有用量（旧版 Claude Code 没有 usage_report），只能读 .claude.json"),
        },
        r => tracing::debug!(result = ?r.map(|o| o.map(|o| o.status)), "claude -p /usage 没有成功"),
    }
}

/// `claude -p /usage --output-format stream-json` 输出里的 `usage_report.rate_limits.limits`：
/// `session` 是 5 小时的，`weekly_all` 是 7 天的（按 `kind` 认，不看显示的文字）。一个都没有时 None。
fn parse_usage_report(out: &str) -> Option<(Window, Window)> {
    let limits = out.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()).find_map(|v| v["usage_report"]["rate_limits"]["limits"].as_array().cloned())?;
    let win = |kind: &str| limits.iter().find(|l| l["kind"] == kind).map(|l| (l["percent"].clone(), l["resets_at"].clone())).unwrap_or_default();
    let (five, seven) = (win("session"), win("weekly_all"));
    (five.0.is_number() || seven.0.is_number()).then_some((five, seven))
}

/// 权限规则（各 settings 文件里的，按来源）和 MCP 服务器（`~/.claude.json` 的用户级、这个项目的，以及项目的 `.mcp.json`）
pub fn config(ctx: &Ctx) -> Value {
    let proj = ctx.workdir.join(".claude");
    let mut rules = vec![];
    for (src, p) in [("user", ctx.config_dir.join("settings.json")), ("project", proj.join("settings.json")), ("local", proj.join("settings.local.json"))] {
        let Some(s) = read_json(&p) else { continue };
        for kind in ["allow", "ask", "deny"] {
            for r in s["permissions"][kind].as_array().into_iter().flatten().filter_map(Value::as_str) {
                rules.push(serde_json::json!({ "kind": kind, "rule": r, "source": src }));
            }
        }
    }
    let state = read_json(&ctx.state_file).unwrap_or(Value::Null);
    let mut mcp = vec![];
    let mut add = |servers: &Value, src: &str| {
        for (name, v) in servers.as_object().into_iter().flatten() {
            let kind = v["type"].as_str().unwrap_or(if v["url"].is_string() { "http" } else { "stdio" });
            mcp.push(serde_json::json!({ "name": name, "type": kind, "source": src }));
        }
    };
    add(&state["mcpServers"], "user");
    let wd = ctx.workdir.to_string_lossy();
    add(&state["projects"][wd.as_ref()]["mcpServers"], "local");
    if let Some(m) = read_json(&ctx.workdir.join(".mcp.json")) {
        add(&m["mcpServers"], "project");
    }
    serde_json::json!({ "rules": rules, "mcp": mcp })
}

fn cap(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
}

// ---------------- 第三方接口的模型列表 ----------------

static PROVIDER_CACHE: Mutex<Option<(String, Instant, Vec<String>)>> = Mutex::new(None);

/// 服务商 `/v1/models` 里的模型 ID（缓存 10 分钟；取不到时为空，不报错）。
pub async fn provider_models(base_url: &str, token: &str) -> Vec<String> {
    let key = base_url.trim_end_matches('/').to_string();
    if let Some((k, at, list)) = PROVIDER_CACHE.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
        if *k == key && at.elapsed() < PROVIDER_TTL {
            return list.clone();
        }
    }
    let list = fetch_models(&key, token).await.unwrap_or_default();
    *PROVIDER_CACHE.lock().unwrap_or_else(|e| e.into_inner()) = Some((key, Instant::now(), list.clone()));
    list
}

async fn fetch_models(base: &str, token: &str) -> Option<Vec<String>> {
    let c = reqwest::Client::builder().timeout(Duration::from_secs(6)).redirect(reqwest::redirect::Policy::none()).build().ok()?;
    let r = c
        .get(format!("{base}/v1/models?limit=1000"))
        .header("x-api-key", token)
        .bearer_auth(token)
        .header("anthropic-version", "2023-06-01")
        .send()
        .await
        .ok()?;
    if !r.status().is_success() {
        tracing::debug!(status = %r.status(), "服务商 /v1/models 取不到");
        return None;
    }
    let v: Value = r.json().await.ok()?;
    let mut ids: Vec<String> = v["data"].as_array()?.iter().filter_map(|m| m["id"].as_str()).filter(|s| !s.is_empty() && s.len() <= 64).map(String::from).collect();
    ids.dedup();
    ids.truncate(200);
    Some(ids)
}

/// 服务商的模型里 Claude Code 选项还没有的，作为“服务商的其他模型”
pub fn provider_options(have: &[Opt], ids: &[String]) -> Vec<Opt> {
    ids.iter()
        .filter(|id| id.chars().all(|c| c.is_ascii_alphanumeric() || "._-[]/:".contains(c)))
        .filter(|id| !have.iter().any(|o| o.value == **id || (!o.model.is_empty() && base_id(&o.model) == base_id(id))))
        .map(|id| Opt { value: id.clone(), label: id.clone(), description: String::new(), section: "provider", model: id.clone(), efforts: vec![], default_effort: None })
        .collect()
}

// ---------------- claude 进程的环境变量 ----------------

/// 面板里的 shell（`pane_pid`）下面的 claude 进程的环境变量。只有 Linux 能直接读，别的系统返回空。
pub fn claude_env(pane_pid: u32) -> HashMap<String, String> {
    #[cfg(target_os = "linux")]
    {
        let Some(pid) = find_claude(pane_pid) else { return HashMap::new() };
        let Ok(raw) = std::fs::read(format!("/proc/{pid}/environ")) else { return HashMap::new() };
        raw.split(|b| *b == 0)
            .filter_map(|kv| {
                let s = String::from_utf8_lossy(kv);
                let (k, v) = s.split_once('=')?;
                (k.starts_with("ANTHROPIC_") || k.starts_with("CLAUDE_")).then(|| (k.to_string(), v.to_string()))
            })
            .collect()
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pane_pid;
        HashMap::new()
    }
}

#[cfg(target_os = "linux")]
fn find_claude(root: u32) -> Option<u32> {
    // 父进程 → 子进程
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    for e in std::fs::read_dir("/proc").ok()?.flatten() {
        let Some(pid) = e.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else { continue };
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else { continue };
        // `pid (comm) state ppid …`：comm 里可能有空格和括号，从最后一个 ')' 后面取
        let Some(ppid) = stat.rsplit_once(')').and_then(|(_, rest)| rest.split_whitespace().nth(1)).and_then(|s| s.parse::<u32>().ok()) else { continue };
        children.entry(ppid).or_default().push(pid);
    }
    let mut queue = vec![(root, 0)];
    while let Some((pid, depth)) = queue.pop() {
        let cmd = std::fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default();
        let argv0 = cmd.split(|b| *b == 0).next().map(|a| String::from_utf8_lossy(a).into_owned()).unwrap_or_default();
        let name = argv0.rsplit('/').next().unwrap_or("");
        if pid != root && (name == "claude" || std::fs::read_link(format!("/proc/{pid}/exe")).is_ok_and(|p| p.to_string_lossy().contains("/claude/versions/"))) {
            return Some(pid);
        }
        if depth < 3 {
            queue.extend(children.get(&pid).into_iter().flatten().map(|c| (*c, depth + 1)));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn write(p: &Path, v: &Value) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, v.to_string()).unwrap();
    }

    fn model(id: &str, name: &str, section: &str, efforts: &[&str]) -> Value {
        json!({ "id": id, "name": name, "description": format!("{name} desc"), "section": section,
            "thinking": { "type": "effort", "effort_options": efforts.iter().map(|e| json!({ "id": e })).collect::<Vec<_>>() },
            "runtime": { "default_effort": "medium" } })
    }

    #[test]
    fn limits_from_cache_or_usage_report() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = tmp.path().join(".claude");
        // .claude.json 写不进去（Docker 里被挂成了目录）：只有 `claude -p /usage` 报出来的
        let state = tmp.path().join(".claude.json");
        std::fs::create_dir_all(&state).unwrap();
        let ctx = Ctx { config_dir: cfg.clone(), state_file: state.clone(), workdir: tmp.path(), env: HashMap::new() };
        assert!(limits(&ctx).is_null());
        let out = [
            json!({ "type": "system", "subtype": "init" }).to_string(),
            json!({ "type": "assistant", "usage_report": { "rate_limits": { "limits": [
                { "kind": "session", "group": "session", "percent": 29, "resets_at": "2999-10-25T03:09:59.616161+00:00" },
                { "kind": "weekly_scoped", "group": "weekly", "percent": 5, "resets_at": "2999-10-27T06:59:59+00:00", "scope": { "model": { "display_name": "Fable" } } },
                { "kind": "weekly_all", "group": "weekly", "percent": 76, "resets_at": "2020-10-27T06:59:59+00:00" }
            ] } } }).to_string(),
        ]
        .join("\n");
        let (five, seven) = parse_usage_report(&out).unwrap();
        assert!(parse_usage_report("not json\n{}").is_none());
        REPORTED.lock().unwrap().get_or_insert_with(HashMap::new).insert(cfg.clone(), (100, five, seven));
        let v = limits(&ctx);
        assert_eq!(v["five_hour"]["used"], 29);
        assert!(v["seven_day"].is_null(), "已经过了重置时间");
        assert_eq!(v["fetched_at"], 100);

        // .claude.json 里的更新就用它的
        std::fs::remove_dir(&state).unwrap();
        let win = |p: u64| json!({ "utilization": p, "resets_at": "2999-01-01T00:00:00Z" });
        write(&state, &json!({ "oauthAccount": { "accountUuid": "a" }, "cachedUsageUtilization": { "accountUuid": "a", "fetchedAtMs": 200, "utilization": { "five_hour": win(40), "seven_day": win(80) } } }));
        let v = limits(&ctx);
        assert_eq!((v["five_hour"]["used"].clone(), v["seven_day"]["used"].clone(), v["fetched_at"].clone()), (json!(40), json!(80), json!(200)));
    }

    #[test]
    fn options_from_catalog_state_and_env() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = tmp.path().join(".claude");
        let state = tmp.path().join(".claude.json");
        write(&state, &json!({ "oauthAccount": { "organizationUuid": "org1" }, "additionalModelOptionsCache": [{ "value": "claude-fable-5-1[1m]", "label": "Fable", "description": "big" }] }));
        let models = vec![model("claude-opus-5-5", "Opus 5.5", "main", &["low", "medium", "high"]), model("claude-sonnet-5-5", "Sonnet 5.5", "main", &["low"]), model("claude-fable-5-1", "Fable 5.1", "main", &["high"]), model("claude-opus-4-6", "Opus 4.6", "overflow", &["low", "max"])];
        write(&cfg.join("cache/model-catalog/org1-abc-cc.json"), &json!({ "fetchedAt": 1, "catalog": { "config": { "models": models } } }));
        // 别的组织的那份更新，但不用它
        write(&cfg.join("cache/model-catalog/org2-abc-cc.json"), &json!({ "fetchedAt": 9, "catalog": { "config": { "models": [model("claude-x", "X", "main", &[])] } } }));
        let mut ctx = Ctx { config_dir: cfg.clone(), state_file: state.clone(), workdir: tmp.path(), env: HashMap::new() };
        let o = options(&ctx);
        let vals: Vec<&str> = o.iter().map(|x| x.value.as_str()).collect();
        assert_eq!(vals, ["default", "claude-opus-5-5", "claude-sonnet-5-5", "claude-fable-5-1", "claude-fable-5-1[1m]", "claude-opus-4-6"]);
        assert_eq!(o[0].description, "Opus 5.5");
        assert_eq!(o[0].model, "claude-opus-5-5");
        assert_eq!(o[4].label, "Fable · 1M");
        assert_eq!(o[4].efforts, ["high"], "1M 版本沿用同一个模型的思考强度");
        assert_eq!(o[5].section, "more");
        assert_eq!(o[5].efforts, ["low", "max"]);

        // 环境变量把 sonnet 指到服务商的模型；settings 的 env 也算
        write(&cfg.join("settings.json"), &json!({ "env": { "ANTHROPIC_DEFAULT_SONNET_MODEL_NAME": "GLM 4.6" } }));
        ctx.env.insert("ANTHROPIC_DEFAULT_SONNET_MODEL".into(), "glm-4.6".into());
        merge_settings_env(&mut ctx);
        let o = options(&ctx);
        assert!(!o.iter().any(|x| x.value == "claude-sonnet-5-5"));
        let s = o.iter().find(|x| x.value == "sonnet").unwrap();
        assert_eq!((s.label.as_str(), s.model.as_str()), ("GLM 4.6", "glm-4.6"));

        // 名单
        write(&tmp.path().join(".claude/settings.local.json"), &json!({ "availableModels": ["opus"] }));
        let o = options(&ctx);
        let vals: Vec<&str> = o.iter().map(|x| x.value.as_str()).collect();
        assert_eq!(vals, ["default", "claude-opus-5-5", "claude-opus-4-6"]);
    }

    #[test]
    fn fallback_aliases_and_provider() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = Ctx { config_dir: tmp.path().join("none"), state_file: tmp.path().join("none.json"), workdir: tmp.path(), env: HashMap::new() };
        let o = options(&ctx);
        let vals: Vec<&str> = o.iter().map(|x| x.value.as_str()).collect();
        assert_eq!(vals, ["default", "opus", "sonnet", "haiku", "fable"]);
        let p = provider_options(&o, &["glm-4.6".into(), "bad id".into(), "kimi-k2".into()]);
        assert_eq!(p.iter().map(|x| x.value.as_str()).collect::<Vec<_>>(), ["glm-4.6", "kimi-k2"]);
    }

    #[test]
    fn base_ids() {
        assert_eq!(base_id("claude-haiku-4-5-20251001"), "claude-haiku-4-5");
        assert_eq!(base_id("claude-fable-5-1[1m]"), "claude-fable-5-1");
        assert_eq!(base_id("Claude-Opus-5-5"), "claude-opus-5-5");
    }
}
