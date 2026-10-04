//! 时间格式、随机 ID、私密文件写入等小工具。

use std::io::Write;
use std::path::Path;

use rand::RngCore;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

/// RFC 3339 UTC，毫秒精度，`Z` 结尾：`2026-09-28T12:00:00.000Z`（与服务端一致）。
pub fn fmt_time(t: OffsetDateTime) -> String {
    let t = t.to_offset(time::UtcOffset::UTC);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        t.year(),
        t.month() as u8,
        t.day(),
        t.hour(),
        t.minute(),
        t.second(),
        t.millisecond()
    )
}

pub fn fmt_ms(ms: i64) -> String {
    fmt_time(from_ms(ms))
}

pub fn from_ms(ms: i64) -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp_nanos(ms as i128 * 1_000_000).unwrap_or(OffsetDateTime::UNIX_EPOCH)
}

pub fn parse_time_ms(s: &str) -> Option<i64> {
    let t = OffsetDateTime::parse(s, &Rfc3339).ok()?;
    Some((t.unix_timestamp_nanos() / 1_000_000) as i64)
}

pub fn system_ms() -> i64 {
    (OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000) as i64
}

pub fn now_rfc3339() -> String {
    fmt_time(OffsetDateTime::now_utc())
}

pub fn random_bytes<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    rand::thread_rng().fill_bytes(&mut b);
    b
}

/// 小写 base32（`a-z2-7`）随机 ID，用于终端实例的访问路径 `/i/{id}/`。
pub fn random_id(len: usize) -> String {
    let s = data_encoding::BASE32_NOPAD.encode(&random_bytes::<16>()).to_ascii_lowercase();
    s[..len.min(s.len())].to_string()
}

pub fn random_secret() -> String {
    looklook_protocol::signing::b64(&random_bytes::<24>())
}

/// 写入只有当前用户可读写的文件（私钥、通道配置）：先写临时文件再改名。
pub fn write_private(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)
}

/// 本机名称，用作“电脑名称”的默认值。
pub fn host_name() -> String {
    hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .map(|h| h.trim_end_matches(".local").to_string())
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "我的电脑".into())
}

/// 桌面环境才自动打开浏览器；systemd 服务、SSH 会话里不打开。
pub fn has_desktop() -> bool {
    if std::env::var_os("INVOCATION_ID").is_some() || std::env::var_os("SSH_CONNECTION").is_some() {
        return false;
    }
    cfg!(any(windows, target_os = "macos")) || std::env::var_os("DISPLAY").is_some() || std::env::var_os("WAYLAND_DISPLAY").is_some()
}

/// 用系统默认浏览器打开网址；成功返回 true。
pub fn open_url(url: &str) -> bool {
    let status = if cfg!(target_os = "macos") {
        std::process::Command::new("open").arg(url).status()
    } else if cfg!(windows) {
        std::process::Command::new("rundll32").args(["url.dll,FileProtocolHandler", url]).status()
    } else {
        std::process::Command::new("xdg-open").arg(url).status()
    };
    matches!(status, Ok(s) if s.success())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_roundtrip() {
        let ms = 1_790_000_000_123;
        let s = fmt_ms(ms);
        assert!(s.ends_with(".123Z"));
        assert_eq!(parse_time_ms(&s), Some(ms));
        assert_eq!(parse_time_ms("2026-09-28T12:00:00.000Z").map(fmt_ms).as_deref(), Some("2026-09-28T12:00:00.000Z"));
    }

    #[test]
    fn ids() {
        let id = random_id(8);
        assert_eq!(id.len(), 8);
        assert!(id.chars().all(|c| c.is_ascii_lowercase() || ('2'..='7').contains(&c)));
    }
}
