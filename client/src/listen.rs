//! 管理台监听端口：默认 `0.0.0.0:1234`，被占用时依次 +10（1244、1254…）直到能用；
//! 用上的端口记在本机数据库，下次优先用它，管理台地址不会每次都变。
//! 指定了 `--listen` / `LOOKLOOK_LISTEN` 时只用指定的地址，不自动换端口。
//!
//! 只看 bind 是否成功不够：Windows（以及开了 `SO_REUSEADDR` 的 macOS）上，别的程序监听着
//! `127.0.0.1:1234` 时，我们照样能 bind `0.0.0.0:1234`，但浏览器打开 `http://127.0.0.1:1234`
//! 会被更具体的那个监听接走——用户看到的是别人的网页（白屏、资源 404）。所以 bind 之前先连一下
//! `127.0.0.1:端口`，有人应答就当作被占用。

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use tokio::net::TcpListener;

use crate::store::Store;

pub const DEFAULT_PORT: u16 = 1234;
pub const DEFAULT: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), DEFAULT_PORT);
const STEP: u16 = 10;
const MAX_TRIES: usize = 200;
/// 数据库 kv 键：上次用上的管理台端口
const KEY: &str = "console_port";

/// 依次尝试的端口：从 `start` 开始每次 +10，不超过 65535。
fn candidates(start: u16) -> impl Iterator<Item = u16> {
    std::iter::successors(Some(start), |p| p.checked_add(STEP)).take(MAX_TRIES)
}

/// 本机这个端口上是否已经有程序在应答。Windows 上连接没人监听的本机端口要重试约 2 秒才报拒绝，
/// 所以给个短超时：超时当作没人。
async fn answered(port: u16) -> bool {
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    matches!(tokio::time::timeout(Duration::from_millis(400), tokio::net::TcpStream::connect(addr)).await, Ok(Ok(_)))
}

async fn try_bind(addr: SocketAddr) -> std::io::Result<TcpListener> {
    // 只有监听所有网卡或回环时，浏览器才会去 127.0.0.1；监听某个具体网卡地址时不用探测。
    if (addr.ip().is_unspecified() || addr.ip().is_loopback()) && answered(addr.port()).await {
        return Err(std::io::ErrorKind::AddrInUse.into());
    }
    TcpListener::bind(addr).await
}

/// 选端口并开始监听。`explicit` 为用户指定的地址；否则按记住的端口（没有则 1234）开始找，用上后记下来。
pub async fn bind(explicit: Option<SocketAddr>, store: &Store) -> Result<TcpListener> {
    if let Some(addr) = explicit {
        return match try_bind(addr).await {
            Ok(l) => Ok(l),
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
                bail!("端口 {} 已被其他程序占用，可以用 --listen 换一个端口", addr.port())
            }
            Err(e) => Err(e).with_context(|| format!("无法监听 {addr}")),
        };
    }
    let remembered = store.get::<u16>(KEY).ok().flatten().filter(|p| *p > 0);
    let start = remembered.unwrap_or(DEFAULT_PORT);
    for port in candidates(start) {
        let addr = SocketAddr::new(DEFAULT.ip(), port);
        match try_bind(addr).await {
            Ok(l) => {
                if remembered != Some(port) {
                    if port != start {
                        tracing::info!(from = start, port, "管理台端口被占用，改用其他端口");
                    }
                    if let Err(e) = store.set(KEY, &port) {
                        tracing::warn!(error = %e, "无法记住管理台端口");
                    }
                }
                return Ok(l);
            }
            // 被占用；Windows 上落在系统保留端口段（Hyper-V/WSL 常见）时报的是“权限不足”
            Err(e) if matches!(e.kind(), std::io::ErrorKind::AddrInUse | std::io::ErrorKind::PermissionDenied) => {
                tracing::debug!(port, error = %e, "端口不可用");
            }
            Err(e) => return Err(e).with_context(|| format!("无法监听 {addr}")),
        }
    }
    bail!("从 {start} 开始找不到可用的端口，可以用 --listen 指定一个")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidates_step_by_ten_without_overflow() {
        assert_eq!(candidates(1234).take(3).collect::<Vec<_>>(), [1234, 1244, 1254]);
        assert_eq!(candidates(65530).collect::<Vec<_>>(), [65530]);
        assert_eq!(candidates(1234).count(), MAX_TRIES);
    }

    #[tokio::test]
    async fn skips_busy_port_and_remembers() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("db.sqlite")).unwrap();
        // 占住一个回环端口（模拟别的程序只监听 127.0.0.1），从它开始找
        let busy = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let busy_port = busy.local_addr().unwrap().port();
        if busy_port > u16::MAX - 2 * STEP {
            return;
        }
        store.set(KEY, &busy_port).unwrap();
        let l = bind(None, &store).await.unwrap();
        let got = l.local_addr().unwrap().port();
        assert_ne!(got, busy_port);
        assert_eq!((got - busy_port) % STEP, 0);
        assert_eq!(store.get::<u16>(KEY).unwrap(), Some(got));
        drop(l);
        // 下次从记住的端口开始
        let l = bind(None, &store).await.unwrap();
        assert_eq!(l.local_addr().unwrap().port(), got);
    }

    #[tokio::test]
    async fn explicit_busy_port_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("db.sqlite")).unwrap();
        let busy = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let addr = SocketAddr::new(Ipv4Addr::UNSPECIFIED.into(), busy.local_addr().unwrap().port());
        assert!(bind(Some(addr), &store).await.is_err());
        assert_eq!(store.get::<u16>(KEY).unwrap(), None);
    }
}
