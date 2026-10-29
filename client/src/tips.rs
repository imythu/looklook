//! 使用提示看过没有（docs/ONBOARDING_2026-10.md §3.1）：一次性提示、上手清单里走过的步骤。
//!
//! - 存在同步集合 `tips`，一条提示一项（键是提示 id，值是 `true`）：几台电脑各记各的不会互相覆盖，合起来就是看过的全部；
//! - 手机、别的浏览器打开的也是这台电脑的管理台，所以天然共用；
//! - “重新显示所有提示”把现有的项都写成墓碑。
use serde_json::{json, Value};

use crate::error::LocalError;
use crate::gateway::App;
use crate::settings_sync;
use crate::store::Store;

const COLLECTION: &str = "tips";
const MAX_ITEMS: usize = 500;
const MAX_ID: usize = 64;

pub fn list(store: &Store) -> Result<Value, LocalError> {
    let mut seen: Vec<String> = settings_sync::values(store, COLLECTION)?.into_keys().collect();
    seen.sort();
    Ok(json!({ "seen": seen }))
}

fn valid(id: &str) -> bool {
    !id.is_empty() && id.len() <= MAX_ID && id.chars().all(|c| c.is_ascii_alphanumeric() || "_-.:".contains(c))
}

fn write(app: &App, items: &[(&str, Option<Value>)]) -> Result<(), LocalError> {
    if settings_sync::write_local(&app.store, app.account.clock.now_ms(), COLLECTION, items)? {
        app.sync.poke();
    }
    Ok(())
}

/// 记下看过（一次可以记几条）；已经记过的跳过
pub fn mark(app: &App, ids: &[String]) -> Result<(), LocalError> {
    if ids.iter().any(|id| !valid(id)) {
        return Err(LocalError::invalid("id", "format"));
    }
    let have = settings_sync::values(&app.store, COLLECTION)?;
    let new: Vec<(&str, Option<Value>)> = ids.iter().filter(|id| !have.contains_key(*id)).map(|id| (id.as_str(), Some(json!(true)))).collect();
    if have.len() + new.len() > MAX_ITEMS {
        return Err(LocalError::invalid("tips", "too_many"));
    }
    write(app, &new)
}

/// 取消看过（例如重新打开上手清单）
pub fn unmark(app: &App, ids: &[String]) -> Result<(), LocalError> {
    let items: Vec<(&str, Option<Value>)> = ids.iter().filter(|id| valid(id)).map(|id| (id.as_str(), None)).collect();
    write(app, &items)
}

/// 重新显示所有提示
pub fn reset(app: &App) -> Result<(), LocalError> {
    let have: Vec<String> = settings_sync::values(&app.store, COLLECTION)?.into_keys().collect();
    let items: Vec<(&str, Option<Value>)> = have.iter().map(|id| (id.as_str(), None)).collect();
    write(app, &items)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids() {
        assert!(valid("chat.slash"));
        assert!(valid("step:phone"));
        assert!(!valid(""));
        assert!(!valid("a b"));
        assert!(!valid(&"x".repeat(65)));
    }

    #[test]
    fn two_computers_union() {
        let a = Store::memory().unwrap();
        let b = Store::memory().unwrap();
        settings_sync::write_local(&a, 1, COLLECTION, &[("one", Some(json!(true)))]).unwrap();
        settings_sync::write_local(&b, 2, COLLECTION, &[("two", Some(json!(true)))]).unwrap();
        let (to_b, _) = settings_sync::missing(&a, &settings_sync::summary(&b).unwrap(), 100).unwrap();
        settings_sync::merge(&b, &to_b).unwrap();
        assert_eq!(list(&b).unwrap()["seen"], json!(["one", "two"]));
    }
}
