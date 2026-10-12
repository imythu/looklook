//! 管理台“文件”页的简单文件管理：新建文件夹、重命名、删除（浏览、预览、下载、上传见 gateway/api.rs 与 transfer.rs）。
//!
//! 能打开管理台的人本来就有这台电脑上的完整 shell，这里不另加路径限制；只防误操作：
//! 新名字只能是一段（不含分隔符、不是 `.` / `..`），不覆盖已有的文件，不删根目录、盘符和用户主目录。

use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use crate::error::LocalError;

type R<T> = Result<T, LocalError>;

fn absolute(path: &str) -> R<PathBuf> {
    let p = PathBuf::from(path.trim());
    if path.trim().is_empty() || !p.is_absolute() {
        return Err(LocalError::invalid("path", "absolute"));
    }
    Ok(p)
}

/// 新名字：一段、不含控制字符、Windows 也能用。
fn valid_name(name: &str) -> R<&str> {
    let n = name.trim();
    let bad = n.is_empty()
        || n == "."
        || n == ".."
        || n.len() > 255
        || n.chars().any(|c| c.is_control() || matches!(c, '/' | '\\'))
        || (cfg!(windows) && (n.chars().any(|c| matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*')) || n.ends_with(['.', ' '])));
    if bad {
        return Err(LocalError::invalid("name", "file_name"));
    }
    Ok(n)
}

fn io_err(e: std::io::Error, path: &Path) -> LocalError {
    let p = path.display().to_string();
    match e.kind() {
        ErrorKind::NotFound => LocalError::not_found().with("path", p),
        ErrorKind::PermissionDenied => LocalError::new("FORBIDDEN").with("path", p),
        ErrorKind::AlreadyExists => LocalError::new("FILE_EXISTS").with("path", p),
        _ => LocalError::from(e),
    }
}

/// 在 `dir` 里新建文件夹 `name`，返回完整路径。
pub fn mkdir(dir: &str, name: &str) -> R<String> {
    let target = absolute(dir)?.join(valid_name(name)?);
    std::fs::create_dir(&target).map_err(|e| io_err(e, &target))?;
    Ok(target.display().to_string())
}

/// 把 `path` 改名为同一文件夹里的 `name`（不覆盖已有的），返回新路径。
pub fn rename(path: &str, name: &str) -> R<String> {
    let from = absolute(path)?;
    let parent = from.parent().ok_or_else(|| LocalError::invalid("path", "not_root"))?;
    let to = parent.join(valid_name(name)?);
    if to == from {
        return Ok(to.display().to_string());
    }
    std::fs::symlink_metadata(&from).map_err(|e| io_err(e, &from))?;
    // 只差大小写的改名（不区分大小写的文件系统上 `to` “已存在”的就是它自己）照常进行
    let same_file = to.to_string_lossy().eq_ignore_ascii_case(&from.to_string_lossy());
    if !same_file && std::fs::symlink_metadata(&to).is_ok() {
        return Err(LocalError::new("FILE_EXISTS").with("path", to.display().to_string()));
    }
    std::fs::rename(&from, &to).map_err(|e| io_err(e, &from))?;
    Ok(to.display().to_string())
}

/// 不允许删除的位置：根目录、盘符、用户主目录。
fn protected(p: &Path) -> bool {
    p.parent().is_none() || dirs::home_dir().is_some_and(|h| h == p)
}

/// 删除文件，或连同内容删除文件夹（符号链接只删链接本身）。
pub fn remove(path: &str) -> R<()> {
    let p = absolute(path)?;
    if protected(&p) {
        return Err(LocalError::new("FORBIDDEN").with("path", p.display().to_string()));
    }
    let m = std::fs::symlink_metadata(&p).map_err(|e| io_err(e, &p))?;
    let r = if m.is_dir() { std::fs::remove_dir_all(&p) } else { std::fs::remove_file(&p) };
    r.map_err(|e| io_err(e, &p))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mkdir_rename_remove() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().display().to_string();
        let d = mkdir(&root, "新建文件夹").unwrap();
        assert!(Path::new(&d).is_dir());
        assert_eq!(mkdir(&root, "新建文件夹").unwrap_err().code, "FILE_EXISTS");
        assert_eq!(mkdir(&root, "../x").unwrap_err().code, "VALIDATION_FAILED");
        assert_eq!(mkdir(&root, "..").unwrap_err().code, "VALIDATION_FAILED");
        assert_eq!(mkdir("relative", "x").unwrap_err().code, "VALIDATION_FAILED");

        std::fs::write(Path::new(&d).join("a.md"), "# hi").unwrap();
        std::fs::write(Path::new(&d).join("b.md"), "").unwrap();
        let a = Path::new(&d).join("a.md").display().to_string();
        assert_eq!(rename(&a, "b.md").unwrap_err().code, "FILE_EXISTS", "不覆盖");
        assert_eq!(rename(&a, "sub/c.md").unwrap_err().code, "VALIDATION_FAILED");
        let c = rename(&a, "c.md").unwrap();
        assert_eq!(std::fs::read_to_string(&c).unwrap(), "# hi");
        assert_eq!(rename(&a, "d.md").unwrap_err().code, "NOT_FOUND");

        remove(&c).unwrap();
        assert!(!Path::new(&c).exists());
        remove(&d).unwrap();
        assert!(!Path::new(&d).exists(), "文件夹连同内容删除");
        assert_eq!(remove(&d).unwrap_err().code, "NOT_FOUND");
        assert_eq!(remove("/").unwrap_err().code, "FORBIDDEN");
        if let Some(h) = dirs::home_dir() {
            assert_eq!(remove(&h.display().to_string()).unwrap_err().code, "FORBIDDEN");
        }
    }
}
