//! 在飞书里把别人的问题交给 AI 时，交给哪个项目（docs/FEISHU_AGENT.md §11）：只有不联网的判断，收发在 feishu_agent.rs。
//!
//! - **来源**（[`Origin`]）：合并转发里说话的人、原来的群，或者机器人在的群；
//! - **你写明的**（[`marks`]）：`#项目`、`@codex`；
//! - **来源的习惯**（[`Habit`]，同步项 `notify/agent_origins`）：绑定的项目、最近几次进了哪个项目、还没结束的会话；
//! - **猜**（[`guess`]）：文字里提到了哪个项目文件夹的名字。

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// 同一个来源的会话多久内算“还没结束”（接着说，不开新的）
pub const OPEN_MS: i64 = 24 * 3600 * 1000;
/// 最近几次都进了同一个项目就直接用
pub const HABIT_RUN: usize = 3;
const HIST_MAX: usize = 5;
const HABITS_MAX: usize = 200;

/// 转过来的东西是谁、在哪说的。认不出来时三项都空（只知道是转发的）
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Origin {
    /// 说话最多的人（不算你自己）的 open_id
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub user: String,
    /// 原来的群：合并转发的子消息带着、并且不是这个私聊；或者机器人在的群
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub chat: String,
    /// 原话的开头（让 AI 回复时找人）
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub hint: String,
}

impl Origin {
    /// 记习惯用的键：群在前（一个反馈群一般就是一个项目），人在后
    pub fn keys(&self) -> Vec<String> {
        let mut k = Vec::new();
        if !self.chat.is_empty() {
            k.push(format!("c:{}", self.chat));
        }
        if !self.user.is_empty() {
            k.push(format!("u:{}", self.user));
        }
        k
    }
}

/// 一个来源的习惯
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Habit {
    /// 绑定的项目 `电脑|文件夹`（选的时候选了“以后都交给这个项目”）；空 = 没绑定
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub bound: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub agent: String,
    /// 最近几次进的项目（旧的在前）
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hist: Vec<String>,
    /// 最近的会话：(电脑, 会话)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open: Option<(String, String)>,
    /// 最近一次交给 AI 的时间
    #[serde(default)]
    pub at: i64,
}

impl Habit {
    /// 最近 [`HABIT_RUN`] 次都是同一个项目
    pub fn usual(&self) -> Option<&str> {
        let n = self.hist.len();
        if n < HABIT_RUN {
            return None;
        }
        let last = self.hist[n - 1].as_str();
        self.hist[n - HABIT_RUN..].iter().all(|p| p == last).then_some(last)
    }
}

/// 交给哪里
#[derive(Debug, Clone, PartialEq)]
pub enum Pick {
    /// 在这里开新会话（`电脑|文件夹`）
    Start { place: String, agent: String },
    /// 接着这个来源还没结束的会话
    Continue { dev: String, inst: String },
    /// 问（`prefer`：卡片上预先选好的）
    Ask { prefer: Option<String> },
}

/// 按顺序定：写明的项目 → 同一来源没结束的会话 → 绑定的项目 → 习惯 → 猜（只当建议，要你点）。
/// `explicit`：`#项目` 认出来的地方；`guessed`：猜的地方；`agent`：写明的助手
pub fn decide(origin: &Origin, habits: &HashMap<String, Habit>, explicit: Option<&str>, guessed: Option<&str>, agent: Option<&str>, now: i64) -> Pick {
    let agent_of = |h: Option<&Habit>| agent.map(String::from).or_else(|| h.map(|h| h.agent.clone()).filter(|a| !a.is_empty())).unwrap_or_default();
    let mine: Vec<&Habit> = origin.keys().iter().filter_map(|k| habits.get(k)).collect();
    if let Some(p) = explicit {
        return Pick::Start { place: p.into(), agent: agent_of(mine.first().copied()) };
    }
    // 写明了助手就不接旧会话（旧会话的助手可能不一样）
    if agent.is_none() {
        if let Some((dev, inst)) = mine.iter().filter(|h| now - h.at < OPEN_MS).find_map(|h| h.open.clone()) {
            return Pick::Continue { dev, inst };
        }
    }
    if let Some(h) = mine.iter().find(|h| !h.bound.is_empty()) {
        return Pick::Start { place: h.bound.clone(), agent: agent_of(Some(h)) };
    }
    if let Some((h, p)) = mine.iter().find_map(|h| h.usual().map(|p| (h, p))) {
        return Pick::Start { place: p.into(), agent: agent_of(Some(h)) };
    }
    Pick::Ask { prefer: guessed.map(String::from) }
}

