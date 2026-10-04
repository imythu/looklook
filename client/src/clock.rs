//! 可信时间（详细设计 §4.5 对客户端的约定）：以平台签名响应里的 `server_time` 为锚点，
//! 用本机单调时钟推算“现在的平台时间”，判断会员到期与离线宽限，不信任本机系统时间。
//!
//! - 单调时钟在部分系统睡眠时不走，所以同时参考系统时间的流逝，取两者较大值；
//!   把系统时间往回调只会让系统时间那一项变小，不会让推算时间倒退。
//! - 锚点与“本锚点以来已确认流逝的时间（含休眠）”定期落盘，客户端重启后接着累计，
//!   重启不能把离线时长清零。

use std::sync::Mutex;
use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::util::system_ms;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct Snapshot {
    /// 平台时间锚点（Unix 毫秒）
    pub server_ms: i64,
    /// 取得锚点时的本机系统时间
    pub system_ms: i64,
    /// 从锚点起已确认流逝的时间（含休眠，跨重启累计）
    pub elapsed_ms: i64,
}

struct Anchor {
    snap: Snapshot,
    since: Instant,
}

#[derive(Default)]
pub struct TrustedClock {
    anchor: Mutex<Option<Anchor>>,
}

impl TrustedClock {
    pub fn restore(snap: Option<Snapshot>) -> Self {
        Self { anchor: Mutex::new(snap.map(|snap| Anchor { snap, since: Instant::now() })) }
    }

    /// 收到平台时间（签名响应里的 `server_time` 或 `TIMESTAMP_SKEW` 的参数）。
    pub fn set(&self, server_ms: i64) {
        let snap = Snapshot { server_ms, system_ms: system_ms(), elapsed_ms: 0 };
        *self.anchor.lock().unwrap_or_else(|e| e.into_inner()) = Some(Anchor { snap, since: Instant::now() });
    }

    pub fn has_anchor(&self) -> bool {
        self.anchor.lock().unwrap_or_else(|e| e.into_inner()).is_some()
    }

    /// 推算的平台时间（Unix 毫秒）；还没有锚点时退回本机时间（只用于登录前的签名时间戳）。
    pub fn now_ms(&self) -> i64 {
        self.now_with(system_ms())
    }

    fn now_with(&self, system_now: i64) -> i64 {
        match &mut *self.anchor.lock().unwrap_or_else(|e| e.into_inner()) {
            None => system_now,
            Some(a) => {
                let instant = Instant::now();
                let mono = a.snap.elapsed_ms.saturating_add(instant.duration_since(a.since).as_millis() as i64);
                let wall = system_now.saturating_sub(a.snap.system_ms);
                // Sleep can advance wall time farther than Instant. Once observed,
                // preserve that elapsed time and continue monotonically from here.
                // Otherwise rolling the wall clock back can unlock an expired lease.
                if wall > mono {
                    a.snap.elapsed_ms = wall;
                    a.since = instant;
                }
                a.snap.server_ms.saturating_add(mono.max(wall).max(0))
            }
        }
    }

    /// 需要落盘的状态。
    pub fn snapshot(&self) -> Option<Snapshot> {
        // Include elapsed wall time from sleep even if no request has read the clock.
        self.now_with(system_ms());
        self.anchor.lock().unwrap_or_else(|e| e.into_inner()).as_ref().map(|a| Snapshot {
            elapsed_ms: a.snap.elapsed_ms + a.since.elapsed().as_millis() as i64,
            ..a.snap
        })
    }

    pub fn clear(&self) {
        *self.anchor.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anchored_time_ignores_backward_system_clock() {
        let c = TrustedClock::restore(Some(Snapshot { server_ms: 1_000_000, system_ms: 5_000_000, elapsed_ms: 60_000 }));
        // 系统时间被往回拨：仍然至少是锚点 + 已累计的单调时间。
        assert!(c.now_with(4_000_000) >= 1_060_000);
        // 系统时间走了 1 小时（例如睡眠期间单调时钟停了）：按系统时间的流逝计算。
        assert_eq!(c.now_with(5_000_000 + 3_600_000), 1_000_000 + 3_600_000);
    }

    #[test]
    fn snapshot_accumulates_across_restart() {
        let c = TrustedClock::default();
        assert!(!c.has_anchor());
        c.set(42);
        let s = c.snapshot().unwrap();
        assert_eq!(s.server_ms, 42);
        let r = TrustedClock::restore(Some(Snapshot { elapsed_ms: 10_000, ..s }));
        assert!(r.now_ms() >= 42 + 10_000);
    }

    #[test]
    fn observed_sleep_time_cannot_be_undone_by_rolling_back_the_clock() {
        let wall = system_ms();
        let c = TrustedClock::restore(Some(Snapshot { server_ms: 1_000_000, system_ms: wall, elapsed_ms: 0 }));
        let after_sleep = c.now_with(wall + 3_600_000);
        assert!(after_sleep >= 4_600_000);
        assert!(c.now_with(wall - 60_000) >= after_sleep);
        let saved = c.snapshot().unwrap();
        assert!(saved.elapsed_ms >= 3_600_000);
        let restarted = TrustedClock::restore(Some(saved));
        assert!(restarted.now_with(wall - 60_000) >= after_sleep);
    }
}
