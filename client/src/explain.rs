//! 把 AI 运行的命令、调用的工具变成“它做了什么、风险多大”，给对话看法的步骤列表和确认卡片用。
//!
//! 纯规则，不调用 AI：按命令的第一个词（和少数子命令）分类。认不出的就是 `run` + 风险未知，界面上
//! 老实写“运行了一个程序”，并给出原命令——不编造它是干什么的。文字由网页按 `action` 翻译。
//!
//! Codex 0.159 起工具调用是一段 JS（`tools.exec_command({cmd:"…"})`、`tools.apply_patch(…)`），
//! 一次调用里可能有好几条命令；旧版本是 `shell` / `exec_command` 函数调用，参数是 JSON。

use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Risk {
    Low,
    Unknown,
    Medium,
    High,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Step {
    /// `read` 查看文件、`search` 搜索、`git_read` 看版本记录、`check` 运行检查或构建、`edit` 改文件、
    /// `write` 新建 / 复制 / 移动文件、`delete` 删除、`install` 安装软件、`network` 联网下载、
    /// `upload` 上传到远程、`system` 改系统、`web` 上网查资料、`image` 看图片、`input` 往运行中的程序输入、
    /// `run` 运行程序（认不出）、`tool` 其他工具
    pub action: &'static str,
    pub risk: Risk,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<String>,
    /// 原始工具名（`exec`、`apply_patch`…）
    pub tool: String,
}

/// 一条 shell 命令（可能用 `;` `&&` `|` 连着好几段）：取风险最高的一段。
pub fn command(cmd: &str) -> (&'static str, Risk) {
    let mut best = ("run", Risk::Unknown);
    let mut first = true;
    for seg in segments(cmd) {
        let c = classify(&seg);
        if first || c.1 > best.1 {
            best = c;
        }
        first = false;
    }
    best
}

/// 按 `;`、`&&`、`||`、`|`、换行切开（引号里的不算）。
fn segments(cmd: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut chars = cmd.chars().peekable();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), _) if c == q => {
                quote = None;
                cur.push(c);
            }
            (Some(_), _) => cur.push(c),
            (None, '"' | '\'') => {
                quote = Some(c);
                cur.push(c);
            }
            (None, ';' | '\n' | '|') => {
                if c == '|' && chars.peek() == Some(&'|') {
                    chars.next();
                }
                out.push(std::mem::take(&mut cur));
            }
            (None, '&') if chars.peek() == Some(&'&') => {
                chars.next();
                out.push(std::mem::take(&mut cur));
            }
            _ => cur.push(c),
        }
    }
    out.push(cur);
    out.into_iter().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()
}

/// 引号外有没有输出重定向（`>`、`>>`，不算 `2>&1` 和 `> $null` / `>/dev/null`）。
fn redirects(seg: &str) -> bool {
    let mut quote: Option<char> = None;
    let b: Vec<char> = seg.chars().collect();
    for (i, &c) in b.iter().enumerate() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None if c == '"' || c == '\'' => quote = Some(c),
            None if c == '>' => {
                let rest: String = b[i + 1..].iter().collect::<String>().trim_start_matches('>').trim_start().to_ascii_lowercase();
                let prev = if i > 0 { b[i - 1] } else { ' ' };
                if prev.is_ascii_digit() || rest.starts_with('&') || rest.starts_with("$null") || rest.starts_with("/dev/null") || rest.starts_with("nul") {
                    continue;
                }
                return true;
            }
            None => {}
        }
    }
    false
}

fn words(seg: &str) -> Vec<String> {
    seg.split_whitespace().map(|w| w.trim_matches(|c| c == '"' || c == '\'').to_string()).collect()
}

/// 第一个词：去掉 PowerShell 的 `&`、路径和 `.exe`，小写。
fn program(w: &[String]) -> (String, usize) {
    let mut i = 0;
    while i < w.len() && (w[i] == "&" || w[i] == "sudo" || w[i].contains('=') && !w[i].starts_with('-')) {
        i += 1;
    }
    let Some(p) = w.get(i) else { return (String::new(), i) };
    let base = p.rsplit(['/', '\\']).next().unwrap_or(p).to_ascii_lowercase();
    let base = base.strip_suffix(".exe").or_else(|| base.strip_suffix(".cmd")).unwrap_or(&base).to_string();
    (base, i)
}

