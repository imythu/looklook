//! 本机设置（存本地 SQLite）。设备额度、到期时间、隧道上限等由服务端决定，不在这里。

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use serde::{Deserialize, Serialize};

use crate::settings_sync;
use crate::store::Store;

const KEY: &str = "settings";
const THEME_REV: u8 = 3;
/// 终端配色（和 instances.rs 的 xterm_theme、src-ui/shared/termTheme.js 保持一致）
pub const THEMES: &[&str] = &["tangerine", "lagoon", "sunset", "mint", "agent", "dark", "light"];
const DEFAULT_THEME: &str = "tangerine";
/// 在电脑之间同步的设置项（docs/SETTINGS_SYNC.md §4.1）：值只以同步记录为准（settings_sync.rs），
/// `kv.settings` 里也留一份，只是给降级到旧版本时读。其余字段和这台电脑的路径、环境有关，不同步。
pub const SYNCED: &[&str] = &["theme", "font", "font_size", "default_full_access", "attach_days"];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Settings {
    /// 新建终端的默认工作文件夹
    pub default_workdir: String,
    /// 终端中文字体：`wenkai`（内置霞鹜文楷等宽）或 `system`（系统黑体）
    pub font: String,
    pub font_size: u8,
    /// 终端配色：见 `THEMES`，默认 `tangerine`（橘子汽水）
    pub theme: String,
    /// 配色设置的版本：0 = 默认还是 `dark` 时存下的设置，1 = 默认是 `agent` 时存下的，2 = 默认是 `lagoon` 时存下的；读的时候把当时的默认
    /// 换成现在的默认一次，之后用户自己再选 `dark` / `agent` / `lagoon` 也会保留。字段自己的 serde 默认是 0（不是 Settings::default 里的值）。
    #[serde(default)]
    pub theme_rev: u8,
    /// 在看看服务端显示的电脑名称；为空时用主机名
    pub device_name: String,
    /// 新建终端默认使用的 shell（写法见 shells.rs）；为空时自动选择
    pub default_shell: String,
    /// 有新版本时在空闲时（没有打开的终端）自动下载安装并重启
    pub auto_update: bool,
    /// 粘贴的截图、“发给 AI”的文件（附件目录）保留几天，过期自动删除
    pub attach_days: u16,
    /// 新建 AI 会话（Claude Code / Codex / OpenCode）默认带全部权限。项目里用过的跟着那个项目上次的（界面决定）
    pub default_full_access: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            default_workdir: default_workdir(),
            font: "wenkai".into(),
            font_size: 15,
            theme: DEFAULT_THEME.into(),
            theme_rev: THEME_REV,
            device_name: String::new(),
            default_shell: String::new(),
            auto_update: true,
            attach_days: 7,
            default_full_access: false,
        }
    }
}

fn default_workdir() -> String {
    dirs::home_dir().map(|p| p.display().to_string()).unwrap_or_else(|| if cfg!(windows) { "C:\\".into() } else { "/".into() })
}

impl Settings {
    pub fn load(store: &Store) -> Self {
        Self::load_local(store).synced(store)
    }

    /// 只看 `kv.settings`，不看同步记录。
    fn load_local(store: &Store) -> Self {
        let s: Self = store.get(KEY).ok().flatten().unwrap_or_default();
        s.migrated()
    }

    /// 同步项换成同步记录里的值（有的话）；某一项的值不合法就保留原来的。
    fn synced(self, store: &Store) -> Self {
        let Ok(vals) = settings_sync::values(store, settings_sync::SETTINGS) else { return self };
        let mut out = self;
        for k in SYNCED {
            let Some(v) = vals.get(*k) else { continue };
            let mut j = serde_json::to_value(&out).unwrap_or_default();
            j[*k] = v.clone();
            if let Ok(s) = serde_json::from_value(j) {
                out = s;
            }
        }
        out
    }

