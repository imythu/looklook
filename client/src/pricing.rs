//! 模型价格和模型目录：定时从 models.dev（开源的模型目录，`https://models.dev/api.json`）取，存在本机
//! `<数据目录>/pricing.json`，取不到时用上次存的。价格是美元 / 百万 token（models.dev 的单位）。
//!
//! 用在两处：用量统计按 API 价格估算费用（[`crate::usage_stats`]），常见服务商的快速配置列出这家现在有哪些模型
//! （[`crate::model_presets`]）。只留下用得到的字段（全量 6 MB，留下的不到十分之一）。

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

const URL: &str = "https://models.dev/api.json";
/// 多久取一次
const REFRESH: Duration = Duration::from_secs(24 * 3600);
/// 取失败后多久再试
const RETRY: Duration = Duration::from_secs(3600);

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Cost {
    #[serde(default)]
    pub input: f64,
    #[serde(default)]
    pub output: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<Cost>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub release_date: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Provider {
    pub name: String,
    /// 服务商的接口地址（OpenAI 或 Anthropic 格式，models.dev 的 `api`）：用来按接口地址认出服务商
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub api: String,
    pub models: BTreeMap<String, ModelInfo>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Catalog {
    pub fetched_at: String,
    pub providers: BTreeMap<String, Provider>,
}

static CURRENT: RwLock<Option<Arc<Catalog>>> = RwLock::new(None);

fn file(home: &Path) -> PathBuf {
    home.join("pricing.json")
}

/// 现在的目录；还没有（第一次启动、没联网）时 None
pub fn catalog() -> Option<Arc<Catalog>> {
    CURRENT.read().unwrap_or_else(|e| e.into_inner()).clone()
}

fn set(c: Catalog) {
    *CURRENT.write().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(c));
}

/// models.dev 的原始 JSON → 只留用得到的
fn parse(v: &Value, now: String) -> Option<Catalog> {
    let mut providers = BTreeMap::new();
    for (pid, p) in v.as_object()? {
        let Some(models) = p["models"].as_object() else { continue };
        let models: BTreeMap<String, ModelInfo> = models
            .iter()
            .map(|(mid, m)| {
                let info = ModelInfo {
                    name: m["name"].as_str().unwrap_or(mid).to_string(),
                    cost: m.get("cost").and_then(|c| serde_json::from_value::<Cost>(c.clone()).ok()),
                    release_date: m["release_date"].as_str().unwrap_or_default().to_string(),
                    context: m["limit"]["context"].as_u64(),
                };
                (mid.clone(), info)
            })
            .collect();
        providers.insert(pid.clone(), Provider { name: p["name"].as_str().unwrap_or(pid).to_string(), api: p["api"].as_str().unwrap_or_default().to_string(), models });
    }
    (!providers.is_empty()).then_some(Catalog { fetched_at: now, providers })
}

async fn fetch() -> Option<Catalog> {
    let c = reqwest::Client::builder().connect_timeout(Duration::from_secs(10)).timeout(Duration::from_secs(60)).build().ok()?;
    let r = c.get(URL).header("user-agent", concat!("looklook-client/", env!("CARGO_PKG_VERSION"))).send().await.ok()?;
    if !r.status().is_success() {
        tracing::debug!(status = %r.status(), "models.dev 取不到");
        return None;
    }
    let v: Value = r.json().await.ok()?;
    parse(&v, crate::util::now_rfc3339())
}

fn age(c: &Catalog) -> Duration {
    time::OffsetDateTime::parse(&c.fetched_at, &time::format_description::well_known::Rfc3339)
        .map(|t| Duration::from_secs((time::OffsetDateTime::now_utc() - t).whole_seconds().max(0) as u64))
        .unwrap_or(Duration::MAX)
}

/// 后台：先读本机存的，过期了（或没有）就去取；之后每天取一次。
pub async fn run(home: PathBuf) {
    if let Some(c) = std::fs::read(file(&home)).ok().and_then(|b| serde_json::from_slice::<Catalog>(&b).ok()) {
        set(c);
    }
    loop {
        let fresh = catalog().is_some_and(|c| age(&c) < REFRESH);
        let wait = if fresh {
            REFRESH.saturating_sub(catalog().map(|c| age(&c)).unwrap_or_default())
        } else if let Some(c) = fetch().await {
            if let Ok(b) = serde_json::to_vec(&c) {
                let _ = std::fs::write(file(&home), b);
            }
            tracing::info!(providers = c.providers.len(), "已更新 models.dev 的模型价格");
            set(c);
            REFRESH
        } else {
            RETRY
        };
        tokio::time::sleep(wait.max(Duration::from_secs(60))).await;
    }
}

fn host_of(url: &str) -> Option<String> {
    Some(url.split("://").nth(1)?.split(['/', ':']).next()?.to_ascii_lowercase())
}

/// 按接口地址认出 models.dev 里的服务商（主机名一样）。同一个主机有好几家（例如普通版和 Coding 套餐）时取第一个。
pub fn provider_for_url(c: &Catalog, base_url: &str) -> Option<String> {
    let host = host_of(base_url)?;
    c.providers.iter().find(|(_, p)| host_of(&p.api).as_deref() == Some(host.as_str())).map(|(id, _)| id.clone())
}

/// 没有服务商线索时按模型名先找这几家（国内用户多，有 `-cn` 的先用 `-cn`）
const PREFERRED: &[&str] = &[
    "anthropic", "openai", "google", "deepseek", "zhipuai", "moonshotai-cn", "minimax-cn", "alibaba-cn", "volcengine", "xiaomi", "zai", "moonshotai", "minimax", "alibaba", "xai", "mistral",
];

/// 记录里的模型名 → models.dev 的写法：小写，去掉 `[1m]` 这类后缀和 `厂商/` 前缀
fn normalize(model: &str) -> String {
    let m = model.trim().to_ascii_lowercase();
    let m = m.split('[').next().unwrap_or(&m);
    m.rsplit('/').next().unwrap_or(m).to_string()
}

/// 去掉末尾的日期（`-20250929`）
fn without_date(m: &str) -> Option<&str> {
    let (head, tail) = m.rsplit_once('-')?;
    (tail.len() == 8 && tail.chars().all(|c| c.is_ascii_digit())).then_some(head)
}

fn find_in<'a>(p: &'a Provider, model: &str) -> Option<&'a Cost> {
    let mut lower: HashMap<String, &ModelInfo> = HashMap::new();
    for (k, v) in &p.models {
        let k = normalize(k);
        // 目录里带日期的（`claude-sonnet-4-5-20250929`），记录里不带日期也认
        if let Some(short) = without_date(&k) {
            lower.entry(short.to_string()).or_insert(v);
        }
        lower.insert(k, v);
    }
    let pick = |m: &str| lower.get(m).and_then(|i| i.cost.as_ref());
    pick(model).or_else(|| without_date(model).and_then(pick))
}

