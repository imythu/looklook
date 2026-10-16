//! Claude Code：读写 `$CLAUDE_CONFIG_DIR/projects/<项目>/<会话 id>.jsonl`（默认 `~/.claude`）。
//!
//! 一行一条记录。对话在 `type: user` / `type: assistant` 里，`message` 就是 Anthropic API 的消息：
//! `content` 是字符串或块数组（`text`、`tool_use`、`tool_result`、`thinking`、`image`）。同一条助手消息按块
//! 拆成好几行（`message.id` 相同）；`parentUuid` 把各行串成链。标题在 `custom-title`（后写的为准）或 `summary` 里。
//! 子代理的记录 `isSidechain: true`，不算；`isMeta`、`<command-…>` 这类是 Claude Code 自己注入的，也不算。
//!
//! 写：按同样的格式写一个新文件，项目目录名是工作目录里非字母数字的字符都换成 `-`（和 Claude Code 一样），
//! 然后在那个目录里 `claude --resume <id>` 接着聊。别的工具的工具调用 Claude Code 不认识，写成文字；
//! 从 Claude Code 来的保持原样（`tool_use` / `tool_result` 成对）。

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};

use super::{parse_ms, title_from, Conversation, Harness, Item, Manifest, Migrated, Summary};

/// 写成文字的工具调用参数 / 结果最多保留这么多字符。
const TOOL_TEXT_MAX: usize = 2000;

fn projects(home: &Path) -> PathBuf {
    home.join("projects")
}

/// 工作目录 → 项目目录名（与 Claude Code 相同：非字母数字的 UTF-16 码元都换成 `-`）。
pub fn project_dir_name(cwd: &str) -> String {
    cwd.encode_utf16().map(|u| match char::from_u32(u32::from(u)) {
        Some(c) if c.is_ascii_alphanumeric() => c,
        _ => '-',
    }).collect()
}

/// 所有会话文件（只看项目目录下第一层的 `.jsonl`，子代理在子目录里）。
fn files(home: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(projects(home)) else { return out };
    for p in rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()) {
        let Ok(inner) = std::fs::read_dir(&p) else { continue };
        out.extend(inner.flatten().map(|e| e.path()).filter(|f| f.is_file() && f.extension().is_some_and(|x| x == "jsonl")));
    }
    out
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 80 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

pub fn find(home: &Path, id: &str) -> Result<PathBuf> {
    if !valid_id(id) {
        bail!("会话 id 不对 / Invalid session id: {id}");
    }
    let name = format!("{id}.jsonl");
    files(home).into_iter().find(|p| p.file_name().is_some_and(|n| n == name.as_str())).ok_or_else(|| anyhow!("找不到 Claude Code 会话 / Claude Code session not found: {id}"))
}

/// Claude Code 自己注入的“用户消息”：命令回显、系统提醒、打断提示等。
fn injected(text: &str) -> bool {
    let t = text.trim_start();
    // 整段用同一个标签包起来的（`<task-notification>…</task-notification>` 这类后台通知）
    if let Some(rest) = t.strip_prefix('<') {
        let name: String = rest.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').collect();
        if !name.is_empty() && t.trim_end().ends_with(&format!("</{name}>")) {
            return true;
        }
    }
    t.starts_with("<command-")
        || t.starts_with("<local-command-")
        || t.starts_with("<system-reminder>")
        || t.starts_with("<bash-")
        || t.starts_with("[Request interrupted by user")
        || t.starts_with("Caveat: The messages below were generated")
}

fn result_text(v: &Value, m: &mut Manifest) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(parts) => {
            let mut out = Vec::new();
            for p in parts {
                match p["type"].as_str() {
                    Some("text") => out.push(p["text"].as_str().unwrap_or_default().to_string()),
                    _ => m.drop("tool_output_media"),
                }
            }
            out.join("\n")
        }
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

