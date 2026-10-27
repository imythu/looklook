//! 第三方接口的套餐用量（docs/PLAN_2026-10-27.md §4）：按接口地址认出服务商，用这个配置自己的 Key 直接问服务商
//! （Key 本来就是发给这家的，不经过看看的服务器）。
//!
//! 这些查询接口都不是正式公开的稳定接口：解析宽松，取不到就当没有（不报错），界面标明数据来自服务商。
//! 结果和官方订阅一样是 `{five_hour, seven_day}`（`used` 是百分比，`resets_at` 是 RFC 3339），另带 `provider`。
//! 同一个地址和 Key 两分钟内只问一次。

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

const CACHE: Duration = Duration::from_secs(120);
const TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Vendor {
    Glm,
    Kimi,
    Minimax,
}

/// 接口地址 → 服务商、查询地址、给人看的名字
fn vendor(base_url: &str) -> Option<(Vendor, String, &'static str)> {
    let host = base_url.split("://").nth(1)?.split(['/', ':']).next()?.to_ascii_lowercase();
    let is = |d: &str| host == d || host.ends_with(&format!(".{d}"));
    if is("bigmodel.cn") {
        return Some((Vendor::Glm, "https://open.bigmodel.cn/api/monitor/usage/quota/limit".into(), "智谱 GLM"));
    }
    if is("z.ai") {
        return Some((Vendor::Glm, "https://api.z.ai/api/monitor/usage/quota/limit".into(), "Z.ai"));
    }
    if is("kimi.com") || is("kimi.ai") || is("moonshot.cn") {
        let base = if is("kimi.ai") { "https://api.kimi.ai" } else { "https://api.kimi.com" };
        return Some((Vendor::Kimi, format!("{base}/coding/v1/usages"), "Kimi"));
    }
    if is("minimaxi.com") {
        return Some((Vendor::Minimax, "https://www.minimaxi.com/v1/api/openplatform/coding_plan/remains".into(), "MiniMax"));
    }
    if is("minimax.io") {
        return Some((Vendor::Minimax, "https://www.minimax.io/v1/api/openplatform/coding_plan/remains".into(), "MiniMax"));
    }
    None
}

/// 一个第三方配置的套餐用量，给界面的样子：`{state: ok|no_limits|unsupported, limits}`
pub async fn usage_view(base_url: Option<&str>, key_file: &std::path::Path) -> Value {
    let Some(url) = base_url.filter(|u| supported(u)) else { return json!({ "state": "unsupported" }) };
    let key = std::fs::read_to_string(key_file).unwrap_or_default();
    if key.trim().is_empty() {
        return json!({ "state": "no_limits" });
    }
    match fetch(url, key.trim()).await {
        Some(limits) => json!({ "state": "ok", "limits": limits }),
        None => json!({ "state": "no_limits" }),
    }
}

/// 这个地址能不能查用量（界面上显示“查用量”按钮）
pub fn supported(base_url: &str) -> bool {
    vendor(base_url).is_some()
}

type Cache = HashMap<String, (Instant, Option<Value>)>;
static CACHED: Mutex<Option<Cache>> = Mutex::new(None);

/// 查用量。不支持的服务商、没取到、看不懂时 None
pub async fn fetch(base_url: &str, key: &str) -> Option<Value> {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    let (v, url, name) = vendor(base_url)?;
    let id = format!("{url}\n{:x}", Sha256::digest(key.as_bytes()));
    if let Some((at, got)) = CACHED.lock().unwrap_or_else(|e| e.into_inner()).as_ref().and_then(|m| m.get(&id)) {
        if at.elapsed() < CACHE {
            return got.clone();
        }
    }
    // 智谱的 Key 直接放在 Authorization 里（不带 Bearer），另外两家是 Bearer
    let auth = if v == Vendor::Glm { key.to_string() } else { format!("Bearer {key}") };
    let body = async {
        let client = CLIENT.get_or_init(|| reqwest::Client::builder().connect_timeout(Duration::from_secs(5)).build().unwrap_or_default());
        let r = client.get(&url).header("Authorization", auth).header("Accept", "application/json").timeout(TIMEOUT).send().await.ok()?;
        if !r.status().is_success() {
            tracing::debug!(status = %r.status(), provider = name, "查第三方接口的用量失败");
            return None;
        }
        r.json::<Value>().await.ok()
    }
    .await;
    let now = crate::util::system_ms();
    let got = body.and_then(|b| parse(v, &b, now)).map(|mut l| {
        l["provider"] = json!(name);
        l["fetched_at"] = json!(now);
        l
    });
    CACHED.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(id, (Instant::now(), got.clone()));
    got
}

