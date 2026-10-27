//! 飞书长连接（docs/TASK_BOARD.md §6.2）：电脑主动连到飞书收事件和卡片回调，不需要公网地址。
//!
//! 照官方 Go SDK 的 `ws` 包（larksuite/oapi-sdk-go v3）移植：
//! 1. `POST {域名}/callback/ws/endpoint` 用 App ID / Secret 换连接地址和 `ClientConfig`；
//! 2. 连上后收发 protobuf 帧 `pbbp2.Frame`（二进制消息）。`method` 0 是控制帧（ping / pong，pong 里可能带新的
//!    `ClientConfig`），1 是数据帧（事件、回调；大的会拆成几片，按 `message_id` + `sum` / `seq` 拼回来）；
//! 3. 每个数据帧都要原样回一帧，payload 换成 `{"code":200,"headers":{},"data":<响应 JSON 的 base64>}`，
//!    卡片回调的响应（toast、新卡片）就放在 `data` 里，要在 3 秒内回；
//! 4. 每 `PingInterval` 秒发一次 ping；`2 × PingInterval + 5` 秒收不到任何帧就当断了，重连。
//!
//! 帧只有九个字段，手写编解码，不引入 prost。

use std::collections::HashMap;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::Message;

/// 一帧（字段号与 Go SDK 的 `pbbp2.pb.go` 一致）
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Frame {
    pub seq_id: u64,
    pub log_id: u64,
    pub service: i32,
    pub method: i32,
    pub headers: Vec<(String, String)>,
    pub payload_encoding: Option<String>,
    pub payload_type: Option<String>,
    pub payload: Option<Vec<u8>>,
    pub log_id_new: Option<String>,
}

pub(crate) const METHOD_CONTROL: i32 = 0;
pub(crate) const METHOD_DATA: i32 = 1;

fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

fn put_bytes(out: &mut Vec<u8>, field: u32, b: &[u8]) {
    put_varint(out, u64::from(field << 3 | 2));
    put_varint(out, b.len() as u64);
    out.extend_from_slice(b);
}

fn put_uint(out: &mut Vec<u8>, field: u32, v: u64) {
    put_varint(out, u64::from(field << 3));
    put_varint(out, v);
}

fn get_varint(b: &[u8], at: &mut usize) -> Result<u64> {
    let mut v = 0u64;
    for shift in (0..64).step_by(7) {
        let byte = *b.get(*at).ok_or_else(|| anyhow!("帧不完整"))?;
        *at += 1;
        v |= u64::from(byte & 0x7f) << shift;
        if byte < 0x80 {
            return Ok(v);
        }
    }
    bail!("varint 太长")
}

fn get_bytes<'a>(b: &'a [u8], at: &mut usize) -> Result<&'a [u8]> {
    let n = get_varint(b, at)? as usize;
    let end = at.checked_add(n).filter(|e| *e <= b.len()).ok_or_else(|| anyhow!("帧不完整"))?;
    let out = &b[*at..end];
    *at = end;
    Ok(out)
}

