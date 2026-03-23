// ABOUTME: Shared error types for REST API and JSON-RPC error responses
// ABOUTME: Provides ErrorResponse for HTTP errors and JSON-RPC error code constants

use serde::Serialize;

// ============================================================================
// JSON-RPC Error Codes (per JSON-RPC 2.0 spec)
// ============================================================================

/// JSON-RPC parse error: invalid JSON received
pub const PARSE_ERROR: i32 = -32_700;

/// JSON-RPC invalid request (e.g. wrong protocol version)
pub const INVALID_REQUEST: i32 = -32_600;

/// JSON-RPC method not found
pub const METHOD_NOT_FOUND: i32 = -32_601;

/// JSON-RPC invalid parameters
pub const INVALID_PARAMS: i32 = -32_602;

/// JSON-RPC internal error
pub const INTERNAL_ERROR: i32 = -32_603;

// ============================================================================
// REST API Error Response
// ============================================================================

/// Standard REST API error response body
///
/// Used by the bearer auth middleware and health check handlers.
/// Projects can also use this for their own REST error responses.
#[derive(Debug, Serialize)]
pub struct ErrorResponse {
    /// Error details
    pub error: ErrorDetail,
}

/// Details within an error response
#[derive(Debug, Serialize)]
pub struct ErrorDetail {
    /// Machine-readable error type (e.g. `authentication_error`)
    #[serde(rename = "type")]
    pub error_type: String,
    /// Human-readable error message
    pub message: String,
}

impl ErrorResponse {
    /// Create a new error response with the given type and message
    pub fn new(error_type: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            error: ErrorDetail {
                error_type: error_type.into(),
                message: message.into(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_response_serializes_correctly() {
        let resp = ErrorResponse::new("test_error", "something broke");
        let json = serde_json::to_value(&resp).expect("serialize");
        assert_eq!(json["error"]["type"], "test_error");
        assert_eq!(json["error"]["message"], "something broke");
    }

    #[test]
    fn error_codes_match_json_rpc_spec() {
        assert_eq!(PARSE_ERROR, -32_700);
        assert_eq!(INVALID_REQUEST, -32_600);
        assert_eq!(METHOD_NOT_FOUND, -32_601);
        assert_eq!(INVALID_PARAMS, -32_602);
        assert_eq!(INTERNAL_ERROR, -32_603);
    }

    #[test]
    fn error_response_debug_impl() {
        let resp = ErrorResponse::new("auth", "denied");
        let debug = format!("{resp:?}");
        assert!(debug.contains("auth"));
        assert!(debug.contains("denied"));
    }
}