/// 交给了 AI 以后记下来：进了哪个项目、哪个会话；`bind`：以后都交给这个项目
pub fn record(habits: &mut HashMap<String, Habit>, origin: &Origin, place: &str, agent: &str, open: Option<(&str, &str)>, bind: bool, now: i64) {
    for k in origin.keys() {
        let h = habits.entry(k).or_default();
        h.hist.push(place.into());
        if h.hist.len() > HIST_MAX {
            h.hist.remove(0);
        }
        if !agent.is_empty() {
            h.agent = agent.into();
        }
        if bind {
            h.bound = place.into();
        }
        if let Some((dev, inst)) = open {
            h.open = Some((dev.into(), inst.into()));
        }
        h.at = now;
    }
    if habits.len() > HABITS_MAX {
        let mut v: Vec<(String, Habit)> = habits.drain().collect();
        v.sort_by_key(|(_, h)| -h.at);
        v.truncate(HABITS_MAX);
        habits.extend(v);
    }
}

/// 接着同一个来源的会话说了：只更新会话和时间，不算进“最近几次”
pub fn touch(habits: &mut HashMap<String, Habit>, origin: &Origin, dev: &str, inst: &str, now: i64) {
    for k in origin.keys() {
        let h = habits.entry(k).or_default();
        h.open = Some((dev.into(), inst.into()));
        h.at = now;
    }
}

/// 选错了（换了项目）：去掉最近一次记的那个地方，绑定的也解开
pub fn unrecord(habits: &mut HashMap<String, Habit>, origin: &Origin, place: &str) {
    for k in origin.keys() {
        if let Some(h) = habits.get_mut(&k) {
            if h.hist.last().map(String::as_str) == Some(place) {
                h.hist.pop();
            }
            if h.bound == place {
                h.bound.clear();
            }
            h.open = None;
        }
    }
}

/// 这个会话结束了（回复了来源、发了“结束”）：之后同一个来源再转来的开新会话
pub fn close(habits: &mut HashMap<String, Habit>, inst: &str) -> bool {
    let mut changed = false;
    for h in habits.values_mut() {
        if h.open.as_ref().is_some_and(|(_, i)| i == inst) {
            h.open = None;
            changed = true;
        }
    }
    changed
}

/// 你写明的：`#项目`（不含纯数字，`#123` 多半是问题编号）、`@codex` / `用 codex` 这类
#[derive(Debug, Default, PartialEq)]
pub struct Marks {
    pub project: Option<String>,
    pub agent: Option<String>,
}

/// 只看你自己写的行（转发的聊天记录是 `> ` 开头的引用，里面的 `#` 不算）
pub fn marks(text: &str) -> Marks {
    let mut m = Marks::default();
    for line in text.lines().filter(|l| !l.trim_start().starts_with('>')) {
        for w in line.split(|c: char| c.is_whitespace() || "，。,;；".contains(c)) {
            if let Some(p) = w.strip_prefix('#').or_else(|| w.strip_prefix('＃')) {
                let p = p.trim_end_matches(|c: char| !c.is_alphanumeric() && !"-_.".contains(c));
                if m.project.is_none() && !p.is_empty() && !p.chars().all(|c| c.is_ascii_digit()) {
                    m.project = Some(p.to_string());
                }
            }
        }
        let l = line.to_lowercase();
        for (agent, words) in [("codex", ["@codex", "用codex", "用 codex"]), ("claude", ["@claude", "用claude", "用 claude"])] {
            if m.agent.is_none() && words.iter().any(|w| l.contains(w)) {
                m.agent = Some(agent.into());
            }
        }
    }
    m
}

/// 去掉写明的 `#项目`（认出来了才去掉，没认出来留着给 AI 看）
pub fn strip_project(text: &str, name: &str) -> String {
    let out = text.replace(&format!("#{name}"), "").replace(&format!("＃{name}"), "");
    out.trim().to_string()
}

/// 文件夹名：认 `#项目` 时不分大小写，完全一样的优先，其次是开头一样的
pub fn find_place<'a>(name: &str, places: &'a [(String, String)]) -> Option<&'a str> {
    let n = name.to_lowercase();
    let folder = |v: &str| v.rsplit(['/', '\\']).next().unwrap_or(v).to_lowercase();
    places.iter().find(|(_, v)| folder(v) == n).or_else(|| places.iter().find(|(_, v)| folder(v).starts_with(&n))).map(|(_, v)| v.as_str())
}

