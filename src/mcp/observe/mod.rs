// ABOUTME: Passive observation of MCP dispatch: an Observer sees each operation start and complete with a typed outcome
// ABOUTME: Payload capture is off by default, redacted by a host hook, and truncated to a UTF-8-safe byte limit
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

//! Observation of the MCP dispatch lifecycle.
//!
//! An [`Observer`] installed with [`McpServer::with_observer`] sees every
//! JSON-RPC message the server dispatches — requests and notifications, on
//! every transport — exactly twice: [`Observer::start`] before dispatch, with
//! what the message says about itself ([`OperationInfo`]), and
//! [`Observer::complete`] after, with how it ended ([`OperationOutcome`]) and
//! how long it took. An operation whose future is dropped before it answered
//! (the client went away) completes as [`OperationOutcome::Cancelled`].
//!
//! The observer is passive: it cannot refuse, delay or rewrite anything. The
//! span [`Observer::start`] returns is the one the dispatch runs in, so every
//! span and event a tool handler emits nests under it.
//!
//! A server with no observer does none of this work. With the `otel` feature,
//! [`OtelObserver`] records the `OpenTelemetry` MCP semantic conventions: a
//! span per operation, joined to a `params._meta.traceparent` when the request
//! carries one, and the `mcp.server.operation.duration` histogram.
//!
//! # Payloads
//!
//! Tool arguments and results routinely carry credentials and personal data —
//! for dravr, health data. Nothing is captured unless a
//! [`PayloadCapturePolicy`] installed with [`McpServer::with_payload_capture`]
//! turns it on, and what is captured goes through the host's
//! [`PayloadRedactor`] before it is serialized, then is cut to the policy's
//! byte limit on a character boundary. An observer only ever sees the
//! [`CapturedPayload`] that comes out.
//!
//! [`McpServer::with_observer`]: crate::mcp::server::McpServer::with_observer
//! [`McpServer::with_payload_capture`]: crate::mcp::server::McpServer::with_payload_capture

#[cfg(feature = "otel")]
mod otel;

use std::fmt;
use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::Value;
use tracing::{Instrument, Span};

use crate::mcp::modern::meta_keys;
use crate::mcp::protocol::{JsonRpcRequest, JsonRpcResponse};
use crate::server::trace_context::TraceContext;

#[cfg(feature = "otel")]
pub use otel::{semconv, OtelObserver};

/// `tools/call`, the method whose arguments and results are payloads.
pub const TOOLS_CALL: &str = "tools/call";

/// `prompts/get`, the other method whose `params.name` names a target.
pub const PROMPTS_GET: &str = "prompts/get";

/// `request.metadata` key under which the HTTP transport records the protocol
/// revision of a request's accepted `MCP-Protocol-Version` header.
const PROTOCOL_VERSION_METADATA: &str = "mcp-protocol-version";

/// Header naming the MCP session a request belongs to, when it has one.
const SESSION_ID_HEADER: &str = "mcp-session-id";

/// Default byte limit of one captured payload.
pub const DEFAULT_MAX_PAYLOAD_BYTES: usize = 4096;

/// Appended to a payload cut at the byte limit; its own bytes count toward it.
pub const TRUNCATION_MARKER: &str = "…(truncated)";

/// A passive observer of MCP dispatch.
///
/// Both methods run on the dispatch path, inline: keep them cheap, and never
/// block or panic in them. The default methods observe nothing, so an
/// observer implements only what it uses.
pub trait Observer: Send + Sync {
    /// An operation is about to be dispatched.
    ///
    /// Returns the span the dispatch runs in; [`Span::none`] for none.
    fn start(&self, info: &OperationInfo) -> Span {
        let _ = info;
        Span::none()
    }

    /// The operation ended. `span` is the one [`start`](Self::start)
    /// returned; `elapsed` runs from just before `start` was called.
    fn complete(
        &self,
        info: &OperationInfo,
        span: &Span,
        outcome: &OperationOutcome,
        elapsed: Duration,
    ) {
        let _ = (info, span, outcome, elapsed);
    }
}

