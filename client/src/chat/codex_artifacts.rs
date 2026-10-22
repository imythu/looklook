//! Codex 对话“产出的文件”：Codex 新建、改过的文件，以及命令输出、回复里提到的、确实存在的本机文件。
//! 资源栏列出来，回复里的路径变成链接；`chat/artifact?path=` 只给这段对话里出现过的文件。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Value};

/// 列表多久重新算一次（取文件时核对用）
pub const TTL: Duration = Duration::from_secs(60);
/// 一段对话最多列这么多个
const MAX: usize = 300;

fn stop(c: char) -> bool {
    c.is_whitespace() || "\"'`<>|*?()[]{},，。；：、（）【】「」《》！？".contains(c)
}

/// 文字里像本机文件路径的片段：`C:\a\b.pdf`、`D:/x/y.png`、`/home/a/b.txt`、`~/x.md`。
/// 路径到空白、引号、括号、反引号、中文标点为止，末尾的句号冒号去掉；最后一段要有扩展名（不认文件夹）。
pub fn path_mentions(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = vec![];
    let mut i = 0;
    while i < chars.len() {
        let prev_ok = i == 0 || !(chars[i - 1].is_alphanumeric() || matches!(chars[i - 1], '/' | '\\' | '.' | ':' | '_' | '-'));
        let drive = prev_ok && i + 2 < chars.len() && chars[i].is_ascii_alphabetic() && chars[i + 1] == ':' && matches!(chars[i + 2], '\\' | '/');
        let unix = prev_ok
            && (chars[i] == '/' || (chars[i] == '~' && chars.get(i + 1) == Some(&'/')))
            && chars.get(i + 1).is_some_and(|c| c.is_alphanumeric() || matches!(c, '/' | '.' | '_'));
        if !(drive || unix) {
            i += 1;
            continue;
        }
        let mut j = i;
        while j < chars.len() && !stop(chars[j]) {
            j += 1;
        }
        let mut s: String = chars[i..j].iter().collect();
        while s.ends_with(['.', ':', ';', '!']) {
            s.pop();
        }
        let last = s.rsplit(['/', '\\']).next().unwrap_or("");
        let ext_ok = last.rsplit_once('.').is_some_and(|(stem, ext)| !stem.is_empty() && (1..=8).contains(&ext.len()) && ext.chars().all(|c| c.is_ascii_alphanumeric()));
        if ext_ok && s.len() <= 400 && !s.contains("://") {
            out.push(s);
        }
        i = j.max(i + 1);
    }
    out
}

/// 一个值里所有的字符串
pub fn strings(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::String(s) => out.push(s.clone()),
        Value::Array(a) => a.iter().for_each(|x| strings(x, out)),
        Value::Object(o) => o.values().for_each(|x| strings(x, out)),
        _ => {}
    }
}

/// 统一路径的写法（Windows 上斜杠换成反斜杠、盘符大写），同一个文件不同写法只列一次；不访问磁盘
pub fn normalize(p: &Path) -> PathBuf {
    if !cfg!(windows) {
        return p.to_path_buf();
    }
    let mut s = p.display().to_string().replace('/', "\\");
    if s.as_bytes().get(1) == Some(&b':') {
        s = s[..1].to_ascii_uppercase() + &s[1..];
    }
    PathBuf::from(s)
}

/// 程序、库、字体、快捷方式：不是对话的产出（命令里常带着 node.exe、系统字体这些）
const NOT_OUTPUT: &[&str] = &["exe", "dll", "sys", "so", "dylib", "ttf", "ttc", "otf", "lnk", "msi", "pdb", "lib"];

/// 系统和程序的目录：这些下面的文件不算产出
pub fn system_dirs() -> Vec<PathBuf> {
    let mut v = vec![];
    for k in ["SystemRoot", "ProgramFiles", "ProgramFiles(x86)", "ProgramData"] {
        if let Some(d) = std::env::var_os(k).filter(|d| !d.is_empty()) {
            v.push(PathBuf::from(d));
        }
    }
    if !cfg!(windows) {
        v.extend(["/usr", "/bin", "/sbin", "/lib", "/etc", "/System", "/Library", "/opt/homebrew"].map(PathBuf::from));
    }
    v
}

