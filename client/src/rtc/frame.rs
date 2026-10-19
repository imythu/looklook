//! 通道上的帧（服务端 docs/06 §6.2）。
//!
//! datachannel-rs 只能发二进制消息，所以每条消息的第一个字节标明类型，浏览器那边也照这个格式发：
//!
//! | 首字节 | 内容 |
//! | --- | --- |
//! | `0x00` | 控制帧：其余是一个 JSON 对象（`req`、`ws`、`res`、`open`、`close`、`end`、`hello`、`err`） |
//! | `0x01` | 二进制数据（HTTP 请求体 / 响应体分块，或 WebSocket 二进制消息） |
//! | `0x02` | 文本数据（WebSocket 文本消息，UTF-8） |
//!
//! 数据帧再或上 `0x80` 表示“这条 WebSocket 消息还没完，下一帧接着”：SCTP 单条消息有上限（浏览器 256 KiB），
//! 大的 WebSocket 消息拆成 [`CHUNK`] 大小的几帧发送，收到后拼回一条。

use serde_json::Value;

pub const CTL: u8 = 0x00;
pub const BIN: u8 = 0x01;
pub const TEXT: u8 = 0x02;
pub const MORE: u8 = 0x80;

/// 每帧数据的最大字节数。
pub const CHUNK: usize = 64 * 1024;

#[derive(Debug, PartialEq)]
pub enum Frame<'a> {
    Ctl(Value),
    Data { text: bool, more: bool, data: &'a [u8] },
}

pub fn ctl(v: &Value) -> Vec<u8> {
    let mut out = vec![CTL];
    serde_json::to_writer(&mut out, v).expect("json");
    out
}

pub fn data(text: bool, more: bool, bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len() + 1);
    out.push(if text { TEXT } else { BIN } | if more { MORE } else { 0 });
    out.extend_from_slice(bytes);
    out
}

pub fn parse(msg: &[u8]) -> Option<Frame<'_>> {
    let (&kind, rest) = msg.split_first()?;
    match kind {
        CTL => serde_json::from_slice(rest).ok().filter(Value::is_object).map(Frame::Ctl),
        k if k & !MORE == BIN || k & !MORE == TEXT => Some(Frame::Data { text: k & !MORE == TEXT, more: k & MORE != 0, data: rest }),
        _ => None,
    }
}

/// 把一条消息拆成若干帧（最后一帧不带 `MORE`）。空消息也是一帧。
pub fn split(text: bool, bytes: &[u8]) -> Vec<Vec<u8>> {
    if bytes.is_empty() {
        return vec![data(text, false, bytes)];
    }
    let n = bytes.len().div_ceil(CHUNK);
    bytes.chunks(CHUNK).enumerate().map(|(i, c)| data(text, i + 1 < n, c)).collect()
}

/// 控制帧的类型（`t` 字段）。
pub fn kind(v: &Value) -> &str {
    v.get("t").and_then(Value::as_str).unwrap_or("")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn roundtrip() {
        let c = ctl(&json!({"t": "end"}));
        assert_eq!(parse(&c), Some(Frame::Ctl(json!({"t": "end"}))));
        assert_eq!(kind(&json!({"t": "res"})), "res");
        let d = data(true, true, b"hi");
        assert_eq!(parse(&d), Some(Frame::Data { text: true, more: true, data: b"hi" }));
        assert_eq!(parse(&data(false, false, b"")), Some(Frame::Data { text: false, more: false, data: b"" }));
        // 不是对象的 JSON、未知类型、空消息
        assert_eq!(parse(&[CTL, b'1']), None);
        assert_eq!(parse(&[0x07, 1]), None);
        assert_eq!(parse(&[]), None);
    }

    #[test]
    fn splits_large_messages() {
        let big = vec![7u8; CHUNK * 2 + 1];
        let parts = split(false, &big);
        assert_eq!(parts.len(), 3);
        assert_eq!(parts[0][0], BIN | MORE);
        assert_eq!(parts[2][0], BIN);
        assert_eq!(parts.iter().map(|p| p.len() - 1).sum::<usize>(), big.len());
        assert_eq!(split(true, b"").len(), 1);
        assert_eq!(split(true, &vec![1u8; CHUNK]).len(), 1);
    }
}
