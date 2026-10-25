//! DataChannel → 本机路由（服务端 docs/06 §6.2、§8.1）。
//!
//! 每个 HTTP 请求或 WebSocket 占一条通道，首帧是控制帧：
//! - `{"t":"req","m":"GET","p":"/api/x","h":[[k,v],…]}`：之后是请求体分块，`{"t":"end"}` 结束。请求带上
//!   [`Access::Rtc`] 交给现有的 `gateway::router`（与远程入口同一套路由与检查）；回 `{"t":"res","s":…,"h":…}`、
//!   响应体分块、`{"t":"end"}`，然后关闭通道。浏览器提前关闭通道就是放弃这个请求，这边随之丢弃。
//!   首帧带 `"z":"gzip"` 时，文本类、长度已知的响应体压缩后再发，`res` 帧带 `"z":"gzip"`，由对端解开（[`gzip_worth`]）。
//! - `{"t":"ws","p":"/i/{id}/ws","proto":[…]}`：在内存里用 hyper 跑一条连接接到同一个路由上，再用 WebSocket 客户端
//!   握手，终端的口令、`ensure_attachable`、更新保护等都沿用 `gateway::terminal` 的逻辑。成功回 `{"t":"open","proto":…}`，
//!   失败回 `{"t":"close","code":…}`；之后每条 WebSocket 消息对应一条（大的拆成几帧）数据帧。首帧带 `"resume": true` 时
//!   `open` 帧带续接令牌，通道断了还能经新连接上的 `{"t":"resume",…}` 接回去（[`resume`](super::resume)）。
//! - `ctl` 通道：`{"t":"hello","proto":1}` 握手，回本机设备 id。

use std::convert::Infallible;
use std::io::Write;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use axum::body::Body;
use axum::http::{header, HeaderName, HeaderValue, Method, Request};
use axum::Router;
use bytes::Bytes;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::{self, Message};

use super::frame::{self, Frame};
use super::peer::{Channel, DcEvent};
use super::resume::{self, In, Out, Port, Terms};
use crate::gateway::Access;

/// 一条 PeerConnection 上所有通道共用的上下文。
#[derive(Clone)]
pub struct Ctx {
    pub router: Router,
    /// 浏览器看到的稳定地址（`alice.looklook.example`），作为请求的 Host。
    pub host: String,
    /// 稳定地址的源（授权票据里平台签的）。
    pub origin: String,
    pub sid: String,
    pub device: String,
    /// 可续接的终端会话（属于整个 `Rtc`，不随这条连接结束）。
    pub terms: Arc<Terms>,
    /// 子站会话的绝对期限：终端会话最晚到这时结束。
    pub expires: tokio::time::Instant,
}

impl Ctx {
    fn access(&self) -> Access {
        Access::Rtc { origin: self.origin.clone() }
    }
}

/// 服务一条通道，直到它关闭或请求完成。
pub async fn serve(ctx: Ctx, mut ch: Channel) {
    let r = if ch.label == "ctl" {
        control(&ctx, &mut ch).await
    } else {
        match ch.recv().await.as_deref().map(frame::parse) {
            Some(Some(Frame::Ctl(head))) => match frame::kind(&head) {
                "req" => http(&ctx, &mut ch, &head).await,
                "ws" => websocket(&ctx, &mut ch, &head).await,
                "resume" => resume_ws(&ctx, &mut ch, &head).await,
                k => Err(anyhow::anyhow!("未知的首帧类型 {k:?}")),
            },
            Some(_) => Err(anyhow::anyhow!("首帧不是控制帧")),
            None => Ok(()),
        }
    };
    if let Err(e) = r {
        tracing::debug!(label = %ch.label, error = %format!("{e:#}"), "通道结束");
    }
}

