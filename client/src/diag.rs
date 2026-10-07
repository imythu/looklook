//! 错误记录与问题报告。
//!
//! - 错误记录：所有 WARN / ERROR 日志（以及管理台页面上的脚本错误）另存一份到 `logs/errors.log`（每行一条 JSON，
//!   超过 1 MB 轮换成 `errors.log.1`），内存里保留最近 300 条给“设置 → 问题与日志”显示。重启后从文件接上。
//! - 问题报告：用户填写描述，可选附带运行信息、最近的错误记录和运行日志（发送前可以预览全部内容），
//!   签名发给看看服务端；没登录或连不上时可以下载成文件自己发给我们。
//!   附带内容先脱敏：看起来像令牌/密钥的长串、访问码都换成 `[已隐藏]`。

use std::collections::VecDeque;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

const KEEP: usize = 300;
const FILE_MAX: u64 = 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    pub time: String,
    /// `ERROR` / `WARN` / `UI`（管理台页面上的脚本错误）
    pub level: String,
    pub message: String,
    /// 日志里的附加字段（`error=…` `id=…`），拼成一行
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub fields: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub target: String,
    /// 同一条消息连续出现的次数（例如通道连不上时每隔几秒重试一次），time 是最后一次的时间
    #[serde(default = "one", skip_serializing_if = "is_one")]
    pub count: u32,
}

fn one() -> u32 {
    1
}

fn is_one(n: &u32) -> bool {
    *n == 1
}

/// 判断“同一条消息”时忽略带数字的词（重试间隔 `1.5s` / `520ms`、端口、耗时每次都不一样）。
fn shape(e: &Entry) -> String {
    let words: Vec<&str> = e.message.split_whitespace().filter(|w| !w.chars().any(|c| c.is_ascii_digit())).collect();
    format!("{}|{}|{}", e.level, e.target, words.join(" "))
}

struct Log {
    path: PathBuf,
    ring: VecDeque<Entry>,
}

static LOG: OnceLock<Mutex<Log>> = OnceLock::new();

fn log() -> Option<std::sync::MutexGuard<'static, Log>> {
    LOG.get().map(|m| m.lock().unwrap_or_else(|e| e.into_inner()))
}

/// 启动时调用一次：指定文件并读回最近的记录。
pub fn init(logs_dir: &Path) {
    let path = logs_dir.join("errors.log");
    let mut ring = VecDeque::with_capacity(KEEP);
    for line in std::fs::read_to_string(&path).unwrap_or_default().lines() {
        if let Ok(e) = serde_json::from_str::<Entry>(line) {
            if ring.len() == KEEP {
                ring.pop_front();
            }
            ring.push_back(e);
        }
    }
    let _ = LOG.set(Mutex::new(Log { path, ring }));
}

pub fn record(e: Entry) {
    let Some(mut l) = log() else { return };
    // 和上一条是同一条消息：只累加次数，不再写文件（文件里留第一次的记录就够排查）
    if let Some(last) = l.ring.back_mut() {
        if shape(last) == shape(&e) {
            last.count += 1;
            last.time = e.time;
            last.fields = e.fields;
            return;
        }
    }
    if std::fs::metadata(&l.path).is_ok_and(|m| m.len() > FILE_MAX) {
        let _ = std::fs::rename(&l.path, l.path.with_extension("log.1"));
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&l.path) {
        if let Ok(s) = serde_json::to_string(&e) {
            let _ = writeln!(f, "{s}");
        }
    }
    if l.ring.len() == KEEP {
        l.ring.pop_front();
    }
    l.ring.push_back(e);
}

/// 最近的记录，新的在前。
pub fn recent() -> Vec<Entry> {
    log().map(|l| l.ring.iter().rev().cloned().collect()).unwrap_or_default()
}

pub fn clear() {
    if let Some(mut l) = log() {
        l.ring.clear();
        let _ = std::fs::remove_file(&l.path);
        let _ = std::fs::remove_file(l.path.with_extension("log.1"));
    }
}

/// 把 WARN / ERROR 事件记进错误记录的 tracing 层。
pub struct ErrorLayer;

#[derive(Default)]
struct Visitor {
    message: String,
    fields: Vec<String>,
}

