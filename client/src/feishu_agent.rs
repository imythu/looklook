//! 在飞书里和 AI 对话（docs/FEISHU_AGENT.md）：私聊里直接发 = 跟“当前会话”说（第一次开一个），`/new` 开新的；
//! 回复某张卡片 = 跟那张卡片的会话说；项目群里一个话题一个会话；转过来的别人的问题按来源定项目、开新会话（§11，`feishu_route.rs`）。
//!
//! - **不另写 agent**：开会话照会话页“新建”（`gateway::create_session`），接着说就是 `Chat::input`，
//!   Claude Code、Codex 各走各的老路；
//! - **新会话的入口**：第一次发消息、`/new`（或“新会话”）、结果卡片上的「新会话」、机器人菜单「新会话」、项目群里发新话题；
//! - **收到**：先给消息加“正在输入”表情；1 秒内连发的几条合成一轮，只发了图片 / 文件就先存着等下一句；
//!   “停止”马上停；合并转发展开成文字，引用的消息带上原文；
//! - **输出**：会话所在的电脑订阅这个会话（跟开着对话页一样），每一轮一张 CardKit 流式卡片（工具步骤、思考折叠，§2.3），
//!   这一轮里提到的新文件（图片、PDF 等）跟着发回去；
//! - **当前会话**、**项目群** 记在同步项里（`notify/agent_current`、`notify/agent_projects`），哪台电脑收到消息都认得；
//! - **转发**：消息事件随机落到任意一台电脑（集群模式），按 `parent_id` / `root_id` 找到会话所在的电脑，
//!   经 WebRTC 带设备签名转过去（`POST /api/notify/agent`）；
//! - **记录**：消息 → 会话 记在发消息的那台电脑上（store，14 天），不另外同步。

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::anyhow;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::chat::Event;
use crate::error::LocalError;
use crate::feishu::card::{self, Act};
use crate::feishu::{self, Api, Resource};
use crate::feishu_route::{self as fr, Origin, Pick};
use crate::gateway::App;
use crate::notify::{self, load_feishu, load_rules, tr, Feishu};

/// 卡片按钮：开会话、停止这一轮、在同一个项目里开新会话、在飞书里接着一个会话
pub const START: &str = "ag_start";
pub const STOP: &str = "ag_stop";
pub const NEW: &str = "ag_new";
pub const PICK: &str = "ag_pick";
/// 转过来的问题：换项目、列出别的会话、接到那个会话、回复来源、当新问题（不接旧会话）
pub const MOVE: &str = "ag_move";
pub const INTO: &str = "ag_into";
pub const INTO_GO: &str = "ag_into_go";
pub const REPLY: &str = "ag_reply";
pub const FRESH: &str = "ag_fresh";
/// 电脑之间请求的签名前缀（和卡片操作的分开，不能互相冒用）
pub const RPC_KIND: &str = "looklook-notify-agent-v1";
const RPC_PATH: &str = "/api/notify/agent";
const ROUTES_KEY: &str = "feishu_agent.routes";
/// 飞书里开的会话的第一句话（换项目、接到别的会话、回复来源时用）
const SEEDS_KEY: &str = "feishu_agent.seeds";
const SEEDS_MAX: usize = 300;
const ROUTES_MAX: usize = 2000;
const ROUTE_TTL_MS: i64 = 14 * 24 * 3600 * 1000;
/// 一句话最多多少字（要放进开会话卡片按钮的 value 里，卡片不能超过 30 KB）
const TEXT_MAX: usize = 4000;
/// 卡片正文最多多少字节（只留最后的；卡片整张不能超过 30 KB，还要放工具步骤和思考）
const BODY_BYTES: usize = 18_000;
/// 思考最多放多少字节（只留开头）
const THINK_BYTES: usize = 2_000;
/// 连发的消息在这么久里合成一轮
const MERGE_GAP: Duration = Duration::from_millis(1000);
/// 只发了图片 / 文件时先存着，最多等下一句话这么久
const HOLD: Duration = Duration::from_secs(600);
/// 同一种缺权限的提示多久最多发一次
const GRANT_GAP: Duration = Duration::from_secs(12 * 3600);
/// “正在输入”的表情
const TYPING: &str = "Typing";
/// 最多多久推一次
const PUSH_GAP: Duration = Duration::from_millis(600);
const PLACES_PER_DEVICE: usize = 8;
const PLACES_MAX: usize = 40;
/// 会话列表最多列几个
const SESSIONS_MAX: usize = 15;
/// 问别的电脑最多等多久（建 WebRTC 连接可能要几秒）
const PEER_WAIT: Duration = Duration::from_secs(8);
/// 会话一直没跑起来（claude 没装、启动失败）最多等多久
const START_WAIT_MS: i64 = 120_000;
/// 一轮结束后再跟读这么久，最后一段回复写进记录文件可能晚一点
const END_SETTLE: Duration = Duration::from_millis(1500);
/// 一条消息最多带几个图片和文件；每个最大多少（飞书发文件上限 30 MB，图片 10 MB）
const FILES_MAX: usize = 9;
const FILE_BYTES: usize = 30 << 20;
const IMAGE_BYTES: u64 = 10 << 20;
/// 一轮最多发回几个文件
const OUTPUTS_MAX: usize = 5;

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// 卡片按钮是不是这里处理（停止在 `Notifier::act` 里，因为要跟着会话所在的电脑走）
pub fn handles(a: &str) -> bool {
    matches!(a, START | NEW | PICK | MOVE | INTO | INTO_GO | REPLY | FRESH)
}

/// 飞书里开的一个会话最开始交给它的东西
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Seed {
    i: String,
    text: String,
    #[serde(default)]
    files: Vec<Resource>,
    /// 原话那条消息
    #[serde(default)]
    m: String,
    #[serde(default)]
    origin: Option<Origin>,
    /// 开在哪个文件夹
    #[serde(default)]
    dir: String,
    at: i64,
}

/// 一条消息 → 一个会话
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Route {
    m: String,
    i: String,
    at: i64,
}

/// 收到的一条消息（等着跟 1 秒内的下一条合并）
#[derive(Debug, Clone)]
struct In {
    id: String,
    text: String,
    files: Vec<Resource>,
    thread: Vec<String>,
    /// 加上的“正在输入”表情
    typing: Option<String>,
    /// 转过来的（合并转发、看起来像转发的）：来源
    origin: Option<Origin>,
    /// 群里 @ 的（不是项目群）
    group: bool,
    at: Instant,
}

#[derive(Default)]
struct Batch {
    gen: u64,
    ins: Vec<In>,
    /// 只有文件时已经回过“要做什么？”
    hinted: bool,
}

#[derive(Default)]
pub struct Agent {
    /// 等着合并的消息：私聊 / 群 + 话题 → 这一批
    pending: Mutex<HashMap<String, Batch>>,
    /// 这台电脑上的会话这一轮结束时要去掉的“正在输入”表情：会话 → (消息, 表情 id)
    typing: Mutex<HashMap<String, Vec<(String, String)>>>,
    /// 缺权限的提示上次什么时候发的（按原因）
    asked: Mutex<HashMap<String, Instant>>,
    /// 消息 → 会话（读一次 store，之后在内存里改，改完写回）
    routes: Mutex<Option<Vec<Route>>>,
    /// 正在推流式卡片的会话 → 停止按钮的 nonce
    live: Mutex<HashMap<String, String>>,
    /// 用流式卡片推过结果的一轮（会话, 结束时间）：“做完了”通知不再另发
    streamed: Mutex<VecDeque<(String, i64)>>,
    /// 正在开的会话（按原话那条消息）：同一条只开一次
    starting: Mutex<HashSet<String>>,
    /// 机器人自己的 open_id（应用 id → open_id），群里认 @
    bot: Mutex<Option<(String, String)>>,
    /// 会话的第一句话（读一次 store）
    seeds: Mutex<Option<Vec<Seed>>>,
}

impl Agent {
    fn with_routes<T>(&self, app: &App, f: impl FnOnce(&mut Vec<Route>) -> T) -> T {
        let mut g = lock(&self.routes);
        let routes = g.get_or_insert_with(|| app.store.get::<Vec<Route>>(ROUTES_KEY).ok().flatten().unwrap_or_default());
        f(routes)
    }

    fn add_route(&self, app: &App, msg: &str, inst: &str) {
        let now = crate::util::system_ms();
        let snapshot = self.with_routes(app, |r| {
            r.retain(|x| now - x.at < ROUTE_TTL_MS && x.m != msg);
            r.push(Route { m: msg.into(), i: inst.into(), at: now });
            if r.len() > ROUTES_MAX {
                r.drain(..r.len() - ROUTES_MAX);
            }
            r.clone()
        });
        if let Err(e) = app.store.set(ROUTES_KEY, &snapshot) {
            tracing::warn!(error = %e, "飞书：消息和会话的对应没存上");
        }
    }

    /// 这几条消息里有没有本机会话的（自己记的，或者本机发的通知卡片）
    fn route_of(&self, app: &App, ids: &[String]) -> Option<String> {
        let found = ids.iter().find_map(|id| self.with_routes(app, |r| r.iter().rev().find(|x| x.m == *id).map(|x| x.i.clone())).or_else(|| app.notify.inst_of_card(id)))?;
        app.store.instance(&found).ok().flatten().map(|_| found)
    }

    /// 不再把消息对应到这个会话（换了项目、接到了别的会话）
    fn forget(&self, app: &App, inst: &str) {
        let snapshot = self.with_routes(app, |r| {
            r.retain(|x| x.i != inst);
            r.clone()
        });
        if let Err(e) = app.store.set(ROUTES_KEY, &snapshot) {
            tracing::warn!(error = %e, "飞书：消息和会话的对应没存上");
        }
    }

    fn with_seeds<T>(&self, app: &App, f: impl FnOnce(&mut Vec<Seed>) -> T) -> T {
        let mut g = lock(&self.seeds);
        let seeds = g.get_or_insert_with(|| app.store.get::<Vec<Seed>>(SEEDS_KEY).ok().flatten().unwrap_or_default());
        f(seeds)
    }

    fn add_seed(&self, app: &App, seed: Seed) {
        let snapshot = self.with_seeds(app, |v| {
            v.retain(|x| x.i != seed.i);
            v.push(seed);
            if v.len() > SEEDS_MAX {
                v.drain(..v.len() - SEEDS_MAX);
            }
            v.clone()
        });
        if let Err(e) = app.store.set(SEEDS_KEY, &snapshot) {
            tracing::warn!(error = %e, "飞书：会话的第一句话没存上");
        }
    }

    fn seed(&self, app: &App, inst: &str) -> Option<Seed> {
        self.with_seeds(app, |v| v.iter().find(|x| x.i == inst).cloned())
    }

    /// 在飞书里对话过的会话：通知卡片回复到它最近一条消息的话题里
    pub fn anchor(&self, app: &App, inst: &str) -> Option<String> {
        self.with_routes(app, |r| r.iter().rev().find(|x| x.i == inst).map(|x| x.m.clone()))
    }

    /// 正在推流式卡片
    pub fn following(&self, inst: &str) -> bool {
        lock(&self.live).contains_key(inst)
    }

    /// 这一轮的结果由流式卡片推（或已经推过了）
    pub fn covers(&self, inst: &str, ended_at: i64) -> bool {
        self.following(inst) || lock(&self.streamed).iter().any(|(i, e)| i == inst && *e == ended_at)
    }

    fn streamed(&self, inst: &str, ended_at: i64) {
        let mut g = lock(&self.streamed);
        g.push_back((inst.into(), ended_at));
        while g.len() > 200 {
            g.pop_front();
        }
    }

    /// 流式卡片上的“停止”
    pub async fn stop(&self, app: &App, act: &Act, en: bool) -> Result<(String, Option<Value>), LocalError> {
        if lock(&self.live).get(&act.inst) != Some(&act.n) {
            return Ok((tr(en, "这一轮已经结束了", "This turn has already ended"), None));
        }
        app.chat.interrupt(&act.inst).await?;
        tracing::info!(inst = %act.inst, "飞书：停止了一轮");
        // 流式期间回调后不能马上改卡片：卡片由推流的任务在这一轮结束时换掉
        Ok((tr(en, "已停止", "Stopped"), None))
    }

    /// 会话这一轮结束时去掉这些表情
    fn hold_typing(&self, inst: &str, list: Vec<(String, String)>) {
        if !list.is_empty() {
            lock(&self.typing).entry(inst.into()).or_default().extend(list);
        }
    }

    async fn release_typing(&self, app: &App, inst: &str) {
        let list = lock(&self.typing).remove(inst).unwrap_or_default();
        if let (false, Some(f)) = (list.is_empty(), load_feishu(&app.store)) {
            drop_typing(&app.notify.api(&f), &list).await;
        }
    }

    /// 机器人的 open_id（换了应用就重新取）
    async fn bot_id(&self, f: &Feishu, api: &Api) -> Option<String> {
        if let Some((app_id, id)) = lock(&self.bot).clone() {
            if app_id == f.app_id {
                return Some(id);
            }
        }
        match api.bot_open_id().await {
            Ok(id) => {
                *lock(&self.bot) = Some((f.app_id.clone(), id.clone()));
                Some(id)
            }
            Err(e) => {
                tracing::warn!(error = %e, "飞书：取不到机器人的 open_id");
                None
            }
        }
    }
}

// ---------------- 收消息 ----------------

/// 消息里的文字：纯文本，或富文本（post）拼成的文字
pub fn message_text(msg: &Value) -> String {
    let c = content(msg);
    let s = match msg["message_type"].as_str() {
        Some("text") => c["text"].as_str().unwrap_or_default().to_string(),
        Some("post") => post_text(&c),
        _ => String::new(),
    };
    s.trim().to_string()
}

fn content(msg: &Value) -> Value {
    msg["content"].as_str().and_then(|c| serde_json::from_str(c).ok()).unwrap_or_default()
}

/// 富文本的正文：`{title, content: [[{tag, …}…]…]}`，有时外面还包一层语言（`{"zh_cn": {…}}`）
fn post_body(c: &Value) -> &Value {
    if c["content"].is_array() {
        c
    } else {
        c.as_object().and_then(|m| m.values().find(|v| v["content"].is_array())).unwrap_or(c)
    }
}

fn post_text(c: &Value) -> String {
    let body = post_body(c);
    let mut lines = Vec::new();
    if let Some(t) = body["title"].as_str().filter(|t| !t.is_empty()) {
        lines.push(t.to_string());
    }
    for para in body["content"].as_array().into_iter().flatten() {
        let mut line = String::new();
        for el in para.as_array().into_iter().flatten() {
            match el["tag"].as_str() {
                Some("text" | "a") => line.push_str(el["text"].as_str().unwrap_or_default()),
                Some("at") => line.push_str(&format!("@{}", el["user_name"].as_str().unwrap_or_default())),
                Some("code_block") => line.push_str(&format!("```{}\n{}\n```", el["language"].as_str().unwrap_or_default().to_lowercase(), el["text"].as_str().unwrap_or_default().trim_end())),
                _ => {}
            }
        }
        lines.push(line);
    }
    lines.join("\n")
}

/// 消息里的图片和文件：图片消息、文件消息、视频（当文件）、富文本里的图片
pub(crate) fn attachments(msg: &Value) -> Vec<Resource> {
    let c = content(msg);
    let id = msg["message_id"].as_str().unwrap_or_default();
    let res = |kind: &str, key: &Value, name: &Value| key.as_str().filter(|k| !k.is_empty()).map(|k| Resource { kind: kind.into(), key: k.into(), name: name.as_str().unwrap_or_default().into(), msg: id.into() });
    let mut out: Vec<Resource> = match msg["message_type"].as_str() {
        Some("image") => res("image", &c["image_key"], &Value::Null).into_iter().collect(),
        Some("file" | "media") => res("file", &c["file_key"], &c["file_name"]).into_iter().collect(),
        Some("post") => post_body(&c)["content"].as_array().into_iter().flatten().flat_map(|p| p.as_array().into_iter().flatten()).filter(|el| el["tag"] == "img").filter_map(|el| res("image", &el["image_key"], &Value::Null)).collect(),
        _ => vec![],
    };
    out.truncate(FILES_MAX);
    out
}

/// 群里：@ 的是不是这个机器人
fn mentions_bot(msg: &Value, bot: &str) -> bool {
    msg["mentions"].as_array().into_iter().flatten().any(|m| m["id"]["open_id"] == bot)
}

/// 文字里的 `@_user_1` 占位：@ 机器人的去掉，@ 别人的换成名字
fn clean_mentions(text: &str, mentions: &Value, bot: &str) -> String {
    let mut t = text.to_string();
    for m in mentions.as_array().into_iter().flatten() {
        let Some(key) = m["key"].as_str().filter(|k| !k.is_empty()) else { continue };
        let with = if m["id"]["open_id"] == bot { String::new() } else { format!("@{}", m["name"].as_str().unwrap_or_default()) };
        t = t.replace(key, &with);
    }
    t.trim().to_string()
}

/// `/new 第一句话`、`新会话 第一句话`：要开新会话，返回后面的话（可以为空）
fn new_command(text: &str) -> Option<String> {
    for p in ["/new", "新会话"] {
        if let Some(rest) = text.strip_prefix(p) {
            if rest.is_empty() || rest.starts_with(char::is_whitespace) || rest.starts_with([':', '：']) {
                return Some(rest.trim_start_matches([':', '：']).trim().to_string());
            }
        }
    }
    None
}

/// “停止”：只认整句（`停止。`、`stop` 都算），“停止那个服务”不算
fn is_stop(text: &str) -> bool {
    let t = text.trim().trim_end_matches(['。', '.', '!', '！', '~', '～']).trim().to_lowercase();
    matches!(t.as_str(), "停止" | "停" | "停下" | "停一下" | "别做了" | "stop" | "/stop" | "abort")
}

/// “结束”：这个会话的事办完了，之后同一个来源再转来的开新会话
fn is_end(text: &str) -> bool {
    let t = text.trim().trim_end_matches(['。', '.', '!', '！']).trim().to_lowercase();
    matches!(t.as_str(), "结束" | "/done" | "/end" | "done")
}

fn is_help(text: &str) -> bool {
    matches!(text.trim().to_lowercase().as_str(), "/help" | "help" | "帮助" | "/帮助")
}