/// 猜：文字里提到了哪个项目文件夹的名字（整个名字算 2 分，名字里 4 个字以上的一段算 1 分）。只有一个最高分时才算
pub fn guess<'a>(text: &str, places: &'a [(String, String)]) -> Option<&'a str> {
    let t = text.to_lowercase();
    let score = |v: &str| {
        let folder = v.rsplit(['/', '\\']).next().unwrap_or(v).to_lowercase();
        if folder.chars().count() >= 3 && t.contains(&folder) {
            return 2;
        }
        let parts = folder.split(['-', '_', '.', ' ']).filter(|p| p.chars().count() >= 4).filter(|p| t.contains(p)).count();
        usize::from(parts > 0)
    };
    let scored: Vec<(usize, &str)> = places.iter().map(|(_, v)| (score(v), v.as_str())).filter(|(s, _)| *s > 0).collect();
    let best = scored.iter().map(|(s, _)| *s).max()?;
    let top: Vec<&str> = scored.iter().filter(|(s, _)| *s == best).map(|(_, v)| *v).collect();
    // 同名的文件夹在几台电脑上都有：算一个，取第一个
    let folder = |v: &str| v.rsplit(['/', '\\']).next().unwrap_or(v).to_lowercase();
    top.iter().all(|v| folder(v) == folder(top[0])).then_some(top[0])
}

