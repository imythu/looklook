//! Codex：读 rollout 文件，写入走 Codex 自己的 app-server。
//!
//! 读：`$CODEX_HOME/sessions/YYYY/MM/DD/rollout-<时间>-<id>.jsonl`（默认 `~/.codex`）。第一行是
//! `session_meta`；对话在 `response_item` 里：`message`（user / assistant / developer）、
//! `function_call` / `custom_tool_call` 及其 `*_output`、`reasoning`（加密，不迁移）、`compaction`（加密摘要）。
//! Codex 会把环境、AGENTS.md、应用状态等上下文作为“用户消息”注入（`<environment_context>` 这类
//! 整段用标签包起来的文字），这些不是用户说的话，读的时候去掉。子代理的会话不列出。
//!
//! 写：启动 `codex app-server`（JSON-RPC，按行收发），`thread/start` 建一个新会话，
//! `thread/inject_items` 把历史作为 Responses API 的条目放进去，`thread/name/set` 设标题。
//! 落盘、格式都由 Codex 自己负责，`codex resume <id>` 就能接着聊。

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

use super::{home, parse_ms, title_from, Conversation, Harness, Item, Manifest, Migrated, Summary};

pub fn codex_home() -> PathBuf {
    std::env::var_os("CODEX_HOME").map(PathBuf::from).unwrap_or_else(|| home().join(".codex"))
}

pub fn rollouts(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            rollouts(&p, out);
        } else if p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("rollout-") && n.ends_with(".jsonl")) {
            out.push(p);
        }
    }
}

/// `session_index.jsonl`：Codex 记下的会话名（后写的覆盖先写的）。
fn names(home: &Path) -> HashMap<String, String> {
    let mut m = HashMap::new();
    let Ok(f) = std::fs::File::open(home.join("session_index.jsonl")) else { return m };
    for line in BufReader::new(f).lines().map_while(Result::ok) {
        if let Ok(v) = serde_json::from_str::<Value>(&line) {
            if let (Some(id), Some(n)) = (v["id"].as_str(), v["thread_name"].as_str().filter(|n| !n.trim().is_empty())) {
                m.insert(id.to_string(), n.trim().to_string());
            }
        }
    }
    m
}

pub fn is_subagent(meta: &Value) -> bool {
    meta["source"].get("subagent").is_some() || meta["thread_source"].as_str() == Some("subagent")
}

pub fn list(home: &Path) -> Result<Vec<Summary>> {
    let mut files = Vec::new();
    rollouts(&home.join("sessions"), &mut files);
    let names = names(home);
    let mut out = Vec::new();
    for p in files {
        let Ok(f) = std::fs::File::open(&p) else { continue };
        let mut r = BufReader::new(f);
        let mut first = String::new();
        if r.read_line(&mut first).is_err() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(&first) else { continue };
        if v["type"] != "session_meta" || is_subagent(&v["payload"]) {
            continue;
        }
        let meta = &v["payload"];
        let Some(id) = meta["id"].as_str() else { continue };
        let updated_ms = std::fs::metadata(&p)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        // 没有会话名时看开头一段里的第一条用户消息（不读整个文件，长会话可能有几十 MB）
        let title = names.get(id).cloned().or_else(|| {
            let mut head = String::new();
            let _ = r.by_ref().take(256 * 1024).read_to_string(&mut head);
            let mut m = Manifest::default();
            let items: Vec<Item> = head.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()).flat_map(|v| items_of(&v, 0, &mut m)).collect();
            title_from(&items)
        });
        out.push(Summary {
            harness: Harness::Codex,
            id: id.to_string(),
            title,
            cwd: meta["cwd"].as_str().map(str::to_string),
            updated_ms,
        });
    }
    Ok(out)
}

pub fn find(home: &Path, id: &str) -> Result<PathBuf> {
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        bail!("会话 id 不对 / Invalid session id: {id}");
    }
    let mut files = Vec::new();
    rollouts(&home.join("sessions"), &mut files);
    let suffix = format!("-{id}.jsonl");
    files.into_iter().find(|p| p.to_string_lossy().ends_with(&suffix)).ok_or_else(|| anyhow!("找不到 Codex 会话 / Codex session not found: {id}"))
}

