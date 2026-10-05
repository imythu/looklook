//! 线路测速（后台）：定期测这台电脑到每条线路的延迟。
//!
//! - “自动选择”模式（没有手动选过线路）：发现明显更快的线路，在没有打开的终端时自动换过去
//!   （带 `auto: true` 提交，平台不会用它覆盖手动选择）；有终端开着时先不换，只提示；
//! - 手动选了线路、而它明显偏慢且有快得多的线路：提示用户可以去“设置 → 远程线路”换；
//! - 测速结果放进 `/api/status` 的 `relay_check`，管理台据此显示不打扰的提示。
//!
//! “明显更快”要同时快 30 ms 以上和 30% 以上，并且连测两次都成立，避免网络抖动时来回换。

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use crate::account::Account;

/// 连上远程通道后多久第一次测速。
const FIRST_DELAY: Duration = Duration::from_secs(20);
/// 平时的测速间隔。
const INTERVAL: Duration = Duration::from_secs(15 * 60);
/// 发现更快的线路后，隔多久再测一次确认。
const CONFIRM_DELAY: Duration = Duration::from_secs(60);
/// 延迟达到这个值（毫秒）算慢。
pub const SLOW_MS: u32 = 150;

#[derive(Debug, Clone, Serialize)]
pub struct Item {
    pub name: String,
    pub address: Option<String>,
    pub latency_ms: Option<u32>,
    pub error: Option<String>,
}

/// 一次测速的结论。
#[derive(Debug, Clone, Default, Serialize)]
pub struct Check {
    /// Unix 毫秒
    pub checked_at: i64,
    pub items: Vec<Item>,
    pub preferred: Option<String>,
    pub current: Option<String>,
    /// 当前是自动选择模式（没有手动选过线路）
    pub auto: bool,
    pub current_ms: Option<u32>,
    pub best: Option<String>,
    pub best_ms: Option<u32>,
    /// 当前线路偏慢（或测不通）
    pub slow: bool,
    /// 建议换到的线路：当前偏慢、且这条明显更快，但没有自动换（手动模式，或者有终端开着）
    pub suggest: Option<String>,
}

/// 测一遍所有线路（并发，最多几秒）。
pub async fn measure(account: &Account) -> crate::platform::PResult<Check> {
    let list = account.relays().await?;
    let pings = futures_util::future::join_all(list.items.iter().map(|i| async {
        match &i.ping_url {
            Some(u) => crate::relay::ping(u).await,
            None => Err("unsupported".to_string()),
        }
    }))
    .await;
    let items: Vec<Item> = list
        .items
        .iter()
        .zip(pings)
        .map(|(i, p)| {
            let (latency_ms, error) = match p {
                Ok(ms) => (Some(ms), None),
                Err(e) => (None, Some(e)),
            };
            Item { name: i.name.clone(), address: i.address.clone(), latency_ms, error }
        })
        .collect();
    Ok(conclude(items, list.preferred, list.current, list.auto_picked, crate::util::system_ms()))
}

fn conclude(items: Vec<Item>, preferred: Option<String>, current: Option<String>, auto_picked: bool, now: i64) -> Check {
    let auto = preferred.is_none() || auto_picked;
    let current_ms = current.as_ref().and_then(|c| items.iter().find(|i| &i.name == c)).and_then(|i| i.latency_ms);
    let best = items.iter().filter_map(|i| i.latency_ms.map(|ms| (ms, &i.name))).min();
    let (best_ms, best) = (best.map(|b| b.0), best.map(|b| b.1.clone()));
    // 能测的线路里有当前这条但测不通，算慢；当前线路不能测（没开直连）时什么也不说。
    let current_testable = current.as_ref().is_some_and(|c| items.iter().any(|i| &i.name == c && i.error.as_deref() != Some("unsupported")));
    let slow = current_testable && current_ms.is_none_or(|ms| ms >= SLOW_MS);
    Check { checked_at: now, items, preferred, current, auto, current_ms, best, best_ms, slow, suggest: None }
}

/// `best` 是否明显比当前线路快。
pub fn clearly_faster(best_ms: Option<u32>, current_ms: Option<u32>) -> bool {
    match (best_ms, current_ms) {
        (Some(_), None) => true,
        (Some(b), Some(c)) => b + 30 <= c && b * 10 <= c * 7,
        _ => false,
    }
}

impl Check {
    /// 值得换的线路（不论是否会自动换）。
    pub fn candidate(&self) -> Option<&str> {
        let best = self.best.as_deref()?;
        (Some(best) != self.current.as_deref() && clearly_faster(self.best_ms, self.current_ms)).then_some(best)
    }
}

pub struct RelayWatch {
    account: Arc<Account>,
    last: Mutex<Option<Check>>,
    wake: Notify,
}

impl RelayWatch {
    pub fn new(account: Arc<Account>) -> Arc<Self> {
        Arc::new(Self { account, last: Mutex::new(None), wake: Notify::new() })
    }

