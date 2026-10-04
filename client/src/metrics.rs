//! 本机资源数据（管理台“系统”页）：CPU、内存、交换区、负载、网络、磁盘与占用最多的进程。
//!
//! - 每 5 秒采样一次，保留最近 1 小时的逐点数据和最近 24 小时的逐分钟平均值（只在内存里，重启后从头开始）。
//! - 进程列表开销较大（要遍历所有进程），只在最近 30 秒内有人打开“系统”页时才刷新。
//! - 磁盘每分钟刷新一次。

use std::collections::VecDeque;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use sysinfo::{Disks, Networks, ProcessRefreshKind, ProcessesToUpdate, System};

const INTERVAL: Duration = Duration::from_secs(5);
/// 逐点数据保留 1 小时
const RECENT_LEN: usize = 720;
/// 逐分钟数据保留 24 小时
const MINUTES_LEN: usize = 1440;
/// 有人在看“系统”页的判断时长
const WATCH_MS: i64 = 30_000;
const TOP_PROCESSES: usize = 8;
const MAX_DISKS: usize = 8;

/// 一个采样点（图表用）。百分比为 0–100；网络为每秒字节数。
#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct Sample {
    /// 毫秒时间戳
    pub t: i64,
    pub cpu: f32,
    pub mem: f32,
    pub rx: u64,
    pub tx: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Disk {
    pub mount: String,
    pub fs: String,
    pub total: u64,
    pub available: u64,
    pub removable: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Proc {
    pub pid: u32,
    pub name: String,
    /// 占整机 CPU 的百分比（0–100，已按核数折算）
    pub cpu: f32,
    pub mem: u64,
}

/// 当前状态（统计卡片与列表用）。
#[derive(Debug, Clone, Default, Serialize)]
pub struct Now {
    pub t: i64,
    pub cpu: f32,
    pub cores: Vec<f32>,
    pub cpu_brand: String,
    pub mem_total: u64,
    pub mem_used: u64,
    pub swap_total: u64,
    pub swap_used: u64,
    /// 1、5、15 分钟平均负载（Windows 没有）
    pub load: Option<[f64; 3]>,
    pub uptime: u64,
    pub rx: u64,
    pub tx: u64,
    pub disks: Vec<Disk>,
    /// 没人看时不刷新进程，为 None
    pub processes: Option<usize>,
    pub top: Vec<Proc>,
}

struct Sampler {
    sys: System,
    nets: Networks,
    disks: Disks,
    disks_at: i64,
    net_at: i64,
    procs_fresh: bool,
}

#[derive(Default)]
struct History {
    now: Now,
    recent: VecDeque<Sample>,
    minutes: VecDeque<Sample>,
    /// 当前这一分钟里的采样（凑满一分钟求平均后进 `minutes`）
    minute: Vec<Sample>,
}

pub struct Metrics {
    history: Mutex<History>,
    watched_at: AtomicI64,
}

fn now_ms() -> i64 {
    crate::util::system_ms()
}

/// 虚拟网卡（本机回环、容器、虚拟机网桥）不算进网速，否则容器之间的流量会被重复计算。
fn counted_interface(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    !(n == "lo" || n.starts_with("lo") && n[2..].chars().all(|c| c.is_ascii_digit()) || n.starts_with("loopback") || ["veth", "docker", "br-", "virbr", "vethernet", "cni", "flannel", "utun", "awdl", "llw"].iter().any(|p| n.starts_with(p)))
}

/// 系统内部的挂载（内存盘、只读的软件包镜像等）不显示。
fn counted_disk(fs: &str, mount: &str) -> bool {
    let fs = fs.to_ascii_lowercase();
    let skip_fs = ["tmpfs", "devtmpfs", "squashfs", "overlay", "proc", "sysfs", "efivarfs", "ramfs", "autofs", "devfs", "nullfs"];
    !skip_fs.contains(&fs.as_str()) && !mount.starts_with("/snap/") && !mount.starts_with("/boot/efi") && !mount.starts_with("/System/Volumes/") && !mount.starts_with("/private/var/vm")
}

fn mean(xs: &[Sample]) -> Sample {
    let n = xs.len().max(1);
    Sample {
        t: xs.last().map(|s| s.t).unwrap_or_default(),
        cpu: xs.iter().map(|s| s.cpu).sum::<f32>() / n as f32,
        mem: xs.iter().map(|s| s.mem).sum::<f32>() / n as f32,
        rx: xs.iter().map(|s| s.rx).sum::<u64>() / n as u64,
        tx: xs.iter().map(|s| s.tx).sum::<u64>() / n as u64,
    }
}

impl Sampler {
    fn new() -> Self {
        let mut sys = System::new();
        sys.refresh_cpu_all();
        sys.refresh_memory();
        Self { sys, nets: Networks::new_with_refreshed_list(), disks: Disks::new(), disks_at: 0, net_at: now_ms(), procs_fresh: false }
    }

    fn sample(&mut self, watched: bool) -> Now {
        let t = now_ms();
        self.sys.refresh_cpu_all();
        self.sys.refresh_memory();
        // 网卡可能增减（插网线、开 VPN），一起刷新列表
        self.nets.refresh(true);
        let secs = ((t - self.net_at).max(1) as f64) / 1000.0;
        self.net_at = t;
        let (rx, tx) = self.nets.iter().filter(|(name, _)| counted_interface(name)).fold((0u64, 0u64), |(r, w), (_, d)| (r + d.received(), w + d.transmitted()));
        if t - self.disks_at >= 60_000 {
            self.disks.refresh(true);
            self.disks_at = t;
        }
        let mut seen = std::collections::HashSet::new();
        let disks: Vec<Disk> = self
            .disks
            .iter()
            .filter(|d| d.total_space() > 0 && counted_disk(&d.file_system().to_string_lossy(), &d.mount_point().to_string_lossy()))
            // 同一块盘挂在多处（bind mount）只算一次
            .filter(|d| seen.insert((d.name().to_os_string(), d.total_space())))
            .take(MAX_DISKS)
            .map(|d| Disk {
                mount: d.mount_point().display().to_string(),
                fs: d.file_system().to_string_lossy().into_owned(),
                total: d.total_space(),
                available: d.available_space(),
                removable: d.is_removable(),
            })
            .collect();
        let cores = self.sys.cpus().iter().map(|c| c.cpu_usage()).collect::<Vec<_>>();
        let (mut processes, mut top) = (None, vec![]);
        if watched {
            self.sys.refresh_processes_specifics(ProcessesToUpdate::All, true, ProcessRefreshKind::nothing().with_cpu().with_memory());
            // 进程 CPU 是两次刷新之间算出来的，刚开始看时第一次的数字不准，先不显示
            if self.procs_fresh {
                let n = cores.len().max(1) as f32;
                let mut all: Vec<Proc> = self
                    .sys
                    .processes()
                    .values()
                    // Linux 上线程也会列出来，只要进程
                    .filter(|p| p.thread_kind().is_none())
                    .map(|p| Proc { pid: p.pid().as_u32(), name: p.name().to_string_lossy().into_owned(), cpu: p.cpu_usage() / n, mem: p.memory() })
                    .collect();
                processes = Some(all.len());
                all.sort_by(|a, b| b.cpu.total_cmp(&a.cpu).then(b.mem.cmp(&a.mem)));
                all.truncate(TOP_PROCESSES);
                top = all;
            }
            self.procs_fresh = true;
        } else if self.procs_fresh {
            // 没人看了：丢掉进程表，省内存
            self.sys = System::new();
            self.sys.refresh_cpu_all();
            self.procs_fresh = false;
        }
        let load = System::load_average();
        let mem_total = self.sys.total_memory();
        Now {
            t,
            cpu: self.sys.global_cpu_usage(),
            cores,
            cpu_brand: self.sys.cpus().first().map(|c| c.brand().trim().to_string()).unwrap_or_default(),
            mem_total,
            // 已用 = 总量 − 可用（缓存可以随时释放，不算占用，与系统自带的任务管理器一致）
            mem_used: mem_total.saturating_sub(self.sys.available_memory()),
            swap_total: self.sys.total_swap(),
            swap_used: self.sys.used_swap(),
            load: (!cfg!(windows)).then_some([load.one, load.five, load.fifteen]),
            uptime: System::uptime(),
            rx: (rx as f64 / secs) as u64,
            tx: (tx as f64 / secs) as u64,
            disks,
            processes,
            top,
        }
    }
}

impl Metrics {
    pub fn new() -> Arc<Self> {
        Arc::new(Self { history: Mutex::new(History::default()), watched_at: AtomicI64::new(0) })
    }

    fn history(&self) -> std::sync::MutexGuard<'_, History> {
        self.history.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn watched(&self) -> bool {
        now_ms() - self.watched_at.load(Ordering::Relaxed) < WATCH_MS
    }

    /// 后台采样（随 run() 收尾时 abort）。
    pub async fn run(self: Arc<Self>) {
        let sampler = Arc::new(Mutex::new(Sampler::new()));
        let mut tick = tokio::time::interval(INTERVAL);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            let (s, watched) = (sampler.clone(), self.watched());
            // 遍历进程在 Windows 上可能要上百毫秒，不占用异步线程
            let Ok(now) = tokio::task::spawn_blocking(move || s.lock().unwrap_or_else(|e| e.into_inner()).sample(watched)).await else { continue };
            self.record(now);
        }
    }

    fn record(&self, now: Now) {
        let s = Sample { t: now.t, cpu: now.cpu, mem: pct(now.mem_used, now.mem_total), rx: now.rx, tx: now.tx };
        let mut h = self.history();
        h.now = now;
        push(&mut h.recent, s, RECENT_LEN);
        // 换了一分钟：把上一分钟的平均值存起来
        if h.minute.first().is_some_and(|f| f.t / 60_000 != s.t / 60_000) {
            let avg = mean(&h.minute);
            push(&mut h.minutes, avg, MINUTES_LEN);
            h.minute.clear();
        }
        h.minute.push(s);
    }

    /// 概要（`/api/status`，侧栏显示）：CPU 与内存百分比。还没采到数据时为 None。
    pub fn brief(&self) -> Option<serde_json::Value> {
        let h = self.history();
        (h.now.t > 0).then(|| serde_json::json!({ "cpu": h.now.cpu, "mem": pct(h.now.mem_used, h.now.mem_total) }))
    }

    /// “系统”页：当前状态与一段时间的曲线。`range`：`5m`、`1h`、`24h`。
    pub fn view(&self, range: &str) -> serde_json::Value {
        self.watched_at.store(now_ms(), Ordering::Relaxed);
        let h = self.history();
        let (series, step): (Vec<Sample>, u64) = match range {
            "24h" => {
                let mut v: Vec<Sample> = h.minutes.iter().copied().collect();
                if !h.minute.is_empty() {
                    v.push(mean(&h.minute));
                }
                (v, 60)
            }
            "1h" => (h.recent.iter().copied().collect(), INTERVAL.as_secs()),
            _ => (h.recent.iter().skip(h.recent.len().saturating_sub(60)).copied().collect(), INTERVAL.as_secs()),
        };
        serde_json::json!({ "now": h.now, "series": series, "step": step })
    }
}

