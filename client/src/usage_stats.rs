//! 用量统计：定时扫 Claude Code 和 Codex 的会话记录，按小时、助手、模型累计 token，按 models.dev 的
//! API 价格（[`crate::pricing`]）估算费用。订阅用户看到的是“按 API 价格折算”的数，界面上写明。
//!
//! - **Claude Code**：`<配置目录>/projects/**/*.jsonl`（另外登录的官方账号的 `projects/` 链到同一处），
//!   `assistant` 记录的 `message.usage`。同一条回复会写好几行（每个内容块一行），按 `message.id` + `requestId` 只算一次。
//! - **Codex**：`$CODEX_HOME/sessions/**/rollout-*.jsonl` 和 `archived_sessions/`。`token_count` 事件的 `last_token_usage`
//!   是一次请求的用量；`total_token_usage` 没变的是重复发的，不算。模型取前面最近的 `turn_context`，
//!   走的哪家取开头 `session_meta` 的 `model_provider`（看看的第三方接口是 `looklook-<配置 ID>`，见 codex_profiles.rs）。
//! - 接着以前的对话、分叉出来的对话会把以前的记录抄进新文件：每条用量记一个指纹，别的文件算过的不再算。
//! - 只重新读变过（长度或修改时间变了）的文件。删掉的会话记录（Claude Code 默认 30 天清理）统计照样留着。
//! - 结果存在 `<数据目录>/usage/stats.json`，只在本机，不上传。