pub fn read(home: &Path, id: &str) -> Result<Conversation> {
    read_file(home, &find(home, id)?)
}

/// 读一个 rollout 文件（会话 id 取自文件里的 `session_meta`）。终端正在用的会话也能读：
/// 写到一半的最后一行会跳过。
pub fn read_file(home: &Path, path: &Path) -> Result<Conversation> {
    let f = std::fs::File::open(path).with_context(|| path.display().to_string())?;
    let mut c = Conversation { harness: Harness::Codex, id: String::new(), title: None, cwd: None, created_ms: 0, model: None, items: Vec::new(), manifest: Manifest::default() };
    for line in BufReader::new(f).lines() {
        let line = line?;
        // 写到一半的最后一行（Codex 正在写）直接跳过
        let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
        let t = parse_ms(v["timestamp"].as_str(), c.created_ms);
        match v["type"].as_str() {
            Some("session_meta") => {
                let p = &v["payload"];
                if c.id.is_empty() {
                    c.id = p["id"].as_str().unwrap_or_default().to_string();
                }
                c.cwd = p["cwd"].as_str().map(str::to_string);
                c.created_ms = parse_ms(p["timestamp"].as_str(), t);
            }
            Some("turn_context") => {
                if let Some(m) = v["payload"]["model"].as_str() {
                    c.model = Some(m.to_string());
                }
            }
            Some("response_item") => {
                let items = items_of(&v, t, &mut c.manifest);
                c.items.extend(items);
            }
            _ => {}
        }
    }
    c.title = names(home).remove(&c.id).or_else(|| title_from(&c.items));
    Ok(c)
}

/// Codex 注入的上下文：整段用同一个标签包起来（`<environment_context>…</environment_context>`），
/// 或 AGENTS.md 说明。图片的 `<image …>` / `</image>` 包裹行也算（图片本身不迁移）。
fn injected(text: &str) -> bool {
    let t = text.trim();
    if t.starts_with("# AGENTS.md instructions") || t.starts_with("<image") || t == "</image>" {
        return true;
    }
    let Some(rest) = t.strip_prefix('<') else { return false };
    let name: String = rest.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-').collect();
    !name.is_empty() && rest[name.len()..].starts_with(['>', ' ', '\n', '\r', '\t']) && t.ends_with(&format!("</{name}>"))
}

fn output_text(v: &Value, m: &mut Manifest) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(parts) => {
            let mut out = Vec::new();
            for p in parts {
                match p["type"].as_str() {
                    Some("input_text" | "output_text" | "text") => out.push(p["text"].as_str().unwrap_or_default().to_string()),
                    _ => m.drop("tool_output_media"),
                }
            }
            out.join("\n")
        }
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// 一行 `response_item` 变成中间表示（可能没有）。
fn items_of(v: &Value, t: i64, m: &mut Manifest) -> Vec<Item> {
    if v["type"] != "response_item" {
        return Vec::new();
    }
    let p = &v["payload"];
    match p["type"].as_str() {
        Some("message") => {
            let role = p["role"].as_str().unwrap_or_default();
            if role != "user" && role != "assistant" {
                m.drop("instructions");
                return Vec::new();
            }
            let mut texts = Vec::new();
            for part in p["content"].as_array().into_iter().flatten() {
                match part["type"].as_str() {
                    Some("input_text" | "output_text") => {
                        let s = part["text"].as_str().unwrap_or_default();
                        if role == "user" && injected(s) {
                            m.drop("injected_context");
                        } else {
                            texts.push(s.to_string());
                        }
                    }
                    Some("input_image") => m.drop("image"),
                    _ => m.drop("other_content"),
                }
            }
            let text = texts.join("\n\n");
            if text.trim().is_empty() {
                return Vec::new();
            }
            vec![if role == "user" { Item::User { text, time_ms: t } } else { Item::Assistant { text, time_ms: t } }]
        }
        Some("function_call") => vec![Item::ToolCall {
            id: p["call_id"].as_str().unwrap_or_default().to_string(),
            name: p["name"].as_str().unwrap_or("tool").to_string(),
            arguments: p["arguments"].as_str().unwrap_or("{}").to_string(),
            time_ms: t,
        }],
        Some("custom_tool_call") => vec![Item::ToolCall {
            id: p["call_id"].as_str().unwrap_or_default().to_string(),
            name: p["name"].as_str().unwrap_or("tool").to_string(),
            arguments: p["input"].as_str().unwrap_or_default().to_string(),
            time_ms: t,
        }],
        Some("function_call_output" | "custom_tool_call_output") => {
            let output = output_text(&p["output"], m);
            vec![Item::ToolResult { call_id: p["call_id"].as_str().unwrap_or_default().to_string(), output, is_error: false, time_ms: t }]
        }
        Some("reasoning") => {
            m.drop("reasoning");
            Vec::new()
        }
        Some("compaction") => {
            m.drop("compaction_summary");
            Vec::new()
        }
        Some(other) => {
            m.drop(other);
            Vec::new()
        }
        None => Vec::new(),
    }
}

