//! 终端使用的 shell：列出本机可用的 shell，解析用户的选择，并按 shell 种类生成启动参数。
//!
//! 每个终端存一个 `shell` 字符串（设置里还有一个默认值）：
//! - 空：自动。Windows 依次 PowerShell 7（pwsh）→ Windows PowerShell → cmd；其他系统用登录 shell；
//! - 内置选项的值：Windows 是 `pwsh` / `powershell` / `cmd` / `git-bash` / `wsl`，其他系统是 shell 的绝对路径；
//! - 其他：用户填写的命令行，例如 `bash`、`fish`、`C:\Program Files\Git\bin\bash.exe`、`zsh -l`。
//!   第一个词按 PATH 查找（Windows 也查 PATHEXT），可以带参数；含空格的路径用引号括起来。
//!
//! 要在 shell 里运行命令（Codex、Claude Code、自己输入的命令）时，按 shell 种类拼参数：
//! POSIX 类（bash、zsh、fish …）用 `-l -i -c "命令; exec shell -l"`，PowerShell 用 `-NoExit -Command`，
//! cmd 用 `/K`，nushell 用 `-e`。认不出的 shell 返回 None，由调用方另想办法（tmux 里把命令“打”进去）。

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::error::LocalError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Family {
    Posix,
    Pwsh,
    Cmd,
    Nu,
    Other,
}

impl Family {
    fn of(program: &str) -> Self {
        // 按两种分隔符取文件名：Windows 路径在测试（Linux）里也要认得
        let name = program.rsplit(['/', '\\']).next().unwrap_or("").to_ascii_lowercase();
        let stem = name.strip_suffix(".exe").unwrap_or(&name);
        match stem {
            "pwsh" | "powershell" => Family::Pwsh,
            "cmd" => Family::Cmd,
            "nu" => Family::Nu,
            "bash" | "zsh" | "sh" | "dash" | "ksh" | "mksh" | "ash" | "fish" | "yash" | "busybox" => Family::Posix,
            _ => Family::Other,
        }
    }
}

/// 解析好的 shell：程序（绝对路径或系统能找到的名字）、用户填写的参数、种类。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shell {
    pub program: String,
    pub args: Vec<String>,
    pub family: Family,
}

impl Shell {
    /// 打开一个交互式 shell 的完整命令行。用户写了参数就照用户的来。
    pub fn interactive(&self) -> Vec<String> {
        let mut v = vec![self.program.clone()];
        if !self.args.is_empty() {
            v.extend(self.args.iter().cloned());
            return v;
        }
        match self.family {
            Family::Posix | Family::Nu => v.push("-l".into()),
            Family::Pwsh => v.push("-NoLogo".into()),
            Family::Cmd | Family::Other => {}
        }
        v
    }

    /// 在这个 shell 里运行命令，命令结束后留在 shell 里（便于看输出、继续操作）。认不出的 shell 返回 None。
    pub fn run(&self, cmd: &str) -> Option<Vec<String>> {
        let cmd = adapt_env_prefix(cmd, self.family);
        let mut v = vec![self.program.clone()];
        v.extend(self.args.iter().cloned());
        match self.family {
            Family::Posix => {
                // exec 回到同一个 shell：Windows 上（Git Bash）用名字，路径里的反斜杠和空格在 bash 里麻烦
                let me = if cfg!(windows) {
                    Path::new(&self.program).file_stem().and_then(|s| s.to_str()).unwrap_or("bash").to_string()
                } else {
                    format!("'{}'", self.program.replace('\'', ""))
                };
                v.extend(["-l".into(), "-i".into(), "-c".into(), format!("{cmd}; exec {me} -l")]);
            }
            Family::Pwsh => v.extend(["-NoLogo".into(), "-NoExit".into(), "-Command".into(), cmd]),
            Family::Cmd => v.extend(["/K".into(), cmd]),
            Family::Nu => v.extend(["-l".into(), "-e".into(), cmd]),
            Family::Other => return None,
        }
        Some(v)
    }
}

