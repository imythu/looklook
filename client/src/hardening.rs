//! 二进制加固（详细设计任务 3）。诚实说明放在最前面：这里的每一项都只是“提高破解成本”的
//! 速度带，不是防线——认真要拆客户端的人（改环境变量、patch 掉这段代码、用内核级调试器……）
//! 都能绕过。真正敏感的信息（账户密码、设备私钥）本来就不在这些机制保护范围内，它们走的是
//! 设备密钥对 + 平台签名（见 `src/platform.rs`、`docs/CLIENT_DESIGN.md`），跟这个模块无关。
//!
//! 包含：
//! - 反调试探测（Linux `/proc/self/status` 的 `TracerPid`；Windows `IsDebuggerPresent`；
//!   macOS `sysctl` 查 `P_TRACED`），只记警告，不强行退出；`LOOKLOOK_ALLOW_DEBUG=1` 跳过，
//!   dev 构建（`cfg(debug_assertions)`）从不检测。
//! - 编译期内置密钥（服务端地址、平台公钥）的运行时还原，配合 `build.rs` 的异或混淆。
//! - 运行文件的 SHA-256 基线，换了字节能发现，仅此而已（见 `check_self_integrity` 文档）。
//! - Windows 专用：`windows_subsystem = "windows"`（见 main.rs）去掉控制台后，从终端启动时
//!   重新接上父进程控制台，命令行子命令才有输出，见 `attach_console_if_launched_from_terminal`。

use anyhow::Result;
use sha2::{Digest, Sha256};

use crate::store::Store;

// build.rs 生成：SECRET_KEY_LEN、SECRET_XOR_KEY、SERVER_URL_XOR、PLATFORM_KEYS_XOR。
include!(concat!(env!("OUT_DIR"), "/secrets.rs"));

fn xor(data: &[u8], key: &[u8; SECRET_KEY_LEN]) -> Vec<u8> {
    data.iter().enumerate().map(|(i, b)| b ^ key[i % SECRET_KEY_LEN]).collect()
}

/// 编译期写入的默认服务端地址；构建时没设置 `LOOKLOOK_SERVER_URL` 时回落到占位地址
/// （跟以前 `option_env!` 版本的行为一致）。
pub fn default_server() -> String {
    let s = String::from_utf8_lossy(&xor(SERVER_URL_XOR, &SECRET_XOR_KEY)).into_owned();
    if s.is_empty() {
        "https://looklook.example".to_string()
    } else {
        s
    }
}

/// 编译期写入的内置平台公钥列表；没设置时为空字符串（客户端首次登录时会从服务端
/// `/.well-known/looklook.json` 取得公钥并固定下来，见 `Platform::pin_keys_from_well_known`）。
pub fn builtin_platform_keys() -> String {
    String::from_utf8_lossy(&xor(PLATFORM_KEYS_XOR, &SECRET_XOR_KEY)).into_owned()
}

/// dev 构建从不检测；release 构建里设 `LOOKLOOK_ALLOW_DEBUG=1` 可以跳过（比如自己要用调试器
/// 挂到 release 包上排障）。
fn debug_checks_enabled() -> bool {
    if cfg!(debug_assertions) {
        return false;
    }
    std::env::var("LOOKLOOK_ALLOW_DEBUG").ok().as_deref() != Some("1")
}

/// 反调试探测：发现调试器只记一条警告日志，不强行退出。做成“检测到就退出”只会误伤
/// 正常用户（比如系统自带的崩溃报告工具偶尔也会短暂挂 ptrace），对真想绕过的人没有实际
/// 阻挡效果，见模块开头的说明。
pub fn probe_debugger() {
    if !debug_checks_enabled() {
        return;
    }
    if let Some(reason) = detect_debugger() {
        tracing::warn!(reason, "检测到可能的调试/注入环境（提示，不阻止运行）");
    }
}

#[cfg(target_os = "linux")]
fn detect_debugger() -> Option<&'static str> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|l| l.starts_with("TracerPid:"))?;
    let pid: i64 = line.split(':').nth(1)?.trim().parse().ok()?;
    (pid != 0).then_some("TracerPid")
}

#[cfg(windows)]
fn detect_debugger() -> Option<&'static str> {
    #[link(name = "kernel32")]
    extern "system" {
        fn IsDebuggerPresent() -> i32;
    }
    (unsafe { IsDebuggerPresent() } != 0).then_some("IsDebuggerPresent")
}

