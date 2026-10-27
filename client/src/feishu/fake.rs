//! 测试用的假飞书：开放接口（记下每个请求，按路径给固定的回答）+ 长连接（发事件、收应答）。
//! 照官方文档的请求和回应格式写，和真飞书对不上的地方要靠真机发现（docs/FEISHU_AGENT.md §7）。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;

use super::ws::{Frame, METHOD_CONTROL, METHOD_DATA};

/// 收到的一个开放接口请求
#[derive(Debug, Clone)]
pub struct Req {
    pub method: String,
    /// 带查询串
    pub path: String,
    pub body: Vec<u8>,
}

impl Req {
    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or_default()
    }

    /// 消息、卡片的 content（是一段 JSON 字符串）
    pub fn content(&self) -> Value {
        let v = self.json();
        let raw = v["content"].as_str().or(v["data"].as_str()).or(v["card"]["data"].as_str()).unwrap_or_default().to_string();
        serde_json::from_str(&raw).unwrap_or(Value::String(raw))
    }

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

type Pending = Arc<Mutex<HashMap<String, oneshot::Sender<Option<Value>>>>>;

pub struct Fake {
    pub base: String,
    pub reqs: Arc<Mutex<Vec<Req>>>,
    events: mpsc::UnboundedSender<Vec<u8>>,
    pending: Pending,
    pub connected: Arc<AtomicBool>,
    seq: AtomicUsize,
}

/// 下载图片时给的：一张 16×16 的红色 PNG（和 chat_e2e_codex 用的同一张）
pub fn png() -> Vec<u8> {
    base64::engine::general_purpose::STANDARD.decode("iVBORw0KGgoAAAANSUhEUgAAABAAAAAQCAIAAACQkWg2AAAAF0lEQVR4nGP4z8BAEiJN9aiGUQ1DSgMAkPn/Afnh+ngAAAAASUVORK5CYII=").unwrap()
}