async fn control(ctx: &Ctx, ch: &mut Channel) -> Result<()> {
    while let Some(m) = ch.recv().await {
        if let Some(Frame::Ctl(v)) = frame::parse(&m) {
            if frame::kind(&v) == "hello" {
                ch.send(&frame::ctl(&json!({ "t": "hello", "proto": looklook_protocol::rtc::PROTO, "dev": ctx.device })))?;
            }
        }
    }
    Ok(())
}

/// 浏览器给的路径：必须是本站的绝对路径。
fn path_of(head: &Value) -> Result<&str> {
    let p = head.get("p").and_then(Value::as_str).unwrap_or("");
    if !p.starts_with('/') || p.starts_with("//") || p.contains(['\r', '\n']) {
        bail!("路径不合法");
    }
    Ok(p)
}

/// 不能由浏览器决定的请求头：连接相关、Host、平台保留的 `x-ll-*`。
fn header_allowed(name: &HeaderName) -> bool {
    let n = name.as_str();
    !(matches!(n, "host" | "connection" | "keep-alive" | "upgrade" | "te" | "trailer" | "transfer-encoding" | "proxy-connection")
        || n.starts_with("x-ll-")
        || n.starts_with("sec-websocket-"))
}

fn headers_of(head: &Value) -> Vec<(HeaderName, HeaderValue)> {
    head.get("h")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|kv| {
            let (k, v) = (kv.get(0)?.as_str()?, kv.get(1)?.as_str()?);
            let name = HeaderName::from_bytes(k.as_bytes()).ok().filter(header_allowed)?;
            Some((name, HeaderValue::from_str(v).ok()?))
        })
        .collect()
}

/// 请求体：把收到的数据帧交给路由；`end` 或路由不再读取后丢弃后续数据。
///
/// 用无界队列：这边在 `select!` 里同时驱动路由与收消息，等队列腾出空位会卡住正在读它的路由。
/// libdatachannel 的回调模式本来也没有接收端背压，积压的上限是浏览器那边的发送窗口。
struct Upload(Option<mpsc::UnboundedSender<Result<Bytes, Infallible>>>);

impl Upload {
    fn feed(&mut self, msg: &[u8]) {
        match frame::parse(msg) {
            Some(Frame::Data { data, .. }) => {
                if let Some(tx) = &self.0 {
                    if tx.send(Ok(Bytes::copy_from_slice(data))).is_err() {
                        self.0 = None;
                    }
                }
            }
            Some(Frame::Ctl(v)) if frame::kind(&v) == "end" => self.0 = None,
            _ => {}
        }
    }
}

/// 小于这么多字节的响应体不压缩（省不了几个字节，还多一个 gzip 头）。
const GZIP_MIN: u64 = 1024;

/// 对端能解压（首帧带 `"z":"gzip"`）时，这个响应值不值得压缩：文本类（不含 SSE）、没压缩过、长度已知且不太小。
///
/// Service Worker 转来的请求里没有 `Accept-Encoding`（浏览器在网络层才加），路由也就不压缩，走中转时受中转带宽限制，
/// 所以在通道这一层压缩。只压长度已知的响应：一次性给出的，攒着压不会拖慢；流式的（SSE、长轮询）原样发。
fn gzip_worth(head: &Value, method: &Method, headers: &axum::http::HeaderMap) -> bool {
    if head.get("z").and_then(Value::as_str) != Some("gzip") || *method == Method::HEAD || headers.contains_key(header::CONTENT_ENCODING) {
        return false;
    }
    let len = headers.get(header::CONTENT_LENGTH).and_then(|v| v.to_str().ok()?.parse::<u64>().ok());
    if !len.is_some_and(|n| n >= GZIP_MIN) {
        return false;
    }
    let ct = headers.get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or("").to_ascii_lowercase();
    (ct.starts_with("text/") && !ct.starts_with("text/event-stream")) || ["json", "javascript", "xml", "wasm"].iter().any(|k| ct.contains(k))
}

