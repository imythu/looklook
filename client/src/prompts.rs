//! 常用任务模板（docs/PLAN_2026-10-27.md §6.2）：新会话输入框旁边的“模板”，点一下填进输入框（不直接发送）。
//!
//! - 存在同步集合 `prompts`（所有电脑一份），一个模板一项（键是模板 id），两台电脑各改各的不会互相覆盖；
//! - 示例模板由界面按语言给出（`seed`），只写一次：写的同时记下 `_seeded`，以后用户删光了也不再出现。
//!   示例的 id 固定（`example-1`…），两台电脑同时写示例也只是同一项后写的胜出，不会重复。
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::error::LocalError;
use crate::gateway::App;
use crate::settings_sync;
use crate::store::Store;

const COLLECTION: &str = "prompts";
const SEEDED: &str = "_seeded";
const MAX_ITEMS: usize = 100;
const MAX_NAME: usize = 40;
const MAX_TEXT: usize = 4000;
const MAX_SEED: usize = 10;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Prompt {
    pub name: String,
    pub text: String,
    /// 新建时间，列表按它排（示例按顺序）
    #[serde(default)]
    pub created_ms: i64,
}

#[derive(Debug, Deserialize)]
pub struct Input {
    pub name: String,
    pub text: String,
}

/// 全部模板（按新建先后）和示例是不是已经给过
pub fn list(store: &Store) -> Result<Value, LocalError> {
    let all = settings_sync::values(store, COLLECTION)?;
    let seeded = all.contains_key(SEEDED);
    let mut items: Vec<(String, Prompt)> = all.into_iter().filter(|(k, _)| k != SEEDED).filter_map(|(k, v)| Some((k, serde_json::from_value(v).ok()?))).collect();
    items.sort_by(|a, b| (a.1.created_ms, &a.0).cmp(&(b.1.created_ms, &b.0)));
    let items: Vec<Value> = items.into_iter().map(|(id, p)| json!({ "id": id, "name": p.name, "text": p.text })).collect();
    Ok(json!({ "items": items, "seeded": seeded }))
}

fn clean(i: Input) -> Result<(String, String), LocalError> {
    let name: String = i.name.trim().chars().filter(|c| !c.is_control()).collect();
    let text = i.text.trim().to_string();
    if name.is_empty() || name.chars().count() > MAX_NAME {
        return Err(LocalError::invalid("name", "length"));
    }
    if text.is_empty() || text.chars().count() > MAX_TEXT {
        return Err(LocalError::invalid("text", "length"));
    }
    Ok((name, text))
}

fn write(app: &App, items: &[(&str, Option<Value>)]) -> Result<(), LocalError> {
    if settings_sync::write_local(&app.store, app.account.clock.now_ms(), COLLECTION, items)? {
        app.sync.poke();
    }
    Ok(())
}

fn count(store: &Store) -> Result<usize, LocalError> {
    Ok(settings_sync::values(store, COLLECTION)?.keys().filter(|k| *k != SEEDED).count())
}

pub fn create(app: &App, i: Input) -> Result<(), LocalError> {
    let (name, text) = clean(i)?;
    if count(&app.store)? >= MAX_ITEMS {
        return Err(LocalError::invalid("prompts", "too_many"));
    }
    let id = crate::util::random_id(10);
    let p = Prompt { name, text, created_ms: app.account.clock.now_ms() };
    write(app, &[(&id, Some(json!(p)))])
}

pub fn update(app: &App, id: &str, i: Input) -> Result<(), LocalError> {
    let (name, text) = clean(i)?;
    let old: Prompt = settings_sync::values(&app.store, COLLECTION)?
        .remove(id)
        .filter(|_| id != SEEDED)
        .and_then(|v| serde_json::from_value(v).ok())
        .ok_or_else(LocalError::not_found)?;
    write(app, &[(id, Some(json!(Prompt { name, text, created_ms: old.created_ms })))])
}

pub fn delete(app: &App, id: &str) -> Result<(), LocalError> {
    if id == SEEDED {
        return Err(LocalError::not_found());
    }
    write(app, &[(id, None)])
}

/// 第一次打开时写入界面给的示例（按界面的语言）；已经给过就什么都不做
pub fn seed(app: &App, items: Vec<Input>) -> Result<(), LocalError> {
    let all = settings_sync::values(&app.store, COLLECTION)?;
    if all.contains_key(SEEDED) {
        return Ok(());
    }
    let now = app.account.clock.now_ms();
    let mut out: Vec<(String, Option<Value>)> = Vec::new();
    for (n, i) in items.into_iter().take(MAX_SEED).enumerate() {
        let (name, text) = clean(i)?;
        out.push((format!("example-{}", n + 1), Some(json!(Prompt { name, text, created_ms: now + n as i64 }))));
    }
    out.push((SEEDED.into(), Some(json!(true))));
    let refs: Vec<(&str, Option<Value>)> = out.iter().map(|(k, v)| (k.as_str(), v.clone())).collect();
    write(app, &refs)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn put(store: &Store, id: &str, v: Value) {
        settings_sync::write_local(store, 1, COLLECTION, &[(id, Some(v))]).unwrap();
    }

    #[test]
    fn list_sorts_and_hides_marker() {
        let store = Store::memory().unwrap();
        assert_eq!(list(&store).unwrap(), json!({ "items": [], "seeded": false }));
        put(&store, "b", json!({ "name": "B", "text": "two", "created_ms": 20 }));
        put(&store, "a", json!({ "name": "A", "text": "one", "created_ms": 10 }));
        put(&store, SEEDED, json!(true));
        put(&store, "junk", json!("not a prompt"));
        let v = list(&store).unwrap();
        assert_eq!(v["seeded"], true);
        assert_eq!(v["items"], json!([{ "id": "a", "name": "A", "text": "one" }, { "id": "b", "name": "B", "text": "two" }]));
        assert_eq!(count(&store).unwrap(), 3);
    }

    #[test]
    fn clean_checks_lengths() {
        let ok = clean(Input { name: "  周报\n ".into(), text: " 写周报 ".into() }).unwrap();
        assert_eq!(ok, ("周报".into(), "写周报".into()));
        assert!(clean(Input { name: " ".into(), text: "x".into() }).is_err());
        assert!(clean(Input { name: "n".into(), text: "  ".into() }).is_err());
        assert!(clean(Input { name: "n".repeat(41), text: "x".into() }).is_err());
        assert!(clean(Input { name: "n".into(), text: "x".repeat(4001) }).is_err());
    }
}