/// 启动命令开头的 `NAME=值 命令`（例如 root 下的 `IS_SANDBOX=1 claude …`）是 POSIX 写法，
/// PowerShell 与 cmd 不认识，换成各自的设置环境变量写法。
fn adapt_env_prefix(cmd: &str, family: Family) -> String {
    if !matches!(family, Family::Pwsh | Family::Cmd) {
        return cmd.to_string();
    }
    let mut vars = vec![];
    let mut rest = cmd.trim_start();
    while let Some((word, tail)) = rest.split_once(' ') {
        let Some((k, v)) = word.split_once('=') else { break };
        if k.is_empty() || !k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') || k.starts_with(|c: char| c.is_ascii_digit()) {
            break;
        }
        vars.push((k.to_string(), v.to_string()));
        rest = tail.trim_start();
    }
    if vars.is_empty() {
        return cmd.to_string();
    }
    let set: Vec<String> = match family {
        Family::Pwsh => vars.iter().map(|(k, v)| format!("$env:{k}='{}';", v.replace('\'', "''"))).collect(),
        _ => vars.iter().map(|(k, v)| format!("set \"{k}={v}\" &&")).collect(),
    };
    format!("{} {rest}", set.join(" "))
}

/// 设置里可选的一项（只列出本机找得到的）。`value` 原样存进终端的 `shell` 字段。
#[derive(Debug, Clone, Default, Serialize)]
pub struct ShellOption {
    pub value: String,
    pub label: String,
    pub path: String,
}

/// 内置选项（Windows）：值、显示名、候选位置。
#[cfg(windows)]
fn windows_builtin(value: &str) -> Option<String> {
    let env = |k: &str| std::env::var_os(k).map(PathBuf::from);
    let first = |c: Vec<Option<PathBuf>>| c.into_iter().flatten().find(|p| p.is_file()).map(|p| p.display().to_string());
    match value {
        "pwsh" => which("pwsh").or_else(|| first(vec![env("ProgramFiles").map(|d| d.join("PowerShell\\7\\pwsh.exe")), env("LOCALAPPDATA").map(|d| d.join("Microsoft\\WindowsApps\\pwsh.exe"))])),
        "powershell" => first(vec![env("SystemRoot").map(|d| d.join("System32\\WindowsPowerShell\\v1.0\\powershell.exe"))]).or_else(|| which("powershell")),
        "cmd" => first(vec![env("ComSpec"), env("SystemRoot").map(|d| d.join("System32\\cmd.exe"))]).or_else(|| which("cmd")),
        "git-bash" => git_bash(),
        "wsl" => first(vec![env("SystemRoot").map(|d| d.join("System32\\wsl.exe"))]),
        _ => None,
    }
}

#[cfg(windows)]
const WINDOWS_BUILTINS: &[(&str, &str)] = &[("pwsh", "PowerShell 7"), ("powershell", "Windows PowerShell"), ("cmd", "cmd"), ("git-bash", "Git Bash"), ("wsl", "WSL")];

/// Git for Windows 自带的 bash（不是 System32 里启动 WSL 的那个 bash.exe）。
#[cfg(windows)]
fn git_bash() -> Option<String> {
    let mut dirs: Vec<PathBuf> = ["ProgramFiles", "ProgramW6432", "ProgramFiles(x86)"].iter().filter_map(|k| std::env::var_os(k)).map(|d| PathBuf::from(d).join("Git")).collect();
    if let Some(d) = std::env::var_os("LOCALAPPDATA") {
        dirs.push(PathBuf::from(d).join("Programs\\Git"));
    }
    // 装在别处时：PATH 里的 git.exe 在 Git\cmd 下
    if let Some(git) = which("git") {
        if let Some(root) = Path::new(&git).parent().and_then(Path::parent) {
            dirs.push(root.to_path_buf());
        }
    }
    dirs.into_iter().map(|d| d.join("bin\\bash.exe")).find(|p| p.is_file()).map(|p| p.display().to_string())
}

