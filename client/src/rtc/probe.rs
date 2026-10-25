//! 丢包率（服务端 docs/07 §7）：DataChannel 走 SCTP，丢了的包会重传，getStats 里也没有 RTP 那样的丢包计数。
//! 所以另开一条不重传、不保序的通道 `ll-probe`，发起方每秒发一个小包（`llp1` + 4 字节序号），应答方原样发回；
//! 3 秒没回就算丢。丢包率取最近 60 个探测包，算的是往返两个方向合起来的丢包。
//!
//! 应答方（[`echo`]）：浏览器连进来的连接、本机 App 连进来的连接都一样，在 `super::run_peer` 里按通道名分出来。
//! 发起方：本机 App 连其他电脑（[`run`]，`outbound`），浏览器在启动页（server-web/rtc/boot.js）。
//! 旧版电脑不认识这条通道（当成请求通道、收到坏帧就关掉），发起方拿不到回包就显示“—”，不算丢包。

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio_util::sync::CancellationToken;

use super::peer::Channel;

pub const LABEL: &str = "ll-probe";
const MAGIC: &[u8; 4] = b"llp1";
const INTERVAL: Duration = if cfg!(test) { Duration::from_millis(30) } else { Duration::from_secs(1) };
const LOST_AFTER: Duration = Duration::from_secs(3);
const WINDOW: usize = 60;
/// 少于这么多个有结果的探测包时不给丢包率（刚连上时一两个包说明不了什么）。
const MIN_SAMPLES: usize = 10;

/// 应答方：收到什么发回什么（只回探测包大小的消息）。
pub async fn echo(mut ch: Channel) {
    while let Some(m) = ch.recv().await {
        if m.len() <= 64 && m.starts_with(MAGIC) && ch.send(&m).is_err() {
            break;
        }
    }
}

/// 发起方的统计。
#[derive(Debug, Default)]
pub struct Stats {
    /// 还没回的：(序号, 发出时间)。
    pending: VecDeque<(u32, Instant)>,
    /// 有结果的，旧的在前：回了是 Some(往返时间)，丢了是 None。
    done: VecDeque<Option<Duration>>,
    /// 收到过回包（对端支持探测）。
    answered: bool,
}

impl Stats {
    fn push(&mut self, r: Option<Duration>) {
        self.done.push_back(r);
        while self.done.len() > WINDOW {
            self.done.pop_front();
        }
    }

    fn sent(&mut self, seq: u32, now: Instant) {
        self.pending.push_back((seq, now));
    }

    fn got(&mut self, seq: u32, now: Instant) {
        if let Some(i) = self.pending.iter().position(|(s, _)| *s == seq) {
            let (_, at) = self.pending.remove(i).unwrap_or((seq, now));
            self.answered = true;
            self.push(Some(now - at));
        }
    }

    /// 超过 [`LOST_AFTER`] 还没回的算丢。
    fn expire(&mut self, now: Instant) {
        while let Some(&(_, at)) = self.pending.front() {
            if now - at < LOST_AFTER {
                break;
            }
            self.pending.pop_front();
            self.push(None);
        }
    }

    /// 最近 [`WINDOW`] 个探测包的丢包率（0–1）；对端从没回过（旧版）或样本太少时为 None。
    pub fn loss(&self) -> Option<f64> {
        if !self.answered || self.done.len() < MIN_SAMPLES {
            return None;
        }
        Some(self.done.iter().filter(|r| r.is_none()).count() as f64 / self.done.len() as f64)
    }
}