/// `/group 名字`、`建群 名字`：给当前项目建一个项目群，返回群名（可以为空）
fn group_command(text: &str) -> Option<String> {
    for p in ["/group", "建群", "项目群"] {
        if let Some(rest) = text.strip_prefix(p) {
            if rest.is_empty() || rest.starts_with(char::is_whitespace) || rest.starts_with([':', '：']) {
                return Some(rest.trim_start_matches([':', '：']).trim().chars().take(60).collect());
            }
        }
    }
    None
}

/// 开放接口取回的消息（`msg_type`、`body.content`）换成事件里的样子（`message_type`、`content`）
fn as_event_msg(m: &Value) -> Value {
    json!({ "message_id": m["message_id"], "message_type": m["msg_type"], "content": m["body"]["content"] })
}

/// 合并转发的聊天记录：子消息的文字（一条一行，前面写是谁说的）、里面的图片和文件、来源。
/// 名字要通讯录权限，不要：你自己写“我”，别人按出现的顺序写 A、B…（只有一个人时写“对方”）。
/// `me`：绑定的人的 open_id；`chat`：转到的这个私聊（子消息带的群不是它时才当原来的群）
fn forwarded_parts(id: &str, items: &[Value], me: &str, chat: &str, en: bool) -> (String, Vec<Resource>, Origin) {
    let subs: Vec<&Value> = items.iter().filter(|m| m["upper_message_id"] == id).collect();
    let who = |m: &Value| m["sender"]["id"].as_str().filter(|_| m["sender"]["sender_type"] == "user").unwrap_or_default().to_string();
    // 别人：按第一次出现的顺序，数一下各说了几条
    let mut others: Vec<(String, usize)> = Vec::new();
    for m in &subs {
        let w = who(m);
        if w.is_empty() || w == me {
            continue;
        }
        match others.iter_mut().find(|(x, _)| *x == w) {
            Some(o) => o.1 += 1,
            None => others.push((w, 1)),
        }
    }
    let label = |w: &str| -> String {
        if !me.is_empty() && w == me {
            return tr(en, "我", "Me");
        }
        match others.iter().position(|(x, _)| x == w) {
            Some(_) if others.len() == 1 => tr(en, "对方", "Them"),
            Some(i) => char::from(b'A' + (i % 26) as u8).to_string(),
            None => String::new(),
        }
    };
    let mut lines = Vec::new();
    let mut files = Vec::new();
    let mut hint = String::new();
    for m in &subs {
        let e = as_event_msg(m);
        let t = message_text(&e);
        if !t.is_empty() {
            let w = label(&who(m));
            if hint.is_empty() && who(m) != me {
                hint = t.chars().take(60).collect();
            }
            let body = t.lines().map(|l| format!("> {l}")).collect::<Vec<_>>().join("\n");
            lines.push(if w.is_empty() { body } else { format!("> **{w}**：\n{body}") });
        }
        files.extend(attachments(&e));
    }
    let user = others.iter().max_by_key(|(_, n)| *n).map(|(w, _)| w.clone()).unwrap_or_default();
    let from = subs.iter().filter_map(|m| m["chat_id"].as_str()).find(|c| c.starts_with("oc_") && *c != chat).unwrap_or_default();
    let origin = Origin { user, chat: from.to_string(), hint };
    if lines.is_empty() {
        return (String::new(), files, origin);
    }
    (format!("{}\n{}", tr(en, "转发的聊天记录：", "Forwarded chat history:"), lines.join("\n>\n")), files, origin)
}

/// 回复的那条消息的文字（当引用带给 AI），最多 1000 字
async fn quoted(app: &App, api: &Api, id: &str) -> Option<String> {
    match api.get_message(id).await {
        Ok(items) => {
            let t = message_text(&as_event_msg(items.first()?));
            (!t.is_empty()).then(|| t.chars().take(1000).collect())
        }
        Err(e) => {
            scope_hint(app, api, &e, "read").await;
            None
        }
    }
}

/// 这条消息在哪个话题里：上一条、话题的第一条（不含它自己）
fn thread_of(msg: &Value) -> Vec<String> {
    let me = msg["message_id"].as_str().unwrap_or_default();
    let mut ids: Vec<String> = Vec::new();
    for k in ["parent_id", "root_id"] {
        if let Some(id) = msg[k].as_str().filter(|id| !id.is_empty() && *id != me) {
            if !ids.iter().any(|x| x == id) {
                ids.push(id.into());
            }
        }
    }
    ids
}

async fn reply(api: &Api, to: &str, text: &str) {
    if let Err(e) = api.reply_text(to, text).await {
        tracing::warn!(error = %e, "飞书：回复没发出去");
    }
}

fn failed(en: bool, e: &LocalError) -> String {
    tr(en, &format!("没办成：{}", e.code), &format!("That didn't work: {}", e.code))
}

fn off_hint(en: bool) -> String {
    tr(en, "在飞书里和 AI 对话没有打开：到看看的设置 → 通知里打开。", "Chatting with the AI in Feishu is off. Turn it on in LookLook Settings → Notifications.")
}

/// 去掉“正在输入”表情
async fn drop_typing(api: &Api, list: &[(String, String)]) {
    for (m, r) in list {
        if let Err(e) = api.remove_reaction(m, r).await {
            tracing::debug!(error = %e, "飞书：表情没去掉");
        }
    }
}

/// 缺权限：发一张卡片，带一个补权限的链接（在飞书里点开确认），补好后卡片改成“已开通”。同一种原因 12 小时最多一次
async fn scope_hint(app: &App, api: &Api, e: &anyhow::Error, why: &str) {
    if !feishu::code_of(e).is_some_and(feishu::is_scope_error) {
        return;
    }
    grant(app, api, why, &[]).await;
}

pub(crate) async fn grant(app: &App, api: &Api, why: &str, extra: &[&str]) -> Option<String> {
    let me = &app.notify.agent;
    {
        let mut g = lock(&me.asked);
        if g.get(why).is_some_and(|at| at.elapsed() < GRANT_GAP) {
            return None;
        }
        g.insert(why.into(), Instant::now());
    }
    let f = load_feishu(&app.store)?;
    let to = f.open_id.clone()?;
    let en = load_rules(&app.store).lang.starts_with("en");
    let reg = match app.notify.reg_start_with(app, true, extra).await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = ?e, "飞书：补权限的链接没拿到");
            return None;
        }
    };
    let text = match why {
        "typing" => tr(en, "收到消息时加“正在输入”的表情，要开通“发送、删除消息表情回复”。", "Showing a “typing” reaction needs the message-reactions scope."),
        "read" => tr(en, "读合并转发、引用的消息，要开通“读取消息”。", "Reading forwarded and quoted messages needs the read-messages scope."),
        "cardkit" => tr(en, "边写边出的回复卡片要开通“创建与更新卡片”。会话已经在电脑上跑了。", "Streaming reply cards need the card-update scope. The session is already running on your computer."),
        "group" => tr(en, "建项目群要开通“创建群”。", "Creating project groups needs the create-group scope."),
        _ => tr(en, "看看用到的功能缺少权限。", "A feature LookLook uses is missing a scope."),
    };
    let msg = match api.send_card(&to, &card::grant(en, &text, &reg.url)).await {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(error = %e, "飞书：补权限的卡片没发出去");
            return None;
        }
    };
    watch_grant(app, reg.id.clone(), msg, None);
    Some(reg.url)
}

/// 等补权限扫完：成功了把卡片换成 `done`（默认“已开通”）
fn watch_grant(app: &App, id: String, msg: String, done: Option<Value>) {
    let app = app.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(3)).await;
            match app.notify.reg() {
                Some(r) if r.id == id && r.state == "pending" => continue,
                Some(r) if r.id == id && r.state == "done" => {
                    let Some(f) = load_feishu(&app.store) else { return };
                    let en = load_rules(&app.store).lang.starts_with("en");
                    if let Err(e) = app.notify.api(&f).patch_card(&msg, &done.unwrap_or_else(|| card::granted(en))).await {
                        tracing::debug!(error = %e, "飞书：补权限的卡片没改上");
                    }
                    return;
                }
                _ => return,
            }
        }
    });
}

/// 绑定的人发来一条消息（私聊里绑定码之外的；群里的）
pub async fn on_message(app: &App, f: &Feishu, ev: &Value) {
    let msg = &ev["message"];
    let Some(id) = msg["message_id"].as_str() else { return };
    let rules = load_rules(&app.store);
    let en = rules.lang.starts_with("en");
    let api = app.notify.api(f);
    let chat = msg["chat_id"].as_str().unwrap_or_default().to_string();
    let mut text = message_text(msg);
    let in_project = msg["chat_type"] == "group" && project(app, &chat).is_some();
    if msg["chat_type"] == "group" {
        let Some(bot) = app.notify.agent.bot_id(f, &api).await else { return };
        // 项目群不用 @；别的群要开了“群里 @ 机器人也能用”并且 @ 了
        if !in_project && !(rules.agent_groups && mentions_bot(msg, &bot)) {
            return;
        }
        text = clean_mentions(&text, &msg["mentions"], &bot);
    }
    if !rules.agent || !rules.actions {
        return reply(&api, id, &off_hint(en)).await;
    }
    let mut files = attachments(msg);
    let operator = f.open_id.clone().unwrap_or_default();
    let mut origin = None;
    if msg["message_type"] == "merge_forward" {
        match api.get_message(id).await {
            Ok(items) => {
                // 子消息里有没有原来的群、发送人是哪种 id：真飞书上还没核对过，先记下来
                tracing::info!(items = %items.iter().skip(1).take(3).map(|m| json!({ "chat_id": m["chat_id"], "sender": m["sender"], "msg_type": m["msg_type"] }).to_string()).collect::<Vec<_>>().join(" "), "飞书：合并转发的子消息");
                let (t, fs, o) = forwarded_parts(id, &items, &operator, &chat, en);
                text = [text, t].into_iter().filter(|t| !t.is_empty()).collect::<Vec<_>>().join("\n\n");
                files.extend(fs);
                origin = Some(o);
            }
            Err(e) => {
                scope_hint(app, &api, &e, "read").await;
                return reply(&api, id, &tr(en, "读不了这段转发的聊天记录。", "Couldn't read the forwarded chat history.")).await;
            }
        }
    }
    files.truncate(FILES_MAX);
    if text.is_empty() && files.is_empty() {
        return reply(&api, id, &tr(en, "暂时只支持文字、图片、文件和合并转发的聊天记录。", "Only text, images, files and forwarded chat history are supported for now.")).await;
    }
    if text.chars().count() > TEXT_MAX {
        return reply(&api, id, &tr(en, &format!("太长了，一次最多 {TEXT_MAX} 字。"), &format!("Too long: at most {TEXT_MAX} characters."))).await;
    }
    let thread = thread_of(msg);
    // 私聊里不在话题中的单条：飞书不带原作者，按样子猜是不是转发的
    if origin.is_none() && msg["chat_type"] == "p2p" && thread.is_empty() && fr::looks_forwarded(&text) {
        tracing::debug!(keys = ?msg.as_object().map(|o| o.keys().cloned().collect::<Vec<_>>()), "飞书：看起来像转发的");
        origin = Some(Origin { hint: text.chars().take(60).collect(), ..Default::default() });
    }
    // 马上处理的命令
    if is_stop(&text) {
        return stop_cmd(app, &api, id, &chat, &thread, in_project, &operator, en).await;
    }
    if is_end(&text) {
        return end_cmd(app, &api, id, &chat, en).await;
    }
    if is_help(&text) {
        if let Err(e) = api.reply(id, "interactive", &card::help(en), false).await {
            app.notify.record_err(&e);
        }
        return;
    }
    if let Some(name) = group_command(&text).filter(|_| !in_project) {
        return create_project(app, &api, id, &chat, &name, &operator, en).await;
    }
    // 收到就加“正在输入”，这一轮结束时去掉
    let typing = match api.add_reaction(id, TYPING).await {
        Ok(r) => Some(r),
        Err(e) => {
            tracing::debug!(error = %e, "飞书：表情没加上");
            scope_hint(app, &api, &e, "typing").await;
            None
        }
    };
    let group = msg["chat_type"] == "group" && !in_project;
    let input = In { id: id.into(), text, files, thread, typing, origin, group, at: Instant::now() };
    enqueue(app, &chat, input);
}

/// 1 秒内连发的合成一批；只有文件时先存着，回一句“要做什么？”
fn enqueue(app: &App, chat: &str, input: In) {
    let key = format!("{chat}|{}", input.thread.last().map(String::as_str).unwrap_or_default());
    let me = &app.notify.agent;
    let gen = {
        let mut g = lock(&me.pending);
        let b = g.entry(key.clone()).or_default();
        b.ins.retain(|x| x.at.elapsed() < HOLD);
        b.ins.push(input);
        b.gen += 1;
        b.gen
    };
    let (app, chat) = (app.clone(), chat.to_string());
    tokio::spawn(async move {
        tokio::time::sleep(MERGE_GAP).await;
        let me = &app.notify.agent;
        let (ins, hint) = {
            let mut g = lock(&me.pending);
            let Some(b) = g.get_mut(&key).filter(|b| b.gen == gen) else { return };
            if b.ins.iter().all(|x| x.text.is_empty()) {
                let first = !std::mem::replace(&mut b.hinted, true);
                (b.ins.clone(), first)
            } else {
                (g.remove(&key).map(|b| b.ins).unwrap_or_default(), false)
            }
        };
        let Some(f) = load_feishu(&app.store) else { return };
        let api = app.notify.api(&f);
        let en = load_rules(&app.store).lang.starts_with("en");
        let typing: Vec<(String, String)> = ins.iter().filter_map(|x| x.typing.clone().map(|r| (x.id.clone(), r))).collect();
        let Some(last) = ins.last().cloned() else { return };
        if ins.iter().all(|x| x.text.is_empty()) {
            // 只有图片、文件：存着，等下一句话
            drop_typing(&api, &typing).await;
            if hint {
                let n: usize = ins.iter().map(|x| x.files.len()).sum();
                reply(&api, &last.id, &tr(en, &format!("收到 {n} 个图片 / 文件。接着说要拿它们做什么（10 分钟内）。"), &format!("Got {n} image(s) / file(s). Tell me what to do with them (within 10 minutes)."))).await;
            }
            return;
        }
        let text = ins.iter().map(|x| x.text.as_str()).filter(|t| !t.is_empty()).collect::<Vec<_>>().join("\n");
        let files: Vec<Resource> = ins.iter().flat_map(|x| x.files.clone()).take(FILES_MAX).collect();
        let operator = f.open_id.clone().unwrap_or_default();
        let origin = ins.iter().find_map(|x| x.origin.clone());
        let group = ins.iter().any(|x| x.group);
        match route(&app, &api, &chat, &last.id, &last.thread, text, files, typing.clone(), &operator, origin, group, en).await {
            // 交给了会话：表情等这一轮结束时由会话所在的电脑去掉
            Ok(true) => {}
            Ok(false) => drop_typing(&api, &typing).await,
            Err(e) => {
                drop_typing(&api, &typing).await;
                reply(&api, &last.id, &failed(en, &e)).await;
            }
        }
    });
}

/// 一批消息交给谁：话题 / 回复的卡片对应的会话 → 项目群的新话题开新会话 → 转过来的、写明了 `#项目` 的按 §11 定 →
/// 当前会话 → 在上次的地方开新会话 → 问。`origin`：转过来的来源；`group`：群里 @ 的（没有当前会话时按群定项目）。
/// 返回是否交给了会话（`Ok(false)`：回了选择卡片或提示）
#[allow(clippy::too_many_arguments)]
async fn route(app: &App, api: &Api, chat: &str, id: &str, thread: &[String], text: String, files: Vec<Resource>, typing: Vec<(String, String)>, operator: &str, origin: Option<Origin>, group: bool, en: bool) -> Result<bool, LocalError> {
    let proj = project(app, chat);
    let cmd = new_command(&text);
    let found = if thread.is_empty() { None } else { find(app, thread, operator).await };
    let mut text = cmd.clone().unwrap_or(text);
    if !thread.is_empty() && found.is_none() {
        if proj.is_some() {
            let hint = tr(en, "这个话题的会话找不到了（可能已删除，或者那台电脑不在线）。在群里发一条新消息开新会话。", "Can't find this topic's session (deleted, or its computer is offline). Post a new message in the group to start one.");
            reply(api, id, &hint).await;
            return Ok(false);
        }
        // 回复的不是哪个会话的消息：当引用带上
        if let Some(q) = quoted(app, api, &thread[0]).await {
            let q = q.lines().map(|l| format!("> {l}")).collect::<Vec<_>>().join("\n");
            text = format!("{q}\n\n{text}");
        }
    }
    // 项目群里的新话题：在群对应的地方开新会话，回复开在话题里
    if let (Some(p), None) = (&proj, &found) {
        let Some((dev, dir)) = p.place.split_once('|') else { return Err(LocalError::new("NOT_FOUND")) };
        let rpc = Rpc::Start { agent: p.agent.clone(), dir: dir.into(), text, anchor: Anchor::Reply { m: id.into(), th: true }, files, typing, origin: None };
        return call(app, dev, rpc, operator).await.map(|_| true);
    }
    let cur = if proj.is_some() { None } else { current(app, chat) };
    if cmd.is_none() && found.is_none() {
        let named = fr::marks(&text).project.is_some();
        let origin = origin.or_else(|| (group && cur.is_none()).then(|| Origin { chat: chat.into(), ..Default::default() }));
        if origin.is_some() || named {
            let t = Turn { chat, id, text, files, typing, operator, en };
            return route_forward(app, api, t, origin, cur).await;
        }
    }
    match (cmd, found) {
        // 只发了 /new：给一张选择卡片，默认选这个话题（或当前会话）所在的地方
        (Some(_), found) if text.is_empty() && files.is_empty() => {
            let prefer = match (found, cur) {
                (Some(x), _) => Some((format!("{}|{}", x.dev, x.dir), x.agent)),
                (None, Some(c)) => call(app, &c.dev, Rpc::Where { inst: c.inst }, operator).await.ok().and_then(|v| Some((format!("{}|{}", c.dev, v["dir"].as_str()?), v["agent"].as_str()?.to_string()))),
                _ => None,
            };
            let c = picker(app, None, &[], None, prefer, None, en).await;
            api.reply(id, "interactive", &c, false).await.map_err(|e| notify::feishu_error(&e))?;
            Ok(false)
        }
        // /new 带着话：在同一个地方（话题的会话，或当前会话）开新会话
        (Some(_), found) => {
            let at = found.map(|x| (x.dev, x.inst)).or_else(|| cur.map(|c| (c.dev, c.inst)));
            if let Some((dev, inst)) = at {
                match call(app, &dev, Rpc::New { inst, text: text.clone(), chat: chat.into(), files: files.clone(), src: None, typing: typing.clone(), origin: None }, operator).await {
                    Ok(v) => {
                        if let Some(i) = v["inst"].as_str() {
                            set_current(app, chat, Some((&dev, i)));
                        }
                        return Ok(true);
                    }
                    Err(e) => tracing::info!(error = ?e, "飞书：在当前会话的地方开不了新会话，改到上次的地方"),
                }
            }
            start_new(app, api, chat, id, text, files, typing, operator, en).await
        }
        (None, Some(x)) => {
            call(app, &x.dev, Rpc::Say { inst: x.inst.clone(), text, msg: id.into(), files, typing }, operator).await?;
            if proj.is_none() {
                set_current(app, chat, Some((&x.dev, &x.inst)));
            }
            Ok(true)
        }
        (None, None) => {
            if let Some(c) = cur {
                match call(app, &c.dev, Rpc::Say { inst: c.inst.clone(), text: text.clone(), msg: id.into(), files: files.clone(), typing: typing.clone() }, operator).await {
                    Ok(_) => return Ok(true),
                    // 当前会话删掉了：开新的
                    Err(e) if e.code == "NOT_FOUND" => set_current(app, chat, None),
                    Err(e) => {
                        let hint = tr(en, "（发 /new 开一个新会话）", " (send /new to start a new session)");
                        reply(api, id, &format!("{}{hint}", failed(en, &e))).await;
                        return Ok(false);
                    }
                }
            }
            start_new(app, api, chat, id, text, files, typing, operator, en).await
        }
    }
}