pub fn list(home: &Path) -> Result<Vec<Summary>> {
    let mut out = Vec::new();
    for p in files(home) {
        let Some(id) = p.file_stem().and_then(|s| s.to_str()).map(str::to_string) else { continue };
        let Ok(f) = std::fs::File::open(&p) else { continue };
        let updated_ms = std::fs::metadata(&p)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let (mut cwd, mut custom, mut summary, mut first) = (None, None, None, None);
        let mut has_user = false;
        // 标题记录可能在文件任何位置（后来改名），整个文件过一遍，但只解析需要的行
        for line in BufReader::new(f).lines().map_while(Result::ok) {
            let wanted = line.contains("\"custom-title\"") || line.contains("\"summary\"") || (!has_user && line.contains("\"type\":\"user\""));
            if !wanted {
                continue;
            }
            let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
            match v["type"].as_str() {
                Some("custom-title") => custom = v["customTitle"].as_str().map(str::to_string).or(custom),
                Some("summary") => summary = v["summary"].as_str().map(str::to_string).or(summary),
                Some("user") if v["isSidechain"] != true && v["isMeta"] != true => {
                    if cwd.is_none() {
                        cwd = v["cwd"].as_str().map(str::to_string);
                    }
                    if let Some(t) = v["message"]["content"].as_str().filter(|t| !injected(t)) {
                        has_user = true;
                        first = title_from(&[Item::User { text: t.to_string(), time_ms: 0 }]);
                    } else if let Some(t) = v["message"]["content"].as_array().and_then(|a| a.iter().find_map(|b| b["text"].as_str().filter(|t| !injected(t)))) {
                        has_user = true;
                        first = title_from(&[Item::User { text: t.to_string(), time_ms: 0 }]);
                    }
                }
                _ => {}
            }
        }
        if !has_user {
            continue;
        }
        out.push(Summary { harness: Harness::Claude, id, title: custom.or(summary).or(first), cwd, updated_ms });
    }
    Ok(out)
}

pub fn read(home: &Path, id: &str) -> Result<Conversation> {
    read_file(&find(home, id)?)
}

/// 新开的 Claude Code 终端对应的会话文件：项目目录（按工作目录算）里在 `started_ms` 之后还在写的、最近的一个。
pub fn discover(home: &Path, cwd: &str, started_ms: i64) -> Option<PathBuf> {
    let want = project_dir_name(cwd);
    // Windows 上盘符等大小写可能和 Claude Code 记下的不一样
    let dir = std::fs::read_dir(projects(home)).ok()?.flatten().map(|e| e.path()).find(|p| {
        let n = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        if cfg!(windows) { n.eq_ignore_ascii_case(&want) } else { n == want }
    })?;
    let since = std::time::UNIX_EPOCH + std::time::Duration::from_millis(started_ms.saturating_sub(10_000).max(0) as u64);
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().is_some_and(|x| x == "jsonl"))
        .filter_map(|p| Some((std::fs::metadata(&p).ok()?.modified().ok()?, p)))
        .filter(|(m, _)| *m >= since)
        .max_by_key(|(m, _)| *m)
        .map(|(_, p)| p)
}