/// 跳过不认识的字段
fn skip(b: &[u8], at: &mut usize, wire: u64) -> Result<()> {
    match wire {
        0 => drop(get_varint(b, at)?),
        1 => *at += 8,
        2 => drop(get_bytes(b, at)?),
        5 => *at += 4,
        _ => bail!("不认识的 wire type {wire}"),
    }
    if *at > b.len() {
        bail!("帧不完整");
    }
    Ok(())
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

impl Frame {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        // proto2 的 required 字段即使是 0 也要写
        put_uint(&mut out, 1, self.seq_id);
        put_uint(&mut out, 2, self.log_id);
        put_uint(&mut out, 3, self.service as i64 as u64);
        put_uint(&mut out, 4, self.method as i64 as u64);
        for (k, v) in &self.headers {
            let mut h = Vec::new();
            put_bytes(&mut h, 1, k.as_bytes());
            put_bytes(&mut h, 2, v.as_bytes());
            put_bytes(&mut out, 5, &h);
        }
        if let Some(s) = &self.payload_encoding {
            put_bytes(&mut out, 6, s.as_bytes());
        }
        if let Some(s) = &self.payload_type {
            put_bytes(&mut out, 7, s.as_bytes());
        }
        if let Some(p) = &self.payload {
            put_bytes(&mut out, 8, p);
        }
        if let Some(s) = &self.log_id_new {
            put_bytes(&mut out, 9, s.as_bytes());
        }
        out
    }

    pub fn decode(b: &[u8]) -> Result<Self> {
        let mut f = Frame::default();
        let mut at = 0;
        while at < b.len() {
            let key = get_varint(b, &mut at)?;
            let (field, wire) = (key >> 3, key & 7);
            match (field, wire) {
                (1, 0) => f.seq_id = get_varint(b, &mut at)?,
                (2, 0) => f.log_id = get_varint(b, &mut at)?,
                (3, 0) => f.service = get_varint(b, &mut at)? as i32,
                (4, 0) => f.method = get_varint(b, &mut at)? as i32,
                (5, 2) => {
                    let h = get_bytes(b, &mut at)?;
                    let (mut k, mut v, mut i) = (String::new(), String::new(), 0);
                    while i < h.len() {
                        let key = get_varint(h, &mut i)?;
                        match (key >> 3, key & 7) {
                            (1, 2) => k = text(get_bytes(h, &mut i)?),
                            (2, 2) => v = text(get_bytes(h, &mut i)?),
                            (_, w) => skip(h, &mut i, w)?,
                        }
                    }
                    f.headers.push((k, v));
                }
                (6, 2) => f.payload_encoding = Some(text(get_bytes(b, &mut at)?)),
                (7, 2) => f.payload_type = Some(text(get_bytes(b, &mut at)?)),
                (8, 2) => f.payload = Some(get_bytes(b, &mut at)?.to_vec()),
                (9, 2) => f.log_id_new = Some(text(get_bytes(b, &mut at)?)),
                (_, w) => skip(b, &mut at, w)?,
            }
        }
        Ok(f)
    }

    pub fn header(&self, k: &str) -> Option<&str> {
        self.headers.iter().find(|(key, _)| key == k).map(|(_, v)| v.as_str())
    }
}

/// 服务端下发的连接参数（秒）
#[derive(Debug, Clone, Copy)]
struct ClientConfig {
    ping_interval: u64,
}

impl ClientConfig {
    fn apply(&mut self, v: &Value) {
        if let Some(p) = v["PingInterval"].as_u64().filter(|p| *p > 0) {
            self.ping_interval = p;
        }
    }
}

/// 拆开的一条消息：开始收的时间、各片
type Parts = (Instant, Vec<Option<Vec<u8>>>);

/// 收到一条事件或回调（已拼好、已解成 JSON），返回要放进响应 `data` 的 JSON（没有就是空响应）。
pub trait Handler: Send + Sync + 'static {
    fn handle(&self, payload: Value) -> impl std::future::Future<Output = Option<Value>> + Send;
}