/// What a dispatched message says about itself, read before dispatch.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct OperationInfo {
    /// The JSON-RPC method.
    pub method: String,
    /// The JSON-RPC id; `None` for a notification.
    pub request_id: Option<Value>,
    /// The tool a `tools/call` names, or the prompt a `prompts/get` names.
    pub target: Option<String>,
    /// The MCP protocol revision the request declared: its modern `_meta`
    /// revision, or the `MCP-Protocol-Version` header the HTTP transport
    /// accepted.
    pub protocol_version: Option<String>,
    /// The `Mcp-Session-Id` header, when the client sent one.
    pub session_id: Option<String>,
    /// The W3C trace context in `params._meta`, when it carries a valid one.
    pub trace_context: Option<TraceContext>,
    /// A `tools/call`'s arguments, when the capture policy takes them.
    pub arguments: Option<CapturedPayload>,
}

impl OperationInfo {
    /// Read `request` under `policy`.
    fn of(request: &JsonRpcRequest, policy: &PayloadCapturePolicy) -> Self {
        let params = request.params.as_ref();
        let target = match request.method.as_str() {
            TOOLS_CALL | PROMPTS_GET => params
                .and_then(|p| p.get("name"))
                .and_then(Value::as_str)
                .map(str::to_owned),
            _ => None,
        };
        let protocol_version = params
            .and_then(|p| p.get("_meta"))
            .and_then(|meta| meta.get(meta_keys::PROTOCOL_VERSION))
            .and_then(Value::as_str)
            .or_else(|| {
                request
                    .metadata
                    .get(PROTOCOL_VERSION_METADATA)
                    .map(String::as_str)
            })
            .filter(|version| is_protocol_version_shaped(version))
            .map(str::to_owned);
        let session_id = request
            .headers
            .as_ref()
            .and_then(|headers| headers.get(SESSION_ID_HEADER))
            .and_then(Value::as_str)
            .map(str::to_owned);
        let arguments = (policy.request_arguments && request.method == TOOLS_CALL).then(|| {
            let arguments = params
                .and_then(|p| p.get("arguments"))
                .cloned()
                .unwrap_or_else(|| Value::Object(serde_json::Map::new()));
            policy.capture(
                &PayloadSite {
                    method: &request.method,
                    target: target.as_deref(),
                    kind: PayloadKind::Arguments,
                },
                arguments,
            )
        });

        Self {
            method: request.method.clone(),
            request_id: request.id.clone(),
            target,
            protocol_version,
            session_id,
            trace_context: TraceContext::from_meta(params),
            arguments,
        }
    }

    /// Whether this is a `tools/call`.
    #[must_use]
    pub fn is_tool_call(&self) -> bool {
        self.method == TOOLS_CALL
    }
}

/// A revision string as MCP writes one (`2025-11-25`): a value a client
/// controls is only recorded when it has that shape, so it cannot carry data
/// or blow up a metric's cardinality.
fn is_protocol_version_shaped(version: &str) -> bool {
    version.len() == 10
        && version.bytes().enumerate().all(|(i, b)| {
            if i == 4 || i == 7 {
                b == b'-'
            } else {
                b.is_ascii_digit()
            }
        })
}

/// How a dispatched operation ended.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum OperationOutcome {
    /// Answered with a result. For a `tools/call`, the tool result when the
    /// capture policy takes it.
    Completed {
        /// The captured `tools/call` result.
        result: Option<CapturedPayload>,
    },
    /// A `tools/call` answered with a result whose `isError` is true: the
    /// tool ran and reported a failure.
    ToolError {
        /// The captured `tools/call` result.
        result: Option<CapturedPayload>,
    },
    /// A `tools/call` answered with a task handle; the work goes on.
    TaskCreated,
    /// Answered with a JSON-RPC error.
    Failed {
        /// The JSON-RPC error code.
        code: i32,
        /// The JSON-RPC error message.
        message: String,
    },
    /// A notification, accepted with no response.
    NotificationAccepted,
    /// The dispatch was dropped before it answered: the client went away or
    /// the server shut down.
    Cancelled,
}