async fn http(ctx: &Ctx, ch: &mut Channel, head: &Value) -> Result<()> {
    let method = Method::from_bytes(head.get("m").and_then(Value::as_str).unwrap_or("GET").as_bytes()).context("请求方法不合法")?;
    let mut req = Request::builder().method(method.clone()).uri(path_of(head)?);
    for (k, v) in headers_of(head) {
        req = req.header(k, v);
    }
    let (tx, rx) = mpsc::unbounded_channel();
    let body = Body::from_stream(futures_util::stream::unfold(rx, |mut rx| async move { rx.recv().await.map(|b| (b, rx)) }));
    let mut req = req.header(header::HOST, &ctx.host).body(body)?;
    req.extensions_mut().insert(ctx.access());
    let mut upload = Upload(Some(tx));

    use tower::ServiceExt;
    let fut = ctx.router.clone().oneshot(req);
    tokio::pin!(fut);
    let resp = loop {
        tokio::select! {
            r = &mut fut => break r.unwrap_or_else(|e| match e {}),
            ev = ch.event() => match ev {
                Some(DcEvent::Msg(m)) => upload.feed(&m),
                Some(_) => {}
                None => return Ok(()), // 浏览器放弃了这个请求
            },
        }
    };

    let (parts, mut body) = resp.into_parts();
    let h: Vec<Value> = parts.headers.iter().filter_map(|(k, v)| Some(json!([k.as_str(), v.to_str().ok()?]))).collect();
    // 头原样给（Content-Length 是解开后的长度，对端解开后照样对）
    let mut gz = gzip_worth(head, &method, &parts.headers).then(|| flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default()));
    let mut res = json!({ "t": "res", "s": parts.status.as_u16(), "h": h });
    if gz.is_some() {
        res["z"] = json!("gzip");
    }
    ch.send(&frame::ctl(&res))?;
    let mut writable = true;
    loop {
        tokio::select! {
            ev = ch.event() => match ev {
                Some(DcEvent::Msg(m)) => upload.feed(&m),
                Some(DcEvent::Low) => writable = true,
                Some(DcEvent::Open) => {}
                Some(DcEvent::Closed) | None => return Ok(()),
            },
            f = body.frame(), if writable => match f {
                None => {
                    if let Some(z) = gz.take() {
                        for c in z.finish()?.chunks(frame::CHUNK) {
                            ch.send(&frame::data(false, false, c))?;
                        }
                    }
                    break;
                }
                Some(Err(e)) => {
                    ch.send(&frame::ctl(&json!({ "t": "err" })))?;
                    bail!("响应体出错：{e}");
                }
                Some(Ok(f)) => {
                    if let Ok(data) = f.into_data() {
                        let data = match &mut gz {
                            Some(z) => {
                                z.write_all(&data)?;
                                Bytes::from(std::mem::take(z.get_mut()))
                            }
                            None => data,
                        };
                        for c in data.chunks(frame::CHUNK) {
                            ch.send(&frame::data(false, false, c))?;
                        }
                        writable = !ch.congested();
                    }
                }
            },
        }
    }
    ch.send(&frame::ctl(&json!({ "t": "end" })))?;
    Ok(())
}