/// 中间表示变成 Responses API 条目（`thread/inject_items` 的参数）。
pub fn response_items(items: &[Item]) -> Vec<Value> {
    items
        .iter()
        .map(|i| match i {
            Item::User { text, .. } => json!({"type": "message", "role": "user", "content": [{"type": "input_text", "text": text}]}),
            Item::Assistant { text, .. } => json!({"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": text}]}),
            Item::ToolCall { id, name, arguments, .. } => json!({"type": "function_call", "name": name, "arguments": arguments, "call_id": id}),
            Item::ToolResult { call_id, output, is_error, .. } => {
                let output = if *is_error && output != super::MISSING_RESULT { format!("Error: {output}") } else { output.clone() };
                json!({"type": "function_call_output", "call_id": call_id, "output": output})
            }
        })
        .collect()
}

/// 找 codex 可执行文件：`LOOKLOOK_CODEX_BIN`；调用方在用户 shell 里找到的路径（`hint`，Windows 上只认
/// .exe / .cmd，`where` 也会列出不能直接启动的 .ps1）；否则在 PATH 里找（Windows 上 npm 装的是 codex.cmd）。
fn codex_bin(hint: Option<&str>) -> Result<PathBuf> {
    if let Some(p) = std::env::var_os("LOOKLOOK_CODEX_BIN") {
        return Ok(PathBuf::from(p));
    }
    if let Some(h) = hint.filter(|h| !cfg!(windows) || [".exe", ".cmd"].iter().any(|e| h.to_ascii_lowercase().ends_with(e))) {
        return Ok(PathBuf::from(h));
    }
    let names: &[&str] = if cfg!(windows) { &["codex.exe", "codex.cmd", "codex.bat"] } else { &["codex"] };
    for dir in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
        for n in names {
            let p = dir.join(n);
            if p.is_file() {
                return Ok(p);
            }
        }
    }
    bail!("没有找到 Codex，请先安装 / Codex not found; install it first")
}

/// 一个很小的 JSON-RPC 客户端：按行收发；服务端发来的请求一律回“不支持”，通知忽略。
struct Rpc {
    child: tokio::process::Child,
    stdin: tokio::process::ChildStdin,
    lines: tokio::io::Lines<tokio::io::BufReader<tokio::process::ChildStdout>>,
    next: i64,
}

impl Rpc {
    async fn send(&mut self, v: &Value) -> Result<()> {
        let mut s = serde_json::to_string(v)?;
        s.push('\n');
        self.stdin.write_all(s.as_bytes()).await?;
        self.stdin.flush().await?;
        Ok(())
    }

    async fn call(&mut self, method: &str, params: Value) -> Result<Value> {
        self.next += 1;
        let id = self.next;
        self.send(&json!({"id": id, "method": method, "params": params})).await?;
        let wait = async {
            loop {
                let Some(line) = self.lines.next_line().await? else { bail!("codex app-server 退出了 / exited") };
                let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
                if v["id"].as_i64() == Some(id) && v.get("method").is_none() {
                    if let Some(e) = v.get("error") {
                        bail!("{method}: {}", e["message"].as_str().unwrap_or("error"));
                    }
                    return Ok(v["result"].clone());
                }
                if v.get("method").is_some() && v.get("id").is_some() {
                    let reply = json!({"id": v["id"], "error": {"code": -32601, "message": "not supported by looklook migration"}});
                    self.send(&reply).await?;
                }
            }
        };
        tokio::time::timeout(Duration::from_secs(60), wait).await.map_err(|_| anyhow!("{method}: codex app-server 没有响应 / timed out"))?
    }
}