impl OperationOutcome {
    /// Classify what dispatch answered.
    fn of(
        info: &OperationInfo,
        response: Option<&JsonRpcResponse>,
        policy: &PayloadCapturePolicy,
    ) -> Self {
        let Some(response) = response else {
            return Self::NotificationAccepted;
        };
        if let Some(error) = &response.error {
            return Self::Failed {
                code: error.code,
                message: error.message.clone(),
            };
        }
        if !info.is_tool_call() {
            return Self::Completed { result: None };
        }
        let result = response.result.as_ref();
        if result
            .and_then(|r| r.get("resultType"))
            .and_then(Value::as_str)
            == Some("task")
        {
            return Self::TaskCreated;
        }
        let is_error = result
            .and_then(|r| r.get("isError"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let captured = policy.response_content.then(|| {
            policy.capture(
                &PayloadSite {
                    method: &info.method,
                    target: info.target.as_deref(),
                    kind: PayloadKind::Result,
                },
                result.cloned().unwrap_or(Value::Null),
            )
        });
        if is_error {
            Self::ToolError { result: captured }
        } else {
            Self::Completed { result: captured }
        }
    }
}

/// Which payload of an operation is being captured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PayloadKind {
    /// A `tools/call`'s `params.arguments`.
    Arguments,
    /// A `tools/call`'s result.
    Result,
}

/// Where a payload being captured comes from, for a [`PayloadRedactor`] to
/// decide what to keep.
#[derive(Debug, Clone, Copy)]
pub struct PayloadSite<'a> {
    /// The JSON-RPC method.
    pub method: &'a str,
    /// The tool the call names.
    pub target: Option<&'a str>,
    /// Arguments or result.
    pub kind: PayloadKind,
}

/// The host's say over what a captured payload keeps.
///
/// Runs on the JSON value, before it is serialized and truncated, so it can
/// drop or mask fields by structure rather than by text. Returning `None`
/// omits the payload entirely. A closure
/// `Fn(&PayloadSite<'_>, Value) -> Option<Value>` is a redactor.
pub trait PayloadRedactor: Send + Sync {
    /// The payload to keep, or `None` to keep nothing.
    fn redact(&self, site: &PayloadSite<'_>, payload: Value) -> Option<Value>;
}

impl<F> PayloadRedactor for F
where
    F: Fn(&PayloadSite<'_>, Value) -> Option<Value> + Send + Sync,
{
    fn redact(&self, site: &PayloadSite<'_>, payload: Value) -> Option<Value> {
        self(site, payload)
    }
}

/// Which payloads observers may see, and how much of each.
///
/// [`disabled`](Self::disabled) by default: capture runs only when a toggle
/// is on and an observer is installed, so a server that does not ask for
/// payloads never serializes one.
#[derive(Clone)]
pub struct PayloadCapturePolicy {
    request_arguments: bool,
    response_content: bool,
    max_bytes: usize,
    redactor: Option<Arc<dyn PayloadRedactor>>,
}

impl PayloadCapturePolicy {
    /// Capture nothing.
    #[must_use]
    pub fn disabled() -> Self {
        Self {
            request_arguments: false,
            response_content: false,
            max_bytes: DEFAULT_MAX_PAYLOAD_BYTES,
            redactor: None,
        }
    }

    /// Capture a `tools/call`'s arguments.
    #[must_use]
    pub fn with_request_arguments(mut self, enabled: bool) -> Self {
        self.request_arguments = enabled;
        self
    }

    /// Capture a `tools/call`'s result.
    #[must_use]
    pub fn with_response_content(mut self, enabled: bool) -> Self {
        self.response_content = enabled;
        self
    }

    /// Cut every captured payload to at most `max_bytes` UTF-8 bytes,
    /// [`TRUNCATION_MARKER`] included. A limit too small to hold the marker
    /// omits every payload that exceeds it.
    #[must_use]
    pub fn with_max_bytes(mut self, max_bytes: usize) -> Self {
        self.max_bytes = max_bytes;
        self
    }

    /// Pass every payload through `redactor` before it is serialized.
    #[must_use]
    pub fn with_redactor(mut self, redactor: Arc<dyn PayloadRedactor>) -> Self {
        self.redactor = Some(redactor);
        self
    }

    /// Whether the policy captures anything at all.
    #[must_use]
    pub fn captures_anything(&self) -> bool {
        self.request_arguments || self.response_content
    }

    /// Redact, serialize and truncate one payload.
    #[must_use]
    pub fn capture(&self, site: &PayloadSite<'_>, payload: Value) -> CapturedPayload {
        let payload = match &self.redactor {
            Some(redactor) => match redactor.redact(site, payload) {
                Some(kept) => kept,
                None => return CapturedPayload::Omitted(OmissionReason::Redacted),
            },
            None => payload,
        };
        serde_json::to_string(&payload).map_or(
            CapturedPayload::Omitted(OmissionReason::Unserializable),
            |json| truncate_utf8(json, self.max_bytes),
        )
    }
}

impl Default for PayloadCapturePolicy {
    fn default() -> Self {
        Self::disabled()
    }
}

impl fmt::Debug for PayloadCapturePolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PayloadCapturePolicy")
            .field("request_arguments", &self.request_arguments)
            .field("response_content", &self.response_content)
            .field("max_bytes", &self.max_bytes)
            .field("redactor", &self.redactor.is_some())
            .finish()
    }
}