use std::collections::{BTreeMap, HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

const SCAN_EVERY: Duration = Duration::from_secs(10 * 60);
const VERSION: u32 = 1;

/// 一个小时里一个模型的用量
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct Row {
    /// 第几个小时（Unix 秒 / 3600，UTC）
    h: i64,
    m: String,
    /// 不含缓存的输入
    #[serde(default)]
    i: u64,
    #[serde(default)]
    o: u64,
    /// 读缓存
    #[serde(default)]
    cr: u64,
    /// 写缓存（5 分钟）
    #[serde(default)]
    cw: u64,
    /// 写缓存（1 小时，价格是输入的两倍）
    #[serde(default)]
    c1: u64,
    /// 请求数
    #[serde(default)]
    n: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct FileState {
    len: u64,
    mtime: i64,
    agent: String,
    /// Codex 的 `model_provider`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    provider: Option<String>,
    rows: Vec<Row>,
    /// 这个文件算过的用量的指纹
    ids: Vec<u64>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct State {
    version: u32,
    #[serde(default)]
    scanned_at: String,
    files: BTreeMap<String, FileState>,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);
/// 同时只扫一遍
static SCANNING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn state_file(home: &Path) -> PathBuf {
    home.join("usage").join("stats.json")
}

fn fingerprint(parts: &[&str]) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    parts.hash(&mut h);
    h.finish()
}

fn hour_of(ts: &str) -> Option<i64> {
    time::OffsetDateTime::parse(ts, &time::format_description::well_known::Rfc3339).ok().map(|t| t.unix_timestamp().div_euclid(3600))
}

#[derive(Default)]
struct Parsed {
    provider: Option<String>,
    rows: HashMap<(i64, String), Row>,
    ids: Vec<u64>,
}

impl Parsed {
    fn add(&mut self, h: i64, model: &str, r: Row) {
        let e = self.rows.entry((h, model.to_string())).or_insert_with(|| Row { h, m: model.to_string(), ..Default::default() });
        e.i += r.i;
        e.o += r.o;
        e.cr += r.cr;
        e.cw += r.cw;
        e.c1 += r.c1;
        e.n += 1;
    }
}

fn n(v: &Value, k: &str) -> u64 {
    v[k].as_u64().unwrap_or(0)
}

/// Claude Code 的会话记录。`seen` 是别的文件算过的指纹
fn parse_claude(text: &str, seen: &HashSet<u64>) -> Parsed {
    // 同一条回复的几行：留输出最多的那行（流式写入时前面的行可能还不全）
    let mut best: HashMap<u64, (i64, String, Value)> = HashMap::new();
    for line in text.lines().filter(|l| l.contains("\"usage\"") && l.contains("\"assistant\"")) {
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        if v["type"] != "assistant" {
            continue;
        }
        let m = &v["message"];
        let (Some(model), Some(h)) = (m["model"].as_str(), v["timestamp"].as_str().and_then(hour_of)) else { continue };
        if model.starts_with('<') || !m["usage"].is_object() {
            continue;
        }
        let id = match m["id"].as_str() {
            Some(id) => fingerprint(&[id, v["requestId"].as_str().unwrap_or("")]),
            None => fingerprint(&[line]),
        };
        if seen.contains(&id) {
            continue;
        }
        let u = &m["usage"];
        if best.get(&id).is_none_or(|(_, _, old)| n(u, "output_tokens") >= n(old, "output_tokens")) {
            best.insert(id, (h, model.to_string(), u.clone()));
        }
    }
    let mut out = Parsed::default();
    for (id, (h, model, u)) in best {
        let c1 = n(&u["cache_creation"], "ephemeral_1h_input_tokens");
        let cw = n(&u, "cache_creation_input_tokens").saturating_sub(c1);
        out.add(h, &model, Row { i: n(&u, "input_tokens"), o: n(&u, "output_tokens"), cr: n(&u, "cache_read_input_tokens"), cw, c1, ..Default::default() });
        out.ids.push(id);
    }
    out
}

/// Codex 的会话记录
fn parse_codex(text: &str, seen: &HashSet<u64>) -> Parsed {
    let mut out = Parsed::default();
    let mut model = String::from("unknown");
    let mut last_total = None;
    for line in text.lines() {
        let kind = if line.contains("\"token_count\"") {
            "token_count"
        } else if line.contains("\"turn_context\"") {
            "turn_context"
        } else if line.contains("\"session_meta\"") {
            "session_meta"
        } else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        let p = &v["payload"];
        match kind {
            "session_meta" if v["type"] == "session_meta" => {
                if out.provider.is_none() {
                    out.provider = p["model_provider"].as_str().map(String::from);
                }
            }
            "turn_context" if v["type"] == "turn_context" => {
                if let Some(m) = p["model"].as_str() {
                    model = m.to_string();
                }
            }
            "token_count" if p["type"] == "token_count" => {
                let info = &p["info"];
                let total = info["total_token_usage"]["total_tokens"].as_u64();
                if total.is_none() || total == last_total {
                    continue;
                }
                last_total = total;
                let (Some(h), Some(ts)) = (v["timestamp"].as_str().and_then(hour_of), v["timestamp"].as_str()) else { continue };
                let id = fingerprint(&[ts, &total.unwrap_or(0).to_string()]);
                if seen.contains(&id) {
                    continue;
                }
                let u = &info["last_token_usage"];
                let cached = n(u, "cached_input_tokens");
                out.add(h, &model, Row { i: n(u, "input_tokens").saturating_sub(cached), o: n(u, "output_tokens"), cr: cached, cw: n(u, "cache_write_input_tokens"), ..Default::default() });
                out.ids.push(id);
            }
            _ => {}
        }
    }
    out
}

fn walk(dir: &Path, want: &dyn Fn(&Path) -> bool, out: &mut Vec<PathBuf>, depth: u32) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        // 软链接的目录（另外的账号目录里的 projects/）不跟进去：同一处只扫一遍
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_dir() && depth < 8 {
            walk(&p, want, out, depth + 1);
        } else if ft.is_file() && want(&p) {
            out.push(p);
        }
    }
}

fn sources() -> Vec<(PathBuf, &'static str)> {
    let jsonl = |p: &Path| p.extension().is_some_and(|e| e == "jsonl");
    let mut out = vec![];
    let mut files = vec![];
    walk(&crate::chat::models::config_dir().join("projects"), &jsonl, &mut files, 0);
    out.extend(files.drain(..).map(|p| (p, "claude")));
    let codex = crate::agents::codex_home();
    for d in ["sessions", "archived_sessions"] {
        walk(&codex.join(d), &|p| jsonl(p) && p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("rollout-")), &mut files, 0);
    }
    out.extend(files.into_iter().map(|p| (p, "codex")));
    out
}

fn mtime_of(m: &std::fs::Metadata) -> i64 {
    m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs() as i64).unwrap_or(0)
}

fn load(home: &Path) -> State {
    std::fs::read(state_file(home)).ok().and_then(|b| serde_json::from_slice::<State>(&b).ok()).filter(|s| s.version == VERSION).unwrap_or(State { version: VERSION, ..Default::default() })
}