    /// 升级到有同步的版本时（每次启动都调，已有记录的项跳过）：把自己改过的同步项导入同步记录（§10），
    /// 优先级最低，之后任何一台电脑上的真实修改都优先。还是默认值的项不导入。
    pub fn import_synced(store: &Store) -> anyhow::Result<()> {
        let mine = serde_json::to_value(Self::load_local(store))?;
        let def = serde_json::to_value(Self::default())?;
        let items: Vec<(&str, serde_json::Value)> = SYNCED.iter().filter(|k| mine[**k] != def[**k]).map(|k| (*k, mine[*k].clone())).collect();
        settings_sync::import(store, settings_sync::SETTINGS, &items)
    }

    /// 旧设置里的 `dark`（rev 0）、`agent`（rev 1）、`lagoon`（rev 2）是当时的默认值：换成现在的默认配色（只换一次，见 `theme_rev`）。
    fn migrated(mut self) -> Self {
        if self.theme_rev < THEME_REV {
            if (self.theme_rev < 1 && self.theme == "dark") || (self.theme_rev < 2 && self.theme == "agent") || self.theme == "lagoon" {
                self.theme = DEFAULT_THEME.into();
            }
            self.theme_rev = THEME_REV;
        }
        self
    }

    /// 保存；同步项有改动时写成本机的同步记录，返回 true（这时要通知 settings_sync 去同步）。
    pub fn save(&self, store: &Store, now_ms: i64) -> anyhow::Result<bool> {
        let old = serde_json::to_value(Self::load(store))?;
        let new = serde_json::to_value(self)?;
        store.set(KEY, self)?;
        let changed: Vec<(&str, Option<serde_json::Value>)> = SYNCED.iter().filter(|k| old[**k] != new[**k]).map(|k| (*k, Some(new[*k].clone()))).collect();
        if changed.is_empty() {
            return Ok(false);
        }
        settings_sync::write_local(store, now_ms, settings_sync::SETTINGS, &changed)
    }

    pub fn normalized(mut self) -> Self {
        if !matches!(self.font.as_str(), "wenkai" | "system") {
            self.font = "wenkai".into();
        }
        if !THEMES.contains(&self.theme.as_str()) {
            self.theme = DEFAULT_THEME.into();
        }
        self.theme_rev = THEME_REV;
        self.font_size = self.font_size.clamp(10, 28);
        self.attach_days = self.attach_days.clamp(1, 365);
        self.default_workdir = self.default_workdir.trim().to_string();
        if self.default_workdir.is_empty() {
            self.default_workdir = default_workdir();
        }
        self.default_shell = self.default_shell.trim().to_string();
        self.device_name = self.device_name.trim().chars().filter(|c| !c.is_control()).take(64).collect();
        self
    }

    pub fn device_name(&self) -> String {
        if self.device_name.is_empty() {
            crate::util::host_name()
        } else {
            self.device_name.clone()
        }
    }

    /// 终端字体栈：英文 JetBrains Mono，中文霞鹜文楷等宽或系统黑体，最后是系统等宽字体。
    pub fn font_family(&self) -> String {
        let cjk = if self.font == "wenkai" { "\"LXGW WenKai Mono\", " } else { "" };
        format!(
            "\"JetBrains Mono\", {cjk}\"PingFang SC\", \"Hiragino Sans GB\", \"Microsoft YaHei UI\", \"Microsoft YaHei\", \
             \"Noto Sans Mono CJK SC\", \"Noto Sans CJK SC\", \"Sarasa Mono SC\", Menlo, Consolas, monospace"
        )
    }
}

/// 管理台打开地址里的主机部分（访问控制里的“打开地址”，见 gateway/access.rs）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenHost {
    Ip(IpAddr),
    Name(String),
}

impl OpenHost {
    /// 放进网址里的写法（IPv6 带方括号）。
    pub fn url_host(&self) -> String {
        match self {
            OpenHost::Ip(IpAddr::V6(ip)) => format!("[{ip}]"),
            other => other.to_string(),
        }
    }

    /// 请求头 Host（去掉端口、方括号，小写）是否就是这个主机。
    pub fn matches(&self, name: &str) -> bool {
        match self {
            OpenHost::Ip(ip) => name.parse::<IpAddr>().is_ok_and(|x| x == *ip),
            OpenHost::Name(n) => name.trim_end_matches('.').eq_ignore_ascii_case(n),
        }
    }
}