/// 读一个会话文件（会话 id 就是文件名）。终端正在写的也能读：写到一半的最后一行会跳过。
pub fn read_file(path: &Path) -> Result<Conversation> {
    let id = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let f = std::fs::File::open(path).with_context(|| path.display().to_string())?;
    let mut c = Conversation { harness: Harness::Claude, id, title: None, cwd: None, created_ms: 0, model: None, items: Vec::new(), manifest: Manifest::default() };
    let (mut custom, mut summary) = (None, None);
    // 同一条助手消息拆成好几行：文字接在一起
    let mut last_msg: Option<String> = None;
    for line in BufReader::new(f).lines() {
        let line = line?;
        let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
        let ty = v["type"].as_str().unwrap_or_default();
        match ty {
            "custom-title" => custom = v["customTitle"].as_str().map(str::to_string).or(custom),
            "summary" => summary = v["summary"].as_str().map(str::to_string).or(summary),
            "user" | "assistant" => {}
            _ => continue,
        }
        if ty != "user" && ty != "assistant" {
            continue;
        }
        if v["isSidechain"] == true {
            continue;
        }
        let t = parse_ms(v["timestamp"].as_str(), c.created_ms);
        if c.created_ms == 0 {
            c.created_ms = t;
        }
        if c.cwd.is_none() {
            c.cwd = v["cwd"].as_str().map(str::to_string);
        }
        if v["isMeta"] == true {
            c.manifest.drop("injected_context");
            continue;
        }
        if v["isCompactSummary"] == true {
            c.manifest.drop("compaction_summary");
            continue;
        }
        let msg = &v["message"];
        if ty == "user" {
            last_msg = None;
            match &msg["content"] {
                Value::String(s) if injected(s) => c.manifest.drop("injected_context"),
                Value::String(s) => c.items.push(Item::User { text: s.clone(), time_ms: t }),
                Value::Array(blocks) => {
                    let mut texts = Vec::new();
                    for b in blocks {
                        match b["type"].as_str() {
                            Some("text") => {
                                let s = b["text"].as_str().unwrap_or_default();
                                if injected(s) {
                                    c.manifest.drop("injected_context");
                                } else {
                                    texts.push(s.to_string());
                                }
                            }
                            Some("tool_result") => c.items.push(Item::ToolResult {
                                call_id: b["tool_use_id"].as_str().unwrap_or_default().to_string(),
                                output: result_text(&b["content"], &mut c.manifest),
                                is_error: b["is_error"] == true,
                                time_ms: t,
                            }),
                            Some("image") => c.manifest.drop("image"),
                            Some("document") => c.manifest.drop("file"),
                            _ => c.manifest.drop("other_content"),
                        }
                    }
                    let text = texts.join("\n\n");
                    if !text.trim().is_empty() {
                        c.items.push(Item::User { text, time_ms: t });
                    }
                }
                _ => {}
            }
            continue;
        }
        // 助手
        if let Some(m) = msg["model"].as_str().filter(|m| !m.starts_with('<')) {
            c.model = Some(m.to_string());
        }
        let mid = msg["id"].as_str().map(str::to_string);
        for b in msg["content"].as_array().into_iter().flatten() {
            match b["type"].as_str() {
                Some("text") => {
                    let s = b["text"].as_str().unwrap_or_default().to_string();
                    let same = mid.is_some() && last_msg == mid;
                    match c.items.last_mut() {
                        Some(Item::Assistant { text, .. }) if same => {
                            text.push_str("\n\n");
                            text.push_str(&s);
                        }
                        _ => c.items.push(Item::Assistant { text: s, time_ms: t }),
                    }
                }
                Some("tool_use") => c.items.push(Item::ToolCall {
                    id: b["id"].as_str().unwrap_or_default().to_string(),
                    name: b["name"].as_str().unwrap_or("tool").to_string(),
                    arguments: serde_json::to_string(&b["input"]).unwrap_or_else(|_| "{}".into()),
                    time_ms: t,
                }),
                Some("thinking" | "redacted_thinking") => c.manifest.drop("reasoning"),
                Some("image") => c.manifest.drop("image"),
                _ => c.manifest.drop("other_content"),
            }
        }
        last_msg = mid;
    }
    c.title = custom.or(summary).or_else(|| title_from(&c.items));
    Ok(c)
}

fn iso(ms: i64) -> String {
    time::OffsetDateTime::from_unix_timestamp_nanos(i128::from(ms) * 1_000_000)
        .ok()
        .and_then(|t| t.format(&time::format_description::well_known::Rfc3339).ok())
        .unwrap_or_default()
}

fn clip(s: &str) -> String {
    match s.char_indices().nth(TOOL_TEXT_MAX) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_string(),
    }
}

