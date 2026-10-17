//! 输入 `/` 时提示的自定义命令和技能（docs/CLAUDE_UI.md §5.3）：
//!
//! - 用户的：`~/.claude/commands/**/*.md`、`~/.claude/skills/*/SKILL.md`（设了 `CLAUDE_CONFIG_DIR` 时用那个目录）；
//! - 项目的：`<工作目录>/.claude/commands/**/*.md`、`<工作目录>/.claude/skills/*/SKILL.md`。
//!
//! 命令名是文件名（子目录只是分组），技能名是 frontmatter 里的 `name`，没有就用目录名。说明取 frontmatter 的
//! `description`，没有就取正文第一行。只是提示：漏了的命令照样可以直接输入。

use std::path::{Path, PathBuf};

use serde::Serialize;

const MAX: usize = 300;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Command {
    pub name: String,
    pub description: String,
    /// `user` | `project` | `user_skill` | `project_skill`
    pub source: &'static str,
}

pub fn config_dir() -> Option<PathBuf> {
    std::env::var_os("CLAUDE_CONFIG_DIR").map(PathBuf::from).or_else(|| dirs::home_dir().map(|h| h.join(".claude")))
}

pub fn scan(workdir: &Path) -> Vec<Command> {
    let mut out = vec![];
    let project = workdir.join(".claude");
    // 项目的排在前面：同名时项目的生效
    commands(&project.join("commands"), "project", &mut out);
    skills(&project.join("skills"), "project_skill", &mut out);
    if let Some(user) = config_dir() {
        if user != project {
            commands(&user.join("commands"), "user", &mut out);
            skills(&user.join("skills"), "user_skill", &mut out);
        }
    }
    let mut seen = std::collections::HashSet::new();
    out.retain(|c| seen.insert(c.name.clone()));
    out.truncate(MAX);
    out
}

fn commands(dir: &Path, source: &'static str, out: &mut Vec<Command>) {
    let mut files = vec![];
    walk(dir, 0, &mut files);
    files.sort();
    for f in files {
        let Some(stem) = f.file_stem().and_then(|s| s.to_str()) else { continue };
        if !valid_name(stem) {
            continue;
        }
        let text = read_head(&f);
        let (fm, body) = frontmatter(&text);
        out.push(Command { name: format!("/{stem}"), description: describe(fm.get("description"), body), source });
    }
}

fn walk(dir: &Path, depth: usize, files: &mut Vec<PathBuf>) {
    if depth > 3 || files.len() > MAX {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(&p, depth + 1, files);
        } else if p.extension().is_some_and(|x| x == "md") {
            files.push(p);
        }
    }
}

fn skills(dir: &Path, source: &'static str, out: &mut Vec<Command>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut dirs: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.join("SKILL.md").is_file()).collect();
    dirs.sort();
    for d in dirs {
        let text = read_head(&d.join("SKILL.md"));
        let (fm, body) = frontmatter(&text);
        let name = fm.get("name").cloned().or_else(|| d.file_name().and_then(|n| n.to_str()).map(String::from)).unwrap_or_default();
        if !valid_name(&name) {
            continue;
        }
        out.push(Command { name: format!("/{name}"), description: describe(fm.get("description"), body), source });
    }
}

fn valid_name(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ':' | '.'))
}

/// 只读开头一点（说明在 frontmatter 或第一行）
fn read_head(p: &Path) -> String {
    use std::io::Read;
    let mut buf = vec![0u8; 8192];
    let n = std::fs::File::open(p).and_then(|mut f| f.read(&mut buf)).unwrap_or(0);
    String::from_utf8_lossy(&buf[..n]).into_owned()
}

/// `---` 包起来的简单 `key: value`（不处理多行值，引号去掉）
fn frontmatter(text: &str) -> (std::collections::HashMap<String, String>, &str) {
    let mut map = std::collections::HashMap::new();
    let Some(rest) = text.strip_prefix("---").map(|r| r.trim_start_matches(['\r', '\n'])) else { return (map, text) };
    let Some(end) = rest.find("\n---") else { return (map, text) };
    for line in rest[..end].lines() {
        if let Some((k, v)) = line.split_once(':') {
            let v = v.trim().trim_matches(['"', '\'']).to_string();
            if !k.starts_with(' ') && !v.is_empty() {
                map.insert(k.trim().to_string(), v);
            }
        }
    }
    let body = rest[end + 4..].trim_start_matches(['-', '\r', '\n']);
    (map, body)
}

fn describe(desc: Option<&String>, body: &str) -> String {
    let s = desc.cloned().unwrap_or_else(|| body.lines().map(|l| l.trim().trim_start_matches('#').trim()).find(|l| !l.is_empty()).unwrap_or("").to_string());
    s.chars().take(120).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scans_commands_and_skills() {
        let d = tempfile::tempdir().unwrap();
        let c = d.path().join(".claude/commands/frontend");
        std::fs::create_dir_all(&c).unwrap();
        std::fs::write(c.join("component.md"), "---\ndescription: \"新建组件\"\nallowed-tools: Bash\n---\n正文").unwrap();
        std::fs::write(d.path().join(".claude/commands/deploy.md"), "# 部署到测试环境\n\n步骤……").unwrap();
        std::fs::write(d.path().join(".claude/commands/bad name.md"), "x").unwrap();
        let s = d.path().join(".claude/skills/pdf-tools");
        std::fs::create_dir_all(&s).unwrap();
        std::fs::write(s.join("SKILL.md"), "---\nname: pdf\ndescription: 处理 PDF\n---\n").unwrap();
        let mut out = vec![];
        commands(&d.path().join(".claude/commands"), "project", &mut out);
        skills(&d.path().join(".claude/skills"), "project_skill", &mut out);
        assert_eq!(
            out,
            vec![
                Command { name: "/deploy".into(), description: "部署到测试环境".into(), source: "project" },
                Command { name: "/component".into(), description: "新建组件".into(), source: "project" },
                Command { name: "/pdf".into(), description: "处理 PDF".into(), source: "project_skill" },
            ]
        );
    }
}
