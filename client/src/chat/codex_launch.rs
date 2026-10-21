//! 找 Codex 程序。
//!
//! Windows 上直接调用 `codex.exe`：npm 的 `codex.cmd` 会把参数里的 `<` `>` 当成重定向，`codex.ps1` 会被执行策略拦住。
//! （以前对话模式的 Codex 用 hooks 接入，profile 和信任哈希也在这里；现在改走 app-server，见 `codex_chat.rs`。）

use std::path::{Path, PathBuf};

/// 启动 Codex 用的程序：Windows 上是 `codex.exe` 的完整路径（找不到时 None，照常用名字）；其他平台 None。
pub fn program() -> Option<PathBuf> {
    if !cfg!(windows) {
        return None;
    }
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let exe = dir.join("codex.exe");
        if exe.is_file() {
            return Some(exe);
        }
        if dir.join("codex.cmd").is_file() {
            if let Some(exe) = npm_vendor_exe(&dir) {
                return Some(exe);
            }
        }
    }
    None
}

/// npm 全局安装：`<目录>/node_modules/@openai/codex/node_modules/@openai/codex-win32-*/vendor/<三元组>/bin/codex.exe`
fn npm_vendor_exe(npm_dir: &Path) -> Option<PathBuf> {
    let base = npm_dir.join("node_modules").join("@openai").join("codex");
    let scopes = [base.join("node_modules").join("@openai"), npm_dir.join("node_modules").join("@openai")];
    for scope in scopes {
        let Ok(rd) = std::fs::read_dir(&scope) else { continue };
        for pkg in rd.flatten().map(|e| e.path()).filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("codex-win32-"))) {
            let Ok(vendor) = std::fs::read_dir(pkg.join("vendor")) else { continue };
            if let Some(exe) = vendor.flatten().map(|e| e.path().join("bin").join("codex.exe")).find(|p| p.is_file()) {
                return Some(exe);
            }
        }
    }
    None
}