impl Fake {
    pub async fn start() -> Arc<Self> {
        let http = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let ws = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", http.local_addr().unwrap());
        let ws_url = format!("ws://{}/?device_id=d&service_id=33", ws.local_addr().unwrap());
        let (tx, rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let fake = Arc::new(Fake { base, reqs: Default::default(), events: tx, pending: Default::default(), connected: Default::default(), seq: AtomicUsize::new(0) });
        let counter = Arc::new(AtomicUsize::new(0));
        let text_calls = Arc::new(AtomicUsize::new(0));
        let reqs = fake.reqs.clone();
        let router = axum::Router::new().fallback(move |method: axum::http::Method, uri: axum::http::Uri, body: axum::body::Bytes| {
            let (reqs, counter, text_calls, ws_url) = (reqs.clone(), counter.clone(), text_calls.clone(), ws_url.clone());
            async move {
                let path = uri.path_and_query().map(|p| p.as_str().to_string()).unwrap_or_default();
                reqs.lock().unwrap().push(Req { method: method.to_string(), path: path.clone(), body: body.to_vec() });
                let n = counter.fetch_add(1, Ordering::SeqCst);
                let p = uri.path();
                use axum::response::IntoResponse;
                let ok = |data: Value| axum::Json(json!({ "code": 0, "msg": "success", "data": data })).into_response();
                if p.contains("/resources/") {
                    return ([(axum::http::header::CONTENT_TYPE, "image/png")], png()).into_response();
                }
                match (method.as_str(), p) {
                    (_, "/open-apis/auth/v3/tenant_access_token/internal") => axum::Json(json!({ "code": 0, "msg": "ok", "tenant_access_token": "t-1", "expire": 7200 })).into_response(),
                    (_, "/callback/ws/endpoint") => ok(json!({ "URL": ws_url, "ClientConfig": { "PingInterval": 30 } })),
                    (_, "/open-apis/bot/v3/info") => axum::Json(json!({ "code": 0, "msg": "ok", "bot": { "open_id": "ou_bot", "app_name": "看看" } })).into_response(),
                    ("POST", "/open-apis/cardkit/v1/cards") => ok(json!({ "card_id": format!("c{n}") })),
                    ("PUT", p) if p.contains("/elements/") => {
                        // 第 3 次推文本时假装流式已经自动关了（200850），看会不会重新打开再推
                        if text_calls.fetch_add(1, Ordering::SeqCst) == 2 {
                            return axum::Json(json!({ "code": 200850, "msg": "streaming timeout" })).into_response();
                        }
                        ok(json!({}))
                    }
                    ("POST", "/open-apis/im/v1/images") => ok(json!({ "image_key": format!("img_up{n}") })),
                    ("POST", "/open-apis/im/v1/files") => ok(json!({ "file_key": format!("file_up{n}") })),
                    ("POST", p) if p.starts_with("/open-apis/im/v1/messages") => ok(json!({ "message_id": format!("om_b{n}") })),
                    _ => ok(json!({})),
                }
            }
        });
        tokio::spawn(async move { axum::serve(http, router).await.unwrap() });
        // 长连接：只接一条（看看每台电脑连一条）
        let (pending, connected) = (fake.pending.clone(), fake.connected.clone());
        tokio::spawn(async move {
            let mut rx = rx;
            loop {
                let Ok((tcp, _)) = ws.accept().await else { return };
                let Ok(sock) = tokio_tungstenite::accept_async(tcp).await else { continue };
                connected.store(true, Ordering::SeqCst);
                let (mut out, mut inp) = sock.split();
                loop {
                    tokio::select! {
                        ev = rx.recv() => {
                            let Some(b) = ev else { return };
                            if out.send(Message::Binary(b.into())).await.is_err() { break }
                        }
                        m = inp.next() => {
                            let Some(Ok(Message::Binary(b))) = m else { break };
                            let Ok(f) = Frame::decode(&b) else { continue };
                            if f.method == METHOD_CONTROL {
                                let pong = Frame { method: METHOD_CONTROL, service: 33, headers: vec![("type".into(), "pong".into())], ..Default::default() };
                                let _ = out.send(Message::Binary(pong.encode().into())).await;
                                continue;
                            }
                            let id = f.header("message_id").unwrap_or_default().to_string();
                            let resp: Value = f.payload.as_deref().and_then(|p| serde_json::from_slice(p).ok()).unwrap_or_default();
                            let data = resp["data"].as_str().and_then(|d| base64::engine::general_purpose::STANDARD.decode(d).ok()).and_then(|d| serde_json::from_slice::<Value>(&d).ok());
                            if let Some(tx) = pending.lock().unwrap().remove(&id) {
                                let _ = tx.send(data);
                            }
                        }
                    }
                }
                connected.store(false, Ordering::SeqCst);
            }
        });
        fake
    }

    /// 经长连接发一条事件或回调，等应答（卡片回调的 toast、新卡片）
    pub async fn push(&self, kind: &str, v: Value) -> Option<Value> {
        let id = format!("m{}", self.seq.fetch_add(1, Ordering::SeqCst));
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id.clone(), tx);
        let f = Frame { method: METHOD_DATA, service: 33, headers: vec![("type".into(), kind.into()), ("message_id".into(), id)], payload: Some(v.to_string().into_bytes()), ..Default::default() };
        self.events.send(f.encode()).unwrap();
        // 飞书要求 3 秒内应答
        tokio::time::timeout(Duration::from_secs(3), rx).await.expect("回调 3 秒内没应答").unwrap()
    }

    pub fn reqs(&self) -> Vec<Req> {
        self.reqs.lock().unwrap().clone()
    }

    /// 等到满足条件的请求出现，返回它
    pub async fn wait(&self, what: &str, secs: u64, f: impl Fn(&Req) -> bool) -> Req {
        self.wait_after(0, what, secs, f).await
    }

    /// 只看第 `from` 个之后的请求
    pub async fn wait_after(&self, from: usize, what: &str, secs: u64, f: impl Fn(&Req) -> bool) -> Req {
        for _ in 0..secs * 5 {
            if let Some(r) = self.reqs().into_iter().skip(from).find(|r| f(r)) {
                return r;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        for r in self.reqs().into_iter().skip(from) {
            eprintln!("  {} {} {}", r.method, r.path, r.text().chars().take(400).collect::<String>());
        }
        panic!("等不到：{what}");
    }
}