/// 一批要交给 AI 的消息
struct Turn<'a> {
    chat: &'a str,
    /// 最后一条（新会话的话题、回答卡片都回复它）
    id: &'a str,
    text: String,
    files: Vec<Resource>,
    typing: Vec<(String, String)>,
    operator: &'a str,
    en: bool,
}

/// 转过来的问题、写明了 `#项目` 的：按 [`fr::decide`] 定交给哪里（docs/FEISHU_AGENT.md §11）
async fn route_forward(app: &App, api: &Api, mut t: Turn<'_>, origin: Option<Origin>, cur: Option<Current>) -> Result<bool, LocalError> {
    let marks = fr::marks(&t.text);
    let mut places: Option<Vec<(String, String)>> = None;
    let mut explicit = None;
    if let Some(name) = &marks.project {
        let list = all_places(app, t.operator).await;
        explicit = fr::find_place(name, &list).map(String::from);
        places = Some(list);
        match &explicit {
            Some(_) => t.text = fr::strip_project(&t.text, name),
            None => reply(api, t.id, &tr(t.en, &format!("没找到叫「{name}」的项目，选一个："), &format!("No project named “{name}”; pick one:"))).await,
        }
    }
    // 不是转发的、写明的项目就是当前会话的：接着说
    if let (None, Some(p), Some(c)) = (&origin, &explicit, &cur) {
        if let Ok(v) = call(app, &c.dev, Rpc::Where { inst: c.inst.clone() }, t.operator).await {
            if v["dir"].as_str().map(|d| format!("{}|{d}", c.dev)).as_deref() == Some(p.as_str()) {
                call(app, &c.dev, Rpc::Say { inst: c.inst.clone(), text: t.text, msg: t.id.into(), files: t.files, typing: t.typing }, t.operator).await?;
                return Ok(true);
            }
        }
    }
    let origin = origin.unwrap_or_default();
    let now = crate::util::system_ms();
    let mut habits: HashMap<String, fr::Habit> = synced(app, "agent_origins");
    let mut pick = fr::decide(&origin, &habits, explicit.as_deref(), None, marks.agent.as_deref(), now);
    if let Pick::Continue { dev, inst } = &pick {
        let ago = habits.values().filter(|h| h.open.as_ref().is_some_and(|(_, i)| i == inst)).map(|h| now - h.at).min().unwrap_or(0);
        match call(app, dev, Rpc::Say { inst: inst.clone(), text: t.text.clone(), msg: t.id.into(), files: t.files.clone(), typing: t.typing.clone() }, t.operator).await {
            Ok(_) => {
                set_current(app, t.chat, Some((dev, inst)));
                fr::touch(&mut habits, &origin, dev, inst, now);
                save_habits(app, &habits);
                let fresh = Act { a: FRESH.into(), dev: dev.clone(), inst: inst.clone(), n: notify::nonce(), t: Some(t.text.clone()), f: t.files.clone(), m: Some(t.id.into()), o: Some(origin.clone()), ..Default::default() };
                if let Err(e) = api.reply(t.id, "interactive", &card::continued(t.en, ago, &fresh), false).await {
                    tracing::debug!(error = %e, "飞书：“接着之前的会话”没发出去");
                }
                return Ok(true);
            }
            Err(e) => {
                tracing::info!(error = ?e, "飞书：同一来源的会话接不上了，改成开新的");
                fr::close(&mut habits, inst);
                save_habits(app, &habits);
                pick = fr::decide(&origin, &habits, explicit.as_deref(), None, marks.agent.as_deref(), now);
            }
        }
    }
    match pick {
        Pick::Start { place, agent } => match start_at(app, &t, &place, &agent, &origin, false).await {
            Ok(()) => Ok(true),
            Err(e) => {
                tracing::info!(error = ?e, place, "飞书：在定好的地方开不了，改成问");
                ask_where(app, api, &t, Some(place), &origin).await
            }
        },
        Pick::Ask { .. } => {
            let list = match places {
                Some(l) => l,
                None => all_places(app, t.operator).await,
            };
            let guessed = fr::guess(&t.text, &list).map(String::from);
            // 认不出来源、也猜不出来：跟以前一样在上次的地方开（新会话，不进当前会话），卡片上可以换项目
            if guessed.is_none() && origin.keys().is_empty() && explicit.is_none() && marks.project.is_none() {
                if let Some((dev, dir, agent)) = last_place(app) {
                    if start_at(app, &t, &format!("{dev}|{dir}"), marks.agent.as_deref().unwrap_or(&agent), &origin, false).await.is_ok() {
                        return Ok(true);
                    }
                }
            }
            ask_where(app, api, &t, guessed, &origin).await
        }
        Pick::Continue { .. } => unreachable!("上面已经处理"),
    }
}

/// 回一张选择卡片：交给哪个项目（`prefer` 预先选好）
async fn ask_where(app: &App, api: &Api, t: &Turn<'_>, prefer: Option<String>, origin: &Origin) -> Result<bool, LocalError> {
    let last = load_last(app);
    let prefer = prefer.map(|p| (p, last.agent.clone()));
    let c = picker(app, Some(&t.text), &t.files, Some(t.id), prefer, Some(origin), t.en).await;
    api.reply(t.id, "interactive", &c, false).await.map_err(|e| notify::feishu_error(&e))?;
    Ok(false)
}

/// 在 `place`（`电脑|文件夹`）开新会话，话题从原话那条消息开始；记下当前会话、上次的地方、来源的习惯
async fn start_at(app: &App, t: &Turn<'_>, place: &str, agent: &str, origin: &Origin, bind: bool) -> Result<(), LocalError> {
    let (dev, dir) = place.split_once('|').ok_or_else(LocalError::not_found)?;
    let agent = if matches!(agent, "claude" | "codex") { agent.to_string() } else { last_place(app).map(|l| l.2).unwrap_or_else(|| "claude".into()) };
    let rpc = Rpc::Start { agent: agent.clone(), dir: dir.into(), text: t.text.clone(), anchor: Anchor::Reply { m: t.id.into(), th: false }, files: t.files.clone(), typing: t.typing.clone(), origin: Some(origin.clone()) };
    let v = call(app, dev, rpc, t.operator).await?;
    let inst = v["inst"].as_str().unwrap_or_default();
    set_current(app, t.chat, Some((dev, inst)));
    save_last(app, dev, dir, &agent);
    remember(app, origin, place, &agent, Some((dev, inst)), bind);
    Ok(())
}

/// 记下来源进了哪个项目（同步项 `notify/agent_origins`）
fn remember(app: &App, origin: &Origin, place: &str, agent: &str, open: Option<(&str, &str)>, bind: bool) {
    if origin.keys().is_empty() {
        return;
    }
    let mut habits: HashMap<String, fr::Habit> = synced(app, "agent_origins");
    fr::record(&mut habits, origin, place, agent, open, bind, crate::util::system_ms());
    save_habits(app, &habits);
}

fn save_habits(app: &App, habits: &HashMap<String, fr::Habit>) {
    if let Err(e) = notify::save(app, "agent_origins", Some(serde_json::to_value(habits).unwrap_or_default())) {
        tracing::warn!(error = ?e, "飞书：来源的习惯没存上");
    }
}

/// “结束”：当前会话的事办完了。下一句话开新会话，同一个来源再转来的也开新的
async fn end_cmd(app: &App, api: &Api, id: &str, chat: &str, en: bool) {
    let text = match current(app, chat) {
        Some(c) => {
            let mut habits: HashMap<String, fr::Habit> = synced(app, "agent_origins");
            if fr::close(&mut habits, &c.inst) {
                save_habits(app, &habits);
            }
            set_current(app, chat, None);
            tr(en, "好，这个会话结束了。下一句话开新会话。", "OK, this session is done. Your next message starts a new one.")
        }
        None => tr(en, "现在没有当前会话。", "There's no current session."),
    };
    reply(api, id, &text).await;
}

/// 每台在线电脑能开会话的地方：(「项目 · 电脑」, `电脑|文件夹`)
async fn all_places(app: &App, operator: &str) -> Vec<(String, String)> {
    let mut list: Vec<(String, String)> = Vec::new();
    for (dev, name, v) in ask_all(app, || Rpc::Places, operator).await {
        let inbox = v["inbox"].as_str().unwrap_or_default();
        for d in v["dirs"].as_array().into_iter().flatten().filter_map(Value::as_str).take(PLACES_PER_DEVICE + 1) {
            let folder = if d == inbox { tr(load_rules(&app.store).lang.starts_with("en"), "杂事（不在项目里）", "Misc (no project)") } else { folder_name(d) };
            list.push((format!("{folder} · {name}"), format!("{dev}|{d}")));
        }
    }
    list.truncate(PLACES_MAX);
    list
}

/// 开新会话：在上次的地方直接开（会话卡片上写着在哪，想换地方发 /new）；没选过、那台电脑不在线时给一张选择卡片
#[allow(clippy::too_many_arguments)]
async fn start_new(app: &App, api: &Api, chat: &str, id: &str, text: String, files: Vec<Resource>, typing: Vec<(String, String)>, operator: &str, en: bool) -> Result<bool, LocalError> {
    if let Some((dev, dir, agent)) = last_place(app) {
        let rpc = Rpc::Start { agent, dir, text: text.clone(), anchor: Anchor::Reply { m: id.into(), th: false }, files: files.clone(), typing, origin: None };
        match call(app, &dev, rpc, operator).await {
            Ok(v) => {
                if let Some(i) = v["inst"].as_str() {
                    set_current(app, chat, Some((&dev, i)));
                }
                return Ok(true);
            }
            Err(e) => tracing::info!(error = ?e, "飞书：在上次的地方开不了，改成问"),
        }
    }
    let c = picker(app, Some(&text), &files, Some(id), None, None, en).await;
    api.reply(id, "interactive", &c, false).await.map_err(|e| notify::feishu_error(&e))?;
    Ok(false)
}

/// “停止”：停下话题的会话（或当前会话）正在跑的这一轮；还没开始的那批消息也不发了
#[allow(clippy::too_many_arguments)]
async fn stop_cmd(app: &App, api: &Api, id: &str, chat: &str, thread: &[String], in_project: bool, operator: &str, en: bool) {
    let key = format!("{chat}|{}", thread.last().map(String::as_str).unwrap_or_default());
    let dropped = lock(&app.notify.agent.pending).remove(&key).map(|b| b.ins).unwrap_or_default();
    let typing: Vec<(String, String)> = dropped.iter().filter_map(|x| x.typing.clone().map(|r| (x.id.clone(), r))).collect();
    drop_typing(api, &typing).await;
    let target = if !thread.is_empty() {
        find(app, thread, operator).await.map(|x| (x.dev, x.inst))
    } else if !in_project {
        current(app, chat).map(|c| (c.dev, c.inst))
    } else {
        None
    };
    let text = match target {
        Some((dev, inst)) => match call(app, &dev, Rpc::Stop { inst }, operator).await {
            Ok(v) if v["stopped"] == true => tr(en, "已停止。", "Stopped."),
            Ok(_) if !dropped.is_empty() => tr(en, "好，刚才那几句不发了。", "OK, those messages won't be sent."),
            Ok(_) => tr(en, "现在没有在跑的。", "Nothing is running."),
            Err(e) => failed(en, &e),
        },
        None if !dropped.is_empty() => tr(en, "好，刚才那几句不发了。", "OK, those messages won't be sent."),
        None => tr(en, "现在没有在跑的会话。", "No session is running."),
    };
    reply(api, id, &text).await;
}

/// 电脑的名字
async fn dev_name(app: &App, dev: &str) -> String {
    if own_dev(app).ok().as_deref() == Some(dev) {
        return crate::settings::Settings::load(&app.store).device_name();
    }
    let own = own_dev(app).unwrap_or_default();
    peers(app, &own).await.into_iter().find(|(id, _)| id == dev).map(|(_, n)| n).unwrap_or_default()
}

/// `/group`：给当前会话（或上次）的地方建一个项目群，群里一个话题一个会话
#[allow(clippy::too_many_arguments)]
async fn create_project(app: &App, api: &Api, id: &str, chat: &str, name: &str, operator: &str, en: bool) {
    let place = match current(app, chat) {
        Some(c) => call(app, &c.dev, Rpc::Where { inst: c.inst }, operator).await.ok().and_then(|v| Some((c.dev, v["dir"].as_str()?.to_string(), v["agent"].as_str()?.to_string()))),
        None => None,
    }
    .or_else(|| last_place(app));
    let Some((dev, dir, agent)) = place else {
        return reply(api, id, &tr(en, "先开一个会话（直接发一句话），再发 /group，群就对应那个会话的项目。", "Start a session first (just send a message), then send /group; the group will use that session's project.")).await;
    };
    let folder = folder_name(&dir);
    let device = dev_name(app, &dev).await;
    let agent_name = if agent == "codex" { "Codex" } else { "Claude" };
    let title = if name.is_empty() { format!("{} · {folder}", tr(en, "看看", "LookLook")) } else { name.to_string() };
    let desc = tr(en, &format!("{folder} · {device} · {agent_name}"), &format!("{folder} · {device} · {agent_name}"));
    let group = match api.create_group(&title, &desc, operator).await {
        Ok(g) => g,
        Err(e) => {
            tracing::warn!(error = %e, "飞书：项目群没建成");
            if feishu::code_of(&e).is_some_and(feishu::is_scope_error) {
                grant(app, api, "group", &[]).await;
                return reply(api, id, &tr(en, "缺“创建群”权限：点上面卡片里的“去开通”，开通后再发一次 /group。", "Missing the create-group scope: tap “Grant” on the card above, then send /group again.")).await;
            }
            return reply(api, id, &failed(en, &notify::feishu_error(&e))).await;
        }
    };
    if let Err(e) = save_project(app, &group, Project { place: format!("{dev}|{dir}"), agent: agent.clone() }) {
        return reply(api, id, &failed(en, &e)).await;
    }
    tracing::info!(group = %group, "飞书：建了项目群");
    // 群里不 @ 也能收到消息要 im:message.group_msg：给一个开通的链接（在群里点开确认）
    let reg = app.notify.reg_start_with(app, true, &[feishu::register::GROUP_MSG]).await.ok();
    let hello = card::project_hello(en, &folder, &device, agent_name, reg.as_ref().map(|r| r.url.as_str()));
    match api.send_to_chat(&group, "interactive", &hello).await {
        Ok(m) => {
            if let Some(r) = reg {
                watch_grant(app, r.id, m, Some(card::project_hello(en, &folder, &device, agent_name, None)));
            }
        }
        Err(e) => tracing::warn!(error = %e, "飞书：项目群的第一条没发出去"),
    }
    reply(api, id, &tr(en, &format!("建好了：在飞书里找「{title}」群。"), &format!("Done: look for the “{title}” group in Feishu."))).await;
}

/// 话题对应的会话
struct Found {
    dev: String,
    inst: String,
    dir: String,
    agent: String,
}

fn found_from(dev: String, v: &Value) -> Option<Found> {
    Some(Found { dev, inst: v["inst"].as_str()?.into(), dir: v["dir"].as_str().unwrap_or_default().into(), agent: v["agent"].as_str().unwrap_or_default().into() })
}

/// 话题对应的会话在哪台电脑上：先看本机，再问在线的电脑
async fn find(app: &App, ids: &[String], operator: &str) -> Option<Found> {
    let own = own_dev(app).ok()?;
    if let Ok(v) = handle_route(app, ids) {
        if let Some(x) = found_from(own.clone(), &v) {
            return Some(x);
        }
    }
    let asks = peers(app, &own).await.into_iter().map(|(dev, _)| async move {
        let v = tokio::time::timeout(PEER_WAIT, call(app, &dev, Rpc::Route { ids: ids.to_vec() }, operator)).await.ok()?.ok()?;
        found_from(dev, &v)
    });
    futures_util::future::join_all(asks).await.into_iter().flatten().next()
}

/// 同一账户在线的其它电脑：(id, 名字)
async fn peers(app: &App, own: &str) -> Vec<(String, String)> {
    match app.outbound.devices(&app.account, false).await {
        Ok(d) => d.items.into_iter().filter(|x| x.online && x.id != own).map(|x| (x.id, x.name)).collect(),
        Err(e) => {
            tracing::debug!(error = ?e, "飞书：取不到其它电脑");
            vec![]
        }
    }
}