async fn websocket(ctx: &Ctx, ch: &mut Channel, head: &Value) -> Result<()> {
    use tungstenite::client::IntoClientRequest;
    let path = path_of(head)?;
    let protos: Vec<&str> = head.get("proto").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).collect();
    let mut req = format!("ws://{}{path}", ctx.host).into_client_request()?;
    if !protos.is_empty() {
        req.headers_mut().insert(header::SEC_WEBSOCKET_PROTOCOL, HeaderValue::from_str(&protos.join(", "))?);
    }

    // 内存里的一条 HTTP/1.1 连接，服务端是本机路由（WebSocket 升级要经过 hyper）。
    let (client_io, server_io) = tokio::io::duplex(256 * 1024);
    let svc = ctx.router.clone().layer(axum::Extension(ctx.access()));
    let server = tokio::spawn(async move {
        let svc = hyper_util::service::TowerToHyperService::new(svc);
        let _ = hyper::server::conn::http1::Builder::new().serve_connection(hyper_util::rt::TokioIo::new(server_io), svc).with_upgrades().await;
    });
    let server = AbortOnDrop(server);

    let (ws, resp) = match tokio_tungstenite::client_async(req, client_io).await {
        Ok(r) => r,
        Err(e) => {
            let code = match &e {
                tungstenite::Error::Http(r) => r.status().as_u16(),
                _ => 1011,
            };
            ch.send(&frame::ctl(&json!({ "t": "close", "code": code })))?;
            bail!("WebSocket 握手失败：{e}");
        }
    };
    let proto = resp.headers().get(header::SEC_WEBSOCKET_PROTOCOL).and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
    let resumable = head.get("resume").and_then(Value::as_bool) == Some(true);
    let (port, key) = resume::spawn(&ctx.terms, &ctx.sid, ws, server, resumable, ctx.expires);
    ch.send(&frame::ctl(&json!({ "t": "open", "proto": proto, "resume": key })))?;
    attach(ch, port).await
}

/// `{"t":"resume","k":令牌,"rx":已收到的消息数}`：把这条通道接到保留中的终端会话上。
async fn resume_ws(ctx: &Ctx, ch: &mut Channel, head: &Value) -> Result<()> {
    let key = head.get("k").and_then(Value::as_str).unwrap_or("");
    let rx = head.get("rx").and_then(Value::as_u64).unwrap_or(0);
    let Some((port, received)) = ctx.terms.resume(&ctx.sid, key, rx).await else {
        ch.send(&frame::ctl(&json!({ "t": "close", "code": 1006 })))?;
        bail!("续接失败");
    };
    tracing::debug!(rx, received, "终端续接");
    ch.send(&frame::ctl(&json!({ "t": "resumed", "rx": received })))?;
    attach(ch, port).await
}

/// 通道 ↔ 终端会话：浏览器的消息（大的拆成几帧，拼回一条）交给会话，会话的输出按发送缓冲的水位发出去。
/// 通道断了就返回，会话自己决定是保留还是结束。
async fn attach(ch: &mut Channel, mut port: Port) -> Result<()> {
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
                            if port.inp.send(In::Msg(msg)).is_err() {
                                return Ok(());
                            }
                        }
                    }
                    Some(Frame::Ctl(v)) if frame::kind(&v) == "close" => {
                        let _ = port.inp.send(In::Close);
                        return Ok(());
                    }
                    _ => {}
                },
                Some(DcEvent::Low) => writable = true,
                Some(DcEvent::Open) => {}
                Some(DcEvent::Closed) | None => return Ok(()),
            },
            o = port.out.recv(), if writable => match o {
                Some(Out::Msg(text, bytes)) => {
                    for f in frame::split(text, &bytes) {
                        ch.send(&f)?;
                    }
                    writable = !ch.congested();
                }
                Some(Out::Close(code)) => {
                    ch.send(&frame::ctl(&json!({ "t": "close", "code": code })))?;
                    return Ok(());
                }
                None => {
                    ch.send(&frame::ctl(&json!({ "t": "close", "code": 1006 })))?;
                    return Ok(());
                }
            },
        }
    }
}

struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_and_headers() {
        assert!(path_of(&json!({"p": "/api/x?y=1"})).is_ok());
        for bad in ["", "api", "//evil.example/x", "/a\r\nb"] {
            assert!(path_of(&json!({ "p": bad })).is_err(), "{bad:?}");
        }
        let h = headers_of(&json!({"h": [["Content-Type", "text/plain"], ["host", "evil"], ["X-LL-Session-Cap", "x"], ["x-looklook-client", "1"], ["bad name", "v"]]}));
        let names: Vec<&str> = h.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(names, ["content-type", "x-looklook-client"]);
    }
}
