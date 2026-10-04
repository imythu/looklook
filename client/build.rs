//! 编译期混淆内置密钥（详细设计任务 3：`LOOKLOOK_SERVER_URL` / `LOOKLOOK_PLATFORM_KEYS`）；
//! 目标是 Windows / macOS 时再生成 Tauri 应用上下文（见文件末尾）。
//!
//! 以前这两个值直接用 `option_env!` 写进二进制的只读数据段，`strings looklook` 就能看到明文。
//! 这里改成异或后连同随机密钥一起写进 `OUT_DIR/secrets.rs`，运行时由 `src/hardening.rs` 还原。
//!
//! 诚实说明：这只是提高静态分析的成本（普通的 `strings`/十六进制编辑器看不到明文），不是加密——
//! 密钥就写在旁边的常量里，专门反的人跟着 `include!` 的生成文件一样能异或回去。真正要保密的话，
//! 应该让密钥完全不出现在客户端里（比如登录时才从服务端下发），这需要改协议，不在这次加固范围内，
//! 见 docs/FAQ.md 里的说明。
use std::env;
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

const KEY_LEN: usize = 16;

fn main() {
    println!("cargo:rerun-if-env-changed=LOOKLOOK_SERVER_URL");
    println!("cargo:rerun-if-env-changed=LOOKLOOK_PLATFORM_KEYS");

    let server = env::var("LOOKLOOK_SERVER_URL").unwrap_or_default();
    let keys = env::var("LOOKLOOK_PLATFORM_KEYS").unwrap_or_default();
    let key = build_key();

    let enc_server = xor(server.as_bytes(), &key);
    let enc_keys = xor(keys.as_bytes(), &key);

    let src = format!(
        "// 由 build.rs 生成，不要手改；被 src/hardening.rs 用 include! 引入。\n\
         pub const SECRET_KEY_LEN: usize = {KEY_LEN};\n\
         pub const SECRET_XOR_KEY: [u8; {KEY_LEN}] = {key:?};\n\
         pub const SERVER_URL_XOR: &[u8] = &{enc_server:?};\n\
         pub const PLATFORM_KEYS_XOR: &[u8] = &{enc_keys:?};\n"
    );

    let out_dir = env::var("OUT_DIR").expect("OUT_DIR 未设置");
    fs::write(Path::new(&out_dir).join("secrets.rs"), src).expect("无法写入 secrets.rs");

    // Windows / macOS 是 Tauri 桌面应用（src/desktop.rs）：生成应用上下文、嵌入图标与 Windows 清单。
    // 这里要看目标平台（CARGO_CFG_TARGET_OS），build.rs 里的 cfg! 是宿主平台。
    let os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if os == "windows" || os == "macos" {
        tauri_build::build();
    }
}

fn xor(data: &[u8], key: &[u8; KEY_LEN]) -> Vec<u8> {
    data.iter().enumerate().map(|(i, b)| b ^ key[i % KEY_LEN]).collect()
}

/// 每次构建生成一把不同的密钥（xorshift64，用构建时间做种子）：不追求密码学强度，
/// 只是不让每个发布版本用同一把固定密钥（那样只要泄露一次就永久失效）。
fn build_key() -> [u8; KEY_LEN] {
    let mut seed = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(0x9E37_79B9_7F4A_7C15) | 1;
    let mut out = [0u8; KEY_LEN];
    for chunk in out.chunks_mut(8) {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        let bytes = seed.to_le_bytes();
        chunk.copy_from_slice(&bytes[..chunk.len()]);
    }
    out
}
