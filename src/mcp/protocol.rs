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

use crate::error::{INVALID_REQUEST, PARSE_ERROR};

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
///
/// The extensions are set by the transport from what it received out of band
/// — the `Authorization` header, the `MCP-Protocol-Version` header — and are
/// never read from the message body. A body is written by the client, so a
/// body field an auth hook trusted would let any caller name its own
/// credential; MCP requires the token to travel in the `Authorization` header
/// (2025-06-18 authorization §Access Token Usage).
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

    /// Authorization header value (bearer token) — MCP/A2A transport extension,
    /// set by the transport and never deserialized from the body.
    #[serde(
        rename = "auth",
        skip_serializing_if = "Option::is_none",
        skip_deserializing
    )]
    pub auth_token: Option<String>,

    /// Forwarded HTTP headers for tenant context and other metadata — MCP
    /// extension, set by the transport and never deserialized from the body.
    #[serde(skip_serializing_if = "Option::is_none", skip_deserializing)]
    pub headers: Option<HashMap<String, Value>>,

    /// Protocol-specific metadata (additional extensions, not part of the
    /// spec), set by the transport and never deserialized from the body.
    #[serde(skip_serializing_if = "HashMap::is_empty", skip_deserializing)]
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
                    // Count and slice by chars (not bytes): a byte slice at index
                    // 10 / len-8 panics when it lands mid-codepoint on a multibyte
                    // token.
                    let char_count = token.chars().count();
                    if char_count > 20 {
                        let first: String = token.chars().take(10).collect();
                        let last: String = token.chars().skip(char_count - 8).collect();
                        format!("{first}...{last}")
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

/// One JSON-RPC 2.0 message a peer sends: a request (or notification), or a
/// response to a request this side sent it.
///
/// A server reads responses too once it sends its client requests of its own
/// (`sampling/createMessage`, `elicitation/create`): the client answers each
/// with a response carrying the id the server minted.
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum JsonRpcMessage {
    /// A request, or a notification when it carries no id.
    Request(JsonRpcRequest),
    /// A response to a request this side sent.
    Response(JsonRpcResponse),
}

impl JsonRpcMessage {
    /// Read one JSON-RPC 2.0 message from a body.
    ///
    /// The body is parsed to a JSON value first, so the two failures JSON-RPC
    /// 2.0 §5.1 keeps apart stay apart: text that is not JSON is a Parse error
    /// (-32700), and JSON that is not a message object is an Invalid Request
    /// (-32600). The second covers an array (a batch, which MCP does not carry
    /// since 2025-06-18), a scalar, and an object missing or mistyping a
    /// member.
    ///
    /// An object with a `method` is a request. A present `id` must be a
    /// string or an integer: MCP basic §Requests says it "MUST be a string or
    /// integer" and "MUST NOT be null". Any other `id` is refused before the
    /// object is read, because serde reads `"id": null` as an absent id and
    /// would turn a malformed request into a notification that is never
    /// answered. Only an absent `id` makes a notification.
    ///
    /// An object without a `method` is a response: it must carry an `id` and
    /// exactly one of `result` and `error`. Its `id` may be null — JSON-RPC
    /// answers a request it could not read that way — and is otherwise a
    /// string or an integer.
    ///
    /// # Errors
    ///
    /// Returns the error response to send back, with a null `id`: when the
    /// message is not well-formed its `id` cannot be trusted to correlate with
    /// anything the peer is waiting on.
    pub fn parse(raw: &str) -> Result<Self, Box<JsonRpcResponse>> {
        let value: Value = serde_json::from_str(raw).map_err(|e| {
            Box::new(JsonRpcResponse::error(
                None,
                PARSE_ERROR,
                format!("Parse error: {e}"),
            ))
        })?;

        let Value::Object(fields) = &value else {
            let reason = if value.is_array() {
                "Invalid Request: batch requests are not supported"
            } else {
                "Invalid Request: a request must be a JSON object"
            };
            return Err(invalid_request(reason));
        };

        if !fields.contains_key("method") {
            return Self::parse_response(value);
        }

        if fields.get("id").is_some_and(|id| !is_request_id(id)) {
            return Err(invalid_request(
                "Invalid Request: id must be a string or an integer",
            ));
        }

        serde_json::from_value(value)
            .map(Self::Request)
            .map_err(|e| invalid_request(&format!("Invalid Request: {e}")))
    }

    /// Read an object without a `method` as a response.
    fn parse_response(value: Value) -> Result<Self, Box<JsonRpcResponse>> {
        let has = |member: &str| value.get(member).is_some();
        let id_ok = value
            .get("id")
            .is_some_and(|id| id.is_null() || is_request_id(id));
        if !id_ok || has("result") == has("error") || value.get("jsonrpc").is_none() {
            return Err(invalid_request(
                "Invalid Request: a message without a method must be a response, with an id \
                 and exactly one of result and error",
            ));
        }
        serde_json::from_value(value)
            .map(Self::Response)
            .map_err(|e| invalid_request(&format!("Invalid Request: {e}")))
    }
}

/// An Invalid Request (-32600) refusal carrying no id.
fn invalid_request(reason: &str) -> Box<JsonRpcResponse> {
    Box::new(JsonRpcResponse::error(None, INVALID_REQUEST, reason))
}

impl JsonRpcRequest {
    /// Read one JSON-RPC 2.0 request or notification from a message body.
    ///
    /// [`JsonRpcMessage::parse`], refusing a response with an Invalid Request
    /// (-32600): for a reader with no request of its own outstanding, a
    /// response answers nothing.
    ///
    /// # Errors
    ///
    /// Returns the error response to send back, with a null `id` (see
    /// [`JsonRpcMessage::parse`]).
    pub fn parse(raw: &str) -> Result<Self, Box<JsonRpcResponse>> {
        match JsonRpcMessage::parse(raw)? {
            JsonRpcMessage::Request(request) => Ok(request),
            JsonRpcMessage::Response(_) => Err(invalid_request(
                "Invalid Request: a response answers no request this reader sent",
            )),
        }
    }

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

/// Whether `id` is an identifier a request may carry: a string or an integer.
///
/// `serde_json` reads a number written with a fraction or an exponent (`1.5`,
/// `1e3`) as a float, which is not an integer id, so those are refused along
/// with `null`, booleans, arrays and objects.
fn is_request_id(id: &Value) -> bool {
    match id {
        Value::String(_) => true,
        Value::Number(number) => number.is_i64() || number.is_u64(),
        _ => false,
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

    /// The error code `parse` answers `raw` with.
    fn parse_error_code(raw: &str) -> i32 {
        JsonRpcRequest::parse(raw)
            .expect_err("the message must be refused") // Safe: test assertion
            .error
            .expect("an error response") // Safe: test assertion
            .code
    }

    #[test]
    fn parse_reads_text_that_is_not_json_as_a_parse_error() {
        assert_eq!(parse_error_code("not json"), PARSE_ERROR);
        assert_eq!(parse_error_code(r#"{"jsonrpc":"2.0","id":1,"#), PARSE_ERROR);
    }

    #[test]
    fn parse_reads_json_that_is_not_a_request_as_an_invalid_request() {
        for raw in [
            // A batch: MCP carries none since 2025-06-18.
            r#"[{"jsonrpc":"2.0","id":1,"method":"ping"}]"#,
            "[]",
            // Scalars.
            "42",
            r#""ping""#,
            "null",
            // A client's response to a server request: no method.
            r#"{"jsonrpc":"2.0","id":7,"result":{}}"#,
            r#"{"jsonrpc":"2.0","id":7,"error":{"code":-1,"message":"x"}}"#,
            // Missing or mistyped members.
            r#"{"id":1,"method":"ping"}"#,
            r#"{"jsonrpc":"2.0","id":1,"method":42}"#,
        ] {
            assert_eq!(parse_error_code(raw), INVALID_REQUEST, "{raw}");
        }
    }

    #[test]
    fn parse_refuses_an_id_that_is_not_a_string_or_an_integer() {
        for id in ["null", "1.5", "1e3", "true", r#"{"a":1}"#, "[1]"] {
            let raw = format!(r#"{{"jsonrpc":"2.0","id":{id},"method":"ping"}}"#);
            let refused = JsonRpcRequest::parse(&raw).expect_err("id must be refused"); // Safe: test assertion
            assert_eq!(
                refused.error.as_ref().map(|e| e.code),
                Some(INVALID_REQUEST),
                "id {id}"
            );
            // The refusal carries no id, so it cannot be mistaken for an
            // answer to a request the client does have outstanding.
            assert_eq!(refused.id, None, "id {id}");
        }
    }

    #[test]
    fn parse_accepts_string_and_integer_ids() {
        for (id, expected) in [
            (r#""req-1""#, Value::from("req-1")),
            ("0", Value::from(0)),
            ("-3", Value::from(-3)),
            ("18446744073709551615", Value::from(u64::MAX)),
        ] {
            let raw = format!(r#"{{"jsonrpc":"2.0","id":{id},"method":"ping"}}"#);
            let req = JsonRpcRequest::parse(&raw).expect("a valid id parses"); // Safe: test assertion
            assert_eq!(req.id, Some(expected), "id {id}");
            assert_eq!(req.method, "ping");
        }
    }

    #[test]
    fn parse_reads_an_absent_id_as_a_notification() {
        let req =
            JsonRpcRequest::parse(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
                .expect("a notification parses"); // Safe: test assertion
        assert_eq!(req.id, None);
        assert_eq!(req.method, "notifications/initialized");
    }

    /// A body cannot name its own credential, headers or metadata: those are
    /// the transport's to set from what it received out of band.
    #[test]
    fn transport_extensions_are_never_read_from_the_body() {
        let raw = r#"{
            "jsonrpc":"2.0","id":1,"method":"tools/list",
            "auth":"admin",
            "headers":{"x-tenant-id":"someone-else"},
            "metadata":{"mcp-protocol-version":"2025-11-25"}
        }"#;
        for req in [
            serde_json::from_str::<JsonRpcRequest>(raw).expect("deserialize"), // Safe: test assertion
            JsonRpcRequest::parse(raw).expect("parse"), // Safe: test assertion
        ] {
            assert_eq!(req.auth_token, None);
            assert_eq!(req.headers, None);
            assert!(req.metadata.is_empty(), "metadata was {:?}", req.metadata);
            assert_eq!(req.method, "tools/list");
        }
    }

    #[test]
    fn a_message_without_a_method_reads_as_a_response() {
        for raw in [
            r#"{"jsonrpc":"2.0","id":"srv-1","result":{}}"#,
            r#"{"jsonrpc":"2.0","id":7,"error":{"code":-1,"message":"x"}}"#,
            r#"{"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"x"}}"#,
        ] {
            let message = JsonRpcMessage::parse(raw).expect("a response parses"); // Safe: test assertion
            assert!(matches!(message, JsonRpcMessage::Response(_)), "{raw}");
        }
        let message = JsonRpcMessage::parse(r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#)
            .expect("a request parses"); // Safe: test assertion
        assert!(matches!(message, JsonRpcMessage::Request(_)));
    }

    #[test]
    fn a_malformed_response_is_an_invalid_request() {
        for raw in [
            // Neither result nor error, or both.
            r#"{"jsonrpc":"2.0","id":1}"#,
            r#"{"jsonrpc":"2.0","id":1,"result":{},"error":{"code":1,"message":"x"}}"#,
            // No id, or one that is neither null, a string nor an integer.
            r#"{"jsonrpc":"2.0","result":{}}"#,
            r#"{"jsonrpc":"2.0","id":1.5,"result":{}}"#,
            // No version.
            r#"{"id":1,"result":{}}"#,
            // An error that is not an error object.
            r#"{"jsonrpc":"2.0","id":1,"error":"boom"}"#,
        ] {
            let refused = JsonRpcMessage::parse(raw).expect_err("must be refused"); // Safe: test assertion
            assert_eq!(
                refused.error.as_ref().map(|e| e.code),
                Some(INVALID_REQUEST),
                "{raw}"
            );
        }
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
    fn debug_redacts_multibyte_auth_token_without_panic() {
        // Regression: 24 multibyte chars. The old byte-slicing impl panicked
        // because byte index 10 (and len-8) land mid-codepoint; char-safe
        // slicing must elide the middle without panicking.
        let mut req = JsonRpcRequest::new("ping", None);
        req.auth_token = Some("あ".repeat(24));
        let debug = format!("{req:?}");
        assert!(debug.contains("..."));
        // The full token must never appear — only the first 10 + last 8 chars.
        assert!(!debug.contains(&"あ".repeat(24)));
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
