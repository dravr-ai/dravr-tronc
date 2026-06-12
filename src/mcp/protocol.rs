// ABOUTME: Canonical JSON-RPC 2.0 wire types shared by all MCP transports
// ABOUTME: Request/response/error structs with a metadata extension field and redacted Debug
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

//! JSON-RPC 2.0 foundation for the MCP protocol.
//!
//! These are the protocol-agnostic wire types every MCP transport speaks. MCP
//! schema types (`initialize`, `tools/*`, capabilities) layer on top in
//! [`crate::mcp::schema`]; the standard error-code constants live in
//! [`crate::error`].

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::fmt;

/// JSON-RPC 2.0 version string.
pub const JSONRPC_VERSION: &str = "2.0";

/// Default MCP protocol revision advertised by the server (current stable spec).
///
/// The modern (stateless) revision lives at
/// [`crate::mcp::modern::PROTOCOL_VERSION_2026_07_28`]; era detection on the
/// dispatch path decides which one a given request speaks.
pub const PROTOCOL_VERSION: &str = "2025-11-25";

/// JSON-RPC 2.0 request.
///
/// Carries the protocol-agnostic envelope plus MCP/A2A transport extensions
/// (`auth` bearer token, forwarded `headers`, free-form `metadata`).
#[derive(Clone, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    /// JSON-RPC version (always `"2.0"`).
    pub jsonrpc: String,

    /// Method name to invoke.
    pub method: String,

    /// Optional parameters for the method.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,

    /// Request identifier (absent for notifications).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,

    /// Authorization header value (bearer token) — MCP/A2A transport extension.
    #[serde(rename = "auth", skip_serializing_if = "Option::is_none", default)]
    pub auth_token: Option<String>,

    /// Forwarded HTTP headers for tenant context and other metadata — MCP extension.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub headers: Option<HashMap<String, Value>>,

    /// Protocol-specific metadata (additional extensions, not part of the spec).
    #[serde(skip_serializing_if = "HashMap::is_empty", default)]
    pub metadata: HashMap<String, String>,
}

// Custom Debug that redacts the bearer token so it never reaches logs.
impl fmt::Debug for JsonRpcRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JsonRpcRequest")
            .field("jsonrpc", &self.jsonrpc)
            .field("method", &self.method)
            .field("params", &self.params)
            .field("id", &self.id)
            .field(
                "auth_token",
                &self.auth_token.as_ref().map(|token| {
                    // Show first 10 + last 8 characters, or "[REDACTED]" if short.
                    if token.len() > 20 {
                        format!("{}...{}", &token[..10], &token[token.len() - 8..])
                    } else {
                        "[REDACTED]".to_owned()
                    }
                }),
            )
            .field("headers", &self.headers)
            .field("metadata", &self.metadata)
            .finish()
    }
}

/// JSON-RPC 2.0 response. Exactly one of `result` or `error` is present.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    /// JSON-RPC version (always `"2.0"`).
    pub jsonrpc: String,

    /// Result of the method call (mutually exclusive with `error`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,

    /// Error information (mutually exclusive with `result`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,

    /// Request identifier for correlation.
    pub id: Option<Value>,
}

/// JSON-RPC 2.0 error object.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcError {
    /// Error code (see [`crate::error`] for the standard constants).
    pub code: i32,

    /// Human-readable error message.
    pub message: String,

    /// Additional error information.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl JsonRpcRequest {
    /// Create a new request with a default id of `1`.
    #[must_use]
    pub fn new(method: impl Into<String>, params: Option<Value>) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_owned(),
            method: method.into(),
            params,
            id: Some(Value::Number(1.into())),
            auth_token: None,
            headers: None,
            metadata: HashMap::new(),
        }
    }

    /// Create a new request with a specific id.
    #[must_use]
    pub fn with_id(method: impl Into<String>, params: Option<Value>, id: Value) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_owned(),
            method: method.into(),
            params,
            id: Some(id),
            auth_token: None,
            headers: None,
            metadata: HashMap::new(),
        }
    }

    /// Create a notification (no id, no response expected).
    #[must_use]
    pub fn notification(method: impl Into<String>, params: Option<Value>) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_owned(),
            method: method.into(),
            params,
            id: None,
            auth_token: None,
            headers: None,
            metadata: HashMap::new(),
        }
    }

    /// Attach a metadata key/value to the request.
    #[must_use]
    pub fn with_metadata(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.metadata.insert(key.into(), value.into());
        self
    }

    /// Look up a metadata value by key.
    #[must_use]
    pub fn get_metadata(&self, key: &str) -> Option<&String> {
        self.metadata.get(key)
    }
}