fn classify(seg: &str) -> (&'static str, Risk) {
    let w = words(seg);
    let (p, at) = program(&w);
    let sub = w.get(at + 1).map(|s| s.to_ascii_lowercase()).unwrap_or_default();
    let has = |flag: &str| w.iter().any(|x| x.eq_ignore_ascii_case(flag));
    if w.first().is_some_and(|f| f.eq_ignore_ascii_case("sudo")) {
        return ("system", Risk::High);
    }
    let c = match p.as_str() {
        "git" => match sub.as_str() {
            "status" | "log" | "diff" | "show" | "branch" | "rev-parse" | "ls-files" | "blame" | "remote" | "config" | "describe" | "shortlog" => ("git_read", Risk::Low),
            "push" => ("upload", Risk::High),
            "clean" => ("delete", Risk::High),
            "reset" if has("--hard") => ("delete", Risk::High),
            "checkout" | "restore" if has("--") || has(".") => ("delete", Risk::High),
            "pull" | "fetch" | "clone" => ("network", Risk::Medium),
            "add" | "commit" | "stash" | "switch" | "checkout" | "merge" | "rebase" | "tag" | "mv" | "rm" | "restore" | "reset" | "apply" | "cherry-pick" => ("write", Risk::Medium),
            _ => ("run", Risk::Unknown),
        },
        "cat" | "type" | "get-content" | "gc" | "head" | "tail" | "less" | "more" | "wc" | "stat" | "file" | "get-item" | "gi" | "test-path" | "resolve-path"
        | "get-itemproperty" | "format-hex" | "nl" | "jq" => ("read", Risk::Low),
        "ls" | "dir" | "get-childitem" | "gci" | "tree" | "pwd" | "get-location" | "du" | "find" | "fd" | "where" | "which" | "get-command" | "echo" | "write-output"
        | "write-host" | "select-object" | "sort-object" | "measure-object" | "format-table" | "format-list" | "convertfrom-json" | "convertto-json" | "foreach-object"
        | "where-object" | "out-string" | "sort" | "uniq" | "cut" | "awk" | "printf" | "date" | "get-date" | "whoami" | "hostname" => ("read", Risk::Low),
        "rg" | "grep" | "findstr" | "select-string" | "sls" | "ag" | "ack" => ("search", Risk::Low),
        "sed" if has("-i") => ("edit", Risk::Medium),
        "sed" => ("read", Risk::Low),
        "rm" | "del" | "erase" | "rmdir" | "rd" | "remove-item" | "ri" | "unlink" | "shred" => ("delete", Risk::High),
        "new-item" | "ni" | "mkdir" | "md" | "touch" | "set-content" | "sc" | "out-file" | "add-content" | "ac" | "cp" | "copy" | "copy-item" | "cpi" | "mv" | "move"
        | "move-item" | "mi" | "rename-item" | "ren" | "rni" | "tee" | "tee-object" | "expand-archive" | "compress-archive" | "tar" | "unzip" | "zip" | "7z" => ("write", Risk::Medium),
        "npm" | "pnpm" | "yarn" | "bun" => match sub.as_str() {
            "test" | "t" | "run" | "exec" | "x" | "ls" | "list" | "outdated" | "view" | "audit" if !(sub == "audit" && has("fix")) => ("check", Risk::Low),
            "install" | "i" | "add" | "ci" | "update" | "upgrade" | "remove" | "uninstall" | "rm" | "link" | "audit" => ("install", Risk::Medium),
            "publish" => ("upload", Risk::High),
            _ => ("run", Risk::Unknown),
        },
        "npx" | "pnpx" => ("run", Risk::Medium),
        "cargo" => match sub.as_str() {
            "test" | "check" | "build" | "clippy" | "fmt" | "doc" | "tree" | "metadata" | "bench" | "run" => ("check", Risk::Low),
            "install" | "add" | "update" | "remove" => ("install", Risk::Medium),
            "publish" => ("upload", Risk::High),
            _ => ("run", Risk::Unknown),
        },
        "go" => match sub.as_str() {
            "test" | "build" | "vet" | "run" | "list" | "fmt" => ("check", Risk::Low),
            "get" | "install" | "mod" => ("install", Risk::Medium),
            _ => ("run", Risk::Unknown),
        },
        "pytest" | "tsc" | "eslint" | "prettier" | "ruff" | "mypy" | "jest" | "vitest" | "make" | "cmake" | "msbuild" | "dotnet" | "gradle" | "mvn" => ("check", Risk::Low),
        "pip" | "pip3" | "uv" | "poetry" | "conda" | "winget" | "choco" | "scoop" | "brew" | "apt" | "apt-get" | "yum" | "dnf" | "pacman" | "install-module" | "gem" => {
            if matches!(sub.as_str(), "list" | "show" | "freeze" | "search" | "info") {
                ("read", Risk::Low)
            } else {
                ("install", Risk::Medium)
            }
        }
        "curl" | "wget" | "invoke-webrequest" | "iwr" | "invoke-restmethod" | "irm" | "start-bitstransfer" | "scp" | "rsync" | "ssh" | "ftp" => ("network", Risk::Medium),
        "reg" | "regedit" | "set-executionpolicy" | "runas" | "shutdown" | "restart-computer" | "stop-computer" | "format" | "diskpart" | "bcdedit" | "net" | "netsh"
        | "sc.exe" | "taskkill" | "stop-process" | "kill" | "killall" | "pkill" | "chmod" | "chown" | "icacls" | "takeown" | "set-itemproperty" | "new-service"
        | "remove-service" | "schtasks" | "crontab" | "systemctl" | "launchctl" => ("system", Risk::High),
        "python" | "python3" | "py" | "node" | "deno" | "powershell" | "pwsh" | "bash" | "sh" | "cmd" | "ruby" | "perl" | "php" | "java" => ("run", Risk::Medium),
        "" => ("run", Risk::Unknown),
        _ => ("run", Risk::Unknown),
    };
    // 只读的命令把结果写进文件：算新建文件
    if redirects(seg) && c.1 < Risk::Medium {
        return ("write", Risk::Medium);
    }
    c
}