/// 单条转发认不出来（飞书不带原作者），按样子猜：第一行像“张三：……”。
/// 带图片、文件的不算：自己发截图再接着问一句很常见，那是跟当前会话说
pub fn looks_forwarded(text: &str) -> bool {
    let first = text.lines().next().unwrap_or_default().trim();
    let Some((name, rest)) = first.split_once(['：', ':']) else { return false };
    let name = name.trim();
    if rest.trim().is_empty() {
        return false;
    }
    // 中文名：2 到 4 个汉字，不以常见的动词、代词开头（“帮我看下：”“只回复两个字：”不是名字）
    const STARTS: &str = "帮请看修改查做写把给只用说问能要我你他她它这那先再还就都也让找加删跑试";
    let cjk = |c: char| ('\u{4e00}'..='\u{9fff}').contains(&c);
    let n = name.chars().count();
    if name.chars().all(cjk) {
        const WORDS: &[&str] = &["注意", "备注", "问题", "需求", "背景", "原因", "结果", "目标", "要求", "补充", "例如", "比如", "现象", "报错", "错误", "日志", "步骤", "地址", "链接", "提示", "结论", "总结", "回复", "答案"];
        return (2..=4).contains(&n) && !name.starts_with(|c: char| STARTS.contains(c)) && !WORDS.contains(&name);
    }
    // 英文名：字母开头，字母、数字、空格、. _ -，不超过 20 个字
    const EN: &[&str] = &["note", "todo", "error", "bug", "issue", "http", "https", "warning", "info", "step", "result", "example", "question", "answer", "q", "a"];
    name.starts_with(|c: char| c.is_ascii_alphabetic())
        && n <= 20
        && name.chars().all(|c| c.is_ascii_alphanumeric() || " ._-".contains(c))
        && name.split_whitespace().count() <= 3
        && !EN.contains(&name.to_lowercase().as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn places() -> Vec<(String, String)> {
        vec![
            ("looklook · 办公室".into(), "d1|/w/looklook".into()),
            ("shop-admin · 办公室".into(), "d1|/w/shop-admin".into()),
            ("looklook · 家里".into(), "d2|/home/me/looklook".into()),
        ]
    }

    #[test]
    fn origin_keys() {
        assert!(Origin::default().keys().is_empty());
        assert_eq!(Origin { user: "ou_a".into(), chat: "oc_g".into(), hint: String::new() }.keys(), vec!["c:oc_g", "u:ou_a"]);
    }

    #[test]
    fn marks_only_from_own_lines() {
        assert_eq!(marks("修一下 #looklook").project.as_deref(), Some("looklook"));
        assert_eq!(marks("> 张三：看下 #shop\n修一下").project, None);
        assert_eq!(marks("修 #123 这个问题").project, None);
        assert_eq!(marks("#shop-admin，看下").project.as_deref(), Some("shop-admin"));
        assert_eq!(marks("用 Codex 看下").agent.as_deref(), Some("codex"));
        assert_eq!(marks("@claude 修一下").agent.as_deref(), Some("claude"));
        assert_eq!(marks("修一下"), Marks::default());
        assert_eq!(strip_project("#looklook 修一下", "looklook"), "修一下");
    }

    #[test]
    fn find_and_guess() {
        let p = places();
        assert_eq!(find_place("LookLook", &p), Some("d1|/w/looklook"));
        assert_eq!(find_place("shop", &p), Some("d1|/w/shop-admin"));
        assert_eq!(find_place("nope", &p), None);
        assert_eq!(guess("shop-admin 的登录页打不开", &p), Some("d1|/w/shop-admin"));
        assert_eq!(guess("admin 后台登录不了", &p), Some("d1|/w/shop-admin"));
        // 两台电脑上都有 looklook：算一个
        assert_eq!(guess("looklook 手机上看不了", &p), Some("d1|/w/looklook"));
        assert_eq!(guess("登录不了", &p), None);
    }

    #[test]
    fn forwarded_looks() {
        assert!(looks_forwarded("张三：登录页点了没反应"));
        assert!(looks_forwarded("Alice: the page is blank"));
        assert!(!looks_forwarded("注意：别改接口"));
        assert!(!looks_forwarded("帮我看下 https://x.com"));
        assert!(!looks_forwarded("修一下登录"));
        assert!(!looks_forwarded("这个页面打开以后很慢怎么回事："));
        assert!(!looks_forwarded("只回复两个字：你好。不要用任何工具。"));
        assert!(!looks_forwarded("帮我看下：登录页"));
        assert!(!looks_forwarded("Note: keep the API"));
        assert!(looks_forwarded("李四：好的"));
        assert!(looks_forwarded("Bob Smith: still broken"));
    }

    #[test]
    fn decide_order() {
        let o = Origin { user: "ou_a".into(), ..Default::default() };
        let mut h = HashMap::new();
        let now = 10 * OPEN_MS;
        assert_eq!(decide(&o, &h, None, Some("d1|/w/shop-admin"), None, now), Pick::Ask { prefer: Some("d1|/w/shop-admin".into()) });
        // 三次都进了 looklook：直接用
        for _ in 0..3 {
            record(&mut h, &o, "d1|/w/looklook", "claude", None, false, now - OPEN_MS * 2);
        }
        assert_eq!(decide(&o, &h, None, None, None, now), Pick::Start { place: "d1|/w/looklook".into(), agent: "claude".into() });
        // 混着项目的不算习惯
        record(&mut h, &o, "d1|/w/shop-admin", "codex", None, false, now - OPEN_MS * 2);
        assert_eq!(decide(&o, &h, None, None, None, now), Pick::Ask { prefer: None });
        // 绑定的
        record(&mut h, &o, "d1|/w/shop-admin", "codex", None, true, now - OPEN_MS * 2);
        assert_eq!(decide(&o, &h, None, None, None, now), Pick::Start { place: "d1|/w/shop-admin".into(), agent: "codex".into() });
        // 24 小时内的会话：接着说
        record(&mut h, &o, "d1|/w/shop-admin", "codex", Some(("d1", "i1")), false, now - 1000);
        assert_eq!(decide(&o, &h, None, None, None, now), Pick::Continue { dev: "d1".into(), inst: "i1".into() });
        // 写明的最优先；写明了助手不接旧会话
        assert_eq!(decide(&o, &h, Some("d2|/home/me/looklook"), None, None, now), Pick::Start { place: "d2|/home/me/looklook".into(), agent: "codex".into() });
        assert_eq!(decide(&o, &h, None, None, Some("claude"), now), Pick::Start { place: "d1|/w/shop-admin".into(), agent: "claude".into() });
        // 会话结束了：不再接
        assert!(close(&mut h, "i1"));
        assert_eq!(decide(&o, &h, None, None, None, now), Pick::Start { place: "d1|/w/shop-admin".into(), agent: "codex".into() });
        // 换了项目：绑定解开
        unrecord(&mut h, &o, "d1|/w/shop-admin");
        assert!(h["u:ou_a"].bound.is_empty());
        // 认不出来源：只能问
        assert_eq!(decide(&Origin::default(), &h, None, None, None, now), Pick::Ask { prefer: None });
    }
}