fn parse(v: Vendor, b: &Value, now_ms: i64) -> Option<Value> {
    let (five, week) = match v {
        Vendor::Glm => glm(b),
        Vendor::Kimi => kimi(b),
        Vendor::Minimax => minimax(b),
    };
    // 已经过了重置时间的不显示（数字不对了）
    let live = |w: Option<Value>| w.filter(|w| w["resets_at"].as_str().and_then(parse_rfc3339).is_none_or(|t| t > now_ms));
    let (five, week) = (live(five), live(week));
    (five.is_some() || week.is_some()).then(|| json!({ "five_hour": five, "seven_day": week }))
}

fn window(used: f64, resets_at: Option<String>) -> Option<Value> {
    used.is_finite().then(|| json!({ "used": (used.clamp(0.0, 100.0) * 10.0).round() / 10.0, "resets_at": resets_at }))
}

fn num(v: &Value) -> Option<f64> {
    v.as_f64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
}

/// 重置时间：毫秒、秒或者字符串时间 → RFC 3339
fn reset_time(v: &Value) -> Option<String> {
    let fmt = |ms: i64| time::OffsetDateTime::from_unix_timestamp_nanos(ms as i128 * 1_000_000).ok()?.format(&time::format_description::well_known::Rfc3339).ok();
    match v {
        Value::Number(n) => {
            let n = n.as_f64()? as i64;
            fmt(if n > 100_000_000_000 { n } else { n * 1000 })
        }
        Value::String(s) if s.chars().all(|c| c.is_ascii_digit()) && !s.is_empty() => reset_time(&json!(s.parse::<i64>().ok()?)),
        Value::String(s) => parse_rfc3339(s).and_then(fmt),
        _ => None,
    }
}

fn parse_rfc3339(s: &str) -> Option<i64> {
    let t = time::OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339).ok()?;
    Some((t.unix_timestamp_nanos() / 1_000_000) as i64)
}

/// 智谱：`data.limits[]` 里 `type=TOKENS_LIMIT` 的，`unit=3` 是 5 小时、`unit=6` 是每周；`TIME_LIMIT` 是联网工具的，不算
fn glm(b: &Value) -> (Option<Value>, Option<Value>) {
    let limits = b["data"]["limits"].as_array().or_else(|| b["limits"].as_array());
    let pick = |unit: i64| {
        let l = limits?.iter().find(|l| l["type"] == "TOKENS_LIMIT" && l["unit"].as_i64() == Some(unit))?;
        window(num(&l["percentage"])?, reset_time(&l["nextResetTime"]))
    };
    (pick(3), pick(6))
}

/// Kimi：`usages.limit_5h` / `limit_7d` 的 `used_ratio`（0–1）、`reset_time`；
/// 也认另一种写法：`limits[]`（`window.duration` 分钟，`detail.limit` / `remaining` / `resetTime`）加上整体的 `usage`（每周）
fn kimi(b: &Value) -> (Option<Value>, Option<Value>) {
    let u = &b["usages"];
    let ratio = |w: &Value| window(num(&w["used_ratio"])? * 100.0, reset_time(&w["reset_time"]));
    let (five, week) = (ratio(&u["limit_5h"]), ratio(&u["limit_7d"]));
    if five.is_some() || week.is_some() {
        return (five, week);
    }
    let counted = |d: &Value| {
        let (limit, remaining) = (num(&d["limit"])?, num(&d["remaining"])?);
        (limit > 0.0).then_some(())?;
        window((limit - remaining) / limit * 100.0, reset_time(&d["resetTime"]).or_else(|| reset_time(&d["reset_time"])))
    };
    let five = b["limits"].as_array().and_then(|a| a.iter().find(|l| num(&l["window"]["duration"]) == Some(300.0)).and_then(|l| counted(&l["detail"])));
    (five, counted(&b["usage"]))
}

/// MiniMax：`model_remains[0]` 的 `current_interval_usage_count` 是**剩余**次数，`current_interval_total_count` 是总数，
/// `end_time` 是这个 5 小时窗口的结束时间。先看 `base_resp.status_code`
fn minimax(b: &Value) -> (Option<Value>, Option<Value>) {
    if b["base_resp"]["status_code"].as_i64().is_some_and(|c| c != 0) {
        return (None, None);
    }
    let five = (|| {
        let m = b["model_remains"].as_array()?.first()?;
        let (total, left) = (num(&m["current_interval_total_count"])?, num(&m["current_interval_usage_count"])?);
        (total > 0.0).then_some(())?;
        window((total - left) / total * 100.0, reset_time(&m["end_time"]))
    })();
    (five, None)
}