#[cfg(target_os = "macos")]
fn detect_debugger() -> Option<&'static str> {
    // `sysctl(CTL_KERN, KERN_PROC, KERN_PROC_PID, getpid())` 取 `kinfo_proc`，查
    // `kp_proc.p_flag & P_TRACED`（Apple 官方 "Determining If You're Being Debugged" 范例的
    // 思路，来自 bsd/sys/proc.h 的 `extern_proc`）。p_flag 在 64 位 Darwin ABI 上的偏移：
    // `p_un`（union，两个指针或一个 timeval，都是 16 字节）+ `p_vmspace`（8）+ `p_sigacts`（8）
    // = 32 字节后。这段结构体布局没法在本环境验证（不能交叉编译到 macOS，见任务说明）；
    // 万一哪个系统版本的 ABI 有出入，`sysctl` 调用本身或后续解析会失败，直接返回 None——
    // 探测失败只是“没提示”，不影响正常运行（这本来就只是个提示，不是拦截）。
    #[repr(C)]
    struct ExternProcPrefix {
        p_un: [u8; 16],
        p_vmspace: u64,
        p_sigacts: u64,
        p_flag: i32,
    }
    #[repr(C)]
    struct KinfoProc {
        kp_proc: ExternProcPrefix,
        _rest: [u8; 600], // kp_eproc 部分用不到，留够大的缓冲区给 sysctl 写
    }
    const CTL_KERN: i32 = 1;
    const KERN_PROC: i32 = 14;
    const KERN_PROC_PID: i32 = 1;
    const P_TRACED: i32 = 0x0000_0800;

    extern "C" {
        fn sysctl(name: *mut i32, namelen: u32, oldp: *mut core::ffi::c_void, oldlenp: *mut usize, newp: *mut core::ffi::c_void, newlen: usize) -> i32;
        fn getpid() -> i32;
    }
    unsafe {
        let mut mib = [CTL_KERN, KERN_PROC, KERN_PROC_PID, getpid()];
        let mut info: KinfoProc = std::mem::zeroed();
        let mut len = std::mem::size_of::<KinfoProc>();
        if sysctl(mib.as_mut_ptr(), 4, &mut info as *mut _ as *mut _, &mut len, std::ptr::null_mut(), 0) != 0 {
            return None;
        }
        (info.kp_proc.p_flag & P_TRACED != 0).then_some("P_TRACED")
    }
}

#[cfg(not(any(target_os = "linux", windows, target_os = "macos")))]
fn detect_debugger() -> Option<&'static str> {
    None
}

/// 运行文件的 SHA-256 基线：第一次运行时记下来（存在本地 `kv` 表的 `self_hash`），以后不一致
/// 就记一条警告。诚实说明：这只是“文件被换过一个字节就能发现”的速度提示——正常的自动升级
/// 也会改哈希（需要手动清掉 `self_hash` 重新建立基线，这里没有做自动识别“是不是自己升级的”），
/// 真要防篡改需要代码签名 + 操作系统级完整性校验，这个模块做不到，见 docs/FAQ.md。
pub fn check_self_integrity(store: &Store) {
    let Ok(hash) = current_exe_hash() else { return };
    match store.get::<String>("self_hash") {
        Ok(Some(baseline)) if baseline != hash => {
            tracing::warn!("运行文件的哈希和上次记录的不一样（可能是正常升级，也可能被改过）");
        }
        Ok(Some(_)) => {}
        _ => {
            let _ = store.set("self_hash", &hash);
        }
    }
}

fn current_exe_hash() -> Result<String> {
    let path = std::env::current_exe()?;
    let bytes = std::fs::read(path)?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    Ok(data_encoding::HEXLOWER.encode(&hasher.finalize()))
}

/// Windows：`windows_subsystem = "windows"`（见 main.rs）去掉了控制台；从真正的终端
/// （cmd.exe / PowerShell）启动时，把标准输入输出接回父进程的控制台，`login` / `url` /
/// `status` 等命令行子命令才看得见输出。双击图标、开机启动等没有父控制台的场景，
/// `AttachConsole` 会失败，直接跳过——这些场景本来就该走托盘（见 `src/desktop.rs`）。
#[cfg(windows)]
pub fn attach_console_if_launched_from_terminal() {
    use std::os::windows::io::AsRawHandle;

    #[link(name = "kernel32")]
    extern "system" {
        fn AttachConsole(dw_process_id: u32) -> i32;
        fn SetStdHandle(std_handle: i32, handle: isize) -> i32;
    }
    const ATTACH_PARENT_PROCESS: u32 = u32::MAX;
    const STD_INPUT_HANDLE: i32 = -10;
    const STD_OUTPUT_HANDLE: i32 = -11;
    const STD_ERROR_HANDLE: i32 = -12;

    unsafe {
        if AttachConsole(ATTACH_PARENT_PROCESS) == 0 {
            return; // 没有父控制台（双击 / 开机启动 / 任务计划），交给托盘
        }
        if let Ok(f) = std::fs::OpenOptions::new().write(true).open("CONOUT$") {
            SetStdHandle(STD_OUTPUT_HANDLE, f.as_raw_handle() as isize);
            std::mem::forget(f); // 句柄现在归控制台/标准输出所有，不在这里关闭
        }
        if let Ok(f) = std::fs::OpenOptions::new().write(true).open("CONOUT$") {
            SetStdHandle(STD_ERROR_HANDLE, f.as_raw_handle() as isize);
            std::mem::forget(f);
        }
        if let Ok(f) = std::fs::OpenOptions::new().read(true).open("CONIN$") {
            SetStdHandle(STD_INPUT_HANDLE, f.as_raw_handle() as isize);
            std::mem::forget(f);
        }
    }
}

/// 非 Windows 平台没有这个问题（见 `docs/FAQ.md` 的 macOS 说明：Finder/LaunchAgent 启动的
/// 进程本来就没有挂控制台，不需要额外处理），提供空实现方便 main.rs 无条件调用。
#[cfg(not(windows))]
pub fn attach_console_if_launched_from_terminal() {}
