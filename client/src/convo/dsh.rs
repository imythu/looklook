//! DSH（DeepSeek Harness）：读写它的 v4 会话日志。
//!
//! 位置：`$DSH_HOME/sessions/<项目目录>/<会话目录>/session.v4.jsonl.zstd`（默认 `~/.dsh`）。项目目录由
//! 工作目录换成可读的名字（`--C-Users-me-app--`），会话目录是转义后的会话 id，两者的转义规则与
//! `@deepseek-ai/dsh-session-persistence-jsonl` 一致（按 UTF-16 码元，`~XXXX`）。文件第一行是头
//! （`{"type":"session","version":4,…}`），之后每行一个事件 `{type, seq, time, data, surfaceOp?, sourceEventSeqs?}`；
//! 默认整段用带校验和的 Zstandard 帧压缩。
//!
//! 写入时按 DSH 自己的 agent loop 的样子组织事件：每条用户消息开一个 turn（从 1 数），turn 里每次
//! 模型调用一个 step（从 1 数）：`step/start` → （第一步）`user/message` → `assistant/message`（带合成的
//! 流记录）→ 每个工具调用一对 `tool/call` / `tool/result` → `step/end`，最后 `turn/end`。日志开头放一个空的
//! `system/message` 作为第 0 个节点（“没有系统提示”），DSH 接着聊时会换成它当时的系统提示；
//! `request/header` 也由 DSH 在第一次请求时自己写。最后写 `session/title`。
//! 写完把会话登记到 `storages/workspace.json` 对应工作目录的工作区里，DSH 的会话列表才看得到。

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Map, Value};

use super::{home, now_ms, title_from, Conversation, Harness, Item, Manifest, Migrated, Summary};

pub const FORMAT_VERSION: u64 = 4;

pub fn dsh_home() -> PathBuf {
    std::env::var_os("DSH_HOME").map(PathBuf::from).unwrap_or_else(|| home().join(".dsh"))
}

fn sessions_root(home: &Path) -> PathBuf {
    home.join("sessions")
}

/// 会话 id → 一个安全的路径段（与 DSH 的 `encodeSegment` 相同）。
pub fn encode_segment(raw: &str) -> String {
    match raw {
        "." => return "~002E".into(),
        ".." => return "~002E~002E".into(),
        _ => {}
    }
    let mut out = String::new();
    for u in raw.encode_utf16() {
        match char::from_u32(u32::from(u)) {
            Some(c) if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') => out.push(c),
            _ => out.push_str(&format!("~{u:04X}")),
        }
    }
    out
}

/// 工作目录 → 可读的项目目录名（与 DSH 的 `projectKey` 相同）。
pub fn project_key(cwd: &str) -> String {
    let mut readable = String::new();
    let mut run = false;
    for u in cwd.encode_utf16() {
        match char::from_u32(u32::from(u)) {
            Some('/' | '\\' | ':') => {
                if !run {
                    readable.push('-');
                }
                run = true;
            }
            Some(c) if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') => {
                readable.push(c);
                run = false;
            }
            _ => {
                readable.push_str(&format!("~{u:04X}"));
                run = false;
            }
        }
    }
    let trimmed = readable.trim_start_matches('-');
    let key: String = (if trimmed.is_empty() { "root" } else { trimmed }).chars().take(251).collect();
    format!("--{key}--")
}

/// 会话目录里版本最高的日志文件：`session.vN.jsonl(.zstd)`（v0 没有 `.vN`）。
fn latest_log(dir: &Path) -> Option<(u64, PathBuf)> {
    let mut best: Option<(u64, PathBuf)> = None;
    for e in std::fs::read_dir(dir).ok()?.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        let base = name.strip_suffix(".zstd").unwrap_or(&name);
        let Some(stem) = base.strip_suffix(".jsonl") else { continue };
        let v = match stem {
            "session" => 0,
            s => match s.strip_prefix("session.v").and_then(|n| n.parse::<u64>().ok()) {
                Some(n) => n,
                None => continue,
            },
        };
        if best.as_ref().is_none_or(|(b, _)| v > *b) {
            best = Some((v, e.path()));
        }
    }
    best
}

