use serde::Serialize;

/// Uniform JSON error response body.
#[derive(Serialize)]
pub struct ErrorResponse {
    pub error: String,
}
