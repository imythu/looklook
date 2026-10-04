//! 本机接口错误：与平台相同的 `{error:{code,params}}` 结构，界面按 `error.<code>` 翻译，
//! 不返回拼好的中文句子。平台错误原样透传。

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Map, Value};

use crate::platform::PlatformError;

#[derive(Debug, Clone)]
pub struct LocalError {
    pub status: StatusCode,
    pub code: String,
    pub params: Map<String, Value>,
}

impl LocalError {
    pub fn new(code: &str) -> Self {
        let status = match code {
            "NOT_FOUND" => StatusCode::NOT_FOUND,
            "NOT_LOGGED_IN" | "UNAUTHENTICATED" | "ACCESS_CODE_REQUIRED" => StatusCode::UNAUTHORIZED,
            "LOCKED" | "FORBIDDEN" | "CSRF_FAILED" | "REMOTE_FORBIDDEN" | "PORT_RESERVED" => StatusCode::FORBIDDEN,
            "INTERNAL" | "START_FAILED" | "TTYD_MISSING" | "NO_FREE_PORT" | "AGENT_INSTALL_FAILED" => StatusCode::INTERNAL_SERVER_ERROR,
            "NETWORK" => StatusCode::BAD_GATEWAY,
            "INSTANCE_RUNNING" | "MCP_DISABLED" | "UPLOAD_OFFSET" | "UPLOAD_INCOMPLETE" | "TRANSFER_GONE" => StatusCode::CONFLICT,
            "UPLOAD_BUSY" => StatusCode::TOO_MANY_REQUESTS,
            "AGENT_NOT_FOUND" => StatusCode::NOT_FOUND,
            _ => StatusCode::BAD_REQUEST,
        };
        Self { status, code: code.into(), params: Map::new() }
    }

    pub fn with(mut self, k: &str, v: impl serde::Serialize) -> Self {
        self.params.insert(k.into(), serde_json::to_value(v).unwrap_or(Value::Null));
        self
    }

    pub fn not_found() -> Self {
        Self::new("NOT_FOUND")
    }

    pub fn invalid(field: &str, rule: &str) -> Self {
        Self::new("VALIDATION_FAILED").with("field", field).with("rule", rule)
    }

    pub fn locked(reason: Option<&str>) -> Self {
        match reason {
            Some("NOT_LOGGED_IN") | None => Self::new("NOT_LOGGED_IN"),
            Some(r) => Self::new("LOCKED").with("reason", r),
        }
    }
}

impl From<PlatformError> for LocalError {
    fn from(e: PlatformError) -> Self {
        match e {
            PlatformError::Network(detail) => {
                tracing::debug!(%detail, "平台请求失败");
                Self::new("NETWORK")
            }
            PlatformError::Api { status, code, params } => {
                Self { status: StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_REQUEST), code, params }
            }
        }
    }
}

impl From<anyhow::Error> for LocalError {
    fn from(e: anyhow::Error) -> Self {
        tracing::error!(error = %e, "本机错误");
        Self::new("INTERNAL")
    }
}

impl From<std::io::Error> for LocalError {
    fn from(e: std::io::Error) -> Self {
        tracing::error!(error = %e, "本机文件错误");
        Self::new("INTERNAL")
    }
}

impl IntoResponse for LocalError {
    fn into_response(self) -> Response {
        // 本机出的错（启动失败、找不到 ttyd …）记进日志与错误记录；连不上平台（NETWORK）会反复出现、INTERNAL 在转换时已经记过，不重复记。
        if self.status.is_server_error() && !matches!(self.code.as_str(), "NETWORK" | "INTERNAL") {
            let params = Value::Object(self.params.clone());
            tracing::error!(code = %self.code, %params, "操作失败");
        }
        (self.status, Json(json!({ "error": { "code": self.code, "params": self.params } }))).into_response()
    }
}
