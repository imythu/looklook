//! 对话事件的多路 WebSocket（`GET /api/chat/ws`）：一个页面一条连接，按终端订阅、退订。
//!
//! 为什么不是每个对话一条 SSE：本机管理台是明文 http，浏览器只用 HTTP/1.1，同一地址最多 6 条连接（所有窗口、标签页共用）。
//! SSE 一直占着一条；挂着的对话（最近打开的最多 8 个）一多，发消息、停止、新开这些请求都在浏览器里排队，界面就点不动了。
//! WebSocket 升级后不占这 6 条。远程打开（平台 https 的 HTTP/2、WebRTC 经 Service Worker）没有这个限制，页面仍用 SSE。
//!
//! - 页面 → 这里：`{"op":"sub","id":"<终端>","device":"<电脑，可省>"}`、`{"op":"unsub","id":"<终端>"}`；
//! - 这里 → 页面：`{"id":…,"event":"snapshot|status|items|reset","data":…}`，和 SSE 的事件一样；
//!   订阅不成或者结束了：`{"id":…,"event":"error","data":{"status":…,"code":…,"params":…}}`，页面过一会儿重新订阅。
//!
//! 别的电脑上的终端：经 WebRTC 取它的 `chat/events`（SSE，旧版本也有），这里拆开再发给页面；
//! 和 [`super::to_peer`] 一样只替这台电脑自己（`Peer::Local`）打开的管理台转发。
//! 每个订阅一个任务，发往页面的队列有上限：页面收得慢时任务停在发送上，本机的事件就会“落后太多”，
//! 跟上时重新发一份快照（和 SSE 一样），不会无限堆积。

use std::collections::HashMap;
use std::pin::Pin;
use std::time::Duration;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Extension;
use futures_util::{SinkExt, Stream, StreamExt};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio::task::AbortHandle;
use tokio_tungstenite::tungstenite::{self, protocol::Role, Message};

use super::access::Peer;
use super::{origin_matches, proxy, Access, App};
use crate::error::LocalError;
use crate::rtc::outbound;

/// 一条连接最多订阅几个对话（页面最多挂 8 个会话，留余量）
const MAX_SUBS: usize = 64;
/// 发往页面的队列长度
const QUEUE: usize = 64;
/// 心跳：中间的代理不会因为空闲断开；页面已经没了时发送失败、及时收掉订阅
const PING: Duration = Duration::from_secs(15);
/// 别的电脑上的对话事件一条最大多少（快照里有最近的条目，几 MB 以内）
const MAX_EVENT: usize = 32 << 20;

type Feed = Pin<Box<dyn Stream<Item = (String, String)> + Send>>;

#[derive(Deserialize)]
struct Op {
    op: String,
    id: String,
    #[serde(default)]
    device: Option<String>,
}