/// apply_patch 的补丁里改了哪些文件。删除文件算 `delete`。
fn patch_steps(patch: &str, tool: &str) -> Vec<Step> {
    let mut edit = Vec::new();
    let mut gone = Vec::new();
    for line in patch.lines() {
        let l = line.trim();
        if let Some(p) = l.strip_prefix("*** Update File: ").or_else(|| l.strip_prefix("*** Add File: ")) {
            edit.push(p.trim().to_string());
        } else if let Some(p) = l.strip_prefix("*** Delete File: ") {
            gone.push(p.trim().to_string());
        } else if let Some(p) = l.strip_prefix("*** Move to: ") {
            edit.push(p.trim().to_string());
        }
    }
    let mut out = Vec::new();
    if !edit.is_empty() || gone.is_empty() {
        out.push(Step { action: "edit", risk: Risk::Medium, command: None, files: edit, tool: tool.to_string() });
    }
    if !gone.is_empty() {
        out.push(Step { action: "delete", risk: Risk::High, command: None, files: gone, tool: tool.to_string() });
    }
    out
}

fn shell_step(cmd: &str, tool: &str) -> Step {
    let (action, risk) = command(cmd);
    Step { action, risk, command: Some(cmd.trim().to_string()), files: Vec::new(), tool: tool.to_string() }
}

/// JS 字符串字面量（`"…"`、`'…'`、`` `…` ``）从 `s` 开头读出来，返回内容。
fn js_string(s: &str) -> Option<String> {
    let mut it = s.chars();
    let q = it.next().filter(|c| matches!(c, '"' | '\'' | '`'))?;
    let mut out = String::new();
    while let Some(c) = it.next() {
        match c {
            '\\' => match it.next()? {
                'n' => out.push('\n'),
                't' => out.push('\t'),
                'r' => {}
                'u' => {
                    let hex: String = it.by_ref().take(4).collect();
                    out.push(u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32).unwrap_or('\u{fffd}'));
                }
                other => out.push(other),
            },
            c if c == q => return Some(out),
            c => out.push(c),
        }
    }
    None
}