impl std::fmt::Display for OpenHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OpenHost::Ip(ip) => write!(f, "{ip}"),
            OpenHost::Name(n) => f.write_str(n),
        }
    }
}

/// 校验打开地址：只接受主机本身——IPv4、IPv6（可带方括号）、域名或局域网主机名（如 `nas`、`nas.local`）。
/// 空字符串表示本机。错误是界面 `error:rule.*` 的规则名。
pub fn parse_open_host(input: &str) -> Result<OpenHost, &'static str> {
    let s = input.trim();
    if s.is_empty() {
        return Ok(OpenHost::Ip(IpAddr::V4(Ipv4Addr::LOCALHOST)));
    }
    if s.contains("://") {
        return Err("host_scheme");
    }
    if s.contains(['/', '?', '#', '@', ' ', '\t']) {
        return Err("host_path");
    }
    // [IPv6] 或裸 IPv6
    if let Some(rest) = s.strip_prefix('[') {
        let Some((inner, tail)) = rest.split_once(']') else { return Err("host_ip") };
        if !tail.is_empty() {
            return Err("host_port");
        }
        let ip: Ipv6Addr = inner.parse().map_err(|_| "host_ip")?;
        return check_ip(IpAddr::V6(ip));
    }
    if s.matches(':').count() >= 2 {
        let ip: Ipv6Addr = s.parse().map_err(|_| "host_ip")?;
        return check_ip(IpAddr::V6(ip));
    }
    if s.contains(':') {
        return Err("host_port");
    }
    let name = s.trim_end_matches('.').to_ascii_lowercase();
    // 全是数字和点：只能是完整的 IPv4（挡住 192.168.1 或 300.1.1.1 这类笔误）。
    if name.chars().all(|c| c.is_ascii_digit() || c == '.') {
        let ip: Ipv4Addr = name.parse().map_err(|_| "host_ip")?;
        return check_ip(IpAddr::V4(ip));
    }
    if name.len() > 253 || name.is_empty() {
        return Err("host_name");
    }
    for label in name.split('.') {
        let ok = !label.is_empty()
            && label.len() <= 63
            && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
            && !label.starts_with('-')
            && !label.ends_with('-');
        if !ok {
            return Err("host_name");
        }
    }
    // 顶级域不能全是数字（例如 foo.123，多半是 IP 打错了）
    if name.rsplit('.').next().is_some_and(|tld| tld.chars().all(|c| c.is_ascii_digit())) {
        return Err("host_name");
    }
    Ok(OpenHost::Name(name))
}