/// 写成一个新的 Claude Code 会话。
pub fn write(home: &Path, c: &Conversation) -> Result<Migrated> {
    let sid = uuid::Uuid::new_v4().to_string();
    let cwd = c.cwd.clone().filter(|d| Path::new(d).is_absolute()).or_else(|| dirs::home_dir().map(|h| h.display().to_string())).unwrap_or_else(|| ".".into());
    let dir = projects(home).join(project_dir_name(&cwd));
    std::fs::create_dir_all(&dir).with_context(|| dir.display().to_string())?;
    let native = c.harness == Harness::Claude;
    let model = c.model.clone().filter(|_| native).unwrap_or_else(|| "imported".into());
    let mut lines: Vec<Value> = Vec::new();
    let mut parent: Option<String> = None;
    // 同一条助手消息（直到下一条用户记录为止）共用一个 message.id，和 Claude Code 自己记录的方式一样
    let mut msg_id: Option<String> = None;
    let mut n = 0usize;
    let base = |parent: &Option<String>, uuid: &str, t: i64, ty: &str| {
        json!({
            "parentUuid": parent,
            "isSidechain": false,
            "userType": "external",
            "cwd": cwd,
            "sessionId": sid,
            "version": "2.0.0",
            "type": ty,
            "uuid": uuid,
            "timestamp": iso(t),
        })
    };
    let push_user = |lines: &mut Vec<Value>, parent: &mut Option<String>, msg_id: &mut Option<String>, content: Value, t: i64| {
        let u = uuid::Uuid::new_v4().to_string();
        let mut v = base(parent, &u, t, "user");
        v["message"] = json!({"role": "user", "content": content});
        lines.push(v);
        *parent = Some(u);
        *msg_id = None;
    };
    let push_assistant = |lines: &mut Vec<Value>, parent: &mut Option<String>, msg_id: &mut Option<String>, n: &mut usize, block: Value, t: i64| {
        let u = uuid::Uuid::new_v4().to_string();
        let id = msg_id.get_or_insert_with(|| {
            *n += 1;
            format!("msg_looklook_{}_{}", &sid[..8], n)
        });
        let mut v = base(parent, &u, t, "assistant");
        v["message"] = json!({
            "id": id, "type": "message", "role": "assistant", "model": model,
            "content": [block], "stop_reason": null, "stop_sequence": null,
            "usage": {"input_tokens": 0, "output_tokens": 0},
        });
        lines.push(v);
        *parent = Some(u);
    };
    // 写成文字的工具调用：它们的结果也写成文字
    let mut as_text: HashMap<String, String> = HashMap::new();
    for it in &c.items {
        match it {
            Item::User { text, time_ms } => push_user(&mut lines, &mut parent, &mut msg_id, json!(text), *time_ms),
            Item::Assistant { text, time_ms } => push_assistant(&mut lines, &mut parent, &mut msg_id, &mut n, json!({"type": "text", "text": text}), *time_ms),
            Item::ToolCall { id, name, arguments, time_ms } => {
                if native {
                    let input: Value = serde_json::from_str(arguments).unwrap_or_else(|_| json!({}));
                    push_assistant(&mut lines, &mut parent, &mut msg_id, &mut n, json!({"type": "tool_use", "id": id, "name": name, "input": input}), *time_ms);
                } else {
                    as_text.insert(id.clone(), name.clone());
                    let text = format!("[Tool call before migration: {name}]\n```\n{}\n```", clip(arguments));
                    push_assistant(&mut lines, &mut parent, &mut msg_id, &mut n, json!({"type": "text", "text": text}), *time_ms);
                }
            }
            Item::ToolResult { call_id, output, is_error, time_ms } => {
                if let Some(name) = as_text.get(call_id) {
                    let label = if *is_error { "error" } else { "result" };
                    let text = format!("[Tool {label}: {name}]\n```\n{}\n```", clip(output));
                    push_assistant(&mut lines, &mut parent, &mut msg_id, &mut n, json!({"type": "text", "text": text}), *time_ms);
                } else {
                    let block = json!({"type": "tool_result", "tool_use_id": call_id, "content": output, "is_error": is_error});
                    push_user(&mut lines, &mut parent, &mut msg_id, json!([block]), *time_ms);
                }
            }
        }
    }
    if lines.is_empty() {
        bail!("这个会话里没有可以迁移的对话 / This session has no conversation to migrate");
    }
    if let Some(t) = &c.title {
        lines.push(json!({"type": "custom-title", "customTitle": t, "sessionId": sid}));
    }
    let path = dir.join(format!("{sid}.jsonl"));
    let tmp = dir.join(format!(".{sid}.jsonl.tmp"));
    {
        let mut f = std::fs::File::create(&tmp)?;
        for l in &lines {
            serde_json::to_writer(&mut f, l)?;
            f.write_all(b"\n")?;
        }
        f.sync_all()?;
    }
    std::fs::rename(&tmp, &path)?;
    Ok(Migrated { harness: Harness::Claude, id: sid.clone(), cwd: Some(cwd), resume: format!("claude --resume {sid}"), items: c.items.len(), manifest: c.manifest.clone() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_dir_matches_claude_code() {
        assert_eq!(project_dir_name("F:\\GitProject"), "F--GitProject");
        assert_eq!(project_dir_name("F:\\GitProject\\LocalMinimax"), "F--GitProject-LocalMinimax");
        assert_eq!(project_dir_name("/home/me/a.b"), "-home-me-a-b");
        assert_eq!(project_dir_name("F:\\视频"), "F----");
    }

    #[test]
    fn round_trip() {
        let home = std::env::temp_dir().join(format!("ll-claude-test-{}", uuid::Uuid::new_v4()));
        let cwd = home.join("work").display().to_string();
        let src = Conversation {
            harness: Harness::Codex,
            id: "x".into(),
            title: Some("测试".into()),
            cwd: Some(cwd.clone()),
            created_ms: 1_000,
            model: Some("gpt".into()),
            items: vec![
                Item::User { text: "列一下文件".into(), time_ms: 1_000 },
                Item::ToolCall { id: "a".into(), name: "exec".into(), arguments: "ls".into(), time_ms: 1_001 },
                Item::ToolResult { call_id: "a".into(), output: "a.txt".into(), is_error: false, time_ms: 1_002 },
                Item::Assistant { text: "有 a.txt".into(), time_ms: 1_003 },
            ],
            manifest: Manifest::default(),
        };
        let m = write(&home, &src).unwrap();
        assert!(m.resume.starts_with("claude --resume "));
        let list = list(&home).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].title.as_deref(), Some("测试"));
        assert_eq!(list[0].cwd.as_deref(), Some(cwd.as_str()));
        let back = read(&home, &m.id).unwrap();
        // 工具调用写成了文字，读回来是三段助手文字（同一条消息里的合在一起）
        assert_eq!(back.items.len(), 2);
        assert!(matches!(&back.items[0], Item::User { text, .. } if text == "列一下文件"));
        match &back.items[1] {
            Item::Assistant { text, .. } => {
                assert!(text.contains("[Tool call before migration: exec]"));
                assert!(text.contains("a.txt"));
                assert!(text.ends_with("有 a.txt"));
            }
            other => panic!("{other:?}"),
        }
        // 从 Claude Code 来的工具调用保持原样
        let mut native = back.clone();
        native.harness = Harness::Claude;
        native.items = vec![
            Item::User { text: "看看".into(), time_ms: 1 },
            Item::ToolCall { id: "toolu_1".into(), name: "Bash".into(), arguments: r#"{"command":"ls"}"#.into(), time_ms: 2 },
            Item::ToolResult { call_id: "toolu_1".into(), output: "a.txt".into(), is_error: false, time_ms: 3 },
        ];
        let m2 = write(&home, &native).unwrap();
        let back2 = read(&home, &m2.id).unwrap();
        assert!(matches!(&back2.items[1], Item::ToolCall { name, .. } if name == "Bash"));
        assert!(matches!(&back2.items[2], Item::ToolResult { call_id, .. } if call_id == "toolu_1"));
        let _ = std::fs::remove_dir_all(&home);
    }
}