impl tracing::field::Visit for Visitor {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "message" {
            self.message = value.to_string();
        } else {
            self.fields.push(format!("{}={value}", field.name()));
        }
    }
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.message = format!("{value:?}");
        } else {
            self.fields.push(format!("{}={value:?}", field.name()));
        }
    }
}

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for ErrorLayer {
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: tracing_subscriber::layer::Context<'_, S>) {
        let meta = event.metadata();
        if *meta.level() > tracing::Level::WARN {
            return;
        }
        let mut v = Visitor::default();
        event.record(&mut v);
        let target = meta.target();
        record(Entry {
            time: crate::util::now_rfc3339(),
            level: meta.level().to_string(),
            message: v.message.chars().take(2000).collect(),
            fields: v.fields.join(" ").chars().take(2000).collect(),
            // 自己的模块名没什么信息量，第三方库（rathole、hyper…）的才记
            target: if target.starts_with("looklook") { String::new() } else { target.to_string() },
            count: 1,
        });
    }
}

/// 管理台页面上的脚本错误（每分钟最多 20 条，防止页面出错时刷屏）。
pub fn record_ui(message: &str, detail: &str) {
    static WINDOW: Mutex<(i64, u32)> = Mutex::new((0, 0));
    let minute = crate::util::system_ms() / 60_000;
    {
        let mut w = WINDOW.lock().unwrap_or_else(|e| e.into_inner());
        if w.0 != minute {
            *w = (minute, 0);
        }
        w.1 += 1;
        if w.1 > 20 {
            return;
        }
    }
    record(Entry {
        time: crate::util::now_rfc3339(),
        level: "UI".into(),
        message: message.chars().take(1000).collect(),
        fields: detail.chars().take(4000).collect(),
        target: String::new(),
        count: 1,
    });
}

/// 错误记录的文本形式（报告与下载用）。
pub fn errors_text(entries: &[Entry]) -> String {
    entries
        .iter()
        .rev()
        .map(|e| {
            let mut s = format!("{} {:5} {}", e.time, e.level, e.message);
            if !e.target.is_empty() {
                s.push_str(&format!(" [{}]", e.target));
            }
            if !e.fields.is_empty() {
                s.push_str(&format!("  {}", e.fields));
            }
            if e.count > 1 {
                s.push_str(&format!("  (×{})", e.count));
            }
            s
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// 主日志 `logs/looklook.log` 的最后 `lines` 行。
pub fn log_tail(logs_dir: &Path, lines: usize) -> String {
    let path = logs_dir.join("looklook.log");
    let Ok(meta) = std::fs::metadata(&path) else { return String::new() };
    // 只读最后 512 KB，日志文件可能很大
    let text = {
        use std::io::{Read, Seek, SeekFrom};
        let mut f = match std::fs::File::open(&path) {
            Ok(f) => f,
            Err(_) => return String::new(),
        };
        let start = meta.len().saturating_sub(512 * 1024);
        let _ = f.seek(SeekFrom::Start(start));
        let mut buf = Vec::new();
        let _ = f.read_to_end(&mut buf);
        String::from_utf8_lossy(&buf).into_owned()
    };
    let all: Vec<&str> = text.lines().collect();
    all[all.len().saturating_sub(lines)..].join("\n")
}

/// 脱敏：长的 base64/十六进制串（令牌、密钥、签名）和给定的秘密字符串都换掉。
pub fn redact(text: &str, secrets: &[String]) -> String {
    let mut out = String::with_capacity(text.len());
    let mut word = String::new();
    let flush = |w: &mut String, out: &mut String| {
        let secretish = w.len() >= 32 && w.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '=')) && w.chars().any(|c| c.is_ascii_digit()) && w.chars().any(|c| c.is_ascii_alphabetic());
        if secretish {
            out.push_str("[已隐藏]");
        } else {
            out.push_str(w);
        }
        w.clear();
    };
    for c in text.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '=') {
            word.push(c);
        } else {
            flush(&mut word, &mut out);
            out.push(c);
        }
    }
    flush(&mut word, &mut out);
    for s in secrets.iter().filter(|s| s.len() >= 4) {
        out = out.replace(s.as_str(), "[已隐藏]");
    }
    out
}

/// 报告附带的全部内容（界面预览、发送、下载用同一份）。
#[derive(Debug, Clone, Serialize)]
pub struct Bundle {
    pub meta: Value,
    pub errors: String,
    pub logs: String,
}