/// A payload as an observer receives it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CapturedPayload {
    /// The redacted JSON, cut to the policy's byte limit.
    Json(String),
    /// Nothing was kept.
    Omitted(OmissionReason),
}

impl CapturedPayload {
    /// The captured JSON, when something was kept.
    #[must_use]
    pub fn as_json(&self) -> Option<&str> {
        match self {
            Self::Json(json) => Some(json),
            Self::Omitted(_) => None,
        }
    }
}

/// Why a payload was not captured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum OmissionReason {
    /// The host's redactor kept nothing.
    Redacted,
    /// The payload exceeds a byte limit too small to hold the truncation
    /// marker.
    LimitTooSmall,
    /// The payload could not be serialized.
    Unserializable,
}

/// `json` cut to at most `max_bytes` bytes on a character boundary, with
/// [`TRUNCATION_MARKER`] appended when it was cut.
fn truncate_utf8(mut json: String, max_bytes: usize) -> CapturedPayload {
    if json.len() <= max_bytes {
        return CapturedPayload::Json(json);
    }
    let Some(budget) = max_bytes.checked_sub(TRUNCATION_MARKER.len()) else {
        return CapturedPayload::Omitted(OmissionReason::LimitTooSmall);
    };
    let cut = json.floor_char_boundary(budget);
    json.truncate(cut);
    json.push_str(TRUNCATION_MARKER);
    CapturedPayload::Json(json)
}

/// Dispatch `request` through `dispatch` under `observer`.
///
/// The server's one hook: it calls this around its dispatch when an observer
/// is installed.
pub(crate) async fn observe<F, Fut>(
    observer: &dyn Observer,
    policy: &PayloadCapturePolicy,
    request: JsonRpcRequest,
    dispatch: F,
) -> Option<JsonRpcResponse>
where
    F: FnOnce(JsonRpcRequest) -> Fut,
    Fut: Future<Output = Option<JsonRpcResponse>>,
{
    let started = Instant::now();
    let info = OperationInfo::of(&request, policy);
    let span = observer.start(&info);
    let mut pending = Pending {
        observer,
        info,
        span,
        started,
        completed: false,
    };

    let response = dispatch(request).instrument(pending.span.clone()).await;

    let outcome = OperationOutcome::of(&pending.info, response.as_ref(), policy);
    pending.complete(&outcome);
    response
}

/// One observed operation between start and complete. Dropped uncompleted, it
/// completes as [`OperationOutcome::Cancelled`], so every start has exactly
/// one complete.
struct Pending<'a> {
    observer: &'a dyn Observer,
    info: OperationInfo,
    span: Span,
    started: Instant,
    completed: bool,
}

impl Pending<'_> {
    fn complete(&mut self, outcome: &OperationOutcome) {
        self.completed = true;
        self.observer
            .complete(&self.info, &self.span, outcome, self.started.elapsed());
    }
}