/// 发起方：每秒发一个探测包，直到通道关闭、连接关闭或 `wanted()` 为假。返回前释放通道
/// （调用方持有的连接要在通道之后释放，见 peer.rs）。
pub async fn run(mut ch: Channel, stats: Arc<Mutex<Stats>>, closed: CancellationToken, wanted: impl Fn() -> bool) {
    if ch.opened(Duration::from_secs(10)).await.is_err() {
        return;
    }
    let lock = || stats.lock().unwrap_or_else(|e| e.into_inner());
    let mut tick = tokio::time::interval(INTERVAL);
    let mut seq: u32 = 0;
    loop {
        tokio::select! {
            _ = closed.cancelled() => break,
            _ = tick.tick() => {
                if !wanted() {
                    break;
                }
                let now = Instant::now();
                lock().expire(now);
                let mut m = MAGIC.to_vec();
                m.extend_from_slice(&seq.to_be_bytes());
                if ch.send(&m).is_err() {
                    break;
                }
                lock().sent(seq, now);
                seq = seq.wrapping_add(1);
            }
            m = ch.recv() => match m {
                Some(m) if m.len() == 8 && m.starts_with(MAGIC) => {
                    let s = u32::from_be_bytes([m[4], m[5], m[6], m[7]]);
                    lock().got(s, Instant::now());
                }
                Some(_) => {}
                None => break,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loss_over_the_window() {
        let t0 = Instant::now();
        let at = |s: u64| t0 + Duration::from_secs(s);
        let mut s = Stats::default();
        for i in 0..20u32 {
            s.sent(i, at(i as u64));
            // 每 4 个丢 1 个
            if i % 4 != 3 {
                s.got(i, at(i as u64) + Duration::from_millis(40));
            }
        }
        assert_eq!(s.loss(), Some(0.0), "没回的还没到 3 秒，不算丢");
        s.expire(at(30));
        assert_eq!(s.loss(), Some(0.25));
        s.got(3, at(31));
        assert_eq!(s.loss(), Some(0.25), "已经算丢的再回来不改");
    }

    #[test]
    fn no_loss_figure_without_answers() {
        let t0 = Instant::now();
        let mut s = Stats::default();
        for i in 0..20u32 {
            s.sent(i, t0);
        }
        s.expire(t0 + Duration::from_secs(10));
        assert_eq!(s.loss(), None, "旧版电脑从不回");
        let mut s = Stats::default();
        for i in 0..5u32 {
            s.sent(i, t0);
            s.got(i, t0);
        }
        assert_eq!(s.loss(), None, "样本太少");
    }

    /// 真的连接上：发起方开不重传的通道，应答方按通道名交给 [`echo`]，丢包率为 0、往返时间有值。
    #[tokio::test(flavor = "multi_thread")]
    async fn measures_over_a_real_connection() {
        use super::super::peer::Pc;
        let (mut a, mut ctl) = Pc::offer(&[]).unwrap();
        let ch = a.unreliable_channel(LABEL).unwrap();
        let offer = a.local_offer(Duration::from_secs(3)).await.unwrap();
        let mut b = Pc::answer(&[], &offer).unwrap();
        let answer = b.local_answer(Duration::from_secs(3)).await.unwrap();
        a.set_answer(&answer).unwrap();
        let echoer = tokio::spawn(async move {
            while let Some(ch) = b.incoming.recv().await {
                if ch.label == LABEL {
                    tokio::spawn(echo(ch));
                }
            }
            b
        });
        ctl.opened(Duration::from_secs(10)).await.unwrap();
        let stats = Arc::new(Mutex::new(Stats::default()));
        let closed = CancellationToken::new();
        let run = tokio::spawn(run(ch, stats.clone(), closed.clone(), || true));
        tokio::time::sleep(INTERVAL * (MIN_SAMPLES as u32 + 10)).await;
        closed.cancel();
        run.await.unwrap();
        let s = stats.lock().unwrap();
        assert_eq!(s.loss(), Some(0.0), "{s:?}");
        assert!(s.done.iter().all(|r| r.is_some_and(|d| d < Duration::from_secs(1))));
        drop(s);
        drop(ctl);
        a.incoming.close();
        echoer.abort();
        tokio::task::spawn_blocking(move || drop(a)).await.unwrap();
    }

    #[test]
    fn window_keeps_recent_results() {
        let t0 = Instant::now();
        let mut s = Stats::default();
        for i in 0..200u32 {
            s.sent(i, t0);
            if i >= 100 {
                s.got(i, t0);
            }
        }
        s.expire(t0 + Duration::from_secs(5));
        // 前 100 个丢了，但窗口只看最近 60 个结果：pending 里先回的 100 个已经进了 done，过期的 100 个在它们后面
        assert_eq!(s.done.len(), WINDOW);
    }
}
