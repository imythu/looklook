//! 对话来源登记表：每种工具自动找到的默认数据目录，加上用户自己登记的目录（同一种工具可以有多个，
//! 例如个人和工作两个 Codex）。登记的存在本地库 kv `convo_sources` 里；默认目录不存，每次现算。
//!
//! 还不支持读写的工具（Claude Code、OpenCode）也能登记：等转换器做好后自动可用，不用重新登记。

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::{Harness, Source};
use crate::store::Store;

const KEY: &str = "convo_sources";
const MAX: usize = 32;
const NAME_MAX: usize = 40;

/// 列表里的一项：来源本身，加上它在不在、能做什么。
#[derive(Debug, Clone, Serialize)]
pub struct SourceView {
    #[serde(flatten)]
    pub src: Source,
    /// 用户登记的（可以删）；否则是自动找到的默认目录
    pub registered: bool,
    /// 数据目录存在
    pub exists: bool,
    pub can_export: bool,
    pub can_import: bool,
}

impl SourceView {
    fn of(src: Source, registered: bool) -> Self {
        SourceView { exists: src.home.is_dir(), can_export: src.kind.can_export(), can_import: src.kind.can_import(), registered, src }
    }
}

/// 登记一个来源时填的东西。
#[derive(Debug, Clone, Deserialize)]
pub struct NewSource {
    pub kind: Harness,
    #[serde(default)]
    pub name: String,
    pub home: String,
    #[serde(default)]
    pub bin: Option<String>,
}

pub fn registered(store: &Store) -> Vec<Source> {
    store.get::<Vec<Source>>(KEY).ok().flatten().unwrap_or_default()
}

/// 全部来源：先是每种工具的默认目录（按 [`Harness::ALL`] 的顺序），再是登记的（按登记顺序）。
pub fn all(store: &Store) -> Vec<SourceView> {
    let mut out: Vec<SourceView> = Harness::ALL.into_iter().map(|k| SourceView::of(Source::default_for(k), false)).collect();
    out.extend(registered(store).into_iter().map(|s| SourceView::of(s, true)));
    out
}

/// 按 id 找来源。默认目录的 id 就是工具名（`codex`、`dsh`），所以旧的调用方式照样能用。
pub fn find(store: &Store, id: &str) -> Option<Source> {
    Harness::parse(id).map(Source::default_for).or_else(|| registered(store).into_iter().find(|s| s.id == id))
}

fn same_dir(a: &std::path::Path, b: &std::path::Path) -> bool {
    let n = |p: &std::path::Path| {
        let s = p.to_string_lossy().trim_end_matches(['/', '\\']).replace('\\', "/");
        if cfg!(windows) { s.to_lowercase() } else { s }
    };
    n(a) == n(b)
}

/// 检查要登记的内容，返回整理好的来源（还没存）。错误是（字段，原因）。
pub fn validate(store: &Store, n: NewSource) -> Result<Source, (&'static str, &'static str)> {
    let home = PathBuf::from(n.home.trim());
    if n.home.trim().is_empty() || !home.is_absolute() {
        return Err(("home", "absolute"));
    }
    if !home.is_dir() {
        return Err(("home", "not_dir"));
    }
    let taken = Harness::ALL.into_iter().map(Source::default_for).chain(registered(store)).any(|s| s.kind == n.kind && same_dir(&s.home, &home));
    if taken {
        return Err(("home", "duplicate"));
    }
    let name = n.name.trim();
    if name.chars().count() > NAME_MAX {
        return Err(("name", "too_long"));
    }
    let bin = n.bin.map(|b| b.trim().to_string()).filter(|b| !b.is_empty());
    if bin.as_deref().is_some_and(|b| !PathBuf::from(b).is_file()) {
        return Err(("bin", "not_file"));
    }
    Ok(Source { id: format!("reg-{}", crate::util::random_id(8).to_lowercase()), kind: n.kind, name: name.to_string(), home, bin })
}

/// 登记。满了（[`MAX`] 个）时返回 `Ok(None)`。
pub fn add(store: &Store, src: Source) -> anyhow::Result<Option<Source>> {
    let mut all = registered(store);
    if all.len() >= MAX {
        return Ok(None);
    }
    all.push(src.clone());
    store.set(KEY, &all)?;
    Ok(Some(src))
}

/// 删除登记（默认目录删不了）。返回是否删掉了。
pub fn remove(store: &Store, id: &str) -> anyhow::Result<bool> {
    let mut all = registered(store);
    let before = all.len();
    all.retain(|s| s.id != id);
    if all.len() == before {
        return Ok(false);
    }
    store.set(KEY, &all)?;
    Ok(true)
}