/// 本机的 Ollama（docs/PLAN_2026-10-27.md §4）：0.14 起支持 Anthropic 格式，可以当 Claude Code 的第三方接口用（额度用完时兜底）。
/// 没在运行时 None；`new_enough` 为 false 时界面提示升级
pub async fn ollama() -> Option<Value> {
    let c = reqwest::Client::builder().no_proxy().timeout(Duration::from_millis(1500)).build().ok()?;
    let get = |path: &str| {
        let req = c.get(format!("{OLLAMA}{path}"));
        async move { req.send().await.ok()?.json::<Value>().await.ok() }
    };
    let version = get("/api/version").await?["version"].as_str()?.to_string();
    let tags = get("/api/tags").await.unwrap_or_default();
    let models: Vec<&str> = tags["models"].as_array().map(|a| a.iter().filter_map(|m| m["name"].as_str()).collect()).unwrap_or_default();
    Some(json!({ "base_url": OLLAMA, "version": version, "new_enough": crate::chat::version_at_least(&version, (0, 14, 0)), "models": models }))
}

/// 本机 Ollama 的地址（`check_base_url` 允许本机的 http）
pub const OLLAMA: &str = "http://localhost:11434";

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_761_500_000_000;

    #[test]
    fn vendors_by_url() {
        assert_eq!(vendor("https://open.bigmodel.cn/api/anthropic").map(|v| v.0), Some(Vendor::Glm));
        assert_eq!(vendor("https://api.z.ai/api/anthropic").map(|v| v.1), Some("https://api.z.ai/api/monitor/usage/quota/limit".into()));
        assert_eq!(vendor("https://api.kimi.com/coding/").map(|v| v.1), Some("https://api.kimi.com/coding/v1/usages".into()));
        assert_eq!(vendor("https://api.minimaxi.com/anthropic").map(|v| v.0), Some(Vendor::Minimax));
        assert_eq!(vendor("https://api.minimax.io/anthropic").map(|v| v.1), Some("https://www.minimax.io/v1/api/openplatform/coding_plan/remains".into()));
        assert!(vendor("https://notbigmodel.cn/x").is_none());
        assert!(vendor("https://api.deepseek.com/anthropic").is_none());
        assert!(vendor("http://localhost:11434").is_none());
    }

    #[test]
    fn glm_tokens_windows() {
        let b = json!({ "code": 200, "data": { "limits": [
            { "type": "TIME_LIMIT", "unit": 5, "percentage": 99 },
            { "type": "TOKENS_LIMIT", "unit": 3, "number": 5, "percentage": 37, "nextResetTime": NOW + 3_600_000 },
            { "type": "TOKENS_LIMIT", "unit": 6, "percentage": "12.5", "nextResetTime": NOW + 86_400_000 },
        ] } });
        let l = parse(Vendor::Glm, &b, NOW).unwrap();
        assert_eq!(l["five_hour"]["used"], 37.0);
        assert_eq!(l["seven_day"]["used"], 12.5);
        assert_eq!(l["five_hour"]["resets_at"].as_str().and_then(parse_rfc3339), Some(NOW + 3_600_000));
        // 已经过了重置时间：不显示
        let old = json!({ "data": { "limits": [{ "type": "TOKENS_LIMIT", "unit": 3, "percentage": 80, "nextResetTime": NOW - 1000 }] } });
        assert!(parse(Vendor::Glm, &old, NOW).is_none());
    }

    #[test]
    fn kimi_both_shapes() {
        let a = json!({ "usages": { "limit_5h": { "used_ratio": 0.25, "reset_time": "2025-10-27T00:00:00Z" }, "limit_7d": { "used_ratio": 0.5 } } });
        let l = parse(Vendor::Kimi, &a, NOW).unwrap();
        assert_eq!((l["five_hour"]["used"].as_f64(), l["seven_day"]["used"].as_f64()), (Some(25.0), Some(50.0)));
        let b = json!({
            "usage": { "limit": "1000", "remaining": "900", "resetTime": "2025-11-01T00:00:00Z" },
            "limits": [{ "window": { "duration": 300, "timeUnit": "TIME_UNIT_MINUTE" }, "detail": { "limit": "100", "remaining": "40" } }],
        });
        let l = parse(Vendor::Kimi, &b, NOW).unwrap();
        assert_eq!((l["five_hour"]["used"].as_f64(), l["seven_day"]["used"].as_f64()), (Some(60.0), Some(10.0)));
        assert!(parse(Vendor::Kimi, &json!({ "error": "unauthorized" }), NOW).is_none());
    }

    #[test]
    fn minimax_counts_remaining() {
        let b = json!({ "base_resp": { "status_code": 0 }, "model_remains": [{ "current_interval_total_count": 600, "current_interval_usage_count": 450, "end_time": NOW + 60_000 }] });
        let l = parse(Vendor::Minimax, &b, NOW).unwrap();
        assert_eq!(l["five_hour"]["used"], 25.0);
        assert!(l["seven_day"].is_null());
        let err = json!({ "base_resp": { "status_code": 1004, "status_msg": "cookie is missing" } });
        assert!(parse(Vendor::Minimax, &err, NOW).is_none());
    }
}