    /// 最近一次测速结论（给 `/api/status`）。
    pub fn view(&self) -> Option<Check> {
        self.last.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// 界面上手动测了一次（或换了线路）：更新结论；换线路后让后台尽快重测。
    pub fn record(&self, mut c: Check) {
        let old = self.view();
        c.suggest = suggest_for(&c, old.as_ref(), false);
        *self.last.lock().unwrap_or_else(|e| e.into_inner()) = Some(c);
    }

    pub fn changed(&self) {
        *self.last.lock().unwrap_or_else(|e| e.into_inner()) = None;
        self.wake.notify_one();
    }

    pub async fn run(self: Arc<Self>, shutdown: CancellationToken) {
        let mut connected_since: Option<tokio::time::Instant> = None;
        let mut next: Option<tokio::time::Instant> = None;
        let mut pending: Option<String> = None;
        loop {
            let now = tokio::time::Instant::now();
            if crate::relay::status().state == "connected" && self.account.session().is_some() {
                let since = *connected_since.get_or_insert(now);
                let due = next.unwrap_or(since + FIRST_DELAY);
                if now >= due {
                    next = Some(now + self.tick(&mut pending).await);
                }
            } else {
                connected_since = None;
            }
            tokio::select! {
                _ = shutdown.cancelled() => return,
                _ = tokio::time::sleep(Duration::from_secs(5)) => {}
                _ = self.wake.notified() => { next = Some(tokio::time::Instant::now() + Duration::from_secs(10)); pending = None; }
            }
        }
    }

    /// 测一次并按结论行动；返回下一次测速的间隔。
    async fn tick(&self, pending: &mut Option<String>) -> Duration {
        let c = match measure(&self.account).await {
            Ok(c) => c,
            Err(e) => {
                tracing::debug!(error = %e, "线路测速失败");
                return INTERVAL;
            }
        };
        let candidate = c.candidate().map(str::to_string);
        let confirmed = candidate.is_some() && *pending == candidate;
        *pending = candidate.clone();
        let mut c = c;
        // 自动模式、连测两次都明显更快、没有打开的终端：自动换。
        if let (true, true, Some(target)) = (c.auto, confirmed, candidate.as_deref()) {
            if crate::updater::terminals_idle_ms().is_some() {
                tracing::info!(from = ?c.current, to = target, current_ms = ?c.current_ms, best_ms = ?c.best_ms, "自动换到更快的线路");
                match self.account.set_relay(Some(target.to_string()), true).await {
                    Ok(r) => {
                        *pending = None;
                        c.auto = r.preferred.is_none() || r.auto_picked;
                        c.preferred = r.preferred;
                        c.current = r.current;
                        c.current_ms = c.current.as_ref().and_then(|n| c.items.iter().find(|i| &i.name == n)).and_then(|i| i.latency_ms);
                        c.slow = c.current_ms.is_none_or(|ms| ms >= SLOW_MS);
                    }
                    Err(e) => tracing::warn!(error = %e, "自动换线路失败"),
                }
            }
        }
        let old = self.view();
        c.suggest = suggest_for(&c, old.as_ref(), confirmed);
        *self.last.lock().unwrap_or_else(|e| e.into_inner()) = Some(c);
        if pending.is_some() {
            CONFIRM_DELAY
        } else {
            INTERVAL
        }
    }
}

/// 要不要提示用户换线路：当前偏慢，且有明显更快的线路。只测到一次时先不提示（等确认），
/// 上一次已经在提示同一条时继续提示。
fn suggest_for(c: &Check, old: Option<&Check>, confirmed: bool) -> Option<String> {
    let candidate = c.candidate()?;
    if !c.slow {
        return None;
    }
    let was = old.and_then(|o| o.suggest.as_deref()) == Some(candidate);
    (confirmed || was).then(|| candidate.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(name: &str, ms: Option<u32>) -> Item {
        Item { name: name.into(), address: None, latency_ms: ms, error: ms.is_none().then(|| "timeout".into()) }
    }

    #[test]
    fn faster_needs_both_margins() {
        assert!(clearly_faster(Some(40), Some(200)));
        assert!(!clearly_faster(Some(80), Some(100)), "只快 20 ms");
        assert!(!clearly_faster(Some(300), Some(340)), "只快 12%");
        assert!(clearly_faster(Some(300), None), "当前测不通");
        assert!(!clearly_faster(None, Some(100)));
    }

    #[test]
    fn concludes_slow_and_candidate() {
        let c = conclude(vec![item("r1", Some(320)), item("r2", Some(60))], None, Some("r1".into()), false, 0);
        assert!(c.auto && c.slow);
        assert_eq!((c.best.as_deref(), c.candidate()), (Some("r2"), Some("r2")));
        // 手动选的是当前这条
        let c = conclude(vec![item("r1", Some(90)), item("r2", Some(30))], Some("r1".into()), Some("r1".into()), false, 0);
        assert!(!c.auto && !c.slow);
        assert_eq!(c.candidate(), Some("r2"));
        assert_eq!(suggest_for(&c, None, true), None, "不慢就不提示");
        // 自动选过的仍算自动模式
        let c = conclude(vec![item("r1", None), item("r2", Some(30))], Some("r1".into()), Some("r1".into()), true, 0);
        assert!(c.auto && c.slow);
        // 当前线路不能测速时不算慢
        let mut u = item("r1", None);
        u.error = Some("unsupported".into());
        let c = conclude(vec![u, item("r2", Some(30))], None, Some("r1".into()), false, 0);
        assert!(!c.slow);
    }

    #[test]
    fn suggestion_waits_for_confirmation() {
        let c = conclude(vec![item("r1", Some(400)), item("r2", Some(50))], Some("r1".into()), Some("r1".into()), false, 0);
        assert_eq!(suggest_for(&c, None, false), None);
        assert_eq!(suggest_for(&c, None, true).as_deref(), Some("r2"));
        let old = Check { suggest: Some("r2".into()), ..Default::default() };
        assert_eq!(suggest_for(&c, Some(&old), false).as_deref(), Some("r2"));
    }
}
