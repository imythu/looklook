//! 信令长连接（服务端 gateway/signal.rs）：登录后一直连着主服务，主服务把浏览器的 offer 直接推过来，
//! 不再经中转转发；连着也就是平台眼里的“在线”。断开后按 1、2、5、10、30 秒退避重连。
//!
//! 每个 offer 交给 [`super::accept`]（与经中转来的同一套校验），回复 answer 或拒绝原因。

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use looklook_protocol::rtc::{OfferRequest, SignalMsg, SIGNAL_PING_SECS};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use tokio_util::sync::CancellationToken;

use crate::gateway::App;

const BACKOFF: [u64; 5] = [1, 2, 5, 10, 30];
/// 这么久收不到任何帧（主服务每 `SIGNAL_PING_SECS` 秒 ping 一次）就当连接已断，重连。
const IDLE: Duration = Duration::from_secs(SIGNAL_PING_SECS * 3);
/// 连了这么久才断的不算失败，下次从头退避。
const STABLE: Duration = Duration::from_secs(60);

pub async fn run(app: App, shutdown: CancellationToken) {
    let account = app.account.clone();
    let mut changed = account.subscribe();
    let mut failures = 0usize;
    loop {
        let device = account.session().map(|s| s.device_id);
        if device.is_none() {
            failures = 0;
            tokio::select! {
                _ = shutdown.cancelled() => return,
                r = changed.changed() => if r.is_err() { return },
            }
            continue;
        }
        let t0 = tokio::time::Instant::now();
        let why = match account.signal_socket().await {
            None => "logged_out",
            Some(Ok(ws)) => {
                tracing::info!(ms = t0.elapsed().as_millis() as u64, "信令长连接已建立");
                let why = serve(&app, ws, &shutdown, &mut changed, device.as_deref()).await;
                tracing::info!(why, secs = t0.elapsed().as_secs(), "信令长连接断开");
                why
            }
            Some(Err(e)) => {
                // 旧服务端没有这个接口：照常经中转收 offer，过一会儿再试（服务端升级后就能用上）
                if failures == 0 {
                    tracing::warn!(error = %e, "信令长连接建立失败");
                } else {
                    tracing::debug!(error = %e, failures, "信令长连接建立失败");
                }
                "error"
            }
        };
        if why == "shutdown" {
            return;
        }
        if t0.elapsed() > STABLE {
            failures = 0;
        }
        let wait = Duration::from_secs(BACKOFF[failures.min(BACKOFF.len() - 1)]);
        failures += 1;
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => return,
                _ = tokio::time::sleep_until(deadline) => break,
                r = changed.changed() => {
                    if r.is_err() {
                        return;
                    }
                    // 重新登录、换了设备：马上连
                    if account.session().map(|s| s.device_id) != device {
                        failures = 0;
                        break;
                    }
                }
            }
        }
    }
}

async fn serve(
    app: &App,
    ws: tokio_tungstenite::WebSocketStream<reqwest::Upgraded>,
    shutdown: &CancellationToken,
    changed: &mut tokio::sync::watch::Receiver<u64>,
    device: Option<&str>,
) -> &'static str {
    let (mut sink, mut stream) = ws.split();
    let (tx, mut rx) = mpsc::channel::<SignalMsg>(16);
    let idle = tokio::time::sleep(IDLE);
    tokio::pin!(idle);
    let why = loop {
        tokio::select! {
            _ = shutdown.cancelled() => break "shutdown",
            _ = &mut idle => break "idle",
            m = stream.next() => match m {
                Some(Ok(msg)) => {
                    idle.as_mut().reset(tokio::time::Instant::now() + IDLE);
                    match msg {
                        Message::Text(t) => on_text(app, t.as_str(), &tx),
                        Message::Close(_) => break "closed",
                        _ => {}
                    }
                }
                _ => break "closed",
            },
            Some(out) = rx.recv() => {
                let text = serde_json::to_string(&out).expect("json");
                if sink.send(Message::Text(text.into())).await.is_err() {
                    break "send";
                }
            }
            r = changed.changed() => {
                if r.is_err() {
                    break "shutdown";
                }
                if app.account.session().map(|s| s.device_id).as_deref() != device {
                    break "session";
                }
            }
        }
    };
    let _ = sink.close().await;
    why
}

fn on_text(app: &App, text: &str, tx: &mpsc::Sender<SignalMsg>) {
    let Ok(SignalMsg::Offer { id, grant, offer }) = serde_json::from_str::<SignalMsg>(text) else {
        tracing::debug!("信令长连接：无法识别的消息");
        return;
    };
    let (app, tx) = (app.clone(), tx.clone());
    tokio::spawn(async move {
        let reply = match super::accept(&app, None, &OfferRequest { grant, offer }).await {
            Ok(r) => SignalMsg::Answer { id, answer: r.answer, key: r.key, sig: r.sig },
            Err((status, why)) => {
                tracing::debug!(why, "WebRTC 信令被拒绝");
                SignalMsg::Reject { id, status: status.as_u16(), why: why.into() }
            }
        };
        let _ = tx.send(reply).await;
    });
}
