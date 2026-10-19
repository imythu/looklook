//! 本机 App 连同一账户的其他电脑（服务端 docs/06 §14）：这台电脑作为 WebRTC 发起方，和浏览器的启动页做的事一样。
//!
//! - **设备列表**（[`Outbound::devices`]）：设备签名向平台取同一账户的电脑、在线状态、终端路由表与 ICE 服务器，缓存 10 秒；
//! - **建连**（[`Outbound::link`]）：每台电脑一条 PeerConnection，用到时才建立。offer 经平台交给那台电脑（平台签发授权票据，
//!   会话是平台替这台电脑开的用户站会话），平台校验过 answer 上那台电脑的签名，这边再按 answer 里的指纹校验 DTLS；
//!   连上后在 `ctl` 通道上握手，确认对端就是那台电脑；
//! - **转发**（[`http`]、[`websocket`]）：本机管理台发给那台电脑的请求，每个占一条通道，帧格式见 `frame`，
//!   那台电脑按 `Access::Rtc` 处理（与浏览器远程打开相同的路由与检查）。
//!
//! 只转发本机（`Peer::Local`）打开的管理台的请求，见 `gateway::to_peer`。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use axum::body::Body;
use axum::extract::Request;
use axum::http::{header, HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use http_body_util::BodyExt;
use looklook_protocol::dto::RemoteDevices;
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::{self, protocol::Role, Message};
use tokio_util::sync::CancellationToken;

use super::frame::{self, Frame};
use super::peer::{Channel, DcEvent, Pc, PcState};
use crate::account::Account;
use crate::error::LocalError;

/// 等候选收集完的上限。
const GATHER_LIMIT: Duration = Duration::from_secs(3);
/// 建一条连接（含平台转交 offer、ICE、DTLS、握手）的期限。
const CONNECT_LIMIT: Duration = Duration::from_secs(25);
const OPEN_LIMIT: Duration = Duration::from_secs(15);
const HELLO_LIMIT: Duration = Duration::from_secs(5);
/// 断开后等它自己恢复的时间，过了就关掉，下一个请求重新建连。
const DISCONNECT_GRACE: Duration = Duration::from_secs(20);
/// 设备列表的缓存时间。
const DEVICES_TTL: Duration = Duration::from_secs(10);

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// 到一台电脑的连接。
pub struct Link {
    closed: CancellationToken,
    // 释放时先关通道，再关连接（见 peer.rs）
    ctl: Mutex<Option<Channel>>,
    pc: Mutex<Option<Pc>>,
}

impl Link {
    fn channel(&self, label: &str) -> Result<Channel> {
        if self.closed.is_cancelled() {
            bail!("连接已关闭");
        }
        lock(&self.pc).as_mut().ok_or_else(|| anyhow!("连接已关闭"))?.channel(label)
    }

    fn alive(&self) -> bool {
        !self.closed.is_cancelled()
    }
}

impl Drop for Link {
    fn drop(&mut self) {
        let ctl = lock(&self.ctl).take();
        let pc = lock(&self.pc).take();
        // rtcDelete* 会等回调结束：不在异步线程上做
        std::thread::spawn(move || {
            drop(ctl);
            drop(pc);
        });
    }
}

/// 一条转发用的通道：持有连接，保证通道先于连接释放（字段按声明顺序释放）。
struct Pipe {
    ch: Channel,
    _link: Arc<Link>,
}

/// 建连失败的原因（界面按错误码显示文案，见 error.json）。
#[derive(Debug)]
pub enum LinkError {
    /// 那台电脑不在线（或者不是这个账户的）。
    Offline,
    /// 平台拒绝了（未登录、会员到期等），原样告诉界面。
    Platform(crate::platform::PlatformError),
    /// 连接没能建立。
    Failed(anyhow::Error),
}

impl LinkError {
    pub fn response(&self) -> Response {
        match self {
            LinkError::Offline => LocalError { status: StatusCode::SERVICE_UNAVAILABLE, code: "DEVICE_OFFLINE".into(), params: Default::default() }.into_response(),
            LinkError::Platform(e) => {
                if e.is("DEVICE_OFFLINE") || e.is("NOT_FOUND") {
                    return LinkError::Offline.response();
                }
                LocalError::from(e.clone()).into_response()
            }
            LinkError::Failed(e) => {
                tracing::info!(error = %format!("{e:#}"), "连不上另一台电脑");
                LocalError { status: StatusCode::BAD_GATEWAY, code: "RTC_FAILED".into(), params: Default::default() }.into_response()
            }
        }
    }
}

#[derive(Default)]
pub struct Outbound {
    /// 每台电脑一个槽：同一台的并发请求只建一条连接。
    slots: Mutex<HashMap<String, Arc<tokio::sync::Mutex<Option<Arc<Link>>>>>>,
    devices: tokio::sync::Mutex<Option<(Instant, RemoteDevices)>>,
    /// 终端 id → 所在的电脑（设备列表的终端路由表，加上管理台在终端地址上给的 `__lldev`）。
    owners: Mutex<HashMap<String, String>>,
}

impl Outbound {
    /// 同一账户的电脑（缓存 [`DEVICES_TTL`]）。`fresh`：不用缓存。
    pub async fn devices(&self, account: &Account, fresh: bool) -> Result<RemoteDevices, crate::platform::PlatformError> {
        let mut g = self.devices.lock().await;
        if let Some((at, d)) = g.as_ref() {
            if !fresh && at.elapsed() < DEVICES_TTL {
                return Ok(d.clone());
            }
        }
        let d = account.rtc_devices().await?;
        {
            let mut o = lock(&self.owners);
            for dev in &d.items {
                for t in &dev.terminals {
                    o.insert(t.clone(), dev.id.clone());
                }
            }
        }
        *g = Some((Instant::now(), d.clone()));
        Ok(d)
    }

    pub fn remember(&self, terminal: &str, dev: &str) {
        lock(&self.owners).insert(terminal.to_string(), dev.to_string());
    }

    pub fn owner(&self, terminal: &str) -> Option<String> {
        lock(&self.owners).get(terminal).cloned()
    }

    /// 关掉全部连接（退出登录、账户停用时）。
    pub fn close_all(&self) {
        for slot in lock(&self.slots).drain().map(|(_, s)| s) {
            if let Ok(mut g) = slot.try_lock() {
                if let Some(l) = g.take() {
                    l.closed.cancel();
                }
            }
        }
    }

    /// 账户状态变化时：退出登录或不能使用就关掉连接。
    pub async fn watch(self: Arc<Self>, account: Arc<Account>) {
        let mut rx = account.subscribe();
        while rx.changed().await.is_ok() {
            if account.session().is_none() || !account.gate().allowed() {
                self.close_all();
            }
        }
    }

    /// 到这台电脑的连接：有可用的就复用，没有就建一条。
    pub async fn link(&self, account: &Arc<Account>, dev: &str) -> Result<Arc<Link>, LinkError> {
        let slot = lock(&self.slots).entry(dev.to_string()).or_default().clone();
        let mut g = slot.lock().await;
        if let Some(l) = g.as_ref().filter(|l| l.alive()) {
            return Ok(l.clone());
        }
        let l = match tokio::time::timeout(CONNECT_LIMIT, self.connect(account, dev)).await {
            Ok(r) => r?,
            Err(_) => return Err(LinkError::Failed(anyhow!("建立连接超时"))),
        };
        *g = Some(l.clone());
        Ok(l)
    }

    async fn connect(&self, account: &Arc<Account>, dev: &str) -> Result<Arc<Link>, LinkError> {
        let mut devices = self.devices(account, false).await.map_err(LinkError::Platform)?;
        if !devices.items.iter().any(|d| d.id == dev && d.online) {
            // 刚上线的电脑：缓存里可能还是离线
            devices = self.devices(account, true).await.map_err(LinkError::Platform)?;
            if !devices.items.iter().any(|d| d.id == dev && d.online) {
                return Err(LinkError::Offline);
            }
        }
        let (mut pc, mut ctl) = Pc::offer(&devices.ice_servers).map_err(LinkError::Failed)?;
        let offer = pc.local_offer(GATHER_LIMIT).await.map_err(LinkError::Failed)?;
        let resp = account.rtc_connect(dev, offer).await.map_err(LinkError::Platform)?;
        pc.set_answer(&resp.answer).map_err(LinkError::Failed)?;
        let state = pc.state.clone();
        ctl.opened(OPEN_LIMIT).await.map_err(LinkError::Failed)?;
        ctl.send(&frame::ctl(&json!({ "t": "hello", "proto": looklook_protocol::rtc::PROTO }))).map_err(LinkError::Failed)?;
        let hello = tokio::time::timeout(HELLO_LIMIT, async {
            while let Some(m) = ctl.recv().await {
                if let Some(Frame::Ctl(v)) = frame::parse(&m) {
                    if frame::kind(&v) == "hello" {
                        return Some(v);
                    }
                }
            }
            None
        })
        .await
        .ok()
        .flatten()
        .ok_or_else(|| LinkError::Failed(anyhow!("握手没有回应")))?;
        if hello.get("dev").and_then(Value::as_str) != Some(dev) {
            return Err(LinkError::Failed(anyhow!("对端不是要连的电脑")));
        }
        tracing::info!(dev, "已连上另一台电脑");
        let link = Arc::new(Link { closed: CancellationToken::new(), ctl: Mutex::new(Some(ctl)), pc: Mutex::new(Some(pc)) });
        tokio::spawn(watch_link(link.closed.clone(), state, dev.to_string()));
        Ok(link)
    }
}

/// 连接失败、关闭，或断开超过 [`DISCONNECT_GRACE`] 时标记为关闭（下一个请求重新建连）。
async fn watch_link(closed: CancellationToken, mut state: tokio::sync::watch::Receiver<PcState>, dev: String) {
    let why = loop {
        let st = *state.borrow_and_update();
        match st {
            PcState::Failed | PcState::Closed => break "failed",
            PcState::Disconnected => {
                let back = tokio::time::timeout(DISCONNECT_GRACE, state.wait_for(|s| *s == PcState::Connected)).await;
                if !matches!(back, Ok(Ok(_))) {
                    break "disconnected";
                }
                continue;
            }
            _ => {}
        }
        tokio::select! {
            _ = closed.cancelled() => break "closed",
            r = state.changed() => if r.is_err() { break "closed" },
        }
    };
    tracing::info!(dev, why, "到另一台电脑的连接关闭");
    closed.cancel();
}

/// 不转给那台电脑的请求头：连接相关、Host、Cookie（本机的访问码 Cookie）、来源（本机已经校验过，见 `gateway::to_peer`）、
/// 平台保留的 `x-ll-*`。
fn forward_header(name: &HeaderName) -> bool {
    let n = name.as_str();
    !(matches!(
        n,
        "host" | "connection" | "keep-alive" | "upgrade" | "te" | "trailer" | "transfer-encoding" | "proxy-connection" | "cookie" | "origin" | "referer"
    ) || n.starts_with("x-ll-")
        || n.starts_with("sec-websocket-"))
}

fn headers_json(h: &HeaderMap) -> Vec<Value> {
    h.iter().filter(|(k, _)| forward_header(k)).filter_map(|(k, v)| Some(json!([k.as_str(), v.to_str().ok()?]))).collect()
}

fn failed(e: anyhow::Error) -> Response {
    LinkError::Failed(e).response()
}

/// 转发一个 HTTP 请求。`path` 是去掉了 `__lldev` 的路径与查询。
pub async fn http(link: Arc<Link>, req: Request, path: String) -> Response {
    match http_inner(link, req, path).await {
        Ok(r) => r,
        Err(e) => failed(e),
    }
}

async fn http_inner(link: Arc<Link>, req: Request, path: String) -> Result<Response> {
    let (parts, mut body) = req.into_parts();
    let mut ch = link.channel("r")?;
    ch.opened(OPEN_LIMIT).await?;
    ch.send(&frame::ctl(&json!({ "t": "req", "m": parts.method.as_str(), "p": path, "h": headers_json(&parts.headers) })))?;
    while let Some(f) = body.frame().await {
        let f = f.map_err(|e| anyhow!("读取请求体失败：{e}"))?;
        let Ok(data) = f.into_data() else { continue };
        for c in data.chunks(frame::CHUNK) {
            ch.send(&frame::data(false, false, c))?;
            while ch.congested() {
                match ch.event().await {
                    Some(DcEvent::Low) => break,
                    Some(_) => {}
                    None => bail!("通道在上传时关闭"),
                }
            }
        }
    }
    ch.send(&frame::ctl(&json!({ "t": "end" })))?;

    let head = loop {
        let m = ch.recv().await.context("那台电脑没有回应")?;
        if let Some(Frame::Ctl(v)) = frame::parse(&m) {
            match frame::kind(&v) {
                "res" => break v,
                "err" => bail!("那台电脑处理请求出错"),
                _ => {}
            }
        }
    };
    let status = head.get("s").and_then(Value::as_u64).and_then(|s| StatusCode::from_u16(s as u16).ok()).unwrap_or(StatusCode::BAD_GATEWAY);
    let mut resp = Response::builder().status(status);
    for kv in head.get("h").and_then(Value::as_array).into_iter().flatten() {
        let (Some(k), Some(v)) = (kv.get(0).and_then(Value::as_str), kv.get(1).and_then(Value::as_str)) else { continue };
        let (Ok(k), Ok(v)) = (HeaderName::from_bytes(k.as_bytes()), HeaderValue::from_str(v)) else { continue };
        if matches!(k.as_str(), "connection" | "keep-alive" | "transfer-encoding" | "upgrade") {
            continue;
        }
        resp = resp.header(k, v);
    }
    // 响应体：交给一个任务从通道里读，读到 `end` 为止；管理台放弃了（不再读）就关掉通道。
    let (tx, rx) = mpsc::channel::<Result<Bytes, std::io::Error>>(16);
    let mut pipe = Pipe { ch, _link: link };
    tokio::spawn(async move {
        loop {
            let Some(m) = pipe.ch.recv().await else {
                let _ = tx.send(Err(std::io::Error::other("连接中断"))).await;
                return;
            };
            match frame::parse(&m) {
                Some(Frame::Data { data, .. }) => {
                    if tx.send(Ok(Bytes::copy_from_slice(data))).await.is_err() {
                        return;
                    }
                }
                Some(Frame::Ctl(v)) if frame::kind(&v) == "end" => return,
                Some(Frame::Ctl(v)) if frame::kind(&v) == "err" => {
                    let _ = tx.send(Err(std::io::Error::other("响应体出错"))).await;
                    return;
                }
                _ => {}
            }
        }
    });
    let stream = futures_util::stream::unfold(rx, |mut rx| async move { rx.recv().await.map(|b| (b, rx)) });
    Ok(resp.body(Body::from_stream(stream))?)
}

/// 转发一个 WebSocket（终端）：先在通道上和那台电脑握手，成功了再回 101。
pub async fn websocket(link: Arc<Link>, mut req: Request, path: String) -> Response {
    let Some(key) = req.headers().get(header::SEC_WEBSOCKET_KEY).cloned() else { return StatusCode::BAD_REQUEST.into_response() };
    let protos: Vec<String> = req
        .headers()
        .get(header::SEC_WEBSOCKET_PROTOCOL)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.split(',').map(|p| p.trim().to_string()).filter(|p| !p.is_empty()).collect())
        .unwrap_or_default();
    let upgrade = hyper::upgrade::on(&mut req);
    let open = async {
        let mut ch = link.channel("w")?;
        ch.opened(OPEN_LIMIT).await?;
        ch.send(&frame::ctl(&json!({ "t": "ws", "p": path, "proto": protos })))?;
        loop {
            let m = ch.recv().await.context("那台电脑没有回应")?;
            if let Some(Frame::Ctl(v)) = frame::parse(&m) {
                match frame::kind(&v) {
                    "open" => return Ok::<_, anyhow::Error>(Ok((ch, v.get("proto").and_then(Value::as_str).unwrap_or("").to_string()))),
                    "close" => return Ok(Err(v.get("code").and_then(Value::as_u64).unwrap_or(1011) as u16)),
                    _ => {}
                }
            }
        }
    };
    let (ch, proto) = match open.await {
        Ok(Ok(r)) => r,
        // 握手被拒（终端不存在、没有权限等）：HTTP 状态码照搬，其他当作网关错误
        Ok(Err(code)) => return StatusCode::from_u16(code).ok().filter(|s| s.as_u16() < 600 && s.as_u16() >= 400).unwrap_or(StatusCode::BAD_GATEWAY).into_response(),
        Err(e) => return failed(e),
    };
    let mut resp = Response::builder()
        .status(StatusCode::SWITCHING_PROTOCOLS)
        .header(header::CONNECTION, "upgrade")
        .header(header::UPGRADE, "websocket")
        .header(header::SEC_WEBSOCKET_ACCEPT, tungstenite::handshake::derive_accept_key(key.as_bytes()));
    if !proto.is_empty() {
        resp = resp.header(header::SEC_WEBSOCKET_PROTOCOL, proto);
    }
    let pipe = Pipe { ch, _link: link };
    tokio::spawn(async move {
        match upgrade.await {
            Ok(io) => {
                let ws = tokio_tungstenite::WebSocketStream::from_raw_socket(hyper_util::rt::TokioIo::new(io), Role::Server, None).await;
                if let Err(e) = pump(ws, pipe).await {
                    tracing::debug!(error = %format!("{e:#}"), "转发的 WebSocket 结束");
                }
            }
            Err(e) => tracing::debug!(error = %e, "WebSocket 升级失败"),
        }
    });
    resp.body(Body::empty()).unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

async fn pump<S>(mut ws: tokio_tungstenite::WebSocketStream<S>, mut pipe: Pipe) -> Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let ch = &mut pipe.ch;
    let (mut buf, mut buf_text) = (Vec::new(), false);
    let mut writable = true;
    loop {
        tokio::select! {
            ev = ch.event() => match ev {
                Some(DcEvent::Msg(m)) => match frame::parse(&m) {
                    Some(Frame::Data { text, more, data }) => {
                        if buf.is_empty() {
                            buf_text = text;
                        }
                        buf.extend_from_slice(data);
                        if !more {
                            let msg = std::mem::take(&mut buf);
                            let msg = if buf_text { Message::Text(String::from_utf8(msg).context("文本消息不是 UTF-8")?.into()) } else { Message::Binary(msg.into()) };
                            ws.send(msg).await?;
                        }
                    }
                    Some(Frame::Ctl(v)) if frame::kind(&v) == "close" => {
                        let _ = ws.close(None).await;
                        return Ok(());
                    }
                    _ => {}
                },
                Some(DcEvent::Low) => writable = true,
                Some(DcEvent::Open) => {}
                Some(DcEvent::Closed) | None => {
                    let _ = ws.close(None).await;
                    return Ok(());
                }
            },
            m = ws.next(), if writable => {
                let (text, bytes): (bool, Bytes) = match m {
                    Some(Ok(Message::Text(t))) => (true, Bytes::from(t)),
                    Some(Ok(Message::Binary(b))) => (false, b),
                    Some(Ok(Message::Close(_))) | None => {
                        let _ = ch.send(&frame::ctl(&json!({ "t": "close" })));
                        return Ok(());
                    }
                    Some(Ok(_)) => continue,
                    Some(Err(e)) => {
                        let _ = ch.send(&frame::ctl(&json!({ "t": "close" })));
                        bail!("WebSocket 出错：{e}");
                    }
                };
                for f in frame::split(text, &bytes) {
                    ch.send(&f)?;
                }
                writable = !ch.congested();
            }
        }
    }
}