pub async fn write(home: &Path, c: &Conversation, hint: Option<&str>) -> Result<Migrated> {
    let bin = codex_bin(hint)?;
    let mut cmd = tokio::process::Command::new(&bin);
    cmd.arg("app-server").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).kill_on_drop(true);
    // 登记的另一个 Codex 数据目录：让 app-server 写到那里
    if home != codex_home() {
        cmd.env("CODEX_HOME", home);
    }
    if let Some(d) = c.cwd.as_deref().filter(|d| Path::new(d).is_dir()) {
        cmd.current_dir(d);
    }
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = cmd.spawn().with_context(|| format!("无法启动 / failed to start {}", bin.display()))?;
    let stdin = child.stdin.take().context("stdin")?;
    let stdout = child.stdout.take().context("stdout")?;
    let mut rpc = Rpc { child, stdin, lines: tokio::io::BufReader::new(stdout).lines(), next: 0 };
    let r = async {
        rpc.call(
            "initialize",
            json!({"clientInfo": {"name": "looklook", "title": "Looklook", "version": env!("CARGO_PKG_VERSION")}, "capabilities": {"experimentalApi": true}}),
        )
        .await?;
        rpc.send(&json!({"method": "initialized"})).await?;
        let mut start = json!({});
        if let Some(d) = &c.cwd {
            start["cwd"] = json!(d);
        }
        let started = rpc.call("thread/start", start).await?;
        let thread = started["thread"]["id"].as_str().ok_or_else(|| anyhow!("thread/start: no thread id"))?.to_string();
        let items = response_items(&c.items);
        // 一次放太多可能超过单条消息的上限，分批放
        for chunk in items.chunks(200) {
            rpc.call("thread/inject_items", json!({"threadId": thread, "items": chunk})).await?;
        }
        if let Some(t) = &c.title {
            // 设标题失败不影响迁移
            let _ = rpc.call("thread/name/set", json!({"threadId": thread, "name": t})).await;
        }
        let cwd = started["cwd"].as_str().map(str::to_string).or_else(|| c.cwd.clone());
        Ok::<_, anyhow::Error>((thread, cwd))
    }
    .await;
    // 关掉输入让 app-server 正常退出、写完文件；等一会儿不退再强制结束
    let Rpc { mut child, stdin, .. } = rpc;
    drop(stdin);
    if tokio::time::timeout(Duration::from_secs(10), child.wait()).await.is_err() {
        let _ = child.kill().await;
    }
    let (thread, cwd) = r?;
    // 确认 Codex 已经把会话写到磁盘上
    if find(home, &thread).is_err() {
        bail!("Codex 没有保存新会话 / Codex did not save the new session ({thread})");
    }
    Ok(Migrated {
        harness: Harness::Codex,
        id: thread.clone(),
        cwd,
        resume: format!("codex resume {thread}"),
        items: c.items.len(),
        manifest: c.manifest.clone(),
    })
}

#[cfg(test)]
fn read_lines<'a>(lines: impl Iterator<Item = &'a str>) -> Result<Conversation> {
    let mut c = Conversation { harness: Harness::Codex, id: String::new(), title: None, cwd: None, created_ms: super::now_ms(), model: None, items: Vec::new(), manifest: Manifest::default() };
    for line in lines {
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        let t = parse_ms(v["timestamp"].as_str(), c.created_ms);
        if v["type"] == "session_meta" {
            c.id = v["payload"]["id"].as_str().unwrap_or_default().to_string();
            c.cwd = v["payload"]["cwd"].as_str().map(str::to_string);
        }
        c.items.extend(items_of(&v, t, &mut c.manifest));
    }
    c.title = title_from(&c.items);
    Ok(c)
}

#[cfg(test)]
mod tests {
    use super::super::normalize;
    use super::*;