/// 扫一遍（阻塞，放在 `spawn_blocking` 里跑）
fn scan_blocking(home: &Path) {
    let mut st = STATE.lock().unwrap_or_else(|e| e.into_inner()).take().unwrap_or_else(|| load(home));
    let mut changed = 0;
    for (path, agent) in sources() {
        let key = path.to_string_lossy().into_owned();
        let Ok(meta) = std::fs::metadata(&path) else { continue };
        let (len, mtime) = (meta.len(), mtime_of(&meta));
        if st.files.get(&key).is_some_and(|f| f.len == len && f.mtime == mtime) {
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else { continue };
        let text = String::from_utf8_lossy(&bytes);
        let seen: HashSet<u64> = st.files.iter().filter(|(k, _)| **k != key).flat_map(|(_, f)| f.ids.iter().copied()).collect();
        let parsed = if agent == "claude" { parse_claude(&text, &seen) } else { parse_codex(&text, &seen) };
        let mut rows: Vec<Row> = parsed.rows.into_values().collect();
        rows.sort_by(|a, b| (a.h, &a.m).cmp(&(b.h, &b.m)));
        st.files.insert(key, FileState { len, mtime, agent: agent.into(), provider: parsed.provider, rows, ids: parsed.ids });
        changed += 1;
    }
    st.scanned_at = crate::util::now_rfc3339();
    if changed > 0 {
        let file = state_file(home);
        if let Some(dir) = file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(b) = serde_json::to_vec(&st) {
            let _ = crate::util::write_private(&file, &b);
        }
        tracing::debug!(changed, "用量统计已更新");
    }
    *STATE.lock().unwrap_or_else(|e| e.into_inner()) = Some(st);
}

/// 马上扫一遍（界面上点“刷新”、定时任务）
pub async fn scan(home: PathBuf) {
    let _guard = SCANNING.lock().await;
    let _ = tokio::task::spawn_blocking(move || scan_blocking(&home)).await;
}

/// 后台：启动一会儿后扫一遍，之后每 10 分钟扫一遍
pub async fn run(home: PathBuf) {
    tokio::time::sleep(Duration::from_secs(30)).await;
    loop {
        scan(home.clone()).await;
        tokio::time::sleep(SCAN_EVERY).await;
    }
}

/// Codex 的 `model_provider` → models.dev 的服务商
fn codex_catalog(c: &crate::pricing::Catalog, store: &crate::store::Store, provider: Option<&str>) -> Option<String> {
    match provider? {
        "openai" => Some("openai".into()),
        p => {
            let id = p.strip_prefix(crate::codex_profiles::PROVIDER_PREFIX)?;
            let prof = crate::codex_profiles::get(store, id)?;
            prof.preset.as_deref().and_then(crate::model_presets::get).map(|x| x.catalog.to_string()).or_else(|| crate::pricing::provider_for_url(c, prof.base_url.as_deref()?))
        }
    }
}

fn cost_of(r: &Row, p: &crate::pricing::Cost) -> f64 {
    let read = p.cache_read.unwrap_or(p.input);
    let write = p.cache_write.unwrap_or(p.input * 1.25);
    (r.i as f64 * p.input + r.o as f64 * p.output + r.cr as f64 * read + r.cw as f64 * write + r.c1 as f64 * p.input * 2.0) / 1_000_000.0
}

/// 最近 `days` 天的统计。`tz` 是界面所在时区比 UTC 早多少分钟（东八区是 480），按当地的日期分天。
pub fn report(store: &crate::store::Store, days: u32, tz: i32) -> Value {
    let guard = STATE.lock().unwrap_or_else(|e| e.into_inner());
    let Some(st) = guard.as_ref() else { return json!({ "ready": false }) };
    let cat = crate::pricing::catalog();
    let now = time::OffsetDateTime::now_utc().unix_timestamp();
    let tz_secs = i64::from(tz.clamp(-14 * 60, 14 * 60)) * 60;
    let today = (now + tz_secs).div_euclid(86400);
    let first = today - i64::from(days.clamp(1, 366)) + 1;
    let date = |d: i64| time::OffsetDateTime::from_unix_timestamp(d * 86400).map(|t| t.date().to_string()).unwrap_or_default();

    #[derive(Default)]
    struct Sum {
        tokens: [u64; 5],
        requests: u64,
        cost: f64,
    }
    impl Sum {
        fn add(&mut self, r: &Row, cost: f64) {
            for (s, v) in self.tokens.iter_mut().zip([r.i, r.o, r.cr, r.cw + r.c1, 0]) {
                *s += v;
            }
            self.tokens[4] += r.i + r.o + r.cr + r.cw + r.c1;
            self.requests += r.n;
            self.cost += cost;
        }
        fn view(&self) -> Value {
            let [input, output, cache_read, cache_write, total] = self.tokens;
            json!({ "input": input, "output": output, "cache_read": cache_read, "cache_write": cache_write, "tokens": total, "requests": self.requests, "cost": (self.cost * 10000.0).round() / 10000.0 })
        }
    }
    let mut by_day: BTreeMap<i64, BTreeMap<String, Sum>> = BTreeMap::new();
    // (助手, 模型) → (用量, 价格来自哪家)
    let mut by_model: BTreeMap<(String, String), (Sum, Option<String>)> = BTreeMap::new();
    let mut total = Sum::default();
    let mut today_sum = Sum::default();
    let mut catalog_of: HashMap<Option<String>, Option<String>> = HashMap::new();
    for f in st.files.values() {
        let hint = match (&cat, f.agent.as_str()) {
            (Some(c), "codex") => catalog_of.entry(f.provider.clone()).or_insert_with(|| codex_catalog(c, store, f.provider.as_deref())).clone(),
            _ => None,
        };
        for r in &f.rows {
            let day = (r.h * 3600 + tz_secs).div_euclid(86400);
            if day < first {
                continue;
            }
            let priced = cat.as_ref().and_then(|c| crate::pricing::price(c, hint.as_deref(), &r.m));
            let cost = priced.as_ref().map(|(p, _)| cost_of(r, p)).unwrap_or(0.0);
            by_day.entry(day).or_default().entry(f.agent.clone()).or_default().add(r, cost);
            let e = by_model.entry((f.agent.clone(), r.m.clone())).or_default();
            e.0.add(r, cost);
            if let Some((_, pid)) = priced {
                e.1 = Some(pid);
            }
            total.add(r, cost);
            if day == today {
                today_sum.add(r, cost);
            }
        }
    }
    let names = |pid: &Option<String>| pid.as_ref().and_then(|p| cat.as_ref()?.providers.get(p).map(|x| x.name.clone()));
    let mut models: Vec<Value> = by_model
        .iter()
        .map(|((agent, model), (s, pid))| {
            let mut v = s.view();
            v["agent"] = json!(agent);
            v["model"] = json!(model);
            v["priced"] = json!(pid.is_some());
            v["price_from"] = json!(names(pid));
            v
        })
        .collect();
    models.sort_by(|a, b| b["cost"].as_f64().partial_cmp(&a["cost"].as_f64()).unwrap_or(std::cmp::Ordering::Equal).then(b["tokens"].as_u64().cmp(&a["tokens"].as_u64())));
    let days: Vec<Value> = (first..=today)
        .map(|d| {
            let agents = by_day.get(&d);
            let mut sum = Sum::default();
            let mut per = serde_json::Map::new();
            for (a, s) in agents.into_iter().flatten() {
                per.insert(a.clone(), s.view());
                sum.tokens.iter_mut().zip(s.tokens).for_each(|(x, y)| *x += y);
                sum.requests += s.requests;
                sum.cost += s.cost;
            }
            let mut v = sum.view();
            v["date"] = json!(date(d));
            v["agents"] = Value::Object(per);
            v
        })
        .collect();
    json!({
        "ready": true,
        "scanned_at": st.scanned_at,
        "pricing": cat.as_ref().map(|c| json!({ "source": "models.dev", "fetched_at": c.fetched_at })),
        "total": total.view(),
        "today": today_sum.view(),
        "days": days,
        "models": models,
    })
}

/// 启动时先把上次存的读进来（还没扫完也能先看到）
pub fn preload(home: &Path) {
    let mut g = STATE.lock().unwrap_or_else(|e| e.into_inner());
    if g.is_none() && state_file(home).exists() {
        *g = Some(load(home));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(v: Value) -> String {
        format!("{v}\n")
    }

    #[test]
    fn claude_dedupes_by_message() {
        let u = |out: u64| json!({ "input_tokens": 3, "output_tokens": out, "cache_read_input_tokens": 100, "cache_creation_input_tokens": 50, "cache_creation": { "ephemeral_1h_input_tokens": 20 } });
        let mut s = String::new();
        s += &line(json!({ "type": "assistant", "timestamp": "2026-10-27T10:17:26.913Z", "requestId": "r1", "message": { "id": "m1", "model": "claude-opus-5-5", "usage": u(1) } }));
        s += &line(json!({ "type": "assistant", "timestamp": "2026-10-27T10:17:27.000Z", "requestId": "r1", "message": { "id": "m1", "model": "claude-opus-5-5", "usage": u(9) } }));
        s += &line(json!({ "type": "assistant", "timestamp": "2026-10-27T11:00:00Z", "requestId": "r2", "message": { "id": "m2", "model": "<synthetic>", "usage": u(1) } }));
        s += &line(json!({ "type": "user", "timestamp": "2026-10-27T11:00:00Z", "message": { "usage": u(1) } }));
        let p = parse_claude(&s, &HashSet::new());
        assert_eq!(p.ids.len(), 1);
        let r = p.rows.values().next().unwrap();
        assert_eq!((r.i, r.o, r.cr, r.cw, r.c1, r.n), (3, 9, 100, 30, 20, 1));
        // 别的文件算过的不再算
        assert!(parse_claude(&s, &p.ids.iter().copied().collect()).rows.is_empty());
    }

    #[test]
    fn codex_skips_repeated_counts() {
        let tc = |ts: &str, total: u64, input: u64| {
            line(json!({ "timestamp": ts, "type": "event_msg", "payload": { "type": "token_count", "info": { "total_token_usage": { "total_tokens": total }, "last_token_usage": { "input_tokens": input, "cached_input_tokens": 40, "output_tokens": 7 } } } }))
        };
        let mut s = line(json!({ "timestamp": "2026-10-23T07:38:16Z", "type": "session_meta", "payload": { "model_provider": "looklook-ab12" } }));
        s += &line(json!({ "timestamp": "2026-10-23T07:38:28Z", "type": "turn_context", "payload": { "model": "glm-5.3" } }));
        s += &tc("2026-10-23T07:40:00Z", 100, 50);
        s += &tc("2026-10-23T07:40:01Z", 100, 50);
        s += &line(json!({ "timestamp": "2026-10-23T07:41:00Z", "type": "event_msg", "payload": { "type": "token_count", "info": null } }));
        s += &tc("2026-10-23T08:01:00Z", 200, 60);
        let p = parse_codex(&s, &HashSet::new());
        assert_eq!(p.provider.as_deref(), Some("looklook-ab12"));
        assert_eq!(p.ids.len(), 2);
        let mut rows: Vec<&Row> = p.rows.values().collect();
        rows.sort_by_key(|r| r.h);
        assert_eq!((rows[0].m.as_str(), rows[0].i, rows[0].cr, rows[0].o), ("glm-5.3", 10, 40, 7));
        assert_eq!(rows[1].i, 20);
    }

    /// 端到端：扫本机真实的会话记录，用 models.dev 的价格算一遍（不联网：价格从文件读）。
    /// `USAGE_E2E_PRICING=<models.dev 的 api.json> cargo test real_scan -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn real_scan() {
        let raw: Value = serde_json::from_slice(&std::fs::read(std::env::var("USAGE_E2E_PRICING").unwrap()).unwrap()).unwrap();
        crate::pricing::set_for_test(crate::pricing::parse_for_test(&raw));
        let home = std::env::temp_dir().join(format!("ll-usage-e2e-{}", std::process::id()));
        std::fs::create_dir_all(&home).unwrap();
        let t = std::time::Instant::now();
        scan_blocking(&home);
        let first = t.elapsed();
        let t = std::time::Instant::now();
        scan_blocking(&home);
        let store = crate::store::Store::open(&home.join("t.db")).unwrap();
        let r = report(&store, 30, 480);
        println!("首次 {first:?}，再扫 {:?}；文件 {}", t.elapsed(), STATE.lock().unwrap().as_ref().unwrap().files.len());
        println!("合计 {}\n今天 {}", r["total"], r["today"]);
        for m in r["models"].as_array().unwrap().iter().take(12) {
            println!("{} {} tokens={} cost={} 价格={}", m["agent"], m["model"], m["tokens"], m["cost"], m["price_from"]);
        }
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn cost_formula() {
        let p = crate::pricing::Cost { input: 3.0, output: 15.0, cache_read: Some(0.3), cache_write: Some(3.75) };
        let r = Row { i: 1_000_000, o: 1_000_000, cr: 1_000_000, cw: 1_000_000, c1: 1_000_000, ..Default::default() };
        assert!((cost_of(&r, &p) - (3.0 + 15.0 + 0.3 + 3.75 + 6.0)).abs() < 1e-9);
        let no_cache = crate::pricing::Cost { input: 1.0, output: 2.0, ..Default::default() };
        assert!((cost_of(&Row { cr: 1_000_000, ..Default::default() }, &no_cache) - 1.0).abs() < 1e-9, "没有缓存价时按输入价");
    }
}
