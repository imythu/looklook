//! 验证过的 Codex 版本（docs/CODEX_UI.md §10.1）。
//!
//! Codex 更新很快，rollout、屏幕文字、hook 字段都可能随版本变。范围外的版本照样用，只是状态里带上
//! `untested_version`，对话界面顶部提示一句“没验证过，显示不对时可以切到终端”。
//! 升级检查清单见 docs/CODEX_UI.md §10.6：验证过新版本后把 `TESTED_MAX` 往后改。

/// 验证过的范围（含两端）：`(主, 次)` 版本，补丁号不限
const TESTED_MIN: (u64, u64) = (0, 159);
const TESTED_MAX: (u64, u64) = (0, 159);

/// `0.159.3`、`0.160.0-alpha.1` → (0, 159)。认不出的不算没验证（不提示）。
fn major_minor(version: &str) -> Option<(u64, u64)> {
    let mut it = version.trim().trim_start_matches('v').split(['.', '-']);
    Some((it.next()?.parse().ok()?, it.next()?.parse().ok()?))
}

pub fn untested(version: &str) -> bool {
    major_minor(version).is_some_and(|v| v < TESTED_MIN || v > TESTED_MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range() {
        assert!(!untested("0.159.3"));
        assert!(!untested("0.159.0"));
        assert!(untested("0.163.0"));
        assert!(untested("0.158.9"));
        assert!(untested("1.0.0"));
        assert!(untested("0.160.0-alpha.1"));
        assert!(!untested("garbage"), "认不出的不提示");
    }
}