    const ROLLOUT: &str = r#"{"timestamp":"2026-10-14T03:07:49.000Z","type":"session_meta","payload":{"id":"019a","cwd":"/work/app","timestamp":"2026-10-14T03:07:49.000Z","source":"cli"}}
{"timestamp":"2026-10-14T03:07:50.000Z","type":"response_item","payload":{"type":"message","role":"developer","content":[{"type":"input_text","text":"<permissions instructions>x</permissions instructions>"}]}}
{"timestamp":"2026-10-14T03:07:50.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<environment_context>\n  <cwd>/work/app</cwd>\n</environment_context>"}]}}
{"timestamp":"2026-10-14T03:07:51.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"把 <b> 标签换成 strong"},{"type":"input_image","image_url":"data:"}]}}
{"timestamp":"2026-10-14T03:07:52.000Z","type":"response_item","payload":{"type":"reasoning","summary":[],"encrypted_content":"gAAA"}}
{"timestamp":"2026-10-14T03:07:53.000Z","type":"response_item","payload":{"type":"custom_tool_call","name":"exec","input":"ls","call_id":"c1"}}
{"timestamp":"2026-10-14T03:07:54.000Z","type":"response_item","payload":{"type":"custom_tool_call_output","call_id":"c1","output":[{"type":"input_text","text":"a.html"}]}}
{"timestamp":"2026-10-14T03:07:55.000Z","type":"response_item","payload":{"type":"function_call","name":"shell","arguments":"{\"cmd\":\"sed\"}","call_id":"c2"}}
{"timestamp":"2026-10-14T03:07:56.000Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"改好了。"}]}}
{"timestamp":"2026-10-14T03:07:57.000Z","type":"response_item","payload":{"type":"message","role":"user","con"#;

    #[test]
    fn reads_rollout_dropping_context_and_private_parts() {
        let mut c = read_lines(ROLLOUT.lines()).unwrap();
        normalize(&mut c);
        assert_eq!(c.cwd.as_deref(), Some("/work/app"));
        assert_eq!(c.title.as_deref(), Some("把 <b> 标签换成 strong"));
        let got: Vec<String> = c
            .items
            .iter()
            .map(|i| match i {
                Item::User { text, .. } => format!("U:{text}"),
                Item::Assistant { text, .. } => format!("A:{text}"),
                Item::ToolCall { id, name, arguments, .. } => format!("C:{id}:{name}:{arguments}"),
                Item::ToolResult { call_id, output, is_error, .. } => format!("R:{call_id}:{output}:{is_error}"),
            })
            .collect();
        assert_eq!(
            got,
            [
                "U:把 <b> 标签换成 strong",
                "C:c1:exec:ls",
                "R:c1:a.html:false",
                "C:c2:shell:{\"cmd\":\"sed\"}",
                &format!("R:c2:{}:true", super::super::MISSING_RESULT),
                "A:改好了。"
            ]
        );
        assert_eq!(c.manifest.dropped["image"], 1);
        assert_eq!(c.manifest.dropped["reasoning"], 1);
        assert_eq!(c.manifest.dropped["instructions"], 1);
        assert_eq!(c.manifest.dropped["injected_context"], 1);
    }

    #[test]
    fn injected_context_detection() {
        assert!(injected("<environment_context>\n<cwd>/x</cwd>\n</environment_context>"));
        assert!(injected("  <codex_internal_context source=\"x\">hi</codex_internal_context>"));
        assert!(injected("# AGENTS.md instructions for /x\n..."));
        assert!(!injected("<b>bold</b> please"));
        assert!(!injected("把 <div> 改成 <section>"));
    }

    #[test]
    fn response_items_shape() {
        let v = response_items(&[
            Item::User { text: "hi".into(), time_ms: 0 },
            Item::ToolCall { id: "c".into(), name: "n".into(), arguments: "{}".into(), time_ms: 0 },
            Item::ToolResult { call_id: "c".into(), output: "boom".into(), is_error: true, time_ms: 0 },
        ]);
        assert_eq!(v[0]["content"][0]["type"], "input_text");
        assert_eq!(v[1]["type"], "function_call");
        assert_eq!(v[2]["output"], "Error: boom");
    }
}