/// 从对话里的文字（最近的在前）找出确实存在的文件：(文件, 文字里出现过的写法)。
/// `skip` 下面的不算（Codex 自己的目录：技能说明、生成的图片另外列；系统和程序的目录）。
pub fn find(texts: &[String], workdir: &Path, skip: &[PathBuf]) -> Vec<(PathBuf, Vec<String>)> {
    let mut found: Vec<(PathBuf, Vec<String>)> = vec![];
    let mut index: HashMap<PathBuf, usize> = HashMap::new();
    let mut checked: HashMap<String, Option<PathBuf>> = HashMap::new();
    let skip: Vec<String> = skip.iter().map(|d| normalize(d).display().to_string().to_lowercase()).collect();
    for text in texts {
        let mut cands = path_mentions(text);
        // 工具参数里的 file_path 是整段路径，可能带空格
        let whole = Path::new(text);
        if !text.contains('\n') && text.len() <= 400 && whole.is_absolute() && whole.extension().is_some() {
            cands.push(text.clone());
        }
        for raw in cands {
            let hit = checked
                .entry(raw.clone())
                .or_insert_with(|| {
                    let p = match raw.strip_prefix("~/") {
                        Some(rest) => dirs::home_dir()?.join(rest),
                        None => PathBuf::from(&raw),
                    };
                    let p = if p.is_absolute() { p } else { workdir.join(p) };
                    let ext = p.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).unwrap_or_default();
                    if NOT_OUTPUT.contains(&ext.as_str()) || !std::fs::metadata(&p).is_ok_and(|m| m.is_file()) {
                        return None;
                    }
                    let p = normalize(&p);
                    // Windows 上路径不分大小写
                    let low = p.display().to_string().to_lowercase();
                    (!skip.iter().any(|d| low.starts_with(d.as_str()))).then_some(p)
                })
                .clone();
            let Some(p) = hit else { continue };
            match index.get(&p) {
                Some(&n) => {
                    if !found[n].1.contains(&raw) {
                        found[n].1.push(raw);
                    }
                }
                None if found.len() < MAX => {
                    index.insert(p.clone(), found.len());
                    found.push((p, vec![raw]));
                }
                None => {}
            }
        }
    }
    found
}

/// 打开时怎么返回：(Content-Type, 能不能在浏览器里直接看)。网页、SVG、代码一类按纯文本给看源码；其余只能下载
pub fn content_type(path: &Path) -> (&'static str, bool) {
    let ext = path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).unwrap_or_default();
    match ext.as_str() {
        "png" => ("image/png", true),
        "jpg" | "jpeg" => ("image/jpeg", true),
        "gif" => ("image/gif", true),
        "webp" => ("image/webp", true),
        "bmp" => ("image/bmp", true),
        "pdf" => ("application/pdf", true),
        "mp4" | "m4v" => ("video/mp4", true),
        "webm" => ("video/webm", true),
        "mov" => ("video/quicktime", true),
        "mp3" => ("audio/mpeg", true),
        "wav" => ("audio/wav", true),
        "ogg" => ("audio/ogg", true),
        "m4a" => ("audio/mp4", true),
        "txt" | "md" | "markdown" | "json" | "jsonl" | "csv" | "tsv" | "log" | "yaml" | "yml" | "toml" | "ini" | "xml" | "html" | "htm" | "svg" | "css" | "js" | "mjs" | "ts"
        | "tsx" | "jsx" | "py" | "rs" | "go" | "java" | "c" | "h" | "cpp" | "hpp" | "cs" | "sh" | "ps1" | "bat" | "sql" | "srt" | "vtt" => ("text/plain; charset=utf-8", true),
        _ => ("application/octet-stream", false),
    }
}

pub fn kind(path: &Path) -> &'static str {
    match content_type(path).0 {
        "application/pdf" => "pdf",
        m if m.starts_with("image/") => "image",
        m if m.starts_with("video/") => "video",
        m if m.starts_with("audio/") => "audio",
        m if m.starts_with("text/") => "text",
        _ => "file",
    }
}

/// 列表里的一项
pub fn view(p: &Path, mentions: &[String]) -> Value {
    let m = std::fs::metadata(p).ok();
    json!({
        "path": p.display().to_string(),
        "name": p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
        "kind": kind(p),
        "viewable": content_type(p).1,
        "size": m.as_ref().map(|m| m.len()),
        "mtime_ms": m.and_then(|m| m.modified().ok()).and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_millis() as u64),
        "mentions": mentions,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mentions_in_text() {
        let t = "已保存到 `F:\\out\\报告.pdf`，图在 D:/x/y.png。另见 /home/a/b.txt: 和 ~/notes/x.md；网址 https://a.com/b.png 不算，文件夹 C:\\Users\\a 不算";
        assert_eq!(path_mentions(t), ["F:\\out\\报告.pdf", "D:/x/y.png", "/home/a/b.txt", "~/notes/x.md"]);
        assert!(path_mentions("1/2.5 与 a/b.c").is_empty(), "相对路径、分数不算");
    }

    #[test]
    fn finds_existing_files_once() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("out.pdf");
        std::fs::write(&f, b"%PDF").unwrap();
        let skip = dir.path().join("gen");
        std::fs::create_dir(&skip).unwrap();
        std::fs::write(skip.join("a.png"), b"x").unwrap();
        let fs = f.display().to_string();
        let texts = vec![format!("写好了 {fs}。"), fs.clone(), format!("{}", skip.join("a.png").display()), "C:\\nope\\x.pdf".into()];
        std::fs::write(dir.path().join("tool.exe"), b"x").unwrap();
        let mut texts = texts;
        texts.push(dir.path().join("tool.exe").display().to_string());
        let r = find(&texts, dir.path(), &[skip]);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].0, normalize(&f));
        assert_eq!(kind(&r[0].0), "pdf");
    }

    #[test]
    fn types() {
        assert_eq!(content_type(Path::new("a.html")), ("text/plain; charset=utf-8", true), "网页只给看源码");
        assert_eq!(content_type(Path::new("a.zip")).1, false);
        assert_eq!(kind(Path::new("a.mp4")), "video");
    }
}
