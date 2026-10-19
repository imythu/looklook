//! 看看（Looklook）客户端与服务端共享的协议定义。
//!
//! 签名规范串、错误码、DTO、网关令牌、会话凭证与用户名规则在这里只有一份实现，
//! 客户端和平台都依赖本 crate，保证两端一致。

pub mod dto;
pub mod edge_auth;
pub mod error_code;
pub mod gateway_token;
pub mod rtc;
pub mod signing;
pub mod username;

pub use error_code::ErrorCode;

/// 多设备（docs/05）：远程入口按这个请求头把接口请求转给指定设备（取值为设备 public_id）。
pub const DEVICE_HEADER: &str = "x-ll-device";
/// 远程入口列出本用户在用设备的路径（由中转/主服务网关自己应答，不转给客户端）。
pub const DEVICES_PATH: &str = "/_ll/devices";
/// 设备在线（presence）：中转经通道向客户端请求这个路径，客户端立即回一个心跳字节，之后每
/// [`PRESENCE_BEAT_SECS`] 秒一个；中转据此马上知道设备上线、断开（不经过浏览器，入口不转发它）。
pub const PRESENCE_PATH: &str = "/_ll/presence";
/// 客户端应答 presence 时带上这个响应头（旧客户端没有这个路径，中转据此退回探测端口）。
pub const PRESENCE_HEADER: &str = "x-looklook-presence";
pub const PRESENCE_BEAT_SECS: u64 = 5;
/// 中转在 presence 请求里带上这个请求头（值为秒数）：它每隔这么久主动断开 presence 再重新请求。
/// 每次请求都要 rathole 新建一条数据通道，所以客户端隔了几个周期还收不到新的 presence 请求，
/// 就说明数据通道建不起来（控制通道却还连着），应当重建通道。旧中转不带这个头，客户端就不做这项检查。
pub const PRESENCE_CYCLE_HEADER: &str = "x-looklook-presence-cycle";