/// 从 Codex 0.159 起的 JS 工具调用里找出每个 `tools.xxx(` 调用。
fn code_mode_steps(js: &str) -> Vec<Step> {
    let mut out = Vec::new();
    let mut rest = js;
    while let Some(i) = rest.find("tools.") {
        let after = &rest[i + 6..];
        let name: String = after.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
        let args = &after[name.len()..];
        rest = args;
        if !args.starts_with('(') {
            continue;
        }
        // 这次调用的参数大致范围：到下一个 tools. 为止
        let end = args[1..].find("tools.").map_or(args.len(), |j| j + 1);
        let body = &args[..end];
        let step = match name.as_str() {
            "exec_command" | "shell" | "shell_command" => {
                let cmd = ["cmd:", "\"cmd\":", "command:", "\"command\":"].iter().find_map(|k| body.find(k).and_then(|j| js_string(body[j + k.len()..].trim_start())));
                match cmd {
                    Some(c) => shell_step(&c, &name),
                    None => Step { action: "run", risk: Risk::Unknown, command: None, files: Vec::new(), tool: name.clone() },
                }
            }
            "apply_patch" => {
                out.extend(patch_steps(&js_string(body[1..].trim_start()).unwrap_or_else(|| body.to_string()), &name));
                continue;
            }
            "write_stdin" => Step { action: "input", risk: Risk::Low, command: None, files: Vec::new(), tool: name.clone() },
            n if n.starts_with("web") => Step { action: "web", risk: Risk::Low, command: None, files: Vec::new(), tool: name.clone() },
            "view_image" => Step { action: "image", risk: Risk::Low, command: None, files: Vec::new(), tool: name.clone() },
            // 只是读写本地 JS 变量的辅助函数
            "set" | "append" | "get" | "description" | "call" => continue,
            _ => Step { action: "tool", risk: Risk::Unknown, command: None, files: Vec::new(), tool: name.clone() },
        };
        out.push(step);
    }
    out
}