impl Bundle {
    pub fn text(&self, description: &str) -> String {
        format!(
            "# 看看客户端问题报告 / Looklook client report\n\n## 描述 / Description\n{description}\n\n## 运行信息 / System\n{}\n\n## 错误记录 / Errors\n{}\n\n## 运行日志 / Log\n{}\n",
            serde_json::to_string_pretty(&self.meta).unwrap_or_default(),
            if self.errors.is_empty() { "—" } else { &self.errors },
            if self.logs.is_empty() { "—" } else { &self.logs },
        )
    }
}

pub fn bundle(meta: Value, logs_dir: &Path, secrets: &[String]) -> Bundle {
    let errors = errors_text(&recent().into_iter().take(200).collect::<Vec<_>>());
    // 与服务端上限一致（错误记录 128 KiB、日志 256 KiB），保证报告请求体不超过服务端限制。
    Bundle { meta, errors: tail_bytes(&redact(&errors, secrets), 128 * 1024), logs: tail_bytes(&redact(&log_tail(logs_dir, 400), secrets), 256 * 1024) }
}

/// 超过 `max` 字节时只保留末尾（最近的内容最有用），不在 UTF-8 字符中间切开。
fn tail_bytes(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut start = s.len() - max;
    while !s.is_char_boundary(start) {
        start += 1;
    }
    format!("…{}", &s[start..])
}

/// 运行信息（不含账户秘密）。
pub fn meta(app: &crate::gateway::Inner, instances: &[crate::instances::View]) -> Value {
    let states: Vec<Value> = instances
        .iter()
        .map(|v| json!({ "launch": v.row.launch, "shell": if v.row.shell.is_empty() { "auto" } else { "custom" }, "running": v.running, "task_alive": v.task_alive, "error": v.error }))
        .collect();
    json!({
        "version": crate::account::VERSION,
        "target": crate::paths::target(),
        "os": std::env::consts::OS,
        "os_version": os_version(),
        "capabilities": app.instances.capabilities(),
        "relay": crate::relay::status(),
        "account_allowed": app.account.gate().allowed(),
        "server": app.account.platform().base,
        "terminals": states,
        "shells": crate::shells::available().iter().map(|o| o.label.clone()).collect::<Vec<_>>(),
        "time": crate::util::now_rfc3339(),
    })
}

fn os_version() -> String {
    #[cfg(target_os = "linux")]
    {
        std::fs::read_to_string("/etc/os-release")
            .ok()
            .and_then(|s| s.lines().find_map(|l| l.strip_prefix("PRETTY_NAME=").map(|v| v.trim_matches('"').to_string())))
            .unwrap_or_default()
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("sw_vers").arg("-productVersion").output().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default()
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        std::process::Command::new("cmd")
            .args(["/C", "ver"])
            .creation_flags(0x0800_0000)
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        String::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_tokens_and_secrets() {
        let t = "token=AbCdEf0123456789AbCdEf0123456789xyz ok path=/home/me/project code=483920";
        let r = redact(t, &["483920".into()]);
        assert!(!r.contains("AbCdEf0123456789"), "{r}");
        assert!(!r.contains("483920"));
        assert!(r.contains("path=/home/me/project"), "普通路径不动：{r}");
        assert_eq!(redact("2026-10-01T09:36:12.220776Z INFO 启动", &[]), "2026-10-01T09:36:12.220776Z INFO 启动");
    }

    #[test]
    fn repeated_messages_share_a_shape() {
        let e = |m: &str| Entry { time: "t".into(), level: "ERROR".into(), message: m.into(), fields: String::new(), target: "rathole::client".into(), count: 1 };
        assert_eq!(shape(&e("Failed to connect to r1:2333. Retry in 1.39s...")), shape(&e("Failed to connect to r1:2333. Retry in 521.7ms...")));
        assert_ne!(shape(&e("Failed to connect")), shape(&e("Authentication failed")));
    }

    #[test]
    fn errors_text_is_oldest_first() {
        let e = |m: &str| Entry { time: "t".into(), level: "WARN".into(), message: m.into(), fields: String::new(), target: String::new(), count: 1 };
        assert_eq!(errors_text(&[e("new"), e("old")]), "t WARN  old\nt WARN  new");
    }
}
