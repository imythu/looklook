//! 错误码（详细设计 §8）。API 只返回 `code` + `params`，由前端 / 客户端按
//! `error.<code>` 的 i18n key 翻译。

use serde::{Deserialize, Serialize};

macro_rules! error_codes {
    ($($name:ident = $status:expr),* $(,)?) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(rename_all = "SCREAMING_SNAKE_CASE")]
        pub enum ErrorCode { $($name),* }

        impl ErrorCode {
            pub const ALL: &'static [ErrorCode] = &[$(ErrorCode::$name),*];

            /// 默认 HTTP 状态码。
            pub fn http_status(self) -> u16 {
                match self { $(ErrorCode::$name => $status),* }
            }
        }
    };
}

error_codes! {
    ValidationFailed = 400,
    Unauthenticated = 401,
    CsrfFailed = 403,
    Forbidden = 403,
    NotFound = 404,
    RateLimited = 429,
    Internal = 500,
    EmailInvalid = 400,
    EmailAlreadyRegistered = 409,
    EmailCodeInvalid = 400,
    EmailCodeExpired = 400,
    EmailCodeTooManyAttempts = 400,
    EmailCodeCooldown = 429,
    UsernameInvalid = 400,
    UsernameReserved = 409,
    UsernameTaken = 409,
    NicknameInvalid = 400,
    PasswordWeak = 400,
    InvalidCredentials = 401,
    AccountDisabled = 403,
    RegistrationClosed = 403,
    InviteCodeRequired = 400,
    InviteCodeInvalid = 400,
    InviteCodeLimitReached = 409,
    RenewalCodeInvalid = 400,
    RenewalCodeAlreadyRedeemed = 409,
    MembershipExpired = 403,
    DeviceAlreadyLoggedIn = 409,
    DeviceLimitReached = 409,
    DeviceRevoked = 401,
    DeviceLoggedOut = 401,
    DeviceOffline = 503,
    AuthorizationPending = 400,
    AuthorizationExpired = 410,
    AuthorizationDenied = 403,
    ClientVersionUnsupported = 426,
    SignatureMissing = 401,
    SignatureInvalid = 401,
    TimestampSkew = 401,
    NonceReplayed = 401,
    KeyRevoked = 401,
    KeyRotationNotFound = 409,
    TunnelLimitReached = 409,
    TunnelPortInvalid = 400,
    TunnelPortDuplicate = 409,
    TunnelDisabled = 403,
    GatewayOriginRejected = 403,
    RelayCapacityExhausted = 503,
    CertOperationInProgress = 409,
    SetupTokenInvalid = 401,
    SetupAlreadyCompleted = 409,
    SetupStepFailed = 400,
}

impl ErrorCode {
    pub fn as_str(self) -> String {
        serde_json::to_value(self).ok().and_then(|v| v.as_str().map(str::to_owned)).unwrap_or_default()
    }
}

impl std::fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.as_str())
    }
}

/// 错误响应体 `{error:{code,params,request_id}}`。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorBody {
    pub error: ErrorDetail,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorDetail {
    pub code: ErrorCode,
    #[serde(default)]
    pub params: serde_json::Map<String, serde_json::Value>,
    #[serde(default)]
    pub request_id: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(ErrorCode::DeviceAlreadyLoggedIn.as_str(), "DEVICE_ALREADY_LOGGED_IN");
        assert_eq!(ErrorCode::CsrfFailed.as_str(), "CSRF_FAILED");
        assert_eq!(ErrorCode::ClientVersionUnsupported.http_status(), 426);
    }
}
