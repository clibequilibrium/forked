//! API errors: a stable JSON error body and HTTP status mapping shared by
//! every transport.

use serde::{Deserialize, Serialize};

/// An API-level error with an HTTP status, a stable machine-readable code,
/// and a human-readable message.
#[derive(Debug, Clone)]
pub struct ApiError {
    /// HTTP status code.
    pub status: u16,
    /// Stable error code (e.g. `"conflict"`, `"not_found"`).
    pub code: String,
    /// Human-readable description.
    pub message: String,
}

/// JSON body for errors: `{"error": {"code", "message"}}`.
#[derive(Debug, Serialize, Deserialize)]
pub struct ErrorBody {
    /// The error payload.
    pub error: ErrorDetail,
}

/// The inner error object.
#[derive(Debug, Serialize, Deserialize)]
pub struct ErrorDetail {
    /// Stable error code.
    pub code: String,
    /// Human-readable description.
    pub message: String,
}

impl ApiError {
    /// Construct an error.
    pub fn new(status: u16, code: &str, message: impl Into<String>) -> Self {
        ApiError {
            status,
            code: code.to_string(),
            message: message.into(),
        }
    }

    /// 400 with code `invalid_request`.
    pub fn invalid(message: impl Into<String>) -> Self {
        ApiError::new(400, "invalid_request", message)
    }

    /// 404 with code `not_found`.
    pub fn not_found(message: impl Into<String>) -> Self {
        ApiError::new(404, "not_found", message)
    }

    /// The JSON body for this error.
    pub fn body(&self) -> ErrorBody {
        ErrorBody {
            error: ErrorDetail {
                code: self.code.clone(),
                message: self.message.clone(),
            },
        }
    }

    /// The JSON body serialized to a string.
    pub fn body_json(&self) -> String {
        serde_json::to_string(&self.body()).unwrap_or_else(|_| {
            // ErrorBody is plain strings; serialization cannot realistically
            // fail, but never panic on an error path.
            format!(
                "{{\"error\":{{\"code\":\"internal\",\"message\":\"{}\"}}}}",
                self.code
            )
        })
    }
}

impl From<forked_core::Error> for ApiError {
    fn from(e: forked_core::Error) -> Self {
        use forked_core::Error as E;
        match &e {
            E::NotFound(_) => ApiError::new(404, "not_found", e.to_string()),
            E::Conflict(_) => ApiError::new(409, "conflict", e.to_string()),
            E::EffectMismatch(_) => ApiError::new(409, "effect_mismatch", e.to_string()),
            E::BranchExists(_) => ApiError::new(409, "branch_exists", e.to_string()),
            E::SessionExists(_) => ApiError::new(409, "session_exists", e.to_string()),
            E::InvalidName(_) | E::InvalidHash(_) => {
                ApiError::new(400, "invalid_request", e.to_string())
            }
            E::Storage(_) => ApiError::new(500, "storage_error", e.to_string()),
            E::MissingBlob(_) | E::Corrupt(_) | E::Encoding(_) => {
                ApiError::new(500, "internal", e.to_string())
            }
        }
    }
}

/// Result alias for API handlers.
pub type ApiResult<T> = Result<T, ApiError>;
