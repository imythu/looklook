//! 客户端 API 请求/响应结构（详细设计 §7.3）。时间字段为 RFC 3339 UTC 字符串。

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceInfo {
    pub install_id: String,
    pub name: String,
    pub os: String,
    pub arch: String,
    pub client_version: String,
    pub target: String,
    #[serde(default)]
    pub locale: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoginRequest {
    pub email: String,
    pub password: String,
    /// Ed25519 公钥，base64url 无填充。
    pub public_key: String,
    pub device: DeviceInfo,
}

/// 浏览器授权登录：客户端发起（`POST /api/client/v1/device/authorize/start`，无需签名）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthorizeStartRequest {
    /// Ed25519 公钥，base64url 无填充。
    pub public_key: String,
    pub device: DeviceInfo,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthorizeStartResponse {
    /// 展示给用户核对的短码，如 `ABCD-EFGH`。
    pub user_code: String,
    /// 在浏览器打开的授权页，如 `https://<server>/authorize?code=ABCD-EFGH`。
    pub verification_url: String,
    /// 客户端轮询用的高熵令牌（服务端只保存哈希）。
    pub poll_token: String,
    /// 建议轮询间隔（秒）。
    pub interval: i64,
    /// 有效期（秒）。
    pub expires_in: i64,
}

/// `POST /api/client/v1/device/authorize/poll`：批准前返回 `AUTHORIZATION_PENDING`，批准后返回 [`LoginResponse`]。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthorizePollRequest {
    pub poll_token: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserSummary {
    pub id: String,
    pub username: String,
    pub nickname: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelayParams {
    /// 中转名（`r1`、`r2`…）：会话凭证的 `aud`。
    #[serde(default)]
    pub name: String,
    pub server_addr: String,
    pub transport: String,
    pub noise_remote_public_key: String,
    pub service_name: String,
    pub token: String,
    /// 网关令牌的 MAC 密钥（32 字节，base64url），每（设备, 中转）一把。
    #[serde(default)]
    pub gateway_mac_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoginResponse {
    pub device_id: String,
    pub key_id: String,
    pub user: UserSummary,
    pub server_time: String,
    pub membership_expires_at: String,
    pub heartbeat_interval_seconds: i64,
    pub offline_grace_seconds: i64,
    pub user_host_url: String,
    pub relay: RelayParams,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HeartbeatRequest {
    pub client_version: String,
    pub target: String,
    #[serde(default = "default_channel")]
    pub channel: String,
    #[serde(default)]
    pub device_name: Option<String>,
    #[serde(default)]
    pub uptime_seconds: Option<i64>,
    /// 本设备当前全部终端实例 id（多设备，docs/05 §5）。平台据此维护终端路由表；None = 不更新。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminals: Option<Vec<String>>,
}

/// `PUT /api/client/v1/terminals`（签名）：终端新建/删除后立即上报本设备的全部终端 id（docs/05 §5）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TerminalReport {
    pub ids: Vec<String>,
}

/// 远程入口 `GET /_ll/devices`（中转直连入口与主服务网关都提供，docs/05 §6）：本用户的在用设备。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RemoteDevice {
    /// 设备 public_id，也是 `X-LL-Device` 请求头的取值。
    pub id: String,
    pub name: String,
    pub os: String,
    pub online: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RemoteDevices {
    pub items: Vec<RemoteDevice>,
    /// 没有 `X-LL-Device` 时请求默认去的设备（“主设备”：最近在线的那台）；没有在线设备时为 None。
    pub default: Option<String>,
    /// 每个账户最多同时登录的设备数。
    pub max: i64,
}

pub fn default_channel() -> String {
    "stable".into()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UpdateHint {
    pub version: String,
    pub required: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeartbeatResponse {
    pub allow_run: bool,
    pub reason: Option<String>,
    pub server_time: String,
    pub membership_expires_at: String,
    pub next_heartbeat_seconds: i64,
    pub offline_grace_seconds: i64,
    pub rotate_key: bool,
    pub update: Option<UpdateHint>,
    pub relay_changed: bool,
    /// 近 1 天内被吊销的子站会话短 id（会话凭证的 `sid`），客户端拒绝带这些凭证的请求。
    #[serde(default)]
    pub revoked_sids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RotateRequest {
    pub new_public_key: String,
    /// 新私钥对 `LL1-ROTATE` 规范串的签名。
    pub proof: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RotateResponse {
    pub rotation_id: String,
    pub new_key_id: String,
    pub relay_token: String,
    pub expires_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RotateConfirmRequest {
    pub rotation_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tunnel {
    pub id: String,
    pub name: String,
    pub target_port: i32,
    pub status: String,
    pub url: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TunnelList {
    pub items: Vec<Tunnel>,
    pub max: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateTunnel {
    pub name: String,
    pub target_port: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UpdateTunnel {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub target_port: Option<i64>,
    #[serde(default)]
    pub status: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountSummary {
    pub id: String,
    pub username: String,
    pub nickname: String,
    pub email: String,
    pub user_host_url: String,
    pub membership_expires_at: String,
    pub server_time: String,
}

/// `/.well-known/looklook.json`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WellKnown {
    pub api_version: u32,
    pub platform_keys: Vec<PlatformKey>,
    pub server_time: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlatformKey {
    pub kid: String,
    pub public_key: String,
}