/// 在每台在线的电脑上都问一遍（本机直接做），返回 (电脑 id, 电脑名字, 回答)
async fn ask_all(app: &App, rpc: impl Fn() -> Rpc, operator: &str) -> Vec<(String, String, Value)> {
    let Ok(own) = own_dev(app) else { return vec![] };
    let mut out = Vec::new();
    if let Ok(v) = handle(app, rpc()).await {
        out.push((own.clone(), crate::settings::Settings::load(&app.store).device_name(), v));
    }
    let asks = peers(app, &own).await.into_iter().map(|(dev, name)| {
        let r = rpc();
        async move {
            let v = tokio::time::timeout(PEER_WAIT, call(app, &dev, r, operator)).await.ok()?.ok()?;
            Some((dev, name, v))
        }
    });
    out.extend(futures_util::future::join_all(asks).await.into_iter().flatten());
    out
}

fn own_dev(app: &App) -> Result<String, LocalError> {
    Ok(app.account.session().ok_or_else(|| LocalError::new("NOT_LOGGED_IN"))?.device_id)
}

/// 本机能开会话的文件夹：已有会话用过的（最近的在前）+ 默认文件夹。不接受消息里写的路径
fn places(app: &App) -> Vec<String> {
    let mut rows = app.store.instances().unwrap_or_default();
    rows.retain(|r| matches!(r.launch.as_str(), "claude" | "codex") && !r.workdir.is_empty());
    rows.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    let mut out: Vec<String> = Vec::new();
    for w in rows.into_iter().map(|r| r.workdir).chain([crate::settings::Settings::load(&app.store).default_workdir]) {
        if !w.is_empty() && !out.contains(&w) {
            out.push(w);
        }
        if out.len() >= PLACES_PER_DEVICE {
            break;
        }
    }
    let inbox = inbox(app);
    if !out.contains(&inbox) {
        out.push(inbox);
    }
    out
}

/// 杂事（跟项目无关的：起草回复、翻译、总结）开在数据目录下的 `inbox/`
fn inbox(app: &App) -> String {
    app.paths.home.join("inbox").display().to_string()
}

fn folder_name(dir: &str) -> String {
    Path::new(dir).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| dir.to_string())
}

/// 上次开会话选的（同步项 `notify/agent_last`）
#[derive(Debug, Default, Serialize, Deserialize)]
struct Last {
    place: String,
    agent: String,
}

fn load_last(app: &App) -> Last {
    crate::settings_sync::values(&app.store, notify::COLLECTION).ok().and_then(|mut m| m.remove("agent_last")).and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default()
}

/// 同步项 `notify/<key>`
fn synced<T: serde::de::DeserializeOwned + Default>(app: &App, key: &str) -> T {
    crate::settings_sync::values(&app.store, notify::COLLECTION).ok().and_then(|mut m| m.remove(key)).and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default()
}

/// 私聊 / 群里的“当前会话”（同步项 `notify/agent_current`：chat_id → 会话）
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Current {
    dev: String,
    inst: String,
    at: i64,
}

fn current(app: &App, chat: &str) -> Option<Current> {
    synced::<HashMap<String, Current>>(app, "agent_current").remove(chat)
}

fn set_current(app: &App, chat: &str, to: Option<(&str, &str)>) {
    if chat.is_empty() {
        return;
    }
    let mut all: HashMap<String, Current> = synced(app, "agent_current");
    let now = crate::util::system_ms();
    match to {
        Some((dev, inst)) => {
            if all.get(chat).is_some_and(|c| c.dev == dev && c.inst == inst) {
                return;
            }
            all.insert(chat.into(), Current { dev: dev.into(), inst: inst.into(), at: now });
        }
        None if all.remove(chat).is_none() => return,
        None => {}
    }
    // 只留最近的 50 个
    if all.len() > 50 {
        let mut v: Vec<(String, Current)> = all.into_iter().collect();
        v.sort_by_key(|(_, c)| -c.at);
        v.truncate(50);
        all = v.into_iter().collect();
    }
    if let Err(e) = notify::save(app, "agent_current", Some(serde_json::to_value(&all).unwrap_or_default())) {
        tracing::warn!(error = ?e, "飞书：当前会话没存上");
    }
}

/// 项目群（同步项 `notify/agent_projects`：chat_id → 在哪开、用哪个助手）
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Project {
    /// `电脑|文件夹`
    place: String,
    agent: String,
}

fn project(app: &App, chat: &str) -> Option<Project> {
    synced::<HashMap<String, Project>>(app, "agent_projects").remove(chat)
}

fn save_project(app: &App, chat: &str, p: Project) -> Result<(), LocalError> {
    let mut all: HashMap<String, Project> = synced(app, "agent_projects");
    all.insert(chat.into(), p);
    notify::save(app, "agent_projects", Some(serde_json::to_value(&all).unwrap_or_default()))
}

/// 新会话开在哪：上次选的地方（没选过、选的不对时 None）
fn last_place(app: &App) -> Option<(String, String, String)> {
    let last = load_last(app);
    let (dev, dir) = last.place.split_once('|')?;
    matches!(last.agent.as_str(), "claude" | "codex").then(|| (dev.to_string(), dir.to_string(), last.agent.clone()))
}

fn save_last(app: &App, dev: &str, dir: &str, agent: &str) {
    let last = Last { place: format!("{dev}|{dir}"), agent: agent.into() };
    if let Err(e) = notify::save(app, "agent_last", Some(serde_json::to_value(&last).unwrap_or_default())) {
        tracing::debug!(error = ?e, "飞书：上次的选择没存上");
    }
}

/// 开会话卡片：列出每台在线电脑的项目文件夹。`text` 为 None 时卡片上写第一句话；
/// `msg`：原话那条消息（新会话就开在它的话题里），None 时开在一条新消息的话题里；`prefer`：默认选哪里；
/// `origin`：转过来的问题的来源（选了以后记下来，认得来源时可以选“以后都交给这个项目”）
async fn picker(app: &App, text: Option<&str>, files: &[Resource], msg: Option<&str>, prefer: Option<(String, String)>, origin: Option<&Origin>, en: bool) -> Value {
    let operator = load_feishu(&app.store).and_then(|f| f.open_id).unwrap_or_default();
    let list = all_places(app, &operator).await;
    let last = load_last(app);
    let (want_place, want_agent) = prefer.unwrap_or((last.place, last.agent));
    let place = if list.iter().any(|(_, v)| *v == want_place) { want_place } else { list.first().map(|(_, v)| v.clone()).unwrap_or_default() };
    let agent = if matches!(want_agent.as_str(), "claude" | "codex") { want_agent } else { "claude".into() };
    let act = Act { a: START.into(), n: notify::nonce(), t: text.map(String::from), m: msg.map(String::from), f: files.to_vec(), o: origin.cloned(), ..Default::default() };
    let who = origin.map(|o| if !o.chat.is_empty() { tr(en, "这个群", "this group") } else { tr(en, "这个人", "this person") }).filter(|_| origin.is_some_and(|o| !o.keys().is_empty()));
    card::start(en, text, &list, &place, &agent, who.as_deref(), &act)
}

/// 卡片回调（3 秒内应答）：开会话、新会话、在飞书里接着。真正的事交给会话所在的电脑在后台做
pub async fn on_card(app: &App, act: &Act, ev: &Value, operator: &str) -> Value {
    let rules = load_rules(&app.store);
    let en = rules.lang.starts_with("en");
    if !rules.agent {
        return notify::toast("info", &off_hint(en));
    }
    let form = &ev["action"]["form_value"];
    let chat = ev["context"]["open_chat_id"].as_str().unwrap_or_default().to_string();
    let typed = form["text"].as_str().map(str::trim).filter(|t| !t.is_empty()).map(String::from);
    if matches!(act.a.as_str(), MOVE | INTO | INTO_GO | REPLY | FRESH) {
        return on_forward_card(app, act, ev, operator, en);
    }
    // 选了项目以后记下来源的习惯：(来源, 地方, 助手, 以后都交给这里)
    let mut rec: Option<(Origin, String, String, bool)> = None;
    let (rpc, dev, done) = match act.a.as_str() {
        START => {
            let place = form["place"].as_str().unwrap_or_default().to_string();
            let agent = form["agent"].as_str().unwrap_or("claude").to_string();
            let text = act.t.clone().or(typed).unwrap_or_default();
            let Some((dev, dir)) = place.split_once('|') else {
                return notify::toast("error", &tr(en, "先选一个项目", "Pick a project first"));
            };
            if !matches!(agent.as_str(), "claude" | "codex") {
                return notify::toast("error", &tr(en, "先选一个助手", "Pick an assistant first"));
            }
            if text.is_empty() && act.f.is_empty() {
                return notify::toast("error", &tr(en, "先写第一句话", "Write the first message first"));
            }
            save_last(app, dev, dir, &agent);
            let anchor = match &act.m {
                Some(m) => Anchor::Reply { m: m.clone(), th: false },
                None => Anchor::Chat { c: chat.clone() },
            };
            if let Some(o) = &act.o {
                rec = Some((o.clone(), place.clone(), agent.clone(), form["remember"] == "1"));
            }
            let card = card::starting(en, &text, &folder_name(dir));
            (Rpc::Start { agent, dir: dir.into(), text, anchor, files: act.f.clone(), typing: vec![], origin: act.o.clone() }, dev.to_string(), Some(card))
        }
        // 以前的结果卡片底下带输入框：照旧在同一个地方开新会话
        NEW if typed.is_some() => {
            let text = typed.unwrap_or_default();
            (Rpc::New { inst: act.inst.clone(), text, chat: chat.clone(), files: vec![], src: None, typing: vec![], origin: None }, act.dev.clone(), None)
        }
        // 「新会话」：下一句话开新会话，还在这个会话的地方
        NEW => {
            let (app2, operator, dev, inst, chat) = (app.clone(), operator.to_string(), act.dev.clone(), act.inst.clone(), chat.clone());
            tokio::spawn(async move {
                if let Ok(v) = call(&app2, &dev, Rpc::Where { inst }, &operator).await {
                    if let (Some(dir), Some(agent)) = (v["dir"].as_str(), v["agent"].as_str()) {
                        save_last(&app2, &dev, dir, agent);
                    }
                }
                set_current(&app2, &chat, None);
            });
            return notify::toast("success", &tr(en, "好，下一句话开一个新会话", "OK, your next message starts a new session"));
        }
        _ => (Rpc::Pick { inst: act.inst.clone(), chat: chat.clone() }, act.dev.clone(), None),
    };
    let (app2, operator) = (app.clone(), operator.to_string());
    let pick = act.a == PICK;
    let picked = act.inst.clone();
    tokio::spawn(async move {
        match call(&app2, &dev, rpc, &operator).await {
            // 开的、接着的会话成了这个私聊（群）的当前会话
            Ok(v) => {
                let inst = if pick { Some(picked.as_str()) } else { v["inst"].as_str() };
                if let (Some(i), None) = (inst, project(&app2, &chat)) {
                    set_current(&app2, &chat, Some((&dev, i)));
                }
                if let (Some((o, place, agent, bind)), Some(i)) = (&rec, inst) {
                    remember(&app2, o, place, agent, Some((&dev, i)), *bind);
                }
            }
            Err(e) => {
                tracing::warn!(error = ?e, dev = %dev, "飞书：卡片上的操作没办成");
                if let Some(f) = load_feishu(&app2.store) {
                    if let Err(e) = app2.notify.api(&f).send_to_chat(&chat, "text", &json!({ "text": failed(en, &e) })).await {
                        tracing::debug!(error = %e, "飞书：失败的提示没发出去");
                    }
                }
            }
        }
    });
    let toast = match act.a.as_str() {
        START => tr(en, "正在开始…", "Starting…"),
        NEW => tr(en, "正在开新会话，看下面新的那条消息", "Starting a new session; see the new message below"),
        _ => tr(en, "好，之后在这里发的消息都跟这个会话说", "OK, messages you send here now go to this session"),
    };
    match done {
        Some(c) => notify::with_card(notify::toast("success", &toast), c),
        None => notify::toast("success", &toast),
    }
}

/// 转过来的问题的卡片按钮：换项目、接到别的会话、回复来源、当新问题。都在后台做，先回一句提示
fn on_forward_card(app: &App, act: &Act, ev: &Value, operator: &str, en: bool) -> Value {
    let chat = ev["context"]["open_chat_id"].as_str().unwrap_or_default().to_string();
    let card_msg = ev["context"]["open_message_id"].as_str().unwrap_or_default().to_string();
    let (app, act, operator) = (app.clone(), act.clone(), operator.to_string());
    let toast = match act.a.as_str() {
        MOVE => tr(en, "停下了，选一个新的项目", "Stopped; pick a new project"),
        INTO => tr(en, "选一个会话", "Pick a session"),
        INTO_GO => tr(en, "正在接过去…", "Moving…"),
        REPLY => tr(en, "正在写回复…", "Writing the reply…"),
        _ => tr(en, "正在开新会话…", "Starting a new session…"),
    };
    tokio::spawn(async move {
        let Some(f) = load_feishu(&app.store) else { return };
        let api = app.notify.api(&f);
        if let Err(e) = forward_card(&app, &api, &act, &chat, &card_msg, &operator, en).await {
            tracing::warn!(error = ?e, a = %act.a, "飞书：转过来的问题的按钮没办成");
            reply(&api, &card_msg, &failed(en, &e)).await;
        }
    });
    notify::toast("success", &toast)
}

async fn forward_card(app: &App, api: &Api, act: &Act, chat: &str, card_msg: &str, operator: &str, en: bool) -> Result<(), LocalError> {
    let seed_of = |dev: String, inst: String| async move {
        let v = call(app, &dev, Rpc::Seed { inst: inst.clone() }, operator).await?;
        let files: Vec<Resource> = serde_json::from_value(v["files"].clone()).unwrap_or_default();
        let origin: Origin = serde_json::from_value(v["origin"].clone()).unwrap_or_default();
        let place = format!("{dev}|{}", v["dir"].as_str().unwrap_or_default());
        Ok::<_, LocalError>((v["text"].as_str().unwrap_or_default().to_string(), files, v["m"].as_str().unwrap_or_default().to_string(), origin, place))
    };
    match act.a.as_str() {
        // 换项目：停下这个会话，原话再问一次交给哪里（来源的习惯去掉这一次）
        MOVE => {
            let (text, files, m, origin, place) = seed_of(act.dev.clone(), act.inst.clone()).await?;
            call(app, &act.dev, Rpc::Drop { inst: act.inst.clone() }, operator).await?;
            let mut habits: HashMap<String, fr::Habit> = synced(app, "agent_origins");
            fr::unrecord(&mut habits, &origin, &place);
            save_habits(app, &habits);
            if current(app, chat).is_some_and(|c| c.inst == act.inst) {
                set_current(app, chat, None);
            }
            let msg = Some(m).filter(|m| !m.is_empty());
            let c = picker(app, Some(&text), &files, msg.as_deref(), None, Some(&origin), en).await;
            api.reply(card_msg, "interactive", &c, false).await.map_err(|e| notify::feishu_error(&e))?;
        }
        // 列出别的会话
        INTO => {
            let mut rows: Vec<(i64, card::SessionRow)> = Vec::new();
            for (dev, name, v) in ask_all(app, || Rpc::List, operator).await {
                for it in v["items"].as_array().into_iter().flatten() {
                    let Some(inst) = it["inst"].as_str().filter(|i| !(dev == act.dev && *i == act.inst)) else { continue };
                    let go = Act { a: INTO_GO.into(), dev: dev.clone(), inst: inst.into(), n: notify::nonce(), s: Some(format!("{}|{}", act.dev, act.inst)), ..Default::default() };
                    let s = |k: &str| it[k].as_str().unwrap_or_default().to_string();
                    let agent = if it["agent"] == "codex" { "Codex" } else { "Claude" };
                    rows.push((it["active_at"].as_i64().unwrap_or(0), card::SessionRow { agent: agent.into(), title: s("title"), project: s("project"), device: name.clone(), board: s("board"), pick: go }));
                }
            }
            rows.sort_by_key(|(at, _)| -at);
            let rows: Vec<card::SessionRow> = rows.into_iter().map(|(_, r)| r).take(SESSIONS_MAX).collect();
            api.reply(card_msg, "interactive", &card::move_into(en, &rows), false).await.map_err(|e| notify::feishu_error(&e))?;
        }
        // 接到选的会话：原话发给它，原来那个会话停下
        INTO_GO => {
            let (sdev, sinst) = act.s.as_deref().and_then(|s| s.split_once('|')).ok_or_else(LocalError::not_found)?;
            let (text, files, m, origin, place) = seed_of(sdev.into(), sinst.into()).await?;
            call(app, sdev, Rpc::Drop { inst: sinst.into() }, operator).await?;
            let msg = if m.is_empty() { card_msg.to_string() } else { m };
            call(app, &act.dev, Rpc::Say { inst: act.inst.clone(), text, msg, files, typing: vec![] }, operator).await?;
            set_current(app, chat, Some((&act.dev, &act.inst)));
            let mut habits: HashMap<String, fr::Habit> = synced(app, "agent_origins");
            fr::unrecord(&mut habits, &origin, &place);
            if let Ok(v) = call(app, &act.dev, Rpc::Where { inst: act.inst.clone() }, operator).await {
                let to = format!("{}|{}", act.dev, v["dir"].as_str().unwrap_or_default());
                fr::record(&mut habits, &origin, &to, v["agent"].as_str().unwrap_or_default(), Some((&act.dev, &act.inst)), false, crate::util::system_ms());
            }
            save_habits(app, &habits);
        }
        // 回复来源：这件事办完了，同一个来源再转来的开新会话
        REPLY => {
            call(app, &act.dev, Rpc::Reply { inst: act.inst.clone(), msg: card_msg.into() }, operator).await?;
            let mut habits: HashMap<String, fr::Habit> = synced(app, "agent_origins");
            if fr::close(&mut habits, &act.inst) {
                save_habits(app, &habits);
            }
        }
        // 当新问题：在同一个地方开新会话，不接旧的
        _ => {
            let origin = act.o.clone().unwrap_or_default();
            let rpc = Rpc::New { inst: act.inst.clone(), text: act.t.clone().unwrap_or_default(), chat: chat.into(), files: act.f.clone(), src: act.m.clone(), typing: vec![], origin: Some(origin.clone()) };
            let v = call(app, &act.dev, rpc, operator).await?;
            if let Some(i) = v["inst"].as_str() {
                set_current(app, chat, Some((&act.dev, i)));
                if let Ok(w) = call(app, &act.dev, Rpc::Where { inst: i.into() }, operator).await {
                    let place = format!("{}|{}", act.dev, w["dir"].as_str().unwrap_or_default());
                    remember(app, &origin, &place, w["agent"].as_str().unwrap_or_default(), Some((&act.dev, i)), false);
                }
            }
        }
    }
    Ok(())
}