/// 这个模型的价格：先在 `provider`（知道走的哪家时）里找，再按 [`PREFERRED`]，最后任何一家有价的。
/// 返回价格和用的哪家。
pub fn price(c: &Catalog, provider: Option<&str>, model: &str) -> Option<(Cost, String)> {
    let model = normalize(model);
    if model.is_empty() || model.starts_with('<') {
        return None;
    }
    let try_p = |id: &str| c.providers.get(id).and_then(|p| find_in(p, &model)).map(|cost| (*cost, id.to_string()));
    if let Some(found) = provider.and_then(try_p) {
        return Some(found);
    }
    PREFERRED.iter().find_map(|id| try_p(id)).or_else(|| {
        c.providers.iter().filter_map(|(id, p)| find_in(p, &model).filter(|cost| cost.input > 0.0 || cost.output > 0.0).map(|cost| (*cost, id.clone()))).next()
    })
}

#[cfg(test)]
pub fn parse_for_test(v: &Value) -> Catalog {
    parse(v, crate::util::now_rfc3339()).unwrap()
}

#[cfg(test)]
pub fn set_for_test(c: Catalog) {
    set(c);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample() -> Catalog {
        let v = json!({
            "anthropic": { "name": "Anthropic", "models": { "claude-sonnet-4-5-20250929": { "name": "Claude Sonnet 4.5", "cost": { "input": 3, "output": 15, "cache_read": 0.3, "cache_write": 3.75 } } } },
            "zhipuai": { "name": "Zhipu AI", "api": "https://open.bigmodel.cn/api/paas/v4", "models": { "glm-5.3": { "name": "GLM-5.3", "cost": { "input": 1.2, "output": 4.2 } } } },
            "zhipuai-coding-plan": { "name": "Zhipu Coding", "api": "https://open.bigmodel.cn/api/coding/paas/v4", "models": { "glm-5.3": { "name": "GLM-5.3", "cost": { "input": 0, "output": 0 } } } },
            "openrouter": { "name": "OpenRouter", "api": "https://openrouter.ai/api/v1", "models": { "moonshotai/kimi-k3": { "name": "Kimi K3", "cost": { "input": 3, "output": 15 } } } },
            "broken": { "name": "x" }
        });
        parse(&v, "2026-10-27T00:00:00Z".into()).unwrap()
    }

    #[test]
    fn lookup() {
        let c = sample();
        assert_eq!(c.providers.len(), 4);
        let (cost, p) = price(&c, None, "claude-sonnet-4-5-20250929").unwrap();
        assert_eq!((cost.input, cost.cache_read, p.as_str()), (3.0, Some(0.3), "anthropic"));
        assert_eq!(price(&c, None, "claude-sonnet-4-5").unwrap().1, "anthropic", "不带日期也认");
        assert_eq!(price(&c, None, "glm-5.3").unwrap().1, "zhipuai");
        assert_eq!(price(&c, Some("zhipuai-coding-plan"), "glm-5.3").unwrap().0.input, 0.0, "知道是套餐时按套餐算");
        assert_eq!(price(&c, None, "kimi-k3").unwrap().1, "openrouter", "常用的几家都没有时找任何一家有价的");
        assert!(price(&c, None, "<synthetic>").is_none());
        assert!(price(&c, None, "nope").is_none());
        assert_eq!(provider_for_url(&c, "https://open.bigmodel.cn/api/anthropic").as_deref(), Some("zhipuai"));
        assert_eq!(provider_for_url(&c, "https://openrouter.ai/api"), Some("openrouter".into()));
        assert_eq!(normalize("Claude-Opus-5-5[1m]"), "claude-opus-5-5");
    }
}
