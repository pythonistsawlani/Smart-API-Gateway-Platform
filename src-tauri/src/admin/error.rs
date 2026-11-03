use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;

pub const ERROR_CODE_UNAUTHORIZED: &str = "UNAUTHORIZED";
pub const ERROR_CODE_FORBIDDEN: &str = "FORBIDDEN";
pub const ERROR_CODE_RATE_LIMITED: &str = "RATE_LIMITED";
pub const ERROR_CODE_BAD_REQUEST: &str = "BAD_REQUEST";
pub const ERROR_CODE_NOT_FOUND: &str = "NOT_FOUND";
pub const ERROR_CODE_PORT_IN_USE: &str = "PORT_IN_USE";
pub const ERROR_CODE_VERSION_MISMATCH: &str = "VERSION_MISMATCH";
pub const ERROR_CODE_CHANNEL_REFERENCED: &str = "CHANNEL_REFERENCED";
pub const ERROR_CODE_EMPTY_MODEL_LIST: &str = "EMPTY_MODEL_LIST";
pub const ERROR_CODE_TIMEOUT: &str = "TIMEOUT";
pub const ERROR_CODE_ENDPOINT_UNREACHABLE: &str = "ENDPOINT_UNREACHABLE";
pub const ERROR_CODE_INVALID_CREDENTIALS: &str = "INVALID_CREDENTIALS";
pub const ERROR_CODE_INTERNAL: &str = "INTERNAL";
pub const ERROR_CODE_INVALID_URL: &str = "INVALID_URL";
pub const ERROR_CODE_UNSUPPORTED_PROVIDER: &str = "UNSUPPORTED_PROVIDER";
pub const ERROR_CODE_ENDPOINT_VALIDATION_FAILED: &str = "ENDPOINT_VALIDATION_FAILED";
pub const ERROR_CODE_ENDPOINT_CORRECTION_FAILED: &str = "ENDPOINT_CORRECTION_FAILED";
pub const ERROR_CODE_FETCH_MODELS_FAILED: &str = "FETCH_MODELS_FAILED";
pub const ERROR_CODE_HTTP_CLIENT_ERROR: &str = "HTTP_CLIENT_ERROR";

#[derive(Debug)]
#[allow(dead_code)]
pub enum AdminError {
    // Authentication & Authorization
    Unauthorized,
    Forbidden,
    RateLimited {
        retry_after_seconds: i64,
        remaining_attempts: i64,
        locked_until: i64,
    },

    // Request errors
    BadRequest(String),
    NotFound(String),

    // Conflict & version control
    Conflict {
        code: &'static str,
        message: String,
        details: Option<serde_json::Value>,
    },

    // Business errors
    PortInUse {
        port: i32,
    },
    VersionMismatch {
        expected: i64,
        current: i64,
    },
    ChannelReferenced {
        channel_id: i64,
    },
    EmptyModelList,

    // Timeout & network
    Timeout {
        url: Option<String>,
    },
    EndpointUnreachable {
        url: String,
    },
    InvalidCredentials {
        remaining_attempts: i64,
        locked_until: Option<i64>,
    },

    // General internal errors
    Internal(String),
}

#[derive(Serialize)]
struct ErrorEnvelope {
    error: ErrorBody,
}

#[derive(Serialize)]
struct ErrorBody {
    code: &'static str,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    details: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    retry_after_seconds: Option<i64>,
}