/// 一次工具调用 → 若干步。`arguments` 是 rollout 里记录的原文（JSON 参数或自定义工具的输入）。
pub fn tool_call(name: &str, arguments: &str) -> Vec<Step> {
    match name {
        "exec" | "js" | "code" if arguments.contains("tools.") => {
            let v = code_mode_steps(arguments);
            if v.is_empty() {
                vec![Step { action: "tool", risk: Risk::Unknown, command: None, files: Vec::new(), tool: name.to_string() }]
            } else {
                v
            }
        }
        "apply_patch" => {
            let patch = serde_json::from_str::<Value>(arguments).ok().and_then(|v| v["input"].as_str().map(str::to_string)).unwrap_or_else(|| arguments.to_string());
            patch_steps(&patch, name)
        }
        "shell" | "shell_command" | "exec_command" | "local_shell" | "container.exec" => {
            let v: Value = serde_json::from_str(arguments).unwrap_or(Value::Null);
            let cmd = match &v["command"] {
                Value::Array(a) => {
                    let parts: Vec<&str> = a.iter().filter_map(Value::as_str).collect();
                    // ["bash", "-lc", "…"] / ["powershell", "-Command", "…"]：取真正的命令
                    match parts.as_slice() {
                        [_, flag, c] if flag.starts_with('-') => c.to_string(),
                        _ => parts.join(" "),
                    }
                }
                Value::String(s) => s.clone(),
                _ => v["cmd"].as_str().unwrap_or_default().to_string(),
            };
            if cmd.is_empty() {
                vec![Step { action: "run", risk: Risk::Unknown, command: None, files: Vec::new(), tool: name.to_string() }]
            } else {
                vec![shell_step(&cmd, name)]
            }
        }
        "view_image" => vec![Step { action: "image", risk: Risk::Low, command: None, files: Vec::new(), tool: name.to_string() }],
        // Claude Code 的工具
        "Bash" | "BashOutput" | "PowerShell" => {
            let v: Value = serde_json::from_str(arguments).unwrap_or(Value::Null);
            match v["command"].as_str() {
                Some(c) => vec![shell_step(c, name)],
                None => vec![Step { action: "run", risk: Risk::Unknown, command: None, files: Vec::new(), tool: name.to_string() }],
            }
        }
        "Read" | "NotebookRead" | "Glob" | "Grep" | "LS" | "Edit" | "MultiEdit" | "Write" | "NotebookEdit" => {
            let v: Value = serde_json::from_str(arguments).unwrap_or(Value::Null);
            let file = v["file_path"].as_str().or_else(|| v["notebook_path"].as_str()).or_else(|| v["path"].as_str());
            let files = file.map(|f| vec![f.to_string()]).unwrap_or_default();
            let (action, risk) = match name {
                "Read" | "NotebookRead" | "LS" => ("read", Risk::Low),
                "Glob" | "Grep" => ("search", Risk::Low),
                "Write" => ("write", Risk::Medium),
                _ => ("edit", Risk::Medium),
            };
            vec![Step { action, risk, command: v["pattern"].as_str().map(str::to_string), files, tool: name.to_string() }]
        }
        "WebFetch" | "WebSearch" => vec![Step { action: "web", risk: Risk::Low, command: None, files: Vec::new(), tool: name.to_string() }],
        n if n.starts_with("web") => vec![Step { action: "web", risk: Risk::Low, command: None, files: Vec::new(), tool: name.to_string() }],
        _ => vec![Step { action: "tool", risk: Risk::Unknown, command: None, files: Vec::new(), tool: name.to_string() }],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_commands() {
        assert_eq!(command("Get-Content a.txt -Encoding utf8"), ("read", Risk::Low));
        assert_eq!(command("rg --files 静默突触_MV"), ("search", Risk::Low));
        assert_eq!(command("git status --short"), ("git_read", Risk::Low));
        assert_eq!(command("npm test -- --watch=false"), ("check", Risk::Low));
        assert_eq!(command("npm install lodash"), ("install", Risk::Medium));
        assert_eq!(command("New-Item -ItemType File b.txt"), ("write", Risk::Medium));
        assert_eq!(command("Get-ChildItem; Remove-Item -Recurse x"), ("delete", Risk::High));
        assert_eq!(command("git push origin main"), ("upload", Risk::High));
        assert_eq!(command("& 'C:/x/python.exe' -c \"print(1)\""), ("run", Risk::Medium));
        assert_eq!(command("frobnicate --all"), ("run", Risk::Unknown));
        assert_eq!(command("ls > list.txt"), ("write", Risk::Medium));
        assert_eq!(command("cargo test 2>&1"), ("check", Risk::Low));
        assert_eq!(command("echo 'a; rm -rf /'"), ("read", Risk::Low));
    }

    #[test]
    fn code_mode_exec() {
        let js = "text(await tools.exec_command({cmd:\"New-Item -ItemType File b.txt\",max_output_tokens:1000}));\n";
        let s = tool_call("exec", js);
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].action, "write");
        assert_eq!(s[0].command.as_deref(), Some("New-Item -ItemType File b.txt"));
        let js = "const a = await tools.exec_command({cmd:\"Get-Content \\\"a b.txt\\\"\"}); tools.set('x', a); await tools.web__run({q:'x'});";
        let s = tool_call("exec", js);
        assert_eq!(s.iter().map(|s| s.action).collect::<Vec<_>>(), ["read", "web"]);
        assert_eq!(s[0].command.as_deref(), Some("Get-Content \"a b.txt\""));
    }

    #[test]
    fn patches() {
        let js = "await tools.apply_patch(\"*** Begin Patch\\n*** Update File: src/a.rs\\n@@\\n-x\\n+y\\n*** Delete File: old.txt\\n*** End Patch\")";
        let s = tool_call("exec", js);
        assert_eq!(s.len(), 2);
        assert_eq!((s[0].action, s[0].files.clone()), ("edit", vec!["src/a.rs".to_string()]));
        assert_eq!((s[1].action, s[1].risk), ("delete", Risk::High));
        let s = tool_call("shell", r#"{"command":["bash","-lc","cat README.md"]}"#);
        assert_eq!((s[0].action, s[0].command.as_deref()), ("read", Some("cat README.md")));
    }
}