/// 本机可用的 shell。
pub fn available() -> Vec<ShellOption> {
    #[cfg(windows)]
    {
        WINDOWS_BUILTINS
            .iter()
            .filter_map(|(v, label)| windows_builtin(v).map(|path| ShellOption { value: v.to_string(), label: label.to_string(), path }))
            .collect()
    }
    #[cfg(not(windows))]
    {
        let mut paths: Vec<String> = std::fs::read_to_string("/etc/shells")
            .unwrap_or_default()
            .lines()
            .map(str::trim)
            .filter(|l| l.starts_with('/'))
            .map(str::to_string)
            .collect();
        paths.push(login_shell());
        for name in ["zsh", "bash", "fish", "nu", "sh"] {
            if let Some(p) = which(name) {
                paths.push(p);
            }
        }
        let mut out: Vec<ShellOption> = vec![];
        for p in paths {
            if !Path::new(&p).is_file() || Family::of(&p) == Family::Other {
                continue;
            }
            // /bin/bash 与 /usr/bin/bash 常常是同一个文件（usr 合并）：按真实路径去重
            let real = std::fs::canonicalize(&p).unwrap_or_else(|_| PathBuf::from(&p));
            if out.iter().any(|o| std::fs::canonicalize(&o.path).unwrap_or_else(|_| PathBuf::from(&o.path)) == real) {
                continue;
            }
            let label = Path::new(&p).file_name().and_then(|s| s.to_str()).unwrap_or(&p).to_string();
            out.push(ShellOption { value: p.clone(), label, path: p });
        }
        // 登录 shell 排第一，其余按名字
        let login = login_shell();
        out.sort_by_key(|o| (o.path != login, o.label.clone()));
        out
    }
}

/// 自动选择的 shell 的值与显示名（界面上显示“自动（PowerShell 7）”）。
pub fn auto() -> ShellOption {
    #[cfg(windows)]
    {
        for (v, label) in WINDOWS_BUILTINS.iter().take(3) {
            if let Some(path) = windows_builtin(v) {
                return ShellOption { value: v.to_string(), label: label.to_string(), path };
            }
        }
        ShellOption { value: "cmd".into(), label: "cmd".into(), path: "cmd.exe".into() }
    }
    #[cfg(not(windows))]
    {
        let p = login_shell();
        let label = Path::new(&p).file_name().and_then(|s| s.to_str()).unwrap_or("sh").to_string();
        ShellOption { value: p.clone(), label, path: p }
    }
}

/// 解析终端的 shell：`spec` 为空时用 `default`（设置里的默认 shell），再为空就自动选择。
pub fn resolve(spec: &str, default: &str) -> Result<Shell, LocalError> {
    let spec = if spec.trim().is_empty() { default.trim() } else { spec.trim() };
    if spec.is_empty() {
        let a = auto();
        return Ok(Shell { family: Family::of(&a.path), program: a.path, args: vec![] });
    }
    #[cfg(windows)]
    if let Some(path) = windows_builtin(spec) {
        return Ok(Shell { family: Family::of(&path), program: path, args: vec![] });
    }
    let not_found = || LocalError::new("SHELL_NOT_FOUND").with("shell", spec);
    // 整串就是一个存在的文件（Windows 上没加引号的 C:\Program Files\… 路径）
    if Path::new(spec).is_absolute() && Path::new(spec).is_file() {
        return Ok(Shell { family: Family::of(spec), program: spec.to_string(), args: vec![] });
    }
    let words = split_words(spec).ok_or_else(not_found)?;
    let (first, args) = words.split_first().ok_or_else(not_found)?;
    let program = find_program(first).ok_or_else(not_found)?;
    Ok(Shell { family: Family::of(&program), program, args: args.to_vec() })
}