/// 机器人菜单（事件 `application.bot.menu_v6`）：`new` 新会话、`sessions` 正在跑的会话、`tasks` 任务页
pub async fn on_menu(app: &App, ev: &Value) {
    let Some(f) = load_feishu(&app.store) else { return };
    let operator = ev["operator"]["operator_id"]["open_id"].as_str().unwrap_or_default();
    if f.open_id.as_deref() != Some(operator) || operator.is_empty() {
        return;
    }
    let rules = load_rules(&app.store);
    let en = rules.lang.starts_with("en");
    let api = app.notify.api(&f);
    let key = ev["event_key"].as_str().unwrap_or_default();
    let c = match key {
        "tasks" => card::tasks_link(en, &notify::page_url(app, "tasks")),
        _ if !rules.agent || !rules.actions => {
            if let Err(e) = api.send_text(operator, &off_hint(en)).await {
                app.notify.record_err(&e);
            }
            return;
        }
        "new" => picker(app, None, &[], None, None, None, en).await,
        "sessions" => sessions_card(app, operator, en).await,
        _ => {
            tracing::debug!(key, "飞书：不认识的菜单");
            return;
        }
    };
    if let Err(e) = api.send_card(operator, &c).await {
        app.notify.record_err(&e);
    }
}

/// 所有在线电脑上的会话
async fn sessions_card(app: &App, operator: &str, en: bool) -> Value {
    let mut rows: Vec<(i64, card::SessionRow)> = Vec::new();
    for (dev, name, v) in ask_all(app, || Rpc::List, operator).await {
        for it in v["items"].as_array().into_iter().flatten() {
            let Some(inst) = it["inst"].as_str() else { continue };
            let pick = Act { a: PICK.into(), dev: dev.clone(), inst: inst.into(), n: notify::nonce(), ..Default::default() };
            let s = |k: &str| it[k].as_str().unwrap_or_default().to_string();
            let agent = if it["agent"] == "codex" { "Codex" } else { "Claude" };
            rows.push((it["active_at"].as_i64().unwrap_or(0), card::SessionRow { agent: agent.into(), title: s("title"), project: s("project"), device: name.clone(), board: s("board"), pick }));
        }
    }
    // 等你处理的在前，其次按最近有动静
    rows.sort_by_key(|(at, r)| (r.board != "attn", r.board != "working", -at));
    let rows: Vec<card::SessionRow> = rows.into_iter().map(|(_, r)| r).take(SESSIONS_MAX).collect();
    card::sessions(en, &rows, &notify::page_url(app, "tasks"))
}

// ---------------- 电脑之间的请求 ----------------

/// 新会话的话题从哪条消息开始
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
enum Anchor {
    /// 原话那条消息。`th`：回复开成话题（项目群）
    Reply {
        m: String,
        #[serde(default)]
        th: bool,
    },
    /// 在这个会话（私聊或群）里发一条新消息当话题的开头
    Chat { c: String },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "k", rename_all = "snake_case")]
enum Rpc {
    /// 这几条消息是不是你的会话的
    Route { ids: Vec<String> },
    /// 你这台电脑能开会话的文件夹
    Places,
    /// 开一个会话
    Start {
        agent: String,
        dir: String,
        text: String,
        anchor: Anchor,
        #[serde(default)]
        files: Vec<Resource>,
        /// 收到消息时加的“正在输入”表情：(消息, 表情 id)，这一轮结束时去掉
        #[serde(default)]
        typing: Vec<(String, String)>,
        /// 转过来的问题的来源（记在会话的第一句话旁边，回复来源时用）
        #[serde(default)]
        origin: Option<Origin>,
    },
    /// 在这个会话的项目里开一个新会话（同一个助手）
    New {
        inst: String,
        text: String,
        chat: String,
        #[serde(default)]
        files: Vec<Resource>,
        /// 图片和文件在哪条消息里
        #[serde(default)]
        src: Option<String>,
        #[serde(default)]
        typing: Vec<(String, String)>,
        #[serde(default)]
        origin: Option<Origin>,
    },
    /// 接着说
    Say {
        inst: String,
        text: String,
        msg: String,
        #[serde(default)]
        files: Vec<Resource>,
        #[serde(default)]
        typing: Vec<(String, String)>,
    },
    /// 你这台电脑上的会话
    List,
    /// 这个会话在哪个文件夹、用的哪个助手
    Where { inst: String },
    /// 停下这个会话正在跑的这一轮
    Stop { inst: String },
    /// 在这个会话（私聊或群）里发一条对应这个会话的消息，之后在它的话题里接着说
    Pick { inst: String, chat: String },
    /// 这个会话最开始交给它的东西（换项目、接到别的会话）
    Seed { inst: String },
    /// 停下这个会话、不再把消息对应到它（换了项目、接到了别的会话）
    Drop { inst: String },
    /// 让这个会话写一段给来源的回复；装了飞书 CLI 就让 AI 以你的身份发（发之前 AI 助手会请你批准）。回答卡片回复 `msg`
    Reply { inst: String, msg: String },
}

#[derive(Serialize, Deserialize)]
struct Payload {
    rpc: Rpc,
    operator: String,
}

/// 在本机或另一台电脑上做一件事
async fn call(app: &App, dev: &str, rpc: Rpc, operator: &str) -> Result<Value, LocalError> {
    if dev == own_dev(app)? {
        return handle(app, rpc).await;
    }
    let payload = serde_json::to_string(&Payload { rpc, operator: operator.into() }).map_err(anyhow::Error::from)?;
    Ok(notify::signed_post(app, dev, RPC_PATH, RPC_KIND, payload).await?.unwrap_or(Value::Null))
}

/// 另一台电脑转来的请求（`POST /api/notify/agent`，只接受 `Access::Rtc`）：校验签名和操作人
pub async fn receive(app: &App, req: notify::ActionReq) -> Result<Value, LocalError> {
    notify::verify_signed(app, &req, RPC_KIND).await?;
    let p: Payload = serde_json::from_str(&req.payload).map_err(|_| LocalError::new("NOTIFY_PROTOCOL"))?;
    let f = load_feishu(&app.store).ok_or_else(|| LocalError::new("NOTIFY_NOT_CONFIGURED"))?;
    if f.open_id.as_deref() != Some(p.operator.as_str()) {
        return Err(LocalError::new("FORBIDDEN"));
    }
    handle(app, p.rpc).await
}

fn handle_route(app: &App, ids: &[String]) -> Result<Value, LocalError> {
    let inst = app.notify.agent.route_of(app, ids);
    let row = inst.as_deref().and_then(|i| app.store.instance(i).ok().flatten());
    Ok(json!({ "inst": inst, "dir": row.as_ref().map(|r| &r.workdir), "agent": row.as_ref().map(|r| &r.launch) }))
}

async fn handle(app: &App, rpc: Rpc) -> Result<Value, LocalError> {
    let rules = load_rules(&app.store);
    if !rules.agent || !rules.actions {
        return Err(LocalError::new("FORBIDDEN"));
    }
    match rpc {
        Rpc::Route { ids } => handle_route(app, &ids),
        Rpc::Places => Ok(json!({ "dirs": places(app), "inbox": inbox(app) })),
        Rpc::Start { agent, dir, text, anchor, files, typing, origin } => {
            if !matches!(agent.as_str(), "claude" | "codex") {
                return Err(LocalError::invalid("agent", "invalid"));
            }
            // 再核对一次：只能是这台电脑自己列出来的文件夹
            if !places(app).contains(&dir) {
                tracing::warn!(dir, "飞书：要在不认识的文件夹里开会话");
                return Err(LocalError::new("FORBIDDEN"));
            }
            if dir == inbox(app) {
                std::fs::create_dir_all(&dir).map_err(anyhow::Error::from)?;
            }
            let src = match &anchor {
                Anchor::Reply { m, .. } => Some(m.clone()),
                Anchor::Chat { .. } => None,
            };
            let inst = start_session(app, agent, dir, text, anchor, files, src, typing, origin).await?;
            Ok(json!({ "inst": inst }))
        }
        Rpc::New { inst, text, chat, files, src, typing, origin } => {
            let row = app.store.instance(&inst)?.ok_or_else(LocalError::not_found)?;
            if !matches!(row.launch.as_str(), "claude" | "codex") {
                return Err(LocalError::invalid("inst", "invalid"));
            }
            let inst = start_session(app, row.launch, row.workdir, text, Anchor::Chat { c: chat }, files, src, typing, origin).await?;
            Ok(json!({ "inst": inst }))
        }
        Rpc::Say { inst, text, msg, files, typing } => {
            app.store.instance(&inst)?.ok_or_else(LocalError::not_found)?;
            let me = &app.notify.agent;
            // 正在推的卡片接着推（Claude 在忙时这句进它自己的队列）；没有就从这句之后新开一张
            let base = if me.following(&inst) { None } else { app.chat.snapshot(&inst).await?["total"].as_u64() };
            let since = crate::util::system_ms();
            let (text, images) = with_files(app, &inst, &msg, &files, text).await?;
            app.chat.input(&inst, &text, &images).await?;
            me.add_route(app, &msg, &inst);
            me.hold_typing(&inst, typing);
            tracing::info!(inst = %inst, "飞书：接着说了一句");
            match base {
                Some(base) => drop(tokio::spawn(stream(app.clone(), inst, msg, base as usize, since, false))),
                // 正在推卡片的那一轮可能刚好结束：没接上就把表情去掉
                None if !me.following(&inst) => me.release_typing(app, &inst).await,
                None => {}
            }
            Ok(json!({}))
        }
        Rpc::Where { inst } => {
            let row = app.store.instance(&inst)?.ok_or_else(LocalError::not_found)?;
            Ok(json!({ "dir": row.workdir, "agent": row.launch }))
        }
        Rpc::Stop { inst } => {
            app.store.instance(&inst)?.ok_or_else(LocalError::not_found)?;
            let running = app.chat.brief(&inst).is_some_and(|b| matches!(b["board"].as_str(), Some("working" | "attn")));
            if running {
                app.chat.interrupt(&inst).await?;
                tracing::info!(inst = %inst, "飞书：发“停止”停了一轮");
            }
            Ok(json!({ "stopped": running }))
        }
        Rpc::List => {
            let mut items = Vec::new();
            for id in app.chat.ids() {
                let (Some(b), Ok(Some(row))) = (app.chat.brief(&id), app.store.instance(&id)) else { continue };
                if b["board"] == "stopped" {
                    continue;
                }
                let title = b["title"].as_str().filter(|t| !t.is_empty()).unwrap_or(&row.name).chars().take(60).collect::<String>();
                items.push(json!({ "inst": id, "agent": b["agent"], "title": title, "project": folder_name(&row.workdir), "board": b["board"], "active_at": b["active_at"] }));
            }
            items.sort_by_key(|v| -v["active_at"].as_i64().unwrap_or(0));
            items.truncate(SESSIONS_MAX);
            Ok(json!({ "items": items }))
        }
        Rpc::Pick { inst, chat } => {
            app.store.instance(&inst)?.ok_or_else(LocalError::not_found)?;
            let root = send_root(app, &inst, &chat, "").await?;
            app.notify.agent.add_route(app, &root, &inst);
            tracing::info!(inst = %inst, "飞书：在飞书里接着一个会话");
            Ok(json!({}))
        }
        Rpc::Seed { inst } => {
            let seed = app.notify.agent.seed(app, &inst).ok_or_else(LocalError::not_found)?;
            Ok(json!({ "text": seed.text, "files": seed.files, "m": seed.m, "origin": seed.origin, "dir": seed.dir }))
        }
        Rpc::Drop { inst } => {
            let me = &app.notify.agent;
            if app.chat.brief(&inst).is_some_and(|b| matches!(b["board"].as_str(), Some("working" | "attn"))) {
                app.chat.interrupt(&inst).await?;
            }
            me.forget(app, &inst);
            tracing::info!(inst = %inst, "飞书：转过来的问题从这个会话挪走了");
            Ok(json!({}))
        }
        Rpc::Reply { inst, msg } => {
            let seed = app.notify.agent.seed(app, &inst).ok_or_else(LocalError::not_found)?;
            let lark = crate::tools::detect(app.instances.shell()).await.lark.is_some();
            let en = rules.lang.starts_with("en");
            let text = reply_prompt(&seed.origin.unwrap_or_default(), lark, en);
            Box::pin(handle(app, Rpc::Say { inst, text, msg, files: vec![], typing: vec![] })).await
        }
    }
}

/// [回复] 交给会话的话：写一段给来源的回复；装了飞书 CLI 就以你的身份发过去（AI 助手跑命令前会请你批准）
fn reply_prompt(o: &Origin, lark: bool, en: bool) -> String {
    let mut s = tr(
        en,
        "把上面的结果写成一段回复，给反馈这个问题的人：口语、简短，说清楚现在怎么样了、对方要做什么，不要用 markdown。",
        "Write the result above as a reply to the person who reported this: plain, short, say where things stand and what they need to do, no markdown.",
    );
    if !lark {
        s.push_str(&tr(en, "\n只写草稿，我自己复制去发。", "\nJust write the draft; I'll copy and send it myself."));
        return s;
    }
    let to = if !o.chat.is_empty() {
        tr(en, &format!("发到原来的群（chat_id {}）", o.chat), &format!("send it to the original group (chat_id {})", o.chat))
    } else if !o.hint.is_empty() {
        tr(en, &format!("对方的原话开头是「{}」：先在我最近的私聊和群里找到这条消息和发它的人，回复给他", o.hint), &format!("their message starts with “{}”: first find that message and its sender in my recent chats, then reply to them", o.hint))
    } else {
        tr(en, "先问我发给谁", "ask me who to send it to first")
    };
    s.push_str(&tr(
        en,
        &format!("\n写好后用飞书 CLI（lark-cli，加 --as user）以我的身份发出去：{to}。发之前把草稿给我看，我同意了再发。"),
        &format!("\nThen send it as me with the Feishu CLI (lark-cli with --as user): {to}. Show me the draft first and only send after I agree."),
    ));
    s
}

/// 在一个会话（私聊或群）里发一条话题的开头，返回消息 id
async fn send_root(app: &App, inst: &str, chat: &str, text: &str) -> Result<String, LocalError> {
    let f = load_feishu(&app.store).ok_or_else(|| LocalError::new("NOTIFY_NOT_CONFIGURED"))?;
    let rules = load_rules(&app.store);
    let info = app.notify.info(app, inst, &app.chat.brief(inst).unwrap_or_default(), &rules).ok_or_else(LocalError::not_found)?;
    app.notify.api(&f).send_to_chat(chat, "interactive", &card::root(&info.info(), text)).await.map_err(|e| notify::feishu_error(&e))
}

/// 开一个会话，并开始推它的回复。带着图片和文件时先不给第一句话，等会话起来了连同附件一起送进去
#[allow(clippy::too_many_arguments)]
async fn start_session(app: &App, agent: String, dir: String, text: String, anchor: Anchor, files: Vec<Resource>, src: Option<String>, typing: Vec<(String, String)>, origin: Option<Origin>) -> Result<String, LocalError> {
    let me = &app.notify.agent;
    // 同一条消息只开一次
    let key = match &anchor {
        Anchor::Reply { m, .. } => {
            if let Some(inst) = me.route_of(app, std::slice::from_ref(m)) {
                return Ok(inst);
            }
            Some(m.clone())
        }
        Anchor::Chat { .. } => None,
    };
    if let Some(k) = &key {
        if !lock(&me.starting).insert(k.clone()) {
            return Err(LocalError::new("CONFLICT"));
        }
    }
    let since = crate::util::system_ms();
    let prompt = Some(text.clone()).filter(|t| files.is_empty() && !t.is_empty());
    let input = crate::instances::Input { launch: Some(agent), workdir: Some(dir), chat_ui: Some(true), full_access: Some(false), prompt: prompt.clone(), ..Default::default() };
    let r = crate::gateway::create_session(app, input, true).await;
    if let Some(k) = &key {
        lock(&me.starting).remove(k);
    }
    let inst = r?.row.id;
    tracing::info!(inst = %inst, "飞书：开了一个会话");
    let (root, thread) = match anchor {
        Anchor::Reply { m, th } => (m, th),
        Anchor::Chat { c } => (send_root(app, &inst, &c, &text).await?, false),
    };
    me.add_route(app, &root, &inst);
    me.add_seed(app, Seed { i: inst.clone(), text: text.clone(), files: files.clone(), m: src.clone().unwrap_or_else(|| root.clone()), origin, dir: app.store.instance(&inst).ok().flatten().map(|r| r.workdir).unwrap_or_default(), at: since });
    me.hold_typing(&inst, typing);
    tokio::spawn(stream(app.clone(), inst.clone(), root.clone(), 0, since, thread));
    if prompt.is_none() {
        let (app, inst) = (app.clone(), inst.clone());
        tokio::spawn(async move {
            let r = async {
                wait_ready(&app, &inst).await?;
                let (text, images) = with_files(&app, &inst, src.as_deref().unwrap_or_default(), &files, text).await?;
                app.chat.input(&inst, &text, &images).await
            }
            .await;
            if let (Err(e), Some(f)) = (r, load_feishu(&app.store)) {
                tracing::warn!(error = ?e, inst = %inst, "飞书：第一句话没送进去");
                let en = load_rules(&app.store).lang.starts_with("en");
                reply(&app.notify.api(&f), &root, &failed(en, &e)).await;
            }
        });
    }
    Ok(inst)
}

/// 等新开的会话能收字（空闲）
async fn wait_ready(app: &App, inst: &str) -> Result<(), LocalError> {
    let until = Instant::now() + Duration::from_millis(START_WAIT_MS as u64);
    while Instant::now() < until {
        // 取一次快照：没在跟读就开始跟读（Codex 也在这时接上 app-server）
        let _ = app.chat.snapshot(inst).await;
        if app.chat.brief(inst).is_some_and(|b| b["phase"] == "idle") {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(800)).await;
    }
    Err(LocalError::new("CHAT_NOT_RUNNING"))
}