/// 读出日志的文本：压缩的逐帧解，写到一半的最后一帧 / 最后一行丢掉（DSH 恢复时也这样处理）。
fn log_text(path: &Path, limit: Option<usize>) -> Result<String> {
    let raw = std::fs::read(path).with_context(|| path.display().to_string())?;
    let bytes = if path.extension().is_some_and(|e| e == "zstd") {
        let mut out = Vec::new();
        let mut d = zstd::stream::read::Decoder::new(&raw[..])?;
        let mut buf = [0u8; 64 * 1024];
        loop {
            match d.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    out.extend_from_slice(&buf[..n]);
                    if limit.is_some_and(|l| out.len() >= l) {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        out
    } else {
        raw
    };
    let mut s = String::from_utf8_lossy(&bytes).to_string();
    if !s.ends_with('\n') {
        s.truncate(s.rfind('\n').map_or(0, |i| i + 1));
    }
    Ok(s)
}

struct Found {
    header: Value,
    version: u64,
    log: PathBuf,
}

fn sessions(home: &Path) -> Vec<Found> {
    let mut out = Vec::new();
    let Ok(projects) = std::fs::read_dir(sessions_root(home)) else { return out };
    for p in projects.flatten().filter(|e| e.path().is_dir()) {
        let Ok(rd) = std::fs::read_dir(p.path()) else { continue };
        for s in rd.flatten().filter(|e| e.path().is_dir()) {
            let Some((version, log)) = latest_log(&s.path()) else { continue };
            let Ok(text) = log_text(&log, Some(64 * 1024)) else { continue };
            let Some(header) = text.lines().next().and_then(|l| serde_json::from_str::<Value>(l).ok()) else { continue };
            if header["type"] != "session" || header["id"].as_str().is_none() {
                continue;
            }
            out.push(Found { header, version, log });
        }
    }
    out
}

/// DSH 的投影缓存里记下的标题（列表不必解整个日志）。
fn cached_title(home: &Path, id: &str) -> Option<String> {
    let p = home.join("storages").join("session_projcache").join("sessions").join(format!("{}.json", encode_segment(id)));
    let v: Value = serde_json::from_slice(&std::fs::read(p).ok()?).ok()?;
    v["record"]["rows"]["title"]["val"].as_str().or_else(|| v["record"]["rows"]["title"]["val"]["title"].as_str()).map(str::to_string)
}

pub fn list(home: &Path) -> Result<Vec<Summary>> {
    let mut out = Vec::new();
    for f in sessions(home) {
        if f.header["origin"] == "subagent" {
            continue;
        }
        let id = f.header["id"].as_str().unwrap_or_default().to_string();
        let updated_ms = std::fs::metadata(&f.log)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let title = cached_title(home, &id).or_else(|| {
            let text = log_text(&f.log, Some(256 * 1024)).ok()?;
            let mut m = Manifest::default();
            let mut title = None;
            let items = fold(text.lines().skip(1).filter_map(|l| serde_json::from_str::<Value>(l).ok()), &mut m, &mut title, &mut None);
            title.or_else(|| title_from(&items))
        });
        // 只有头、没有对话的空会话不列出
        if title.is_none() {
            continue;
        }
        out.push(Summary { harness: Harness::Dsh, id, title, cwd: f.header["cwd"].as_str().map(str::to_string), updated_ms });
    }
    Ok(out)
}

fn text_blocks(content: &Value, m: &mut Manifest) -> String {
    let mut out = Vec::new();
    for b in content.as_array().into_iter().flatten() {
        match b["type"].as_str() {
            Some("text") => out.push(b["text"].as_str().unwrap_or_default().to_string()),
            Some("reasoning") => m.drop("reasoning"),
            Some("image") => m.drop("image"),
            Some("file") => m.drop("file"),
            Some(other) => m.drop(other),
            None => {}
        }
    }
    out.join("\n\n")
}

/// 事件流 → 中间表示。只取追加到对话里的消息；压缩等替换产生的摘要不取（原消息都还在日志里）。
fn fold(events: impl Iterator<Item = Value>, m: &mut Manifest, title: &mut Option<String>, model: &mut Option<String>) -> Vec<Item> {
    let mut items = Vec::new();
    for e in events {
        let t = e["time"].as_i64().unwrap_or(0);
        let d = &e["data"];
        let appended = e["surfaceOp"] == "append";
        match e["type"].as_str() {
            Some("session/title") => *title = d["title"].as_str().map(str::to_string),
            Some("user/message") => {
                if !appended {
                    m.drop("compaction_summary");
                } else if d["source"]["kind"] != "user" {
                    m.drop("injected_context");
                } else {
                    items.push(Item::User { text: text_blocks(&d["content"], m), time_ms: t });
                }
            }
            Some("assistant/message") => {
                if !appended {
                    m.drop("compaction_summary");
                    continue;
                }
                let msg = &d["message"];
                if let Some(mo) = msg["source"]["model"].as_str() {
                    *model = Some(mo.to_string());
                }
                let mut text = Vec::new();
                let mut calls = Vec::new();
                for b in msg["content"].as_array().into_iter().flatten() {
                    match b["type"].as_str() {
                        Some("text") => text.push(b["text"].as_str().unwrap_or_default().to_string()),
                        Some("tool-call") => calls.push(Item::ToolCall {
                            id: b["id"].as_str().unwrap_or_default().to_string(),
                            name: b["name"].as_str().unwrap_or("tool").to_string(),
                            arguments: b["arguments"].as_str().unwrap_or("{}").to_string(),
                            time_ms: t,
                        }),
                        Some("reasoning") => m.drop("reasoning"),
                        Some(other) => m.drop(other),
                        None => {}
                    }
                }
                let text = text.join("\n\n");
                if !text.trim().is_empty() {
                    items.push(Item::Assistant { text, time_ms: t });
                }
                items.extend(calls);
            }
            Some("tool/result") => {
                if !appended {
                    m.drop("compaction_summary");
                    continue;
                }
                let msg = &d["message"];
                items.push(Item::ToolResult {
                    call_id: msg["toolCallId"].as_str().unwrap_or_default().to_string(),
                    output: text_blocks(&msg["content"], m),
                    is_error: msg["isError"].as_bool().unwrap_or(false),
                    time_ms: t,
                });
            }
            Some("system/message" | "developer/message") => m.drop("instructions"),
            _ => {}
        }
    }
    items
}

pub fn read(home: &Path, id: &str) -> Result<Conversation> {
    read_log(&find_log(home, id)?)
}

/// 会话 id → 它最新的日志文件。
pub fn find_log(home: &Path, id: &str) -> Result<PathBuf> {
    let f = sessions(home).into_iter().find(|f| f.header["id"] == id).ok_or_else(|| anyhow!("找不到 DSH 会话 / DSH session not found: {id}"))?;
    if f.version != FORMAT_VERSION {
        bail!("这个 DSH 会话是旧格式（v{}），请先在 DSH 里打开一次让它升级 / Old DSH session format (v{}); open it in DSH once to upgrade it", f.version, f.version);
    }
    Ok(f.log)
}

/// 新开的 DSH 终端对应的会话日志：工作目录相同、在 `started_ms` 之后创建的会话里最近还在写的那个。
pub fn discover(home: &Path, cwd: &str, started_ms: i64) -> Option<PathBuf> {
    sessions(home)
        .into_iter()
        .filter(|f| f.version == FORMAT_VERSION && f.header["origin"] != "subagent")
        .filter(|f| f.header["createdAt"].as_i64().unwrap_or(0) + 10_000 >= started_ms)
        .filter(|f| f.header["cwd"].as_str().is_some_and(|c| same_dir(c, cwd)))
        .filter_map(|f| Some((std::fs::metadata(&f.log).ok()?.modified().ok()?, f.log)))
        .max_by_key(|(m, _)| *m)
        .map(|(_, p)| p)
}

/// 读一个会话日志（会话 id、工作目录取自日志第一行的头）。
pub fn read_log(log: &Path) -> Result<Conversation> {
    let text = log_text(log, None)?;
    let header: Value = text.lines().next().and_then(|l| serde_json::from_str(l).ok()).ok_or_else(|| anyhow!("DSH 会话没有头 / session header is missing"))?;
    let id = header["id"].as_str().unwrap_or_default().to_string();
    let f = Found { header, version: FORMAT_VERSION, log: log.to_path_buf() };
    let mut m = Manifest::default();
    let mut title = None;
    let mut model = None;
    let items = fold(text.lines().skip(1).filter_map(|l| serde_json::from_str::<Value>(l).ok()), &mut m, &mut title, &mut model);
    Ok(Conversation {
        harness: Harness::Dsh,
        id: id.to_string(),
        title: title.or_else(|| title_from(&items)),
        cwd: f.header["cwd"].as_str().map(str::to_string),
        created_ms: f.header["createdAt"].as_i64().unwrap_or(0),
        model,
        items,
        manifest: m,
    })
}

// ---------------- 写入 ----------------

fn uuid() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// 工具参数必须是 JSON 对象（DeepSeek 等 OpenAI 兼容接口会解析历史里的参数）；不是的包成 `{"input": …}`。
fn json_arguments(raw: &str) -> String {
    match serde_json::from_str::<Value>(raw) {
        Ok(Value::Object(_)) => raw.to_string(),
        _ => json!({ "input": raw }).to_string(),
    }
}

/// 一次模型调用：助手说的话和它要的工具调用，以及这些调用的结果。
#[derive(Default)]
struct Step {
    user: Option<(String, i64)>,
    text: Vec<(String, i64)>,
    calls: Vec<(String, String, String, i64)>,
    results: Vec<(String, String, bool, i64)>,
}

impl Step {
    fn has_model(&self) -> bool {
        !self.text.is_empty() || !self.calls.is_empty()
    }
}

/// 按用户消息分 turn，turn 内按“结果之后又有模型输出”分 step。
fn turns(items: &[Item]) -> Vec<Vec<Step>> {
    let mut turns: Vec<Vec<Step>> = Vec::new();
    for it in items {
        match it {
            Item::User { text, time_ms } => turns.push(vec![Step { user: Some((text.clone(), *time_ms)), ..Default::default() }]),
            Item::Assistant { .. } | Item::ToolCall { .. } => {
                if turns.is_empty() {
                    turns.push(vec![Step::default()]);
                }
                let steps = turns.last_mut().unwrap();
                if !steps.last().unwrap().results.is_empty() {
                    steps.push(Step::default());
                }
                let s = steps.last_mut().unwrap();
                match it {
                    Item::Assistant { text, time_ms } => s.text.push((text.clone(), *time_ms)),
                    Item::ToolCall { id, name, arguments, time_ms } => s.calls.push((id.clone(), name.clone(), json_arguments(arguments), *time_ms)),
                    _ => unreachable!(),
                }
            }
            Item::ToolResult { call_id, output, is_error, time_ms } => {
                if let Some(s) = turns.last_mut().and_then(|t| t.last_mut()) {
                    s.results.push((call_id.clone(), output.clone(), *is_error, *time_ms));
                }
            }
        }
    }
    turns
}

struct Log {
    events: Vec<Value>,
}

impl Log {
    /// 追加一个事件，返回它的 seq。`surface`：追加到对话里的消息（`surfaceOp: "append"`）。
    fn push(&mut self, ty: &str, time: i64, data: Value, surface: bool, sources: Option<Vec<usize>>) -> usize {
        let seq = self.events.len();
        let mut e = Map::new();
        e.insert("type".into(), json!(ty));
        e.insert("seq".into(), json!(seq));
        e.insert("time".into(), json!(time));
        e.insert("data".into(), data);
        if surface {
            e.insert("surfaceOp".into(), json!("append"));
        }
        if let Some(s) = sources {
            e.insert("sourceEventSeqs".into(), json!(s));
        }
        self.events.push(Value::Object(e));
        seq
    }
}

/// 合成助手消息的流记录：与 DSH 的 `AssistantStreamAccumulator` 压缩后的样子相同，每个块一次增量。
fn stream(blocks: &[Value], time: i64, finish: &str) -> Vec<Value> {
    let mut s = Vec::new();
    for (i, b) in blocks.iter().enumerate() {
        let ty = b["type"].as_str().unwrap_or("text");
        s.push(json!({"type": "chunk", "time": time, "chunk": {"type": "block-start", "index": i, "blockType": ty}}));
        if ty == "tool-call" {
            s.push(json!({"type": "tool-call-chunks", "time0": time, "index": i, "dt": [], "id": b["id"], "name": b["name"], "args": [b["arguments"]]}));
        } else {
            s.push(json!({"type": "text-chunks", "time0": time, "index": i, "dt": [], "texts": [b["text"]]}));
        }
        s.push(json!({"type": "chunk", "time": time, "chunk": {"type": "block-end", "index": i, "block": b}}));
    }
    s.push(json!({"type": "chunk", "time": time, "chunk": {"type": "finish", "reason": {"kind": finish}}}));
    s
}

/// 中间表示 → DSH 事件（不含头）。
fn events(c: &Conversation, now: i64) -> Vec<Value> {
    let mut log = Log { events: Vec::new() };
    let provider = format!("imported-{}", c.harness.name());
    let model = c.model.clone().unwrap_or_else(|| c.harness.name().to_string());
    let all = turns(&c.items);
    let n = all.len();
    for (ti, steps) in all.into_iter().enumerate() {
        let turn = ti + 1;
        let t0 = steps.first().and_then(|s| s.user.as_ref().map(|u| u.1)).unwrap_or(now);
        log.push("turn/start", t0, json!({"turn": turn}), false, None);
        let mut answered = false;
        for (si, s) in steps.into_iter().enumerate() {
            let step = si + 1;
            let ts = s.user.as_ref().map(|u| u.1).or(s.text.first().map(|x| x.1)).or(s.calls.first().map(|x| x.3)).unwrap_or(t0);
            log.push("step/start", ts, json!({"turn": turn, "step": step}), false, None);
            if turn == 1 && step == 1 {
                // 第 0 个节点：空的系统提示（“没有系统提示”）；DSH 接着聊时会换成当时的系统提示
                let sys = json!({"turn": 1, "step": 1, "message": {"id": uuid(), "role": "system", "content": [], "source": {"kind": "system-prompt"}}});
                log.push("system/message", ts, sys, true, None);
            }
            if let Some((text, t)) = &s.user {
                let msg = json!({"id": uuid(), "role": "user", "content": [{"type": "text", "text": text}], "source": {"kind": "user"}});
                log.push("user/message", *t, msg, true, None);
            }
            if s.has_model() {
                answered = true;
                let mut blocks: Vec<Value> = s.text.iter().map(|(t, _)| json!({"type": "text", "text": t})).collect();
                blocks.extend(s.calls.iter().map(|(id, name, args, _)| json!({"type": "tool-call", "id": id, "name": name, "arguments": args})));
                let ta = s.text.first().map(|x| x.1).or(s.calls.first().map(|x| x.3)).unwrap_or(ts);
                let finish = if s.calls.is_empty() { "stop" } else { "tool-calls" };
                let data = json!({
                    "turn": turn,
                    "step": step,
                    "message": {"id": uuid(), "role": "assistant", "content": blocks, "source": {"kind": "model", "provider": provider, "model": model}},
                    "stream": stream(&blocks, ta, finish),
                });
                log.push("assistant/message", ta, data, true, None);
                let results: HashMap<&str, &(String, String, bool, i64)> = s.results.iter().map(|r| (r.0.as_str(), r)).collect();
                for (id, name, args, tc) in &s.calls {
                    let call_seq = log.push("tool/call", *tc, json!({"turn": turn, "step": step, "callId": id, "name": name, "arguments": args}), false, None);
                    let (output, is_error, tr) = match results.get(id.as_str()) {
                        Some((_, o, e, t)) => (o.clone(), *e, *t),
                        None => (super::MISSING_RESULT.to_string(), true, *tc),
                    };
                    let mut msg = json!({"id": uuid(), "role": "tool", "source": {"kind": "tool", "callId": id}, "toolCallId": id, "content": [{"type": "text", "text": output}]});
                    if is_error {
                        msg["isError"] = json!(true);
                    }
                    log.push("tool/result", tr.max(*tc), json!({"turn": turn, "step": step, "message": msg}), true, Some(vec![call_seq]));
                }
            }
            let te = s.results.last().map(|r| r.3).or(s.text.last().map(|x| x.1)).unwrap_or(ts);
            log.push("step/end", te, json!({"turn": turn, "step": step}), false, None);
        }
        let last = log.events.last().and_then(|e| e["time"].as_i64()).unwrap_or(now);
        // 最后一条用户消息原会话里还没有回答：按中断处理，接着聊时模型能看到它
        let reason = if answered || ti + 1 < n { json!({"kind": "completed"}) } else { json!({"kind": "interrupted"}) };
        log.push("turn/end", last, json!({"turn": turn, "reason": reason}), false, None);
    }
    if let Some(title) = &c.title {
        log.push("session/title", now, json!({"title": title, "messageSeqs": [], "source": {"kind": "user"}}), false, None);
    }
    // 时间单调不减（来源里的时间可能有先后颠倒）
    let mut last = 0;
    for e in &mut log.events {
        let t = e["time"].as_i64().unwrap_or(now).max(last);
        e["time"] = json!(t);
        last = t;
    }
    log.events
}

pub fn write(home: &Path, c: &Conversation) -> Result<Migrated> {
    let now = now_ms();
    let id = format!("session-{}", uuid());
    let cwd = c.cwd.clone().filter(|d| Path::new(d).is_absolute());
    let project = cwd.as_deref().map(project_key).unwrap_or_else(|| "_no-cwd".into());
    let dir = sessions_root(home).join(project).join(encode_segment(&id));
    let mut header = Map::new();
    header.insert("type".into(), json!("session"));
    header.insert("version".into(), json!(FORMAT_VERSION));
    header.insert("id".into(), json!(id));
    header.insert("createdAt".into(), json!(now));
    if let Some(d) = &cwd {
        header.insert("cwd".into(), json!(d));
    }
    header.insert("isSeeded".into(), json!(false));
    header.insert("delegationDepth".into(), json!(0));
    header.insert("agentPreset".into(), json!("standard"));
    let mut head = serde_json::to_string(&Value::Object(header))?;
    head.push('\n');
    let mut body = String::new();
    for e in events(c, now) {
        body.push_str(&serde_json::to_string(&e)?);
        body.push('\n');
    }
    std::fs::create_dir_all(&dir).with_context(|| dir.display().to_string())?;
    let path = dir.join(format!("session.v{FORMAT_VERSION}.jsonl.zstd"));
    let tmp = dir.join(format!(".session.v{FORMAT_VERSION}.jsonl.zstd.tmp"));
    {
        // DSH 要求第一帧正好是头这一行，事件放在后面的帧里（它只解第一帧来读头）
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(&zstd_frame(head.as_bytes())?)?;
        f.write_all(&zstd_frame(body.as_bytes())?)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, &path)?;
    if let Some(d) = &cwd {
        if let Err(e) = register(home, &id, d, now) {
            tracing::warn!(error = %e, "无法登记到 DSH 工作区");
        }
    }
    Ok(Migrated {
        harness: Harness::Dsh,
        id: id.clone(),
        cwd: cwd.clone(),
        resume: format!("dsh tui --resume {id}"),
        items: c.items.len(),
        manifest: c.manifest.clone(),
    })
}

/// 一个带内容校验和的 Zstandard 帧。
fn zstd_frame(data: &[u8]) -> Result<Vec<u8>> {
    let mut enc = zstd::stream::write::Encoder::new(Vec::new(), 3)?;
    enc.include_checksum(true)?;
    enc.write_all(data)?;
    Ok(enc.finish()?)
}

fn same_dir(a: &str, b: &str) -> bool {
    let n = |s: &str| {
        let s = s.trim_end_matches(['/', '\\']).replace('\\', "/");
        if cfg!(windows) { s.to_lowercase() } else { s }
    };
    n(a) == n(b)
}

/// 把会话登记到 `storages/workspace.json` 里这个工作目录的工作区（没有就新建一个）。
fn register(home: &Path, id: &str, cwd: &str, now: i64) -> Result<()> {
    let path = home.join("storages").join("workspace.json");
    let Ok(raw) = std::fs::read(&path) else { return Ok(()) };
    let mut v: Value = serde_json::from_slice(&raw)?;
    let stamp = time::OffsetDateTime::from_unix_timestamp_nanos(i128::from(now) * 1_000_000)
        .ok()
        .and_then(|t| t.format(&time::format_description::well_known::Rfc3339).ok())
        .unwrap_or_default();
    let tables = v["tables"]["workspaces"].as_object_mut().ok_or_else(|| anyhow!("workspace.json: no workspaces table"))?;
    let existing = tables.iter().find(|(_, w)| w["path"].as_str().is_some_and(|p| same_dir(p, cwd))).map(|(k, _)| k.clone());
    match existing {
        Some(k) => {
            let w = &mut tables[&k];
            if let Some(ids) = w["sessionIds"].as_array_mut() {
                ids.push(json!(id));
            } else {
                w["sessionIds"] = json!([id]);
            }
            w["updatedAt"] = json!(stamp);
        }
        None => {
            let k = uuid();
            let title = Path::new(cwd).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| cwd.to_string());
            tables.insert(k.clone(), json!({"path": cwd, "title": title, "sessionIds": [id], "createdAt": stamp, "updatedAt": stamp}));
            if let Some(ids) = v["global"]["workspaceIds"].as_array_mut() {
                ids.push(json!(k));
            }
        }
    }
    let tmp = path.with_extension("json.looklook-tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(&v)?)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_encoding_matches_dsh() {
        assert_eq!(encode_segment("session-5427fae7-a22c"), "session-5427fae7-a22c");
        assert_eq!(encode_segment("a/b~c"), "a~002Fb~007Ec");
        assert_eq!(encode_segment(".."), "~002E~002E");
        assert_eq!(project_key("C:\\Users\\YJN\\Documents\\deepseek-harness\\default-workspace"), "--C-Users-YJN-Documents-deepseek-harness-default-workspace--");
        assert_eq!(project_key("/home/me/项目"), "--home-me-~9879~76EE--");
        assert_eq!(project_key("/"), "--root--");
    }

    fn sample() -> Conversation {
        Conversation {
            harness: Harness::Codex,
            id: "src".into(),
            title: Some("修复".into()),
            cwd: Some("/w".into()),
            created_ms: 1,
            model: Some("gpt-x".into()),
            items: vec![
                Item::User { text: "看看 a.html".into(), time_ms: 10 },
                Item::Assistant { text: "我先看一下".into(), time_ms: 11 },
                Item::ToolCall { id: "c1".into(), name: "exec".into(), arguments: "ls".into(), time_ms: 12 },
                Item::ToolResult { call_id: "c1".into(), output: "a.html".into(), is_error: false, time_ms: 13 },
                Item::Assistant { text: "改好了".into(), time_ms: 14 },
                Item::User { text: "再改一下".into(), time_ms: 20 },
            ],
            manifest: Manifest::default(),
        }
    }

    #[test]
    fn events_follow_the_agent_loop_shape() {
        let ev = events(&sample(), 100);
        let kinds: Vec<&str> = ev.iter().map(|e| e["type"].as_str().unwrap()).collect();
        assert_eq!(
            kinds,
            [
                "turn/start", "step/start", "system/message", "user/message", "assistant/message", "tool/call", "tool/result", "step/end",
                "step/start", "assistant/message", "step/end", "turn/end",
                "turn/start", "step/start", "user/message", "step/end", "turn/end", "session/title"
            ]
        );
        assert!(ev.iter().enumerate().all(|(i, e)| e["seq"] == i));
        assert!(ev.windows(2).all(|w| w[0]["time"].as_i64() <= w[1]["time"].as_i64()));
        assert_eq!(ev[9]["data"]["step"], 2);
        // 非 JSON 参数包成对象；结果引用自己的调用
        assert_eq!(ev[5]["data"]["arguments"], "{\"input\":\"ls\"}");
        assert_eq!(ev[6]["sourceEventSeqs"], json!([5]));
        assert_eq!(ev[6]["data"]["message"]["toolCallId"], "c1");
        // 流记录能还原出消息内容
        let s = ev[4]["data"]["stream"].as_array().unwrap();
        assert_eq!(s.last().unwrap()["chunk"]["reason"]["kind"], "tool-calls");
        assert_eq!(ev[16]["data"]["reason"]["kind"], "interrupted");
        assert!(ev.iter().filter(|e| e.get("surfaceOp").is_some()).all(|e| matches!(
            e["type"].as_str().unwrap(),
            "system/message" | "user/message" | "assistant/message" | "tool/result"
        )));
    }

    #[test]
    fn roundtrip_through_fold() {
        let c = sample();
        let mut m = Manifest::default();
        let mut title = None;
        let mut model = None;
        let back = fold(events(&c, 100).into_iter(), &mut m, &mut title, &mut model);
        let mut want = c.items.clone();
        if let Item::ToolCall { arguments, .. } = &mut want[2] {
            *arguments = "{\"input\":\"ls\"}".into();
        }
        let strip = |v: &[Item]| v.iter().map(|i| { let mut j = i.clone(); match &mut j { Item::User { time_ms, .. } | Item::Assistant { time_ms, .. } | Item::ToolCall { time_ms, .. } | Item::ToolResult { time_ms, .. } => *time_ms = 0 }; j }).collect::<Vec<_>>();
        assert_eq!(strip(&back), strip(&want));
        assert_eq!(title.as_deref(), Some("修复"));
        assert_eq!(model.as_deref(), Some("gpt-x"));
        assert_eq!(m.dropped["instructions"], 1);
    }
}