impl IntoResponse for AdminError {
    fn into_response(self) -> Response {
        let (status, code, message, details, retry_after_seconds) = match self {
            AdminError::Unauthorized => (
                StatusCode::UNAUTHORIZED,
                ERROR_CODE_UNAUTHORIZED,
                "Unauthorized".to_string(),
                None,
                None,
            ),
            AdminError::Forbidden => (
                StatusCode::FORBIDDEN,
                ERROR_CODE_FORBIDDEN,
                "Forbidden".to_string(),
                None,
                None,
            ),
            AdminError::RateLimited {
                retry_after_seconds,
                remaining_attempts,
                locked_until,
            } => (
                StatusCode::TOO_MANY_REQUESTS,
                ERROR_CODE_RATE_LIMITED,
                "Too many login attempts".to_string(),
                Some(serde_json::json!({
                    "remaining_attempts": remaining_attempts,
                    "locked_until": locked_until,
                })),
                Some(retry_after_seconds),
            ),
            AdminError::BadRequest(message) => (
                StatusCode::BAD_REQUEST,
                ERROR_CODE_BAD_REQUEST,
                message,
                None,
                None,
            ),
            AdminError::NotFound(message) => (
                StatusCode::NOT_FOUND,
                ERROR_CODE_NOT_FOUND,
                message,
                None,
                None,
            ),
            AdminError::Conflict {
                code,
                message,
                details,
            } => (StatusCode::CONFLICT, code, message, details, None),
            AdminError::PortInUse { port } => (
                StatusCode::CONFLICT,
                ERROR_CODE_PORT_IN_USE,
                format!("Port {} is already in use", port),
                Some(serde_json::json!({ "port": port })),
                None,
            ),
            AdminError::VersionMismatch { expected, current } => (
                StatusCode::CONFLICT,
                ERROR_CODE_VERSION_MISMATCH,
                "Settings were modified by another session".to_string(),
                Some(serde_json::json!({ "expected": expected, "current": current })),
                None,
            ),
            AdminError::ChannelReferenced { channel_id } => (
                StatusCode::CONFLICT,
                ERROR_CODE_CHANNEL_REFERENCED,
                format!("Channel {} is still referenced", channel_id),
                Some(serde_json::json!({ "channel_id": channel_id })),
                None,
            ),
            AdminError::EmptyModelList => (
                StatusCode::BAD_REQUEST,
                ERROR_CODE_EMPTY_MODEL_LIST,
                "Model list is empty".to_string(),
                None,
                None,
            ),
            AdminError::Timeout { url } => (
                StatusCode::GATEWAY_TIMEOUT,
                ERROR_CODE_TIMEOUT,
                "The upstream request timed out".to_string(),
                url.map(|value| serde_json::json!({ "url": value })),
                None,
            ),
            AdminError::EndpointUnreachable { url } => (
                StatusCode::BAD_GATEWAY,
                ERROR_CODE_ENDPOINT_UNREACHABLE,
                "The endpoint is unreachable".to_string(),
                Some(serde_json::json!({ "url": url })),
                None,
            ),
            AdminError::InvalidCredentials {
                remaining_attempts,
                locked_until,
            } => (
                StatusCode::UNAUTHORIZED,
                ERROR_CODE_INVALID_CREDENTIALS,
                "Invalid credentials".to_string(),
                Some(serde_json::json!({
                    "remaining_attempts": remaining_attempts,
                    "locked_until": locked_until,
                })),
                None,
            ),
            AdminError::Internal(message) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                ERROR_CODE_INTERNAL,
                message,
                None,
                None,
            ),
        };

        (
            status,
            Json(ErrorEnvelope {
                error: ErrorBody {
                    code,
                    message,
                    details,
                    retry_after_seconds,
                },
            }),
        )
            .into_response()
    }
}

impl From<crate::AppError> for AdminError {
    fn from(value: crate::AppError) -> Self {
        // Map AppError variants to specific AdminError variants
        match value {
            crate::AppError::NotFound(msg) => AdminError::NotFound(msg),
            crate::AppError::Validation(msg) => AdminError::BadRequest(msg),
            crate::AppError::Database(msg) => {
                AdminError::Internal(format!("Database error: {}", msg))
            }
            crate::AppError::Proxy(msg) => AdminError::Internal(format!("Proxy error: {}", msg)),
            crate::AppError::Network(msg) => {
                // Classify network errors based on message content
                let msg_lower = msg.to_lowercase();
                if msg_lower.contains("timeout") || msg_lower.contains("timed out") {
                    AdminError::Timeout { url: None }
                } else if msg_lower.contains("connection") || msg_lower.contains("unreachable") {
                    AdminError::EndpointUnreachable {
                        url: extract_url_from_msg(&msg),
                    }
                } else {
                    AdminError::Internal(msg)
                }
            }
            crate::AppError::Internal(msg) => AdminError::Internal(msg),
        }
    }
}

fn extract_url_from_msg(msg: &str) -> String {
    // Try to extract URL from error message
    if let Some(start) = msg.find("http://").or_else(|| msg.find("https://")) {
        if let Some(end) = msg[start..].find(' ').or_else(|| msg[start..].find(',')) {
            return msg[start..start + end].to_string();
        }
        msg[start..].to_string()
    } else {
        "unknown".to_string()
    }
}