/// 终端地址上管理台给的 `__lldev`（终端所在的电脑）：取出来，返回去掉它之后的路径与查询。
pub fn take_device_hint(path_and_query: &str) -> (String, Option<String>) {
    let Some((path, query)) = path_and_query.split_once('?') else { return (path_and_query.to_string(), None) };
    let mut hint = None;
    let rest: Vec<&str> = query
        .split('&')
        .filter(|kv| {
            if let Some(v) = kv.strip_prefix("__lldev=") {
                hint = Some(v.to_string());
                false
            } else {
                !kv.is_empty()
            }
        })
        .collect();
    let out = if rest.is_empty() { path.to_string() } else { format!("{path}?{}", rest.join("&")) };
    (out, hint.filter(|h| !h.is_empty() && h.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')))
}

/// 路径里的终端 id：`/i/{id}…` 或 `/api/instances/{id}…`。
pub fn terminal_of(path: &str) -> Option<&str> {
    let rest = path.strip_prefix("/i/").or_else(|| path.strip_prefix("/api/instances/"))?;
    let id = rest.split(['/', '?']).next()?;
    (!id.is_empty() && id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())).then_some(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_hint_and_terminal() {
        assert_eq!(take_device_hint("/i/abc/?__lldev=d1"), ("/i/abc/".into(), Some("d1".into())));
        assert_eq!(take_device_hint("/i/abc/ws?a=1&__lldev=d1&b=2"), ("/i/abc/ws?a=1&b=2".into(), Some("d1".into())));
        assert_eq!(take_device_hint("/api/x?y=1"), ("/api/x?y=1".into(), None));
        assert_eq!(take_device_hint("/i/abc/?__lldev=a%2Fb").1, None, "只接受设备 id 的字符");
        assert_eq!(terminal_of("/i/abcd2345/ws"), Some("abcd2345"));
        assert_eq!(terminal_of("/api/instances/abcd2345/chat/events"), Some("abcd2345"));
        assert_eq!(terminal_of("/api/instances"), None);
        assert_eq!(terminal_of("/api/instances?x"), None);
        assert_eq!(terminal_of("/api/status"), None);
    }

    #[test]
    fn headers_kept_for_peer() {
        let mut h = HeaderMap::new();
        for (k, v) in [("content-type", "application/json"), ("x-looklook-client", "1"), ("cookie", "ll_access=x"), ("origin", "http://127.0.0.1:1234"), ("x-ll-device", "d2"), ("host", "127.0.0.1:1234")] {
            h.insert(HeaderName::from_static(k), HeaderValue::from_static(v));
        }
        let names: Vec<String> = headers_json(&h).iter().map(|kv| kv[0].as_str().unwrap().to_string()).collect();
        assert_eq!(names, ["content-type", "x-looklook-client"]);
    }
}

/// 两条真实的 PeerConnection 在本机回环上对接：发起方（本模块）经通道发请求，应答方用 `bridge` 交给路由。
#[cfg(test)]
mod loopback {
    use axum::routing::{get, post};

    use super::*;
    use crate::rtc::bridge;

    async fn pair(router: axum::Router) -> Arc<Link> {
        let (mut a, mut ctl) = Pc::offer(&[]).unwrap();
        let offer = a.local_offer(Duration::from_secs(3)).await.unwrap();
        let mut b = Pc::answer(&[], &offer).unwrap();
        let answer = b.local_answer(Duration::from_secs(3)).await.unwrap();
        a.set_answer(&answer).unwrap();
        let ctx = bridge::Ctx { router, host: "u.looklook.test".into(), origin: "http://u.looklook.test".into(), sid: "s".into(), device: "devb".into() };
        tokio::spawn(async move {
            while let Some(ch) = b.incoming.recv().await {
                tokio::spawn(bridge::serve(ctx.clone(), ch));
            }
            drop(b);
        });
        ctl.opened(Duration::from_secs(10)).await.unwrap();
        ctl.send(&frame::ctl(&json!({ "t": "hello", "proto": 1 }))).unwrap();
        let m = ctl.recv().await.unwrap();
        let Some(Frame::Ctl(v)) = frame::parse(&m) else { panic!("hello") };
        assert_eq!(v["dev"], "devb");
        Arc::new(Link { closed: CancellationToken::new(), ctl: Mutex::new(Some(ctl)), pc: Mutex::new(Some(a)) })
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn requests_cross_the_channel() {
        let big = vec![b'x'; 300 * 1024];
        let big2 = big.clone();
        let router = axum::Router::new()
            .route("/api/echo", post(|h: HeaderMap, body: Bytes| async move { (StatusCode::CREATED, [("x-got-client", h.get("x-looklook-client").map(|v| v.to_str().unwrap().to_string()).unwrap_or_default())], body) }))
            .route("/api/big", get(move || async move { big2.clone() }));
        let link = pair(router).await;

        let req = Request::builder().method("POST").uri("/api/echo").header("x-looklook-client", "1").header("cookie", "secret").body(Body::from("hello")).unwrap();
        let r = http(link.clone(), req, "/api/echo".into()).await;
        assert_eq!(r.status(), StatusCode::CREATED);
        assert_eq!(r.headers()["x-got-client"], "1");
        assert_eq!(r.into_body().collect().await.unwrap().to_bytes(), "hello");

        // 大的响应体分成多帧
        let r = http(link.clone(), Request::get("/api/big").body(Body::empty()).unwrap(), "/api/big".into()).await;
        assert_eq!(r.status(), StatusCode::OK);
        assert_eq!(r.into_body().collect().await.unwrap().to_bytes().len(), big.len());

        // 大的请求体
        let req = Request::builder().method("POST").uri("/api/echo").body(Body::from(big.clone())).unwrap();
        let r = http(link.clone(), req, "/api/echo".into()).await;
        assert_eq!(r.into_body().collect().await.unwrap().to_bytes().len(), big.len());
    }

    /// 测试用的 WebSocket 回显（那台电脑上的终端）：子协议 tty。
    async fn echo_ws(mut req: Request) -> Response {
        let key = req.headers()[header::SEC_WEBSOCKET_KEY].clone();
        let upgrade = hyper::upgrade::on(&mut req);
        tokio::spawn(async move {
            let io = upgrade.await.unwrap();
            let mut ws = tokio_tungstenite::WebSocketStream::from_raw_socket(hyper_util::rt::TokioIo::new(io), Role::Server, None).await;
            while let Some(Ok(m)) = ws.next().await {
                if m.is_close() {
                    break;
                }
                ws.send(m).await.unwrap();
            }
        });
        Response::builder()
            .status(StatusCode::SWITCHING_PROTOCOLS)
            .header(header::CONNECTION, "upgrade")
            .header(header::UPGRADE, "websocket")
            .header(header::SEC_WEBSOCKET_ACCEPT, tungstenite::handshake::derive_accept_key(key.as_bytes()))
            .header(header::SEC_WEBSOCKET_PROTOCOL, "tty")
            .body(Body::empty())
            .unwrap()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn websocket_crosses_the_channel() {
        use tungstenite::client::IntoClientRequest;
        let link = pair(axum::Router::new().route("/i/abc/ws", get(echo_ws))).await;
        // 本机这边：一个只做转发的服务
        let app = axum::Router::new().fallback(move |req: Request| {
            let link = link.clone();
            async move { websocket(link, req, "/i/abc/ws".into()).await }
        });
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
        let mut req = format!("ws://{addr}/i/abc/ws").into_client_request().unwrap();
        req.headers_mut().insert(header::SEC_WEBSOCKET_PROTOCOL, HeaderValue::from_static("tty"));
        let tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
        let (mut ws, resp) = tokio_tungstenite::client_async(req, tcp).await.unwrap();
        assert_eq!(resp.headers()[header::SEC_WEBSOCKET_PROTOCOL], "tty");
        ws.send(Message::Text("hi".into())).await.unwrap();
        assert_eq!(ws.next().await.unwrap().unwrap(), Message::Text("hi".into()));
        let big = vec![7u8; 200 * 1024];
        ws.send(Message::Binary(big.clone().into())).await.unwrap();
        assert_eq!(ws.next().await.unwrap().unwrap().into_data().len(), big.len());
        ws.close(None).await.unwrap();
    }
}
