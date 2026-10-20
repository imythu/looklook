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

/// `GET /api/client/v1/relays`：用户可以选的线路（中转）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelayChoices {
    pub items: Vec<RelayChoice>,
    /// 用户自选的中转名；None = 自动。
    pub preferred: Option<String>,
    /// 当前所在的中转名（自选的中转不可用时与 `preferred` 不同）。
    pub current: Option<String>,
    /// `preferred` 是客户端按延迟自动选的（不是用户手动选的）。`preferred` 为 None 时也算自动。
    #[serde(default)]
    pub auto_picked: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelayChoice {
    /// 中转名（`r2`…），选择时提交这个值。
    pub name: String,
    /// 中转的地址（`r2.example.com`）。
    pub address: Option<String>,
    /// 测延迟的地址：中转上 TURN 服务（coturn）的 STUN 端口，`主机:端口`。远程控制台的连接（WebRTC）
    /// 经它中继，测的就是这条路；它不通（比如 UDP 被拦）时这条线路也用不了。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stun: Option<String>,
    /// 中转所在地（按 IP 库或管理员填写，到省/州一级）；未知时为 None。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<RelayLocation>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RelayLocation {
    /// ISO 3166-1 两位代码（可能为空字符串）
    #[serde(default)]
    pub country_code: String,
    pub country: LocalizedName,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<LocalizedName>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LocalizedName {
    pub en: String,
    pub zh: String,
}

/// `PUT /api/client/v1/relays/preference`：`relay` 为 None = 自动。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetRelayPreference {
    pub relay: Option<String>,
    /// 客户端按延迟自动选的：只在用户没有手动选线路时生效，不覆盖手动选择。
    #[serde(default)]
    pub auto: bool,
}

/// 给设备改名：`PUT /api/client/v1/device/name`（签名）与网页 `PATCH /api/web/v1/devices/{id}`。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RenameDevice {
    pub name: String,
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
    /// 这台设备上的终端 id（终端路由表）：WebRTC 启动页按它把 `/i/{id}…` 转给终端所在的设备（docs/06 §6.1）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub terminals: Vec<String>,
    /// 只在本机客户端取的列表（`GET /api/client/v1/rtc/devices`）里有：这台电脑最近一次心跳的公网 IP
    /// 与发起请求的电脑相同（IPv6 比较 /64），多半在同一个局域网。不下发 IP 本身。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub same_network: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RemoteDevices {
    pub items: Vec<RemoteDevice>,
    /// 没有 `X-LL-Device` 时请求默认去的设备（“主设备”：最近在线的那台）；没有在线设备时为 None。
    pub default: Option<String>,
    /// 每个账户最多同时登录的设备数。
    pub max: i64,
    /// 浏览器建 WebRTC 连接用的 ICE 服务器：用户所在中转上的 STUN/TURN 与短期凭证（docs/06 §7.2）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ice_servers: Vec<crate::rtc::IceServer>,
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
    /// 平台上这台设备的名称：在网页上改过名时与客户端上报的不同，客户端据此同步本机设置。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_name: Option<String>,
    /// 平台看到的这台设备的公网 IP。远程打开时浏览器的公网 IP 与它相同，
    /// 说明浏览器多半和这台电脑在同一个网络里，界面提示改走局域网（客户端 gateway/lan.rs）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_ip: Option<String>,
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

/// 本机网页的临时分享：在有效期内允许指定的看看账户打开这个网页。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageShare {
    pub id: String,
    /// 被分享的账户（用户名）。
    pub username: String,
    pub created_at: String,
    pub expires_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageShareList {
    pub items: Vec<PageShare>,
    /// 每个网页同时有效的分享上限。
    pub max: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreatePageShare {
    pub username: String,
    /// 有效期（小时），1–168。
    pub hours: i64,
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