fn check_ip(ip: IpAddr) -> Result<OpenHost, &'static str> {
    let bad = ip.is_unspecified()
        || ip.is_multicast()
        || matches!(ip, IpAddr::V4(v4) if v4.is_broadcast() || v4.octets()[0] == 0);
    if bad {
        return Err("host_unusable");
    }
    Ok(OpenHost::Ip(ip))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_default_theme_moves_to_tangerine_once() {
        // 加 agent 配色之前存下的设置：没有 theme_rev，theme 是当时的默认 dark
        let old: Settings = serde_json::from_str(r#"{"theme":"dark","font_size":15}"#).unwrap();
        assert_eq!(old.theme_rev, 0);
        assert_eq!(old.clone().migrated().theme, "tangerine");
        // 默认还是 agent（rev 1）、lagoon（rev 2）时存下的：换成新默认；当时自己选的 dark 不动
        let agent: Settings = serde_json::from_str(r#"{"theme":"agent","theme_rev":1}"#).unwrap();
        assert_eq!(agent.migrated().theme, "tangerine");
        let lagoon: Settings = serde_json::from_str(r#"{"theme":"lagoon","theme_rev":2}"#).unwrap();
        assert_eq!(lagoon.migrated().theme, "tangerine");
        let dark1: Settings = serde_json::from_str(r#"{"theme":"dark","theme_rev":1}"#).unwrap();
        assert_eq!(dark1.migrated().theme, "dark");
        let agent2: Settings = serde_json::from_str(r#"{"theme":"agent","theme_rev":2}"#).unwrap();
        assert_eq!(agent2.migrated().theme, "agent", "rev 2 时自己选的 agent 不动");
        let light: Settings = serde_json::from_str(r#"{"theme":"light"}"#).unwrap();
        assert_eq!(light.migrated().theme, "light", "自己选的浅色不动");
        // 之后又自己选回 agent / dark / lagoon 并保存：保留
        for t in ["agent", "dark", "sunset", "lagoon"] {
            let chosen = Settings { theme: t.into(), ..Settings::default() }.normalized();
            let back: Settings = serde_json::from_str(&serde_json::to_string(&chosen).unwrap()).unwrap();
            assert_eq!(back.migrated().theme, t);
        }
        assert_eq!(Settings::default().theme, "tangerine");
        assert_eq!(Settings { theme: "neon".into(), ..Settings::default() }.normalized().theme, "tangerine");
    }

    #[test]
    fn synced_fields_come_from_sync_records() {
        let store = Store::memory().unwrap();
        // 升级前改过字号：导入后照旧
        store.set(KEY, &Settings { font_size: 18, default_workdir: "/w".into(), ..Settings::default() }).unwrap();
        Settings::import_synced(&store).unwrap();
        assert_eq!(Settings::load(&store).font_size, 18);
        // 另一台电脑改了配色和字号（比导入的新）：这台跟着变，不同步的字段不动
        let other = Store::memory().unwrap();
        settings_sync::write_local(&other, 5, settings_sync::SETTINGS, &[("theme", Some("mint".into())), ("font_size", Some(16.into()))]).unwrap();
        let (recs, _) = settings_sync::missing(&other, &Default::default(), 100).unwrap();
        settings_sync::merge(&store, &recs).unwrap();
        let s = Settings::load(&store);
        assert_eq!((s.theme.as_str(), s.font_size, s.default_workdir.as_str()), ("mint", 16, "/w"));
        // 只改不同步的字段：不算同步项改动
        assert!(!Settings { default_workdir: "/x".into(), ..s.clone() }.save(&store, 10).unwrap());
        assert!(Settings { font: "system".into(), ..s }.save(&store, 11).unwrap());
        assert_eq!(Settings::load(&store).font, "system");
    }

    #[test]
    fn open_host_accepts() {
        for ok in ["", "127.0.0.1", "localhost", "192.168.1.20", "10.0.0.5", "nas", "NAS.local", "my-nas.lan", "home.example.com", "fd00::1", "[fd00::1]", "::1", "xn--fiqs8s.example"] {
            assert!(parse_open_host(ok).is_ok(), "{ok}");
        }
        assert_eq!(parse_open_host(" NAS.Local. ").unwrap(), OpenHost::Name("nas.local".into()));
        assert_eq!(parse_open_host("[fd00::1]").unwrap().url_host(), "[fd00::1]");
    }

    #[test]
    fn open_host_rejects() {
        let cases = [
            ("http://192.168.1.20", "host_scheme"),
            ("192.168.1.20:1234", "host_port"),
            ("nas.local:8080", "host_port"),
            ("[fd00::1]:1234", "host_port"),
            ("192.168.1.20/", "host_path"),
            ("nas local", "host_path"),
            ("192.168.1", "host_ip"),
            ("300.1.1.1", "host_ip"),
            ("192.168.01.1", "host_ip"),
            ("fd00::zz", "host_ip"),
            ("0.0.0.0", "host_unusable"),
            ("::", "host_unusable"),
            ("255.255.255.255", "host_unusable"),
            ("224.0.0.1", "host_unusable"),
            ("-nas.local", "host_name"),
            ("nas_box", "host_name"),
            ("nas..local", "host_name"),
            ("中文.com", "host_name"),
            ("foo.123", "host_name"),
        ];
        for (input, rule) in cases {
            assert_eq!(parse_open_host(input), Err(rule), "{input}");
        }
    }

    #[test]
    fn open_host_matches_host_header() {
        let h = parse_open_host("nas.local").unwrap();
        assert!(h.matches("NAS.local") && h.matches("nas.local.") && !h.matches("evil.example"));
        let ip = parse_open_host("fd00::1").unwrap();
        assert!(ip.matches("fd00:0::1") && !ip.matches("fd00::2"));
    }
}