/// 把消息里的图片和文件下载到会话的附件目录：图片作为图片交给 AI，其它文件把路径写在话后面
async fn with_files(app: &App, inst: &str, src: &str, files: &[Resource], text: String) -> Result<(String, Vec<String>), LocalError> {
    if files.is_empty() {
        return Ok((text, vec![]));
    }
    let f = load_feishu(&app.store).ok_or_else(|| LocalError::new("NOTIFY_NOT_CONFIGURED"))?;
    let api = app.notify.api(&f);
    let dir = app.paths.attach_dir(inst);
    tokio::fs::create_dir_all(&dir).await.map_err(anyhow::Error::from)?;
    let (mut images, mut others) = (Vec::new(), Vec::new());
    let stamp = crate::util::system_ms();
    for (n, r) in files.iter().take(FILES_MAX).enumerate() {
        let kind = if r.kind == "image" { "image" } else { "file" };
        let from = if r.msg.is_empty() { src } else { &r.msg };
        let bytes = api.download(from, &r.key, kind, FILE_BYTES).await.map_err(|e| notify::feishu_error(&e))?;
        let name = if kind == "image" { format!("image.{}", image_ext(&bytes)) } else { crate::transfer::sanitize_name(&r.name) };
        let name = if name.is_empty() { "file".to_string() } else { name };
        let path = dir.join(format!("feishu-{stamp}-{n}-{name}"));
        tokio::fs::write(&path, &bytes).await.map_err(anyhow::Error::from)?;
        let p = path.display().to_string();
        if kind == "image" { images.push(p) } else { others.push(p) }
    }
    let mut text = text;
    if !others.is_empty() {
        let en = load_rules(&app.store).lang.starts_with("en");
        let list = others.join("\n");
        text = if text.is_empty() { list } else { format!("{text}\n\n{}\n{list}", tr(en, "附件：", "Attachments:")) };
    }
    Ok((text, images))
}

/// 按文件头认图片格式（飞书下载回来不带类型）
fn image_ext(b: &[u8]) -> &'static str {
    match b {
        [0x89, b'P', b'N', b'G', ..] => "png",
        [0xFF, 0xD8, ..] => "jpg",
        [b'G', b'I', b'F', ..] => "gif",
        [b'R', b'I', b'F', b'F', _, _, _, _, b'W', b'E', b'B', b'P', ..] => "webp",
        [b'B', b'M', ..] => "bmp",
        _ => "png",
    }
}

// ---------------- 流式卡片 ----------------

/// 一轮的回复推到一张流式卡片上，直到这一轮结束
async fn stream(app: App, inst: String, reply_to: String, base: usize, since: i64, thread: bool) {
    let me = &app.notify.agent;
    let nonce = notify::nonce();
    {
        let mut g = lock(&me.live);
        if g.contains_key(&inst) {
            return;
        }
        g.insert(inst.clone(), nonce.clone());
    }
    if let Err(e) = stream_turn(&app, &inst, &reply_to, base, since, &nonce, thread).await {
        tracing::warn!(error = %e, inst = %inst, "飞书：流式卡片没推完");
    }
    lock(&me.live).remove(&inst);
    me.release_typing(&app, &inst).await;
}

struct Streamer<'a> {
    api: &'a Api,
    card: String,
    seq: u32,
    body: String,
    status: String,
    tools: String,
}

impl Streamer<'_> {
    async fn text(&mut self, element: &str, content: &str) -> anyhow::Result<()> {
        self.seq += 1;
        match self.api.card_text(&self.card, element, content, self.seq).await {
            // 流式开了 10 分钟会自动关：重新打开再推
            Err(e) if feishu::code_of(&e).is_some_and(feishu::is_stream_closed) => {
                self.seq += 1;
                self.api.card_settings(&self.card, &json!({ "streaming_mode": true }), self.seq).await?;
                self.seq += 1;
                self.api.card_text(&self.card, element, content, self.seq).await
            }
            r => r,
        }
    }

    async fn push(&mut self, body: String, status: String, tools: String) {
        if !tools.is_empty() && tools != self.tools {
            if let Err(e) = self.text(card::STREAM_TOOLS, &tools).await {
                tracing::debug!(error = %e, "飞书：工具步骤没推上");
            }
            self.tools = tools;
        }
        if body != self.body {
            if let Err(e) = self.text(card::STREAM_BODY, &body).await {
                tracing::debug!(error = %e, "飞书：正文没推上");
            }
            self.body = body;
        }
        if status != self.status {
            if let Err(e) = self.text(card::STREAM_STATUS, &status).await {
                tracing::debug!(error = %e, "飞书：状态行没推上");
            }
            self.status = status;
        }
    }

    /// 关流式，整卡换成结果
    async fn finish(&mut self, c: &Value) -> anyhow::Result<()> {
        self.seq += 1;
        if let Err(e) = self.api.card_settings(&self.card, &json!({ "streaming_mode": false }), self.seq).await {
            tracing::debug!(error = %e, "飞书：流式没关上");
        }
        self.seq += 1;
        self.api.card_update(&self.card, c, self.seq).await
    }
}

async fn stream_turn(app: &App, inst: &str, reply_to: &str, base: usize, since: i64, nonce: &str, thread: bool) -> anyhow::Result<()> {
    let f = load_feishu(&app.store).ok_or_else(|| anyhow!("没有配置飞书"))?;
    let api = app.notify.api(&f);
    let rules = load_rules(&app.store);
    let en = rules.lang.starts_with("en");
    let (mut rx, _viewer) = app.chat.subscribe(inst).map_err(|e| anyhow!("订阅不了会话：{}", e.code))?;
    let info = app.notify.info(app, inst, &app.chat.brief(inst).unwrap_or_default(), &rules).ok_or_else(|| anyhow!("会话没了"))?;
    let own = own_dev(app).map_err(|e| anyhow!(e.code))?;
    let stop = Act { a: STOP.into(), dev: own.clone(), inst: inst.into(), n: nonce.into(), ..Default::default() };
    let card_id = match api.card_create(&card::stream(&info.info(), &stop)).await {
        Ok(c) => c,
        Err(e) => {
            app.notify.record_err(&e);
            scope_hint(app, &api, &e, "cardkit").await;
            return Err(e);
        }
    };
    let msg = api.reply_card_entity(reply_to, &card_id, thread).await?;
    app.notify.agent.add_route(app, &msg, inst);
    let mut s = Streamer { api: &api, card: card_id, seq: 0, body: "…".into(), status: String::new(), tools: String::new() };
    let mut items: BTreeMap<usize, Value> = BTreeMap::new();
    resnap(app, inst, base, &mut items).await;
    let mut last_push = Instant::now() - PUSH_GAP;
    let mut ran = false;
    let mut settling: Option<Instant> = None;
    let (outcome, took) = loop {
        tokio::select! {
            ev = rx.recv() => match ev {
                Ok(Event::Items { start, items: got }) => {
                    for (k, it) in got.into_iter().enumerate() {
                        if start + k >= base {
                            items.insert(start + k, serde_json::to_value(it).unwrap_or_default());
                        }
                    }
                }
                Ok(Event::Reset) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => resnap(app, inst, base, &mut items).await,
                Ok(Event::Status(_)) => {}
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break ("stopped", 0),
            },
            _ = tokio::time::sleep(PUSH_GAP) => {}
        }
        let Some(b) = app.chat.brief(inst) else { break ("stopped", 0) };
        ran |= matches!(b["board"].as_str(), Some("working" | "attn"));
        if last_push.elapsed() >= PUSH_GAP {
            let steps = steps_of(&items);
            let tools = if steps.is_empty() { String::new() } else { card::steps_markdown(&steps, en) };
            s.push(body_of(&items, en), status_line(&b, en), tools).await;
            last_push = Instant::now();
        }
        let turn = &b["turn"];
        let done = turn["ended_at"].as_i64().is_some_and(|e| e > since) && !matches!(b["phase"].as_str(), Some("running" | "approval"));
        if done {
            let at = *settling.get_or_insert_with(Instant::now);
            if at.elapsed() >= END_SETTLE {
                let outcome = match turn["outcome"].as_str() {
                    Some("done") => "done",
                    Some("failed") => "failed",
                    _ => "stopped",
                };
                let end = turn["ended_at"].as_i64().unwrap_or(since);
                app.notify.agent.streamed(inst, end);
                break (outcome, end - turn["started_at"].as_i64().unwrap_or(end));
            }
        } else {
            settling = None;
        }
        // 会话退出了，或者一直没跑起来
        if b["board"] == "stopped" && (ran || crate::util::system_ms() - since > START_WAIT_MS) {
            break ("stopped", 0);
        }
    };
    resnap(app, inst, base, &mut items).await;
    let body = body_of(&items, en);
    let new = Act { a: NEW.into(), dev: own.clone(), inst: inst.into(), n: notify::nonce(), ..Default::default() };
    let more = forward_buttons(app, inst, &own, base == 0, en);
    let done = card::stream_done(&info.info(), outcome, if body == "…" { "" } else { &body }, &steps_of(&items), &thinking_of(&items), took, &new, &more);
    let r = s.finish(&done).await;
    send_outputs(app, &api, &msg, inst, &items, since).await;
    r
}

/// 回答卡片上多的按钮：第一轮可以换项目、接到别的会话；转过来的问题可以回复来源
fn forward_buttons(app: &App, inst: &str, own: &str, first: bool, en: bool) -> Vec<(String, Act)> {
    let Some(seed) = app.notify.agent.seed(app, inst) else { return vec![] };
    let act = |a: &str| Act { a: a.into(), dev: own.into(), inst: inst.into(), n: notify::nonce(), ..Default::default() };
    let mut out = Vec::new();
    // 群里 @ 的（来源只有这个群）：回答本来就在群里，不用另外回复
    if seed.origin.as_ref().is_some_and(|o| !o.user.is_empty() || !o.hint.is_empty()) {
        out.push((tr(en, "回复对方", "Reply to them"), act(REPLY)));
    }
    if first {
        out.push((tr(en, "换项目", "Other project"), act(MOVE)));
        out.push((tr(en, "接到别的会话", "Move to a session"), act(INTO)));
    }
    out
}

/// 这一轮回复里提到的、这一轮新写出来的文件（图片、PDF、文档、压缩包等，不含代码和文本）发进话题
async fn send_outputs(app: &App, api: &Api, to: &str, inst: &str, items: &BTreeMap<usize, Value>, since: i64) {
    let Ok(Some(row)) = app.store.instance(inst) else { return };
    let texts: Vec<String> = assistant_texts(items).into_iter().map(String::from).collect();
    if texts.is_empty() {
        return;
    }
    let workdir = row.workdir.clone();
    let found = tokio::task::spawn_blocking(move || {
        use crate::chat::codex_artifacts as art;
        art::find(&texts, Path::new(&workdir), &art::system_dirs())
    })
    .await
    .unwrap_or_default();
    let mut sent = 0;
    for (p, _) in found {
        if sent >= OUTPUTS_MAX {
            break;
        }
        let kind = crate::chat::codex_artifacts::kind(&p);
        let Ok(m) = tokio::fs::metadata(&p).await else { continue };
        let fresh = m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).is_some_and(|d| d.as_millis() as i64 >= since - 5_000);
        let limit = if kind == "image" { IMAGE_BYTES } else { FILE_BYTES as u64 };
        if kind == "text" || !fresh || m.len() == 0 || m.len() > limit {
            continue;
        }
        let Ok(bytes) = tokio::fs::read(&p).await else { continue };
        let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "file".into());
        let r = if kind == "image" {
            match api.upload_image(&name, bytes).await {
                Ok(k) => api.reply(to, "image", &json!({ "image_key": k }), true).await,
                Err(e) => Err(e),
            }
        } else {
            match api.upload_file(&name, bytes).await {
                Ok(k) => api.reply(to, "file", &json!({ "file_key": k }), true).await,
                Err(e) => Err(e),
            }
        };
        match r {
            Ok(_) => sent += 1,
            Err(e) => {
                // 多半是没开 im:resource：记下，设置页会显示
                app.notify.record_err(&e);
                break;
            }
        }
    }
}

/// 从快照重新拼（换了会话、落下了事件）
async fn resnap(app: &App, inst: &str, base: usize, items: &mut BTreeMap<usize, Value>) {
    let Ok(snap) = app.chat.snapshot(inst).await else { return };
    let start = snap["start"].as_u64().unwrap_or(0) as usize;
    for (k, it) in snap["items"].as_array().into_iter().flatten().enumerate() {
        if start + k >= base {
            items.insert(start + k, it.clone());
        }
    }
}

/// 这一轮 AI 说的每一段话
fn assistant_texts(items: &BTreeMap<usize, Value>) -> Vec<&str> {
    items
        .values()
        .filter(|it| it["k"] == "assistant")
        .flat_map(|it| it["blocks"].as_array().into_iter().flatten())
        .filter(|b| b["t"] == "text")
        .filter_map(|b| b["text"].as_str().map(str::trim))
        .filter(|t| !t.is_empty())
        .collect()
}

/// 这一轮 AI 说的话（按顺序拼起来）；太长只留最后的
fn body_of(items: &BTreeMap<usize, Value>, en: bool) -> String {
    let text = assistant_texts(items).join("\n\n");
    if text.is_empty() {
        return "…".into();
    }
    let text = if text.len() > BODY_BYTES {
        let mut cut = text.len() - BODY_BYTES;
        while !text.is_char_boundary(cut) {
            cut += 1;
        }
        format!("{}\n\n{}", tr(en, "…（前面省略，打开看全文）", "… (earlier part omitted; open to read it all)"), &text[cut..])
    } else {
        text
    };
    card::body_markdown(&text)
}

/// 这一轮的思考（Claude 的 thinking 块）：只留开头
fn thinking_of(items: &BTreeMap<usize, Value>) -> String {
    let all: Vec<&str> = items
        .values()
        .filter(|it| it["k"] == "assistant")
        .flat_map(|it| it["blocks"].as_array().into_iter().flatten())
        .filter(|b| b["t"] == "thinking")
        .filter_map(|b| b["text"].as_str().map(str::trim))
        .filter(|t| !t.is_empty())
        .collect();
    let text = all.join("\n\n");
    if text.len() <= THINK_BYTES {
        return text;
    }
    let mut cut = THINK_BYTES;
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}…", &text[..cut])
}

/// 工具调用一行里的关键信息：命令的第一行、文件名、搜的词、网址…（一行，最多 60 字）
fn tool_detail(input: &Value) -> String {
    let s = |k: &str| input[k].as_str().map(str::trim).filter(|v| !v.is_empty());
    let base = |p: &str| Path::new(p).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.to_string());
    let raw = s("command")
        .map(|c| c.lines().next().unwrap_or_default().to_string())
        .or_else(|| s("file_path").or(s("notebook_path")).or(s("path")).map(base))
        .or_else(|| s("pattern").or(s("query")).or(s("url")).or(s("description")).or(s("skill")).map(String::from))
        .unwrap_or_default();
    let mut t: String = raw.chars().take(60).collect();
    if raw.chars().count() > 60 {
        t.push('…');
    }
    t
}

/// 这一轮用的工具（按顺序），带着做完没有、出错没有
fn steps_of(items: &BTreeMap<usize, Value>) -> Vec<card::Step> {
    let mut steps = Vec::new();
    let mut ids: HashMap<String, usize> = HashMap::new();
    for it in items.values() {
        if it["k"] == "assistant" {
            for b in it["blocks"].as_array().into_iter().flatten().filter(|b| b["t"] == "tool_use") {
                let name = b["name"].as_str().unwrap_or_default();
                // mcp__server__tool → tool
                let name = name.rsplit("__").next().unwrap_or(name);
                if let Some(id) = b["id"].as_str() {
                    ids.insert(id.into(), steps.len());
                }
                steps.push(card::Step { name: name.into(), detail: tool_detail(&b["input"]), done: false, failed: false });
            }
        } else if it["k"] == "tool_result" {
            if let Some(&n) = it["tool_use_id"].as_str().and_then(|id| ids.get(id)) {
                steps[n].done = true;
                steps[n].failed = it["is_error"] == true;
            }
        }
    }
    steps
}

