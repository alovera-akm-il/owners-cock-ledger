//! JSON route handlers under `/api/v1` (03-api-design.md). Thin over
//! `domain/` — handlers translate between HTTP and domain calls, they
//! don't hold business logic themselves.

pub mod api_tokens;
pub mod assignments;
pub mod auth;
pub mod chastity;
pub mod invites;
pub mod notifications;
pub mod profiles;
pub mod proofs;
pub mod roster;
pub mod safety;
pub mod templates;
pub mod verification;

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

/// The documented error envelope (03-api-design.md conventions):
/// `{"error": {"code": "...", "message": "..."}}`.
pub struct ApiError {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: &'static str,
}

impl ApiError {
    pub const fn new(status: StatusCode, code: &'static str, message: &'static str) -> Self {
        Self {
            status,
            code,
            message,
        }
    }
}

#[derive(Serialize)]
struct ErrorBody {
    error: ErrorDetail,
}

#[derive(Serialize)]
struct ErrorDetail {
    code: &'static str,
    message: &'static str,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = ErrorBody {
            error: ErrorDetail {
                code: self.code,
                message: self.message,
            },
        };
        (self.status, Json(body)).into_response()
    }
}

pub const INTERNAL_ERROR: ApiError = ApiError::new(
    StatusCode::INTERNAL_SERVER_ERROR,
    "internal_error",
    "something went wrong",
);

/// Converts a stored epoch-seconds timestamp to the ISO-8601 string every
/// JSON response uses at the API boundary (03-api-design.md conventions).
pub fn iso8601(epoch_secs: i64) -> String {
    chrono::DateTime::from_timestamp(epoch_secs, 0)
        .expect("epoch_secs out of chrono's representable range")
        .to_rfc3339()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso8601_formats_a_known_timestamp() {
        assert_eq!(iso8601(0), "1970-01-01T00:00:00+00:00");
    }
}