impl Drop for Pending<'_> {
    fn drop(&mut self) {
        if !self.completed {
            self.complete(&OperationOutcome::Cancelled);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tool_call(arguments: &Value) -> JsonRpcRequest {
        serde_json::from_value(json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "tools/call",
            "params": { "name": "get_sleep", "arguments": arguments }
        }))
        .unwrap() // Safe: test assertion
    }

    #[test]
    fn a_payload_under_the_limit_is_kept_whole() {
        assert_eq!(
            truncate_utf8("{\"a\":1}".to_owned(), 7),
            CapturedPayload::Json("{\"a\":1}".to_owned())
        );
    }

    #[test]
    fn a_long_payload_is_cut_on_a_character_boundary() {
        // Every `é` is two bytes: a cut at an odd byte would split one.
        let json = "é".repeat(100);
        let max = TRUNCATION_MARKER.len() + 7;
        let cut = truncate_utf8(json, max);
        assert_eq!(
            cut,
            CapturedPayload::Json(format!("{}{TRUNCATION_MARKER}", "é".repeat(3)))
        );
        assert!(cut.as_json().map_or(usize::MAX, str::len) <= max);
    }

    #[test]
    fn a_limit_too_small_for_the_marker_omits_the_payload() {
        assert_eq!(
            truncate_utf8("x".repeat(50), 3),
            CapturedPayload::Omitted(OmissionReason::LimitTooSmall)
        );
    }

    #[test]
    fn the_default_policy_captures_nothing() {
        let policy = PayloadCapturePolicy::default();
        assert!(!policy.captures_anything());
        let info = OperationInfo::of(&tool_call(&json!({"hrv": 42})), &policy);
        assert_eq!(info.arguments, None);
        assert_eq!(info.target.as_deref(), Some("get_sleep"));
    }

    #[test]
    fn the_redactor_runs_before_serialization() {
        let policy = PayloadCapturePolicy::disabled()
            .with_request_arguments(true)
            .with_redactor(Arc::new(|site: &PayloadSite<'_>, mut payload: Value| {
                assert_eq!(site.kind, PayloadKind::Arguments);
                assert_eq!(site.target, Some("get_sleep"));
                if let Some(object) = payload.as_object_mut() {
                    object.insert("hrv".to_owned(), json!("[redacted]"));
                }
                Some(payload)
            }));
        let info = OperationInfo::of(&tool_call(&json!({"hrv": 42, "day": "mon"})), &policy);
        let json = info.arguments.as_ref().and_then(CapturedPayload::as_json);
        let parsed: Value = serde_json::from_str(json.unwrap()).unwrap(); // Safe: test assertion
        assert_eq!(parsed, json!({"hrv": "[redacted]", "day": "mon"}));
    }

    #[test]
    fn a_redactor_that_keeps_nothing_omits_the_payload() {
        let policy = PayloadCapturePolicy::disabled()
            .with_request_arguments(true)
            .with_redactor(Arc::new(|_: &PayloadSite<'_>, _: Value| None));
        let info = OperationInfo::of(&tool_call(&json!({"hrv": 42})), &policy);
        assert_eq!(
            info.arguments,
            Some(CapturedPayload::Omitted(OmissionReason::Redacted))
        );
    }

    #[test]
    fn the_meta_trace_context_and_protocol_version_are_read() {
        let request: JsonRpcRequest = serde_json::from_value(json!({
            "jsonrpc": "2.0",
            "id": "a",
            "method": "tools/list",
            "params": { "_meta": {
                "traceparent": "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
                "io.modelcontextprotocol/protocolVersion": "2026-07-28"
            }}
        }))
        .unwrap(); // Safe: test assertion
        let info = OperationInfo::of(&request, &PayloadCapturePolicy::default());
        assert_eq!(info.protocol_version.as_deref(), Some("2026-07-28"));
        assert_eq!(
            info.trace_context.as_ref().map(TraceContext::trace_id),
            Some("4bf92f3577b34da6a3ce929d0e0e4736")
        );
        assert_eq!(info.target, None);
    }

    #[test]
    fn a_protocol_version_of_another_shape_is_not_recorded() {
        for odd in ["latest", "2026-07-28-evil", "2026/07/28", ""] {
            assert!(!is_protocol_version_shaped(odd), "{odd:?}");
        }
        assert!(is_protocol_version_shaped("2025-11-25"));
    }
}