/// 状态行：正在做什么
fn status_line(b: &Value, en: bool) -> String {
    let short = |s: &str| -> String {
        let one = s.lines().next().unwrap_or_default();
        let mut t: String = one.chars().take(80).collect();
        if one.chars().count() > 80 || s.lines().nth(1).is_some() {
            t.push('…');
        }
        card::body_markdown(&t.replace('`', "'"))
    };
    match b["board"].as_str() {
        Some("attn") => tr(en, "在等你处理（看下面的卡片）", "Waiting for you (see the card below)"),
        Some("stopped") => tr(en, "正在启动…", "Starting…"),
        _ => match (b["doing"]["tool"].as_str(), b["doing"]["detail"].as_str()) {
            (Some(t), Some(d)) if !d.is_empty() => tr(en, &format!("正在用 {t}：`{}`", short(d)), &format!("Using {t}: `{}`", short(d))),
            (Some(t), _) => tr(en, &format!("正在用 {t}"), &format!("Using {t}")),
            _ => tr(en, "正在思考…", "Thinking…"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(kind: &str, content: Value) -> Value {
        json!({ "message_id": "om_1", "message_type": kind, "content": content.to_string() })
    }

    #[test]
    fn text_from_messages() {
        assert_eq!(message_text(&msg("text", json!({ "text": "  修一下登录页 " }))), "修一下登录页");
        let post = json!({ "title": "需求", "content": [[{ "tag": "text", "text": "看看 " }, { "tag": "a", "text": "这个", "href": "https://x" }], [{ "tag": "code_block", "language": "RUST", "text": "fn main() {}\n" }]] });
        assert_eq!(message_text(&msg("post", post.clone())), "需求\n看看 这个\n```rust\nfn main() {}\n```");
        // 外面包了一层语言
        assert_eq!(message_text(&msg("post", json!({ "zh_cn": post }))), "需求\n看看 这个\n```rust\nfn main() {}\n```");
        assert_eq!(message_text(&msg("image", json!({ "image_key": "k" }))), "");
    }

    #[test]
    fn files_in_messages() {
        let img = |k: &str| Resource { kind: "image".into(), key: k.into(), name: String::new(), msg: "om_1".into() };
        assert_eq!(attachments(&msg("image", json!({ "image_key": "img_1" }))), vec![img("img_1")]);
        assert_eq!(attachments(&msg("file", json!({ "file_key": "f_1", "file_name": "报告.pdf" }))), vec![Resource { kind: "file".into(), key: "f_1".into(), name: "报告.pdf".into(), msg: "om_1".into() }]);
        let post = json!({ "content": [[{ "tag": "text", "text": "看图" }, { "tag": "img", "image_key": "img_2" }], [{ "tag": "img", "image_key": "img_3" }]] });
        assert_eq!(attachments(&msg("post", post)), vec![img("img_2"), img("img_3")]);
        assert!(attachments(&msg("text", json!({ "text": "hi" }))).is_empty());
        assert_eq!(image_ext(b"\x89PNG\r\n"), "png");
        assert_eq!(image_ext(b"\xFF\xD8\xFF"), "jpg");
        assert_eq!(image_ext(b"RIFF\0\0\0\0WEBPVP8"), "webp");
    }

    #[test]
    fn group_mentions() {
        let m = json!({
            "message_id": "om_1",
            "mentions": [{ "key": "@_user_1", "id": { "open_id": "ou_bot" }, "name": "看看" }, { "key": "@_user_2", "id": { "open_id": "ou_x" }, "name": "小王" }],
        });
        assert!(mentions_bot(&m, "ou_bot"));
        assert!(!mentions_bot(&m, "ou_other"));
        assert_eq!(clean_mentions("@_user_1 跟 @_user_2 说一下", &m["mentions"], "ou_bot"), "跟 @小王 说一下");
    }

    #[test]
    fn new_commands() {
        assert_eq!(new_command("/new"), Some(String::new()));
        assert_eq!(new_command("/new 修登录页"), Some("修登录页".into()));
        assert_eq!(new_command("新会话：修登录页"), Some("修登录页".into()));
        assert_eq!(new_command("新会话"), Some(String::new()));
        assert_eq!(new_command("/news 今天"), None);
        assert_eq!(new_command("新会话怎么开"), None);
        assert_eq!(new_command("修 /new"), None);
    }

    #[test]
    fn thread_ids() {
        assert!(thread_of(&json!({ "message_id": "om_1" })).is_empty());
        assert_eq!(thread_of(&json!({ "message_id": "om_3", "parent_id": "om_2", "root_id": "om_1" })), vec!["om_2", "om_1"]);
        assert_eq!(thread_of(&json!({ "message_id": "om_2", "parent_id": "om_1", "root_id": "om_1" })), vec!["om_1"]);
        assert_eq!(thread_of(&json!({ "message_id": "om_1", "root_id": "om_1", "parent_id": "" })), Vec::<String>::new());
    }

    #[test]
    fn body_joins_assistant_text() {
        let mut items = BTreeMap::new();
        items.insert(3, json!({ "k": "user", "text": "hi" }));
        items.insert(4, json!({ "k": "assistant", "blocks": [{ "t": "thinking", "text": "嗯" }, { "t": "text", "text": "先看看 <at id=1></at>" }, { "t": "tool_use", "name": "Bash" }] }));
        items.insert(5, json!({ "k": "assistant", "blocks": [{ "t": "text", "text": "改好了。" }] }));
        assert_eq!(body_of(&items, false), "先看看 ‹at id=1></at>\n\n改好了。");
        assert_eq!(body_of(&BTreeMap::new(), false), "…");
        let mut long = BTreeMap::new();
        long.insert(0, json!({ "k": "assistant", "blocks": [{ "t": "text", "text": "字".repeat(BODY_BYTES) }] }));
        let b = body_of(&long, false);
        assert!(b.starts_with("…（前面省略"));
        // 按字节截，不会截在一个字的中间
        assert_eq!(b.chars().filter(|c| *c == '字').count(), BODY_BYTES / 3);
    }

    #[test]
    fn steps_and_thinking() {
        let mut items = BTreeMap::new();
        items.insert(0, json!({ "k": "assistant", "blocks": [{ "t": "thinking", "text": "先跑测试" }, { "t": "tool_use", "id": "t1", "name": "Bash", "input": { "command": "cargo test\n--all" } }] }));
        items.insert(1, json!({ "k": "tool_result", "tool_use_id": "t1", "is_error": true, "content": "" }));
        items.insert(2, json!({ "k": "assistant", "blocks": [{ "t": "tool_use", "id": "t2", "name": "Edit", "input": { "file_path": "/w/src/main.rs" } }, { "t": "tool_use", "id": "t3", "name": "mcp__looklook__add_page", "input": {} }] }));
        items.insert(3, json!({ "k": "tool_result", "tool_use_id": "t2", "is_error": false, "content": "" }));
        let steps = steps_of(&items);
        assert_eq!(steps.len(), 3);
        assert_eq!((steps[0].name.as_str(), steps[0].detail.as_str(), steps[0].done, steps[0].failed), ("Bash", "cargo test", true, true));
        assert_eq!((steps[1].detail.as_str(), steps[1].done, steps[1].failed), ("main.rs", true, false));
        assert_eq!((steps[2].name.as_str(), steps[2].done), ("add_page", false));
        assert_eq!(thinking_of(&items), "先跑测试");
        let mut long = BTreeMap::new();
        long.insert(0, json!({ "k": "assistant", "blocks": [{ "t": "thinking", "text": "想".repeat(THINK_BYTES) }] }));
        assert!(thinking_of(&long).ends_with('…') && thinking_of(&long).len() <= THINK_BYTES + 3);
        assert_eq!(tool_detail(&json!({ "url": "https://example.com/".to_string() + &"a".repeat(80) })).chars().count(), 61);
    }

    #[test]
    fn commands() {
        for t in ["停止", "停止。", " stop ", "STOP!", "/stop", "别做了"] {
            assert!(is_stop(t), "{t}");
        }
        for t in ["停止那个服务", "stop the server", "停止了吗？"] {
            assert!(!is_stop(t), "{t}");
        }
        assert!(is_help("/help") && is_help("帮助") && !is_help("帮助我改一下"));
        assert!(is_end("结束") && is_end("/done") && !is_end("结束了吗"));
        assert_eq!(group_command("/group"), Some(String::new()));
        assert_eq!(group_command("建群：支付后台"), Some("支付后台".into()));
        assert_eq!(group_command("/groups"), None);
    }

    #[test]
    fn forwarded_messages() {
        let items = vec![
            json!({ "message_id": "om_f", "msg_type": "merge_forward", "body": { "content": "Merged and Forwarded Message" } }),
            json!({ "message_id": "om_s1", "upper_message_id": "om_f", "msg_type": "text", "body": { "content": "{\"text\":\"登录页 500 了\\n看下日志\"}" } }),
            json!({ "message_id": "om_s2", "upper_message_id": "om_f", "msg_type": "image", "body": { "content": "{\"image_key\":\"img_9\"}" } }),
            json!({ "message_id": "om_s3", "upper_message_id": "om_other", "msg_type": "text", "body": { "content": "{\"text\":\"更深一层\"}" } }),
        ];
        let (text, files, origin) = forwarded_parts("om_f", &items, "ou_me", "oc_dm", false);
        assert_eq!(text, "转发的聊天记录：\n> 登录页 500 了\n> 看下日志");
        assert_eq!(files, vec![Resource { kind: "image".into(), key: "img_9".into(), name: String::new(), msg: "om_s2".into() }]);
        assert_eq!(origin, Origin { hint: "登录页 500 了\n看下日志".into(), ..Default::default() });
        assert_eq!(forwarded_parts("om_none", &items, "ou_me", "oc_dm", false), (String::new(), vec![], Origin::default()));
        // 带发送人、原来的群：自己写“我”，一个别人写“对方”，说得最多的人和群当来源
        let user = |id: &str, from: &str, text: &str| json!({ "message_id": id, "upper_message_id": "om_f", "chat_id": "oc_feedback", "sender": { "id": from, "id_type": "open_id", "sender_type": "user" }, "msg_type": "text", "body": { "content": json!({ "text": text }).to_string() } });
        let items = vec![user("om_1", "ou_zhang", "点了没反应"), user("om_2", "ou_me", "哪个页面？"), user("om_3", "ou_zhang", "登录页")];
        let (text, _, origin) = forwarded_parts("om_f", &items, "ou_me", "oc_dm", false);
        assert_eq!(text, "转发的聊天记录：\n> **对方**：\n> 点了没反应\n>\n> **我**：\n> 哪个页面？\n>\n> **对方**：\n> 登录页");
        assert_eq!(origin, Origin { user: "ou_zhang".into(), chat: "oc_feedback".into(), hint: "点了没反应".into() });
        // 两个别人：A、B；转到的私聊本身不当原来的群
        let items = vec![user("om_1", "ou_a", "1"), user("om_2", "ou_b", "2"), user("om_3", "ou_b", "3")];
        let (text, _, origin) = forwarded_parts("om_f", &items, "ou_me", "oc_feedback", false);
        assert!(text.contains("**A**") && text.contains("**B**"));
        assert_eq!((origin.user.as_str(), origin.chat.as_str()), ("ou_b", ""));
    }

    #[test]
    fn status_lines() {
        assert_eq!(status_line(&json!({ "board": "attn" }), false), "在等你处理（看下面的卡片）");
        assert_eq!(status_line(&json!({ "board": "working", "doing": { "tool": "Bash", "detail": "cargo test\nmore" } }), false), "正在用 Bash：`cargo test…`");
        assert_eq!(status_line(&json!({ "board": "working", "doing": null }), true), "Thinking…");
    }

    #[test]
    fn rpc_wire_format() {
        let p = Payload { rpc: Rpc::Say { inst: "abc".into(), text: "继续".into(), msg: "om_1".into(), files: vec![], typing: vec![("om_1".into(), "r1".into())] }, operator: "ou_1".into() };
        let v = serde_json::to_value(&p).unwrap();
        assert_eq!(v["rpc"]["k"], "say");
        let back: Payload = serde_json::from_value(v).unwrap();
        assert!(matches!(back.rpc, Rpc::Say { .. }));
        assert!(matches!(serde_json::from_value::<Rpc>(json!({ "k": "places" })).unwrap(), Rpc::Places));
        let start: Rpc = serde_json::from_value(json!({ "k": "start", "agent": "codex", "dir": "/w", "text": "x", "anchor": { "t": "chat", "c": "oc_1" } })).unwrap();
        assert!(matches!(start, Rpc::Start { anchor: Anchor::Chat { .. }, .. }));
        // 老版本发来的：没有 th、typing
        let old: Rpc = serde_json::from_value(json!({ "k": "start", "agent": "claude", "dir": "/w", "text": "x", "anchor": { "t": "reply", "m": "om_1" } })).unwrap();
        assert!(matches!(old, Rpc::Start { anchor: Anchor::Reply { th: false, .. }, ref typing, .. } if typing.is_empty()));
        assert!(matches!(serde_json::from_value::<Rpc>(json!({ "k": "stop", "inst": "a" })).unwrap(), Rpc::Stop { .. }));
    }

    /// 端到端（模拟飞书）：真的 App、真的 tmux + Claude Code（haiku），飞书是本机的假服务器（`feishu::fake`，
    /// 开放接口 + 长连接）。要登录好的 claude，会花一点 haiku 额度，默认不跑：
    /// `cargo build && TTYD_BIN=<ttyd> FEISHU_E2E_DIR=<空的项目目录> cargo test feishu_e2e -- --ignored --nocapture`
    #[tokio::test(flavor = "multi_thread")]
    #[ignore]
    async fn feishu_e2e() {
        use crate::feishu::fake::{Fake, Req};
        use std::sync::Arc;
        let dir = std::env::var("FEISHU_E2E_DIR").expect("FEISHU_E2E_DIR");
        std::fs::create_dir_all(&dir).unwrap();
        // 最便宜的模型；默认权限模式（要审批）
        std::env::set_var("LLV_CLAUDE_ARGS", "--permission-mode default");
        std::env::set_var("ANTHROPIC_MODEL", "haiku");
        let exe = std::env::current_exe().unwrap().parent().unwrap().parent().unwrap().join("looklook");
        assert!(exe.is_file(), "先 cargo build");
        std::env::set_var("LOOKLOOK_HOOK_EXE", exe);

        let fake = Fake::start().await;
        crate::feishu::TEST_BASE.set(Box::leak(fake.base.clone().into_boxed_str())).unwrap();

        // 一个已登录的看看（设备 dev1；平台地址连不上，所以没有别的电脑）
        let home = tempfile::tempdir().unwrap();
        let paths = crate::paths::Paths::discover(Some(home.path().to_path_buf())).unwrap();
        assert!(paths.ttyd.is_some(), "要 ttyd：设置 TTYD_BIN");
        let store = Arc::new(crate::store::Store::memory().unwrap());
        let shutdown = tokio_util::sync::CancellationToken::new();
        let account = crate::account::test_support::logged_in(store.clone(), paths.clone(), "http://127.0.0.1:9", "dev1", ed25519_dalek::SigningKey::from_bytes(&[7u8; 32]));
        let instances = crate::instances::Instances::new(store.clone(), paths.clone());
        let crate::instances::Backend::Tmux(tmux) = instances.backend.clone() else { panic!("需要 tmux") };
        let chat = crate::chat::Chat::new(store.clone(), paths.clone(), instances.clone());
        let ui = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let ui_addr = ui.local_addr().unwrap();
        let app: App = Arc::new(crate::gateway::Inner {
            account: account.clone(),
            instances: instances.clone(),
            updater: crate::updater::Updater::new(store.clone(), paths.clone(), account.clone(), shutdown.clone()),
            metrics: crate::metrics::Metrics::new(),
            relay_watch: crate::relay_watch::RelayWatch::new(account.clone()),
            transfers: crate::transfer::Transfers::new(paths.clone()),
            chat: chat.clone(),
            store: store.clone(),
            paths: paths.clone(),
            ui_addr,
            relay_addr: ui_addr,
            http: crate::gateway::proxy_client(),
            access: std::sync::RwLock::new(crate::gateway::access::AccessConfig::load(&store)),
            limiter: Default::default(),
            resolved: Default::default(),
            lan_ips: vec![],
            remote: Default::default(),
            rtc: Default::default(),
            outbound: Default::default(),
            ssh_transfer: Default::default(),
            sync: Default::default(),
            notify: Default::default(),
        });
        tokio::spawn(crate::gateway::serve(ui, crate::gateway::local_app(app.clone()), shutdown.clone()));
        chat.publish(ui_addr).unwrap();
        tokio::spawn(chat.clone().background());
        tokio::spawn(app.notify.clone().run(app.clone(), shutdown.clone()));
        let kill = || {
            let mut args = instances.tmux_socket_args();
            args.push("kill-server".into());
            let _ = std::process::Command::new(&tmux).args(&args).status();
        };

        let mut s = crate::settings::Settings::load(&store);
        s.default_workdir = dir.clone();
        s.save(&store, account.clock.now_ms()).unwrap();
        notify::save_feishu(&app, Some(&Feishu { app_id: "cli_test".into(), app_secret: "secret".into(), domain: "feishu".into(), open_id: Some("ou_me".into()), bound_at: Some(1), callback_ok: true })).unwrap();
        notify::save_rules(&app, &notify::Rules { agent: true, agent_groups: true, ..Default::default() }).unwrap();
        for _ in 0..150 {
            if fake.connected.load(std::sync::atomic::Ordering::SeqCst) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        assert!(fake.connected.load(std::sync::atomic::Ordering::SeqCst), "长连接没连上");
        println!("长连接已连上");

        let msg_ev = |id: &str, chat_type: &str, sender: &str, kind: &str, content: Value, root: Option<&str>, mentions: Value| {
            json!({
                "schema": "2.0",
                "header": { "event_id": format!("ev-{id}"), "event_type": "im.message.receive_v1" },
                "event": {
                    "sender": { "sender_id": { "open_id": sender } },
                    "message": { "message_id": id, "chat_id": if chat_type == "group" { "oc_group" } else { "oc_dm" }, "chat_type": chat_type, "message_type": kind, "content": content.to_string(), "root_id": root, "parent_id": root, "mentions": mentions },
                },
            })
        };
        let click = |n: &str, value: &Value, form: Value| {
            json!({
                "schema": "2.0",
                "header": { "event_id": format!("cb-{n}"), "event_type": "card.action.trigger" },
                "event": { "operator": { "open_id": "ou_me" }, "action": { "value": value, "form_value": form }, "context": { "open_message_id": format!("om_card_{n}"), "open_chat_id": "oc_dm" } },
            })
        };
        fn buttons(v: &Value, out: &mut Vec<Value>) {
            if v["tag"] == "button" {
                if let Some(b) = v["behaviors"][0]["value"].as_object() {
                    out.push(Value::Object(b.clone()));
                }
            }
            match v {
                Value::Array(a) => a.iter().for_each(|x| buttons(x, out)),
                Value::Object(m) => m.values().for_each(|x| buttons(x, out)),
                _ => {}
            }
        }
        let is_card_update = |r: &Req| r.method == "PUT" && r.path.starts_with("/open-apis/cardkit/v1/cards/") && !r.path.contains("/elements/");

        // 模拟用户：看到“在等你选择”（信任目录）点第一项，看到审批点“允许”
        let clicked = Arc::new(Mutex::new(Vec::<Value>::new()));
        {
            let (fake, clicked) = (fake.clone(), clicked.clone());
            tokio::spawn(async move {
                let mut seen = HashSet::new();
                loop {
                    for r in fake.reqs() {
                        if !r.path.starts_with("/open-apis/im/v1/messages") {
                            continue;
                        }
                        let mut bs = vec![];
                        buttons(&r.content(), &mut bs);
                        for b in bs {
                            let pick = (b["a"] == "dialog" && b["i"] == 0) || b["a"] == "allow";
                            let key = b.to_string();
                            if pick && seen.insert(key.clone()) {
                                let n = format!("auto{}", seen.len());
                                let resp = fake.push("card", json!({
                                    "schema": "2.0",
                                    "header": { "event_id": format!("cb-{n}"), "event_type": "card.action.trigger" },
                                    "event": { "operator": { "open_id": "ou_me" }, "action": { "value": b, "form_value": {} }, "context": { "open_message_id": "om_x", "open_chat_id": "oc_dm" } },
                                }))
                                .await;
                                println!("模拟用户点了 {key} → {resp:?}");
                                clicked.lock().unwrap().push(b);
                            }
                        }
                    }
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
            });
        }

        // 1. 私聊第一次发一句：先加“正在输入”表情；没选过地方，回一张开会话卡片（普通回复），列出这台电脑的默认文件夹
        fake.push("event", msg_ev("om_u1", "p2p", "ou_me", "text", json!({ "text": "只回复两个字：你好。不要用任何工具。" }), None, json!([]))).await;
        fake.wait("正在输入的表情", 10, |r| r.method == "POST" && r.path == "/open-apis/im/v1/messages/om_u1/reactions" && r.text().contains("Typing")).await;
        let picker = fake.wait("开会话卡片", 30, |r| r.path.contains("/om_u1/reply") && r.text().contains("开一个新会话")).await;
        assert_eq!(picker.json()["reply_in_thread"], false);
        fake.wait("只回了卡片，表情去掉", 10, |r| r.method == "DELETE" && r.path.starts_with("/open-apis/im/v1/messages/om_u1/reactions/")).await;
        let card = picker.content();
        let form = &card["body"]["elements"][1];
        let place = form["elements"][0]["options"][0]["value"].as_str().unwrap().to_string();
        assert_eq!(place, format!("dev1|{dir}"));
        let start = form["elements"][2]["behaviors"][0]["value"].clone();
        assert_eq!(start["a"], START);
        println!("1. 开会话卡片 ok：{place}");

        // 2. 点「开始」：3 秒内应答，卡片换成“正在开始”；会话建起来了，不用全部权限
        let resp = fake.push("card", click("start", &start, json!({ "place": place, "agent": "claude" }))).await.unwrap();
        assert_eq!(resp["toast"]["type"], "success", "{resp}");
        assert!(resp["card"]["data"].to_string().contains("正在"), "{resp}");
        let mut inst = None;
        for _ in 0..100 {
            inst = store.instances().unwrap().into_iter().find(|r| r.launch == "claude").map(|r| r.id);
            if inst.is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        let inst = inst.expect("会话没建起来");
        assert!(!instances.opts(&inst).full_access, "从飞书开的会话不能用全部权限");
        assert!(instances.opts(&inst).chat_ui);
        let created = fake.wait("建流式卡片", 30, |r| r.method == "POST" && r.path == "/open-apis/cardkit/v1/cards").await;
        assert_eq!(created.content()["config"]["streaming_mode"], true);
        let sent = fake.wait("流式卡片发在原话的话题里", 30, |r| r.path.contains("/om_u1/reply") && r.text().contains("card_id")).await;
        assert_eq!(sent.json()["msg_type"], "interactive");
        let done = fake.wait("第一轮做完", 240, |r| is_card_update(r) && r.text().contains("已完成")).await;
        let snap = chat.snapshot(&inst).await.unwrap();
        let model = snap["items"].as_array().unwrap().iter().find_map(|i| i["model"].as_str().map(String::from)).unwrap_or_default();
        if !model.contains("haiku") {
            kill();
            panic!("用的不是 haiku：{model}");
        }
        let pushed: Vec<String> = fake.reqs().iter().filter(|r| r.path.contains("/elements/body/content")).map(|r| r.json()["content"].as_str().unwrap_or_default().to_string()).collect();
        println!("推过的正文：{pushed:?}");
        assert!(done.content().to_string().contains("你好") || pushed.iter().any(|p| p.contains("你好")), "正文里有 AI 的回复");
        assert!(done.content().to_string().contains("新会话"), "结果卡片底下有「新会话」");
        let texts = fake.reqs().iter().filter(|r| r.path.contains("/elements/")).count();
        if texts >= 3 {
            // 假飞书第 3 次推文本时回 200850：要重新打开流式
            assert!(fake.reqs().iter().any(|r| r.method == "PATCH" && r.json()["settings"].as_str().is_some_and(|s| s.contains("\"streaming_mode\":true"))), "流式被关后没有重新打开");
        }
        assert!(!fake.reqs().iter().any(|r| r.path.contains("receive_id_type=open_id") && r.text().contains("做完了")), "流式推过的一轮不再另发“做完了”");
        assert!(done.content().get("header").is_none(), "回答卡片没有卡片头");
        assert!(done.text().contains("Claude · "), "页脚写着在跟哪个会话说");
        println!("2. 第一轮 ok（{model}），模拟用户点过：{:?}", clicked.lock().unwrap());

        // 3. 话题里回一条带图片的富文本：下载图片交给 AI，新开一张流式卡片
        let before = fake.reqs().len();
        let post = json!({ "content": [[{ "tag": "text", "text": "这张图是什么颜色？只回答一个词。" }, { "tag": "img", "image_key": "img_1" }]] });
        fake.push("event", msg_ev("om_u2", "p2p", "ou_me", "post", post, Some("om_u1"), json!([]))).await;
        fake.wait_after(before, "下载图片", 30, |r| r.path.contains("/om_u2/resources/img_1?type=image")).await;
        fake.wait_after(before, "第二张流式卡片回在这条消息下", 60, |r| r.path.contains("/om_u2/reply") && r.text().contains("card_id")).await;
        let done = fake.wait_after(before, "第二轮做完", 240, |r| is_card_update(r) && r.text().contains("已完成")).await;
        let snap = chat.snapshot(&inst).await.unwrap();
        assert!(snap["items"].as_array().unwrap().iter().any(|i| i["k"] == "user" && i["images"] == 1), "图片作为图片交给了 Claude");
        println!("3. 图片 ok：{}", done.content()["config"]["summary"]["content"]);

        // 3b. 私聊里直接发（不在话题里）：接着当前会话说，不开新会话；先发图、1 秒内再发字，合成一轮
        let before = fake.reqs().len();
        let sessions = store.instances().unwrap().len();
        fake.push("event", msg_ev("om_u5", "p2p", "ou_me", "image", json!({ "image_key": "img_5" }), None, json!([]))).await;
        fake.push("event", msg_ev("om_u6", "p2p", "ou_me", "text", json!({ "text": "上一张图和这张图颜色一样吗？只回答一样或不一样。" }), None, json!([]))).await;
        fake.wait_after(before, "合成一轮：下载前一条里的图", 30, |r| r.path.contains("/om_u5/resources/img_5?type=image")).await;
        let card = fake.wait_after(before, "当前会话的流式卡片回在最后一条下", 60, |r| r.path.contains("/om_u6/reply") && r.text().contains("card_id")).await;
        assert_eq!(card.json()["reply_in_thread"], false);
        fake.wait_after(before, "当前会话这一轮做完", 240, |r| is_card_update(r) && r.text().contains("已完成")).await;
        fake.wait_after(before, "这一轮结束去掉表情", 30, |r| r.method == "DELETE" && r.path.starts_with("/open-apis/im/v1/messages/om_u6/reactions/")).await;
        assert_eq!(store.instances().unwrap().len(), sessions, "接着当前会话，没开新会话");
        // 合并转发（张三说的）：不进当前会话；第一次认得这个人，问交给哪个项目，选“以后都交给这里”
        let before = fake.reqs().len();
        fake.push("event", msg_ev("om_fwd", "p2p", "ou_me", "merge_forward", json!("Merged and Forwarded Message"), None, json!([]))).await;
        fake.wait_after(before, "取转发的子消息", 10, |r| r.method == "GET" && r.path == "/open-apis/im/v1/messages/om_fwd").await;
        let ask = fake.wait_after(before, "问交给哪个项目", 30, |r| r.path.contains("/om_fwd/reply") && r.text().contains("交给哪个项目")).await;
        assert!(ask.text().contains("以后这个人的都交给这个项目"));
        let form = &ask.content()["body"]["elements"][1];
        let go = form["elements"][3]["behaviors"][0]["value"].clone();
        assert_eq!(go["o"]["user"], "ou_zhang");
        let resp = fake.push("card", click("fwd", &go, json!({ "place": place, "agent": "claude", "remember": "1" }))).await.unwrap();
        assert_eq!(resp["toast"]["type"], "success", "{resp}");
        let done = fake.wait_after(before, "转发那一轮做完", 240, |r| is_card_update(r) && r.text().contains("已完成")).await;
        assert!(done.text().contains("回复对方") && done.text().contains("换项目"), "转过来的问题的回答卡片有回复、换项目");
        let inst3 = store.instances().unwrap().into_iter().find(|r| r.launch == "claude" && r.id != inst).map(|r| r.id).expect("转发开了新会话");
        let snap = chat.snapshot(&inst3).await.unwrap();
        assert!(snap["items"].as_array().unwrap().iter().any(|i| i["k"] == "user" && i["text"].as_str().is_some_and(|t| t.contains("暗号是 ZEBRA") && t.contains("**对方**"))), "转发的内容交给了新会话");
        let habits: HashMap<String, fr::Habit> = synced(&app, "agent_origins");
        assert_eq!(habits["u:ou_zhang"].bound, place, "记住了张三的项目");
        // 同一个人再转来一段：接着刚才那个会话说，带「当新问题」
        let before = fake.reqs().len();
        let n = store.instances().unwrap().len();
        fake.push("event", msg_ev("om_fwd_b", "p2p", "ou_me", "merge_forward", json!("Merged and Forwarded Message"), None, json!([]))).await;
        fake.wait_after(before, "接着同一个来源的会话", 30, |r| r.path.contains("/om_fwd_b/reply") && r.text().contains("当新问题")).await;
        fake.wait_after(before, "第二段那一轮做完", 240, |r| is_card_update(r) && r.text().contains("已完成")).await;
        assert_eq!(store.instances().unwrap().len(), n, "没开新会话");
        let snap = chat.snapshot(&inst3).await.unwrap();
        assert!(snap["items"].as_array().unwrap().iter().any(|i| i["k"] == "user" && i["text"].as_str().is_some_and(|t| t.contains("OTTER"))), "第二段交给了同一个会话");
        // 回到第一个会话，后面的步骤还用它
        set_current(&app, "oc_dm", Some(("dev1", &inst)));
        // 没在跑时发“停止”
        let before = fake.reqs().len();
        fake.push("event", msg_ev("om_u7", "p2p", "ou_me", "text", json!({ "text": "停止" }), None, json!([]))).await;
        fake.wait_after(before, "停止的回答", 10, |r| r.path.contains("/om_u7/reply") && r.text().contains("没有在跑")).await;
        println!("3b. 当前会话、合并、转发、停止 ok");

        // 4. 话题里 /new 带一句话：开一个新话题、新会话。这台电脑的 Claude 默认是 auto 模式（写文件不问），
        // 所以先切到普通模式，再在新话题里让它写文件 → 审批卡片发进话题（模拟用户允许）→ 文件发回话题
        let before = fake.reqs().len();
        fake.push("event", msg_ev("om_u3", "p2p", "ou_me", "text", json!({ "text": "/new 只回复一个字：好" }), Some("om_u1"), json!([]))).await;
        let root = fake.wait_after(before, "新话题的开头", 60, |r| r.path.contains("receive_id_type=chat_id") && r.text().contains("新会话 · Claude")).await;
        assert_eq!(root.json()["receive_id"], "oc_dm");
        // 假飞书回的 message_id 是 om_b{第几个请求}
        let root_id = format!("om_b{}", fake.reqs().iter().position(|r| r.path == root.path && r.body == root.body).unwrap());
        let inst2 = store.instances().unwrap().into_iter().find(|r| r.launch == "claude" && r.id != inst && r.id != inst3).map(|r| r.id).expect("第二个会话");
        fake.wait_after(before, "新会话第一轮做完", 240, |r| is_card_update(r) && r.text().contains("已完成")).await;
        chat.setting(&inst2, "mode", "default").await.unwrap();
        let before = fake.reqs().len();
        let ask = "用 Write 工具在当前目录创建文件 result.dat，内容是 42。写好后只回复这个文件的绝对路径。";
        fake.push("event", msg_ev("om_u4", "p2p", "ou_me", "text", json!({ "text": ask }), Some(&root_id), json!([]))).await;
        fake.wait_after(before, "审批卡片发在话题里", 180, |r| r.path.ends_with("/reply") && r.text().contains("在等你批准")).await;
        let up = fake.wait_after(before, "上传文件", 240, |r| r.path == "/open-apis/im/v1/files").await;
        assert!(up.text().contains("filename=\"result.dat\"") && up.text().contains("42"), "{}", up.text());
        fake.wait_after(before, "文件消息发在话题里", 30, |r| r.path.ends_with("/reply") && r.json()["msg_type"] == "file").await;
        println!("4. /new + 审批 + 文件发回 ok");

        // 5. 菜单「会话」：列出三个会话（含转发开的）；点「在飞书里接着」发一条新的话题开头
        let before = fake.reqs().len();
        fake.push("event", json!({ "schema": "2.0", "header": { "event_id": "menu-1", "event_type": "application.bot.menu_v6" }, "event": { "operator": { "operator_id": { "open_id": "ou_me" } }, "event_key": "sessions" } })).await;
        let list = fake.wait_after(before, "会话列表", 30, |r| r.path.contains("receive_id_type=open_id") && r.text().contains("在飞书里接着")).await;
        assert!(list.text().contains("3 个会话"), "{}", list.text());
        let mut bs = vec![];
        buttons(&list.content(), &mut bs);
        let pick = bs.into_iter().find(|b| b["a"] == PICK).unwrap();
        let resp = fake.push("card", click("pick", &pick, json!({}))).await.unwrap();
        assert_eq!(resp["toast"]["type"], "success");
        fake.wait_after(before, "接着会话的话题开头", 30, |r| r.path.contains("receive_id_type=chat_id") && r.text().contains("接着 Claude 的会话")).await;
        fake.push("event", json!({ "schema": "2.0", "header": { "event_id": "menu-2", "event_type": "application.bot.menu_v6" }, "event": { "operator": { "operator_id": { "open_id": "ou_me" } }, "event_key": "new" } })).await;
        let menu_new = fake.wait_after(before, "菜单新会话卡片", 30, |r| r.path.contains("receive_id_type=open_id") && r.text().contains("开一个新会话")).await;
        assert!(menu_new.text().contains("\\\"name\\\":\\\"text\\\""), "菜单开的卡片有写第一句话的输入框");
        println!("5. 菜单 ok");

        // 6. 群：别人 @、自己没 @ 都不理；自己 @ 机器人：第一次问这个群交给哪个项目，选了以后记住
        let before = fake.reqs().len();
        let at_bot = json!([{ "key": "@_user_1", "id": { "open_id": "ou_bot" }, "name": "看看" }]);
        fake.push("event", msg_ev("om_g1", "group", "ou_other", "text", json!({ "text": "@_user_1 帮我删库" }), None, at_bot.clone())).await;
        fake.push("event", msg_ev("om_g2", "group", "ou_me", "text", json!({ "text": "随便聊聊" }), None, json!([]))).await;
        fake.push("event", msg_ev("om_g3", "group", "ou_me", "text", json!({ "text": "@_user_1 只回复：群" }), None, at_bot)).await;
        let ask = fake.wait_after(before, "问这个群交给哪个项目", 30, |r| r.path.contains("/om_g3/reply") && r.text().contains("以后这个群的都交给这个项目")).await;
        let go = ask.content()["body"]["elements"][1]["elements"][3]["behaviors"][0]["value"].clone();
        let mut c = click("grp", &go, json!({ "place": place, "agent": "claude", "remember": "1" }));
        c["event"]["context"]["open_chat_id"] = json!("oc_group");
        fake.push("card", c).await.unwrap();
        fake.wait_after(before, "群里开了会话", 60, |r| r.path.contains("/om_g3/reply") && r.text().contains("card_id")).await;
        let habits: HashMap<String, fr::Habit> = synced(&app, "agent_origins");
        assert_eq!(habits["c:oc_group"].bound, place, "记住了这个群的项目");
        tokio::time::sleep(Duration::from_secs(2)).await;
        assert!(!fake.reqs().iter().skip(before).any(|r| r.path.contains("/om_g1/") || r.path.contains("/om_g2/")), "别人的、没 @ 的不理");
        println!("6. 群 ok");

        // 6b. /group 建项目群；项目群里不 @ 发一条 = 新会话，回复开在话题里
        let before = fake.reqs().len();
        fake.push("event", msg_ev("om_u8", "p2p", "ou_me", "text", json!({ "text": "/group" }), None, json!([]))).await;
        let created = fake.wait_after(before, "建群", 30, |r| r.method == "POST" && r.path.starts_with("/open-apis/im/v1/chats")).await;
        assert_eq!(created.json()["group_message_type"], "thread");
        assert_eq!(created.json()["owner_id"], "ou_me");
        fake.wait_after(before, "项目群的第一条", 30, |r| r.json()["receive_id"] == "oc_proj" && r.text().contains("项目群")).await;
        fake.wait_after(before, "告诉私聊建好了", 30, |r| r.path.contains("/om_u8/reply") && r.text().contains("建好了")).await;
        let n = store.instances().unwrap().len();
        let mut ev = msg_ev("om_p1", "group", "ou_me", "text", json!({ "text": "只回复一个字：项" }), None, json!([]));
        ev["event"]["message"]["chat_id"] = json!("oc_proj");
        fake.push("event", ev).await;
        let card = fake.wait_after(before, "项目群话题里的流式卡片", 60, |r| r.path.contains("/om_p1/reply") && r.text().contains("card_id")).await;
        assert_eq!(card.json()["reply_in_thread"], true);
        assert_eq!(store.instances().unwrap().len(), n + 1, "项目群里的新话题开了新会话");
        println!("6b. 项目群 ok");

        // 7. 过期的停止按钮；关掉开关后私聊只回一句怎么打开
        let resp = fake.push("card", click("stop", &json!({ "a": STOP, "dev": "dev1", "inst": inst, "n": "old" }), json!({}))).await.unwrap();
        assert!(resp.to_string().contains("已经结束"), "{resp}");
        notify::save_rules(&app, &notify::Rules { agent: false, ..Default::default() }).unwrap();
        let before = fake.reqs().len();
        fake.push("event", msg_ev("om_u9", "p2p", "ou_me", "text", json!({ "text": "在吗" }), None, json!([]))).await;
        fake.wait_after(before, "开关关着的提示", 30, |r| r.path.contains("/om_u9/reply") && r.text().contains("没有打开")).await;
        println!("7. 开关 ok");

        println!("全部请求：");
        for r in fake.reqs() {
            println!("  {} {}", r.method, r.path);
        }
        shutdown.cancel();
        kill();
    }
}
