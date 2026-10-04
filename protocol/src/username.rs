//! 用户名规范化与校验（详细设计 §4.1.3）。
//!
//! 用户名同时是用户站 `{username}.{root}` 与隧道站 `{username}--{tunnel_id}.{root}`
//! 的 DNS 标签，因此规则按 DNS 标签收紧。客户端注册页、网页端与平台共用本实现。

pub const MIN_LEN: usize = 3;
pub const MAX_LEN: usize = 32;

/// 违反的规则，对应错误码 `USERNAME_INVALID` 的 `params.rule`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rule {
    Length,
    Charset,
    Start,
    End,
    DoubleHyphen,
}

impl Rule {
    pub fn as_str(self) -> &'static str {
        match self {
            Rule::Length => "length",
            Rule::Charset => "charset",
            Rule::Start => "start",
            Rule::End => "end",
            Rule::DoubleHyphen => "double_hyphen",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UsernameError {
    Invalid(Rule),
    Reserved,
}

/// 内置保留字，只能追加（系统设置 `reserved_usernames_extra`），不能删除。
pub const RESERVED: &[&str] = &[
    // 平台固定子域
    "www", "admin", "relay", "api", "app", "static", "assets", "cdn", "img", "images", "media",
    "files", "upload", "uploads", "dl", "download", "downloads", "update", "updates", "release",
    "releases",
    // 平台功能词
    "console", "dashboard", "panel", "portal", "manage", "manager", "management", "gateway",
    "proxy", "tunnel", "tunnels", "relay-server", "ws", "wss", "rpc", "grpc", "graphql", "webhook",
    "webhooks", "callback", "oauth", "auth", "sso", "login", "logout", "signin", "signup",
    "register", "account", "accounts", "user", "users", "profile", "settings", "billing", "pay",
    "payment", "payments", "checkout", "order", "orders", "invoice", "invite", "promo", "ads", "ad",
    // 管理与身份
    "administrator", "root", "system", "sys", "sysadmin", "superuser", "operator", "ops", "staff",
    "team", "official", "owner", "moderator", "mod", "security", "abuse", "support", "help",
    "helpdesk", "service", "services", "contact", "feedback", "legal", "privacy", "terms", "tos",
    "compliance",
    // 邮件
    "mail", "email", "smtp", "imap", "pop", "pop3", "mx", "webmail", "postmaster", "hostmaster",
    "webmaster", "noreply", "no-reply", "mailer", "newsletter", "bounce", "bounces", "autoconfig",
    "autodiscover", "mta-sts", "openpgpkey", "dkim", "spf", "dmarc", "bimi",
    // DNS 与网络
    "ns", "ns1", "ns2", "ns3", "ns4", "dns", "dns1", "dns2", "ntp", "time", "vpn", "ftp", "sftp",
    "ssh", "git", "svn", "ldap", "kerberos", "radius", "proxy-pac",
    // 安全敏感
    "localhost", "local", "wpad", "isatap", "broadcasthost", "ip6-localhost", "ip6-loopback",
    // 运维与环境
    "status", "health", "healthz", "metrics", "monitor", "monitoring", "grafana", "prometheus",
    "logs", "log", "trace", "tracing", "sentry", "backup", "backups", "db", "database", "redis",
    "postgres", "mysql", "cache", "queue", "internal", "intranet", "dev", "devel", "develop",
    "test", "testing", "qa", "staging", "stage", "uat", "prod", "production", "preview", "demo",
    "sandbox", "beta", "alpha", "canary", "edge", "origin",
    // 内容与品牌
    "docs", "doc", "wiki", "blog", "news", "about", "home", "index", "default", "example",
    "examples", "store", "shop", "market", "community", "forum", "bbs", "status-page", "looklook",
    "kankan", "looklook-app", "looklook-official",
    // 开发工具
    "npm", "registry", "docker", "hub", "pypi", "maven", "mirror", "mirrors", "repo", "repos",
    "packages", "ci", "cd", "build", "builds", "jenkins", "runner", "runners",
];

/// 以这些前缀开头的用户名同样视为保留（防冒充），如 `admin2`、`looklook-help`。
pub const RESERVED_PREFIXES: &[&str] = &["admin", "support", "official", "security", "looklook", "kankan"];

/// 步骤 1：去首尾空白，只接受 ASCII，`A-Z` 转小写。
pub fn normalize(raw: &str) -> Result<String, UsernameError> {
    let s = raw.trim();
    if !s.is_ascii() {
        return Err(UsernameError::Invalid(Rule::Charset));
    }
    Ok(s.to_ascii_lowercase())
}

/// 步骤 2–5：格式校验（输入应已规范化）。
pub fn check_format(name: &str) -> Result<(), Rule> {
    let len = name.len();
    if !(MIN_LEN..=MAX_LEN).contains(&len) {
        return Err(Rule::Length);
    }
    if !name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-') {
        return Err(Rule::Charset);
    }
    let bytes = name.as_bytes();
    if !bytes[0].is_ascii_lowercase() {
        return Err(Rule::Start);
    }
    if bytes[len - 1] == b'-' {
        return Err(Rule::End);
    }
    if name.contains("--") {
        return Err(Rule::DoubleHyphen);
    }
    Ok(())
}

/// 中转的主机名（`r1`、`r2`…）：DNS 里是仅 DNS 记录，与同名用户站冲突，因此保留。
pub fn is_relay_name(name: &str) -> bool {
    name.len() > 1 && name.starts_with('r') && name[1..].bytes().all(|b| b.is_ascii_digit())
}

/// 步骤 6：内置保留字与前缀、中转名称；`extra` 为管理员追加的保留字。
pub fn is_reserved(name: &str, extra: &[String]) -> bool {
    RESERVED.contains(&name)
        || RESERVED_PREFIXES.iter().any(|p| name.starts_with(p))
        || is_relay_name(name)
        || extra.iter().any(|e| e.eq_ignore_ascii_case(name))
}

/// 规范化 + 格式 + 保留字（步骤 1–6）。历史保留与唯一性需要查库，由平台处理。
pub fn validate(raw: &str, extra: &[String]) -> Result<String, UsernameError> {
    let name = normalize(raw)?;
    check_format(&name).map_err(UsernameError::Invalid)?;
    if is_reserved(&name, extra) {
        return Err(UsernameError::Reserved);
    }
    Ok(name)
}

/// 只做规范化与格式校验，不合法返回 `None`（网关解析 Host、按用户名查询时使用）。
pub fn parse(raw: &str) -> Option<String> {
    let name = normalize(raw).ok()?;
    check_format(&name).ok()?;
    Some(name)
}

/// 隧道 ID：8 位小写 base32（`a-z2-7`）。
pub fn is_tunnel_id(s: &str) -> bool {
    s.len() == 8 && s.bytes().all(|b| b.is_ascii_lowercase() || (b'2'..=b'7').contains(&b))
}

/// 拆分 Host 最左标签：不含 `--` 为用户站，含 `--` 按第一个 `--` 拆成用户名与隧道 ID。
pub fn split_host_label(label: &str) -> (&str, Option<&str>) {
    match label.find("--") {
        Some(i) => (&label[..i], Some(&label[i + 2..])),
        None => (label, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_names() {
        for n in ["abc", "a1-b2", &"a".repeat(32), "alice", "bob-7"] {
            assert_eq!(validate(n, &[]), Ok(n.to_string()), "{n}");
        }
        assert_eq!(validate("  Alice ", &[]), Ok("alice".into()));
    }

    #[test]
    fn invalid_names() {
        use Rule::*;
        let cases: &[(&str, UsernameError)] = &[
            ("ab", UsernameError::Invalid(Length)),
            (&"a".repeat(33), UsernameError::Invalid(Length)),
            ("1abc", UsernameError::Invalid(Start)),
            ("-abc", UsernameError::Invalid(Start)),
            ("abc-", UsernameError::Invalid(End)),
            ("a--b", UsernameError::Invalid(DoubleHyphen)),
            ("xn--abc", UsernameError::Invalid(DoubleHyphen)),
            ("a.b", UsernameError::Invalid(Charset)),
            ("a_b", UsernameError::Invalid(Charset)),
            ("用户名", UsernameError::Invalid(Charset)),
            ("ａｂｃ", UsernameError::Invalid(Charset)),
        ];
        for (n, e) in cases {
            assert_eq!(validate(n, &[]).as_ref(), Err(e), "{n}");
        }
    }

    #[test]
    fn reserved_names() {
        for n in ["admin", "admin2", "looklook-help", "wpad", "autodiscover", "www", "relay", "kankan1", "r12", "r100"] {
            assert_eq!(validate(n, &[]), Err(UsernameError::Reserved), "{n}");
        }
        assert_eq!(validate("carol", &["Carol".into()]), Err(UsernameError::Reserved));
        for n in ["r1a", "ray", "r-12", "rr12"] {
            assert!(validate(n, &[]).is_ok(), "{n}");
        }
    }

    #[test]
    fn reserved_list_is_well_formed() {
        // 设计表中保留了少量两字符的词（ns、ws、db…），它们已被长度规则排除。
        for n in RESERVED.iter().filter(|n| n.len() >= MIN_LEN) {
            assert_eq!(check_format(n), Ok(()), "{n}");
        }
    }

    #[test]
    fn host_labels() {
        assert_eq!(split_host_label("alice"), ("alice", None));
        assert_eq!(split_host_label("alice--k7m2q6xa"), ("alice", Some("k7m2q6xa")));
        assert!(is_tunnel_id("k7m2q6xa"));
        assert!(!is_tunnel_id("k7m2q9xa"));
        assert!(!is_tunnel_id("k7m2q6x1"));
        assert!(!is_tunnel_id("K7M2Q9XA"));
    }
}