/// 连接一次，直到断开或出错（重连由调用方负责）。`on_open` 在连上后调用一次。
pub async fn run_once<H: Handler>(http: &reqwest::Client, base: &str, app_id: &str, app_secret: &str, handler: &H, on_open: impl FnOnce()) -> Result<()> {
    let resp: Value = http
        .post(format!("{base}/callback/ws/endpoint"))
        .header("locale", "zh")
        .json(&json!({ "AppID": app_id, "AppSecret": app_secret }))
        .send()
        .await
        .context("取长连接地址")?
        .json()
        .await
        .context("长连接地址的响应")?;
    let code = resp["code"].as_i64().unwrap_or(-1);
    if code != 0 {
        return Err(super::ApiError { code, msg: resp["msg"].as_str().unwrap_or_default().to_string() }.into());
    }
    let url = resp["data"]["URL"].as_str().filter(|u| u.starts_with("wss://") || u.starts_with("ws://")).ok_or_else(|| anyhow!("长连接地址无效"))?.to_string();
    let mut conf = ClientConfig { ping_interval: 120 };
    conf.apply(&resp["data"]["ClientConfig"]);
    let service: i32 = reqwest::Url::parse(&url).ok().and_then(|u| u.query_pairs().find(|(k, _)| k == "service_id").and_then(|(_, v)| v.parse().ok())).unwrap_or(0);

    let (socket, _) = tokio::time::timeout(Duration::from_secs(20), tokio_tungstenite::connect_async(url.as_str())).await.context("连接超时")?.context("连接长连接")?;
    let (mut tx, mut rx) = socket.split();
    on_open();
    let ping = |service| Frame { service, method: METHOD_CONTROL, headers: vec![("type".into(), "ping".into())], ..Default::default() }.encode();
    tx.send(Message::Binary(ping(service).into())).await?;
    let mut next_ping = Instant::now() + Duration::from_secs(conf.ping_interval);
    let mut last_rx = Instant::now();
    // 拆开的消息：message_id → 各片
    let mut parts: HashMap<String, Parts> = HashMap::new();

    loop {
        let silent = Duration::from_secs(conf.ping_interval * 2 + 5);
        let wake = next_ping.min(last_rx + silent);
        tokio::select! {
            msg = rx.next() => {
                let msg = match msg {
                    Some(m) => m?,
                    None => bail!("长连接被关闭"),
                };
                last_rx = Instant::now();
                let bytes = match msg {
                    Message::Binary(b) => b,
                    Message::Close(c) => bail!("长连接被关闭：{c:?}"),
                    _ => continue,
                };
                let frame = match Frame::decode(&bytes) {
                    Ok(f) => f,
                    Err(e) => {
                        tracing::debug!(error = %e, "飞书长连接：帧解不开");
                        continue;
                    }
                };
                if frame.method == METHOD_CONTROL {
                    if frame.header("type") == Some("pong") {
                        if let Some(v) = frame.payload.as_deref().and_then(|p| serde_json::from_slice::<Value>(p).ok()) {
                            conf.apply(&v);
                        }
                    }
                    continue;
                }
                if frame.method != METHOD_DATA {
                    continue;
                }
                let sum: usize = frame.header("sum").and_then(|s| s.parse().ok()).unwrap_or(1);
                let seq: usize = frame.header("seq").and_then(|s| s.parse().ok()).unwrap_or(0);
                let mut payload = frame.payload.clone().unwrap_or_default();
                if sum > 1 {
                    let id = frame.header("message_id").unwrap_or_default().to_string();
                    parts.retain(|_, (at, _)| at.elapsed() < Duration::from_secs(5));
                    let (_, slots) = parts.entry(id.clone()).or_insert_with(|| (Instant::now(), vec![None; sum.min(64)]));
                    if let Some(slot) = slots.get_mut(seq) {
                        *slot = Some(payload);
                    }
                    if slots.iter().any(Option::is_none) {
                        continue;
                    }
                    payload = parts.remove(&id).map(|(_, s)| s.into_iter().flatten().flatten().collect()).unwrap_or_default();
                }
                let kind = frame.header("type").unwrap_or_default();
                if kind != "event" && kind != "card" {
                    continue;
                }
                // 回调要在 3 秒内应答：处理函数自己保证不超时（要等的活放到后台去做）
                let Ok(v) = serde_json::from_slice::<Value>(&payload) else { continue };
                let started = Instant::now();
                let data = handler.handle(v).await.map(|d| base64::engine::general_purpose::STANDARD.encode(d.to_string()));
                let mut frame = frame;
                frame.headers.push(("biz_rt".into(), started.elapsed().as_millis().to_string()));
                frame.payload = Some(json!({ "code": 200, "headers": {}, "data": data }).to_string().into_bytes());
                tx.send(Message::Binary(frame.encode().into())).await?;
            }
            _ = tokio::time::sleep_until(wake.into()) => {
                if last_rx.elapsed() >= silent {
                    bail!("长连接 {} 秒没有动静", silent.as_secs());
                }
                tx.send(Message::Binary(ping(service).into())).await?;
                next_ping = Instant::now() + Duration::from_secs(conf.ping_interval);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_round_trip() {
        let f = Frame {
            seq_id: 0,
            log_id: 300,
            service: 7,
            method: 1,
            headers: vec![("type".into(), "event".into()), ("message_id".into(), "m1".into())],
            payload: Some(br#"{"a":1}"#.to_vec()),
            ..Default::default()
        };
        let b = f.encode();
        assert_eq!(Frame::decode(&b).unwrap(), f);
        // 0 值的 required 字段也写了：1、2 号字段都在
        assert_eq!(&b[..2], &[0x08, 0x00]);
    }

    struct Echo;

    impl Handler for Echo {
        async fn handle(&self, payload: Value) -> Option<Value> {
            Some(json!({ "toast": { "content": payload["header"]["event_type"] } }))
        }
    }

    /// 本机起一个假的飞书：先给连接地址，再在长连接上发一条拆成两片的回调，看应答帧对不对
    #[tokio::test]
    async fn run_once_against_fake_server() {
        let ws_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let ws_addr = ws_listener.local_addr().unwrap();
        let http_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let http_addr = http_listener.local_addr().unwrap();
        let url = format!("ws://{ws_addr}/?device_id=d&service_id=33");
        let router = axum::Router::new().route(
            "/callback/ws/endpoint",
            axum::routing::post(move |axum::Json(b): axum::Json<Value>| {
                let url = url.clone();
                async move {
                    assert_eq!(b["AppID"], "cli_x");
                    axum::Json(json!({ "code": 0, "data": { "URL": url, "ClientConfig": { "PingInterval": 1 } } }))
                }
            }),
        );
        tokio::spawn(async move { axum::serve(http_listener, router).await.unwrap() });
        let server = tokio::spawn(async move {
            let (tcp, _) = ws_listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
            let body = json!({ "schema": "2.0", "header": { "event_type": "card.action.trigger" }, "event": {} }).to_string().into_bytes();
            let (a, b) = body.split_at(10);
            for (seq, part) in [a, b].iter().enumerate() {
                let f = Frame {
                    method: METHOD_DATA,
                    service: 33,
                    headers: vec![("type".into(), "event".into()), ("message_id".into(), "m1".into()), ("sum".into(), "2".into()), ("seq".into(), seq.to_string())],
                    payload: Some(part.to_vec()),
                    ..Default::default()
                };
                ws.send(Message::Binary(f.encode().into())).await.unwrap();
            }
            let (mut ping, mut reply) = (None, None);
            while ping.is_none() || reply.is_none() {
                let Some(Ok(Message::Binary(b))) = ws.next().await else { panic!("连接断了") };
                let f = Frame::decode(&b).unwrap();
                if f.method == METHOD_CONTROL {
                    ping = Some(f);
                } else {
                    reply = Some(f);
                }
            }
            (ping.unwrap(), reply.unwrap())
        });
        let http = reqwest::Client::builder().no_proxy().build().unwrap();
        let base = format!("http://{http_addr}");
        let client = tokio::spawn(async move { run_once(&http, &base, "cli_x", "secret", &Echo, || {}).await });
        let (ping, reply) = tokio::time::timeout(Duration::from_secs(10), server).await.unwrap().unwrap();
        client.abort();
        assert_eq!((ping.service, ping.header("type")), (33, Some("ping")));
        assert_eq!(reply.header("message_id"), Some("m1"));
        assert!(reply.header("biz_rt").is_some());
        let resp: Value = serde_json::from_slice(reply.payload.as_deref().unwrap()).unwrap();
        assert_eq!(resp["code"], 200);
        let data = base64::engine::general_purpose::STANDARD.decode(resp["data"].as_str().unwrap()).unwrap();
        assert_eq!(serde_json::from_slice::<Value>(&data).unwrap()["toast"]["content"], "card.action.trigger");
    }

    #[test]
    fn decode_skips_unknown_fields() {
        let mut b = Frame { method: 0, headers: vec![("type".into(), "pong".into())], ..Default::default() }.encode();
        // 字段 15，varint
        put_uint(&mut b, 15, 9);
        let f = Frame::decode(&b).unwrap();
        assert_eq!(f.header("type"), Some("pong"));
        assert!(Frame::decode(&[0x0a, 0x05, 0x01]).is_err());
    }
}