impl JsonRpcResponse {
    /// Build a success response carrying the given result.
    #[must_use]
    pub fn success(id: Option<Value>, result: Value) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_owned(),
            result: Some(result),
            error: None,
            id,
        }
    }

    /// Build an error response with the given code and message.
    #[must_use]
    pub fn error(id: Option<Value>, code: i32, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_owned(),
            result: None,
            error: Some(JsonRpcError {
                code,
                message: message.into(),
                data: None,
            }),
            id,
        }
    }

    /// Build an error response carrying additional structured `data`.
    #[must_use]
    pub fn error_with_data(
        id: Option<Value>,
        code: i32,
        message: impl Into<String>,
        data: Value,
    ) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_owned(),
            result: None,
            error: Some(JsonRpcError {
                code,
                message: message.into(),
                data: Some(data),
            }),
            id,
        }
    }

    /// Whether this is a success response.
    #[must_use]
    pub const fn is_success(&self) -> bool {
        self.error.is_none() && self.result.is_some()
    }

    /// Whether this is an error response.
    #[must_use]
    pub const fn is_error(&self) -> bool {
        self.error.is_some()
    }
}

impl JsonRpcError {
    /// Create a new error.
    #[must_use]
    pub fn new(code: i32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }

    /// Create an error carrying additional structured `data`.
    #[must_use]
    pub fn with_data(code: i32, message: impl Into<String>, data: Value) -> Self {
        Self {
            code,
            message: message.into(),
            data: Some(data),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::PARSE_ERROR;

    #[test]
    fn serialize_success_response() {
        let resp = JsonRpcResponse::success(Some(Value::from(1)), serde_json::json!({"ok": true}));
        let json = serde_json::to_string(&resp).expect("serialize"); // Safe: test assertion
        assert!(json.contains("\"result\""));
        assert!(!json.contains("\"error\""));
    }

    #[test]
    fn serialize_error_response() {
        let resp = JsonRpcResponse::error(Some(Value::from(1)), PARSE_ERROR, "bad json");
        let json = serde_json::to_string(&resp).expect("serialize"); // Safe: test assertion
        assert!(json.contains("\"error\""));
        assert!(json.contains("-32700"));
        assert!(!json.contains("\"result\""));
    }

    #[test]
    fn error_with_data_carries_payload() {
        let resp = JsonRpcResponse::error_with_data(
            Some(Value::from(1)),
            -32_004,
            "unsupported version",
            serde_json::json!({"supported": ["2025-11-25"]}),
        );
        let err = resp.error.expect("error"); // Safe: test assertion
        assert_eq!(err.code, -32_004);
        assert_eq!(err.data.expect("data")["supported"][0], "2025-11-25"); // Safe: test assertion
    }

    #[test]
    fn deserialize_request_with_params() {
        let raw = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"test"}}"#;
        let req: JsonRpcRequest = serde_json::from_str(raw).expect("deserialize"); // Safe: test assertion
        assert_eq!(req.method, "tools/call");
        assert!(req.params.is_some());
    }

    #[test]
    fn deserialize_request_without_params() {
        let raw = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#;
        let req: JsonRpcRequest = serde_json::from_str(raw).expect("deserialize"); // Safe: test assertion
        assert_eq!(req.method, "tools/list");
        assert!(req.params.is_none());
    }

    #[test]
    fn deserialize_notification_has_no_id() {
        let raw = r#"{"jsonrpc":"2.0","method":"notifications/cancelled"}"#;
        let req: JsonRpcRequest = serde_json::from_str(raw).expect("deserialize"); // Safe: test assertion
        assert!(req.id.is_none());
    }

    #[test]
    fn debug_redacts_long_auth_token() {
        let mut req = JsonRpcRequest::new("ping", None);
        req.auth_token = Some("abcdefghij_secret_middle_part_klmnopqr".to_owned());
        let debug = format!("{req:?}");
        assert!(!debug.contains("secret_middle_part"));
        assert!(debug.contains("..."));
    }

    #[test]
    fn success_response_omits_error_field() {
        let resp = JsonRpcResponse::success(Some(Value::from(1)), Value::Null);
        let json = serde_json::to_value(&resp).expect("serialize"); // Safe: test assertion
        assert!(json.get("error").is_none());
        assert!(json.get("result").is_some());
    }

    #[test]
    fn error_response_omits_result_field() {
        let resp = JsonRpcResponse::error(Some(Value::from(1)), -1, "fail");
        let json = serde_json::to_value(&resp).expect("serialize"); // Safe: test assertion
        assert!(json.get("result").is_none());
        assert!(json.get("error").is_some());
    }

    #[test]
    fn is_success_and_is_error_are_exclusive() {
        let ok = JsonRpcResponse::success(None, Value::Null);
        assert!(ok.is_success());
        assert!(!ok.is_error());

        let err = JsonRpcResponse::error(None, INTERNAL_ERR, "x");
        assert!(err.is_error());
        assert!(!err.is_success());
    }

    const INTERNAL_ERR: i32 = -32_603;
}