pub async fn ws(State(app): State<App>, Extension(access): Extension<Access>, peer: Option<Extension<Peer>>, mut req: Request) -> Response {
    if !proxy::is_websocket(req.headers()) {
        return StatusCode::BAD_REQUEST.into_response();
    }
    // WebSocket 不受 CORS 限制：别的网站也能连 127.0.0.1，来源必须是管理台自己
    if !origin_matches(req.headers(), &access) {
        return (StatusCode::FORBIDDEN, "origin").into_response();
    }
    let Some(key) = req.headers().get(header::SEC_WEBSOCKET_KEY).cloned() else { return StatusCode::BAD_REQUEST.into_response() };
    let forward = peer.is_some_and(|p| p.0 == Peer::Local);
    let upgrade = hyper::upgrade::on(&mut req);
    tokio::spawn(async move {
        match upgrade.await {
            Ok(io) => {
                let ws = tokio_tungstenite::WebSocketStream::from_raw_socket(hyper_util::rt::TokioIo::new(io), Role::Server, None).await;
                let open = move |id: String, device: Option<String>| {
                    let app = app.clone();
                    async move { open_feed(app, id, device, forward).await }
                };
                serve(ws, open).await;
            }
            Err(e) => tracing::debug!(error = %e, "对话事件的 WebSocket 升级失败"),
        }
    });
    Response::builder()
        .status(StatusCode::SWITCHING_PROTOCOLS)
        .header(header::CONNECTION, "upgrade")
        .header(header::UPGRADE, "websocket")
        .header(header::SEC_WEBSOCKET_ACCEPT, tungstenite::handshake::derive_accept_key(key.as_bytes()))
        .body(Body::empty())
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

/// 一条连接：收订阅、退订，把各订阅的事件发给页面。`open` 打开一个对话的事件（[`open_feed`]；测试里换成假的）
async fn serve<S, F, Fut>(mut ws: tokio_tungstenite::WebSocketStream<S>, open: F)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    F: Fn(String, Option<String>) -> Fut,
    Fut: std::future::Future<Output = Result<Feed, LocalError>> + Send + 'static,
{
    let (tx, mut rx) = mpsc::channel::<String>(QUEUE);
    let mut subs: HashMap<String, AbortHandle> = HashMap::new();
    let mut ping = tokio::time::interval(PING);
    ping.tick().await;
    loop {
        tokio::select! {
            Some(text) = rx.recv() => {
                if ws.send(Message::Text(text.into())).await.is_err() {
                    break;
                }
            }
            m = ws.next() => match m {
                Some(Ok(Message::Text(t))) => {
                    let Ok(op) = serde_json::from_str::<Op>(&t) else { continue };
                    if !valid_id(&op.id) {
                        continue;
                    }
                    // 订阅过的再订阅：先停掉旧的（页面重新订阅时会重新拿一份快照）
                    if let Some(h) = subs.remove(&op.id) {
                        h.abort();
                    }
                    if op.op != "sub" {
                        continue;
                    }
                    if subs.len() >= MAX_SUBS {
                        let _ = tx.send(message(&op.id, "error", &error_data(&LocalError::new("TOO_MANY_REQUESTS")))).await;
                        continue;
                    }
                    let device = op.device.filter(|d| valid_device(d));
                    let task = tokio::spawn(run(op.id.clone(), open(op.id.clone(), device), tx.clone()));
                    subs.insert(op.id, task.abort_handle());
                }
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                Some(Ok(_)) => {}
            },
            _ = ping.tick() => {
                if ws.send(Message::Ping(Vec::new().into())).await.is_err() {
                    break;
                }
            }
        }
    }
    for h in subs.into_values() {
        h.abort();
    }
}

/// 打开一个对话的事件：本机的直接订阅，别的电脑上的经 WebRTC 取
async fn open_feed(app: App, id: String, device: Option<String>, forward: bool) -> Result<Feed, LocalError> {
    match peer_of(&app, &id, device, forward) {
        Some(dev) => remote_feed(&app, &id, &dev).await,
        None => super::chat::feed(app, id).await.map(|s| Box::pin(s.map(|(ev, data)| (ev.to_string(), data))) as Feed),
    }
}

/// 一个订阅：取事件发给页面；取不到或者结束了发 `error`
async fn run(id: String, feed: impl std::future::Future<Output = Result<Feed, LocalError>>, tx: mpsc::Sender<String>) {
    let mut feed = match feed.await {
        Ok(f) => f,
        Err(e) => {
            let _ = tx.send(message(&id, "error", &error_data(&e))).await;
            return;
        }
    };
    while let Some((ev, data)) = feed.next().await {
        if tx.send(message(&id, &ev, &data)).await.is_err() {
            return;
        }
    }
    let _ = tx.send(message(&id, "error", &error_data(&LocalError::new("CHAT_ENDED")))).await;
}

/// 这个终端在别的电脑上时是哪台（和 [`super::to_peer`] 的判断一样：页面给的、记下的终端路由）
fn peer_of(app: &App, id: &str, device: Option<String>, forward: bool) -> Option<String> {
    if !forward || super::api::local_only(app) {
        return None;
    }
    let own = app.account.session()?.device_id;
    device.or_else(|| app.outbound.owner(id)).filter(|d| *d != own)
}

/// 别的电脑上的对话：经 WebRTC 取它的 SSE，拆成事件
async fn remote_feed(app: &App, id: &str, dev: &str) -> Result<Feed, LocalError> {
    let link = app.outbound.link(&app.account, dev).await.map_err(|e| e.local())?;
    let path = format!("/api/instances/{id}/chat/events");
    let req = Request::builder().uri(&path).header(header::ACCEPT, "text/event-stream").body(Body::empty()).map_err(anyhow::Error::from)?;
    let resp = outbound::http(link, req, path).await;
    let status = resp.status();
    if !status.is_success() {
        let bytes = axum::body::to_bytes(resp.into_body(), 64 << 10).await.unwrap_or_default();
        let e = serde_json::from_slice::<Value>(&bytes).ok().and_then(|v| {
            let code = v["error"]["code"].as_str()?.to_string();
            Some(LocalError { status, code, params: v["error"]["params"].as_object().cloned().unwrap_or_default() })
        });
        return Err(e.unwrap_or(LocalError { status, code: "RTC_FAILED".into(), params: Default::default() }));
    }
    Ok(Box::pin(sse_events(resp.into_body().into_data_stream())))
}

/// SSE 正文拆成（事件名、data）。空行结束一个事件；`:` 开头的是心跳；多行 data 用换行连起来
fn sse_events<B, E>(body: B) -> impl Stream<Item = (String, String)> + Send
where
    B: Stream<Item = Result<bytes::Bytes, E>> + Send + Unpin + 'static,
{
    futures_util::stream::unfold((body, Vec::<u8>::new(), String::new(), String::new()), |(mut body, mut buf, mut ev, mut data)| async move {
        loop {
            if let Some(at) = buf.iter().position(|&b| b == b'\n') {
                let line = String::from_utf8_lossy(&buf[..at]).trim_end_matches('\r').to_string();
                buf.drain(..=at);
                if line.is_empty() {
                    if !data.is_empty() {
                        let out = (std::mem::replace(&mut ev, String::new()), std::mem::take(&mut data));
                        let name = if out.0.is_empty() { "message".to_string() } else { out.0 };
                        return Some(((name, out.1), (body, buf, ev, data)));
                    }
                    ev.clear();
                } else if let Some(v) = line.strip_prefix("event:") {
                    ev = v.trim_start().to_string();
                } else if let Some(v) = line.strip_prefix("data:") {
                    if !data.is_empty() {
                        data.push('\n');
                    }
                    data.push_str(v.strip_prefix(' ').unwrap_or(v));
                }
                continue;
            }
            if buf.len() > MAX_EVENT {
                return None;
            }
            match body.next().await {
                Some(Ok(chunk)) => buf.extend_from_slice(&chunk),
                _ => return None,
            }
        }
    })
}

/// 发给页面的一条：`data` 已经是 JSON 文本，原样嵌进去
fn message(id: &str, event: &str, data: &str) -> String {
    format!(r#"{{"id":{},"event":{},"data":{}}}"#, json!(id), json!(event), data)
}

fn error_data(e: &LocalError) -> String {
    json!({ "status": e.status.as_u16(), "code": e.code, "params": e.params }).to_string()
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
}

fn valid_device(d: &str) -> bool {
    !d.is_empty() && d.len() <= 128 && d.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn splits_sse() {
        let chunks: Vec<Result<bytes::Bytes, std::convert::Infallible>> = vec![
            Ok(bytes::Bytes::from(": keep-alive\n\nevent: snapshot\ndata: {\"a\":")),
            Ok(bytes::Bytes::from("1}\n\nevent: items\r\ndata: [1,\r\ndata: 2]\r\n\r\n")),
            Ok(bytes::Bytes::from("event: reset\ndata: {}\n\n")),
        ];
        let got: Vec<(String, String)> = sse_events(futures_util::stream::iter(chunks)).collect().await;
        assert_eq!(
            got,
            vec![("snapshot".into(), r#"{"a":1}"#.into()), ("items".into(), "[1,\n2]".into()), ("reset".into(), "{}".into())],
            "心跳跳过；拆在两块里的事件拼起来；CRLF 和多行 data"
        );
    }

    /// 真的 WebSocket（内存里的一对流）：订阅拿到事件、订阅不成拿到 error、退订后不再收到、坏消息不影响连接
    #[tokio::test]
    async fn subscribe_unsubscribe() {
        use tokio_tungstenite::WebSocketStream;
        let (a, b) = tokio::io::duplex(1 << 16);
        let server = WebSocketStream::from_raw_socket(a, Role::Server, None).await;
        let mut client = WebSocketStream::from_raw_socket(b, Role::Client, None).await;
        // “aa” 一直有事件（每 20 毫秒一条），“bb” 打不开
        let open = |id: String, _device: Option<String>| async move {
            if id == "bb" {
                return Err(LocalError::not_found());
            }
            let s = futures_util::stream::unfold(0u32, |n| async move {
                tokio::time::sleep(Duration::from_millis(20)).await;
                Some(((if n == 0 { "snapshot" } else { "status" }.to_string(), format!(r#"{{"n":{n}}}"#)), n + 1))
            });
            Ok(Box::pin(s) as Feed)
        };
        tokio::spawn(serve(server, open));
        async fn recv(c: &mut WebSocketStream<tokio::io::DuplexStream>) -> Value {
            loop {
                if let Message::Text(t) = c.next().await.unwrap().unwrap() {
                    return serde_json::from_str(&t).unwrap();
                }
            }
        }
        client.send(Message::Text("not json".into())).await.unwrap();
        client.send(Message::Text(r#"{"op":"sub","id":"aa"}"#.into())).await.unwrap();
        assert_eq!(recv(&mut client).await, json!({ "id": "aa", "event": "snapshot", "data": { "n": 0 } }));
        assert_eq!(recv(&mut client).await["event"], "status");
        client.send(Message::Text(r#"{"op":"sub","id":"bb"}"#.into())).await.unwrap();
        loop {
            let m = recv(&mut client).await;
            if m["id"] == "bb" {
                assert_eq!(m["event"], "error");
                assert_eq!(m["data"]["code"], "NOT_FOUND");
                assert_eq!(m["data"]["status"], 404);
                break;
            }
        }
        client.send(Message::Text(r#"{"op":"unsub","id":"aa"}"#.into())).await.unwrap();
        // 退订之前已经在路上的可能还有几条；之后安静下来
        tokio::time::sleep(Duration::from_millis(100)).await;
        while let Ok(Some(_)) = tokio::time::timeout(Duration::from_millis(10), client.next()).await {}
        assert!(tokio::time::timeout(Duration::from_millis(150), client.next()).await.is_err(), "退订后不再收到");
        // 重新订阅：先来一份快照
        client.send(Message::Text(r#"{"op":"sub","id":"aa"}"#.into())).await.unwrap();
        assert_eq!(recv(&mut client).await["event"], "snapshot");
    }

    #[test]
    fn message_embeds_json() {
        let m: Value = serde_json::from_str(&message("ab1", "status", r#"{"phase":"idle"}"#)).unwrap();
        assert_eq!(m, json!({ "id": "ab1", "event": "status", "data": { "phase": "idle" } }));
        assert!(!valid_id("Ab"), "只认小写字母和数字");
        assert!(!valid_device("a/b"));
    }
}