fn pct(used: u64, total: u64) -> f32 {
    if total == 0 {
        0.0
    } else {
        (used as f64 * 100.0 / total as f64) as f32
    }
}

fn push(q: &mut VecDeque<Sample>, s: Sample, cap: usize) {
    if q.len() == cap {
        q.pop_front();
    }
    q.push_back(s);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interfaces_and_disks() {
        assert!(counted_interface("eth0") && counted_interface("en0") && counted_interface("Wi-Fi") && counted_interface("wlan0"));
        assert!(!counted_interface("lo") && !counted_interface("lo0") && !counted_interface("docker0") && !counted_interface("veth12ab") && !counted_interface("br-1a2b"));
        assert!(counted_interface("long-name"), "以 lo 开头但不是回环");
        assert!(counted_disk("ext4", "/") && counted_disk("NTFS", "C:\\") && counted_disk("apfs", "/"));
        assert!(!counted_disk("tmpfs", "/run") && !counted_disk("squashfs", "/snap/core/1") && !counted_disk("overlay", "/"));
    }

    #[test]
    fn minutes_are_averaged() {
        let m = Metrics::new();
        let base = 1_700_000_040_000; // 整分钟
        for (i, cpu) in [10.0, 30.0, 50.0].into_iter().enumerate() {
            m.record(Now { t: base + i as i64 * 5000, cpu, mem_total: 100, mem_used: 50, ..Default::default() });
        }
        m.record(Now { t: base + 60_000, cpu: 90.0, mem_total: 100, mem_used: 50, ..Default::default() });
        let v = m.view("24h");
        let s = v["series"].as_array().unwrap();
        assert_eq!(s.len(), 2, "一个整分钟 + 当前未满的一分钟");
        assert_eq!(s[0]["cpu"].as_f64().unwrap(), 30.0);
        assert_eq!(s[0]["mem"].as_f64().unwrap(), 50.0);
        assert_eq!(s[1]["cpu"].as_f64().unwrap(), 90.0);
        assert_eq!(m.view("5m")["series"].as_array().unwrap().len(), 4);
        assert!(m.watched());
    }

    #[test]
    fn samples_this_machine() {
        let mut s = Sampler::new();
        std::thread::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL);
        let n = s.sample(true);
        assert!(n.mem_total > 0 && !n.cores.is_empty());
        assert!(n.cpu >= 0.0 && n.cpu <= 100.0);
        std::thread::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL);
        let n = s.sample(true);
        assert!(n.processes.is_some_and(|p| p > 0) && !n.top.is_empty());
    }
}