/// 校验用户填写的 shell（新建/编辑终端、保存设置时）。
pub fn validate(spec: &str) -> Result<String, LocalError> {
    let spec = spec.trim();
    if spec.len() > 500 || spec.contains(['\n', '\r', '\0']) {
        return Err(LocalError::invalid("shell", "too_long"));
    }
    if !spec.is_empty() {
        resolve(spec, "")?;
    }
    Ok(spec.to_string())
}

/// 把命令行拆成词：空白分隔，支持单引号、双引号（Windows 路径里的反斜杠原样保留）。引号不配对返回 None。
fn split_words(s: &str) -> Option<Vec<String>> {
    let mut out = vec![];
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut has = false;
    for c in s.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => cur.push(c),
            (None, '"' | '\'') => {
                quote = Some(c);
                has = true;
            }
            (None, c) if c.is_whitespace() => {
                if has || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                    has = false;
                }
            }
            (None, c) => cur.push(c),
        }
    }
    if quote.is_some() {
        return None;
    }
    if has || !cur.is_empty() {
        out.push(cur);
    }
    Some(out)
}

/// 程序名或路径 → 能运行的绝对路径。
fn find_program(name: &str) -> Option<String> {
    let p = Path::new(name);
    if p.components().count() > 1 || p.is_absolute() {
        return with_exts(p).map(|p| p.display().to_string());
    }
    #[cfg(windows)]
    {
        // 装了 Git for Windows 时，填 bash 指的是 Git Bash，而不是 System32 里启动 WSL 的 bash.exe
        let lower = name.to_ascii_lowercase();
        if lower == "bash" || lower == "bash.exe" {
            let found = which(name).filter(|p| !p.to_ascii_lowercase().contains("\\system32\\"));
            return found.or_else(git_bash).or_else(|| which(name));
        }
        if let Some(b) = windows_builtin(&lower.trim_end_matches(".exe").replace("windowspowershell", "powershell")) {
            return Some(b);
        }
    }
    which(name)
}

/// 在 PATH 里找程序。
pub fn which(name: &str) -> Option<String> {
    let dirs: Vec<PathBuf> = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
    #[cfg(not(windows))]
    let dirs = {
        let mut d = dirs;
        d.extend(["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin"].map(PathBuf::from));
        d
    };
    dirs.iter().find_map(|d| with_exts(&d.join(name))).map(|p| p.display().to_string())
}

/// 文件本身，或 Windows 上补上 PATHEXT 里的扩展名（.exe、.cmd …）之后存在的文件。
fn with_exts(p: &Path) -> Option<PathBuf> {
    if p.is_file() {
        return Some(p.to_path_buf());
    }
    if cfg!(windows) && p.extension().is_none() {
        let exts = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into());
        for e in exts.split(';').filter(|e| !e.is_empty()) {
            let c = PathBuf::from(format!("{}{}", p.display(), e.to_ascii_lowercase()));
            if c.is_file() {
                return Some(c);
            }
        }
    }
    None
}

/// 登录 shell（`$SHELL`，否则账户数据库里的 shell，再否则 zsh/bash/sh）。
#[cfg(not(windows))]
pub fn login_shell() -> String {
    if let Some(s) = std::env::var("SHELL").ok().filter(|s| Path::new(s).is_file()) {
        return s;
    }
    unsafe {
        let pw = libc::getpwuid(libc::getuid());
        if !pw.is_null() && !(*pw).pw_shell.is_null() {
            if let Ok(s) = std::ffi::CStr::from_ptr((*pw).pw_shell).to_str() {
                if Path::new(s).is_file() {
                    return s.to_string();
                }
            }
        }
    }
    ["/bin/zsh", "/bin/bash", "/bin/sh"].iter().find(|p| Path::new(p).is_file()).unwrap_or(&"/bin/sh").to_string()
}

