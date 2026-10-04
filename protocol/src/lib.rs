//! 看看（Looklook）客户端与服务端共享的协议定义。
//!
//! 签名规范串、错误码、DTO、网关令牌、会话凭证与用户名规则在这里只有一份实现，
//! 客户端和平台都依赖本 crate，保证两端一致。

pub mod dto;
pub mod edge_auth;
pub mod error_code;
pub mod gateway_token;
pub mod signing;
pub mod username;

pub use error_code::ErrorCode;

/// 多设备（docs/05）：远程入口按这个请求头把接口请求转给指定设备（取值为设备 public_id）。
pub const DEVICE_HEADER: &str = "x-ll-device";
/// 远程入口列出本用户在用设备的路径（由中转/主服务网关自己应答，不转给客户端）。
pub const DEVICES_PATH: &str = "/_ll/devices";