#[cfg(windows)]
pub fn login_shell() -> String {
    auto().path
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn families() {
        assert_eq!(Family::of("/bin/zsh"), Family::Posix);
        assert_eq!(Family::of("C:\\Program Files\\Git\\bin\\bash.exe"), Family::Posix);
        assert_eq!(Family::of("pwsh.exe"), Family::Pwsh);
        assert_eq!(Family::of("C:\\Windows\\System32\\cmd.exe"), Family::Cmd);
        assert_eq!(Family::of("/usr/bin/fish"), Family::Posix);
        assert_eq!(Family::of("wsl.exe"), Family::Other);
    }

    #[test]
    fn words() {
        assert_eq!(split_words("bash -l").unwrap(), ["bash", "-l"]);
        assert_eq!(split_words(r#""C:\Program Files\Git\bin\bash.exe" --login"#).unwrap(), [r"C:\Program Files\Git\bin\bash.exe", "--login"]);
        assert_eq!(split_words("wsl -d 'Ubuntu 22'").unwrap(), ["wsl", "-d", "Ubuntu 22"]);
        assert!(split_words("bash \"oops").is_none());
        assert!(split_words("   ").unwrap().is_empty());
    }

    #[test]
    fn argv_by_family() {
        let sh = |p: &str, args: &[&str]| Shell { program: p.into(), args: args.iter().map(|s| s.to_string()).collect(), family: Family::of(p) };
        assert_eq!(sh("/bin/zsh", &[]).interactive(), ["/bin/zsh", "-l"]);
        assert_eq!(sh("/bin/zsh", &["-f"]).interactive(), ["/bin/zsh", "-f"]);
        assert_eq!(sh("pwsh.exe", &[]).interactive(), ["pwsh.exe", "-NoLogo"]);
        assert_eq!(sh("cmd.exe", &[]).interactive(), ["cmd.exe"]);
        assert_eq!(sh("cmd.exe", &[]).run("npm run dev").unwrap(), ["cmd.exe", "/K", "npm run dev"]);
        assert_eq!(sh("pwsh.exe", &[]).run("codex").unwrap(), ["pwsh.exe", "-NoLogo", "-NoExit", "-Command", "codex"]);
        assert_eq!(sh("/usr/bin/nu", &[]).run("ls").unwrap(), ["/usr/bin/nu", "-l", "-e", "ls"]);
        assert!(sh("wsl.exe", &[]).run("ls").is_none());
        if !cfg!(windows) {
            assert_eq!(sh("/bin/bash", &[]).run("claude").unwrap(), ["/bin/bash", "-l", "-i", "-c", "claude; exec '/bin/bash' -l"]);
        }
    }

    #[test]
    fn env_prefix_for_windows_shells() {
        let c = "IS_SANDBOX=1 claude --dangerously-skip-permissions";
        assert_eq!(adapt_env_prefix(c, Family::Posix), c);
        assert_eq!(adapt_env_prefix(c, Family::Pwsh), "$env:IS_SANDBOX='1'; claude --dangerously-skip-permissions");
        assert_eq!(adapt_env_prefix(c, Family::Cmd), "set \"IS_SANDBOX=1\" && claude --dangerously-skip-permissions");
        assert_eq!(adapt_env_prefix("npm run dev", Family::Pwsh), "npm run dev");
        assert_eq!(adapt_env_prefix("a=b", Family::Pwsh), "a=b");
    }

    #[cfg(unix)]
    #[test]
    fn resolves_names_and_paths() {
        let s = resolve("sh -x", "").unwrap();
        assert!(s.program.ends_with("/sh") && s.args == ["-x"] && s.family == Family::Posix);
        assert_eq!(resolve("/bin/sh", "").unwrap().program, "/bin/sh");
        assert!(resolve("", "").is_ok());
        assert_eq!(resolve("", "/bin/sh").unwrap().program, "/bin/sh");
        let e = resolve("no-such-shell-xyz", "").unwrap_err();
        assert_eq!(e.code, "SHELL_NOT_FOUND");
        assert!(validate("no-such-shell-xyz").is_err());
        assert_eq!(validate("  ").unwrap(), "");
        assert!(available().iter().any(|o| o.label == "sh" || o.label == "bash" || o.label == "dash"));
    }
}
