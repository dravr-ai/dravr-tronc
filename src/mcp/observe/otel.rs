// ABOUTME: OtelObserver: an MCP server span and an mcp.server.operation.duration measurement per dispatched message
// ABOUTME: Follows the OpenTelemetry MCP semantic conventions; a `_meta` traceparent makes the span continue that trace
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use std::borrow::Cow;
use std::sync::OnceLock;
use std::time::Duration;

use opentelemetry::metrics::{Histogram, Meter};
use opentelemetry::trace::Status;
use opentelemetry::{global, KeyValue};
use serde_json::Value;
use tracing::{info_span, Span};
use tracing_opentelemetry::OpenTelemetrySpanExt as _;

use super::{CapturedPayload, Observer, OperationInfo, OperationOutcome, PROMPTS_GET, TOOLS_CALL};
use crate::mcp::protocol::JSONRPC_VERSION;
use crate::server::trace_context::join_trace;

/// The `OpenTelemetry` MCP and `GenAI` semantic-convention keys and values
/// [`OtelObserver`] records, verbatim.
///
/// Declared here rather than taken from a semconv crate: the MCP keys moved to
/// the `GenAI` conventions repository, which publishes no Rust constants.
pub mod semconv {
    /// The JSON-RPC method.
    pub const MCP_METHOD_NAME: &str = "mcp.method.name";
    /// The MCP protocol revision of the request.
    pub const MCP_PROTOCOL_VERSION: &str = "mcp.protocol.version";
    /// The MCP session id.
    pub const MCP_SESSION_ID: &str = "mcp.session.id";
    /// Always `2.0`.
    pub const JSONRPC_PROTOCOL_VERSION: &str = "jsonrpc.protocol.version";
    /// The JSON-RPC request id. Span only: it would explode a metric.
    pub const JSONRPC_REQUEST_ID: &str = "jsonrpc.request.id";
    /// The JSON-RPC error code of an error response, as a string.
    pub const RPC_RESPONSE_STATUS_CODE: &str = "rpc.response.status_code";
    /// The class of error an operation ended with.
    pub const ERROR_TYPE: &str = "error.type";
    /// The tool a `tools/call` names.
    pub const GEN_AI_TOOL_NAME: &str = "gen_ai.tool.name";
    /// The prompt a `prompts/get` names.
    pub const GEN_AI_PROMPT_NAME: &str = "gen_ai.prompt.name";
    /// The `GenAI` operation; [`EXECUTE_TOOL`] for a `tools/call`.
    pub const GEN_AI_OPERATION_NAME: &str = "gen_ai.operation.name";
    /// A `tools/call`'s captured arguments. Opt-in: they carry personal data.
    pub const GEN_AI_TOOL_CALL_ARGUMENTS: &str = "gen_ai.tool.call.arguments";
    /// A `tools/call`'s captured result. Opt-in, like the arguments.
    pub const GEN_AI_TOOL_CALL_RESULT: &str = "gen_ai.tool.call.result";
    /// [`GEN_AI_OPERATION_NAME`] of a tool call.
    pub const EXECUTE_TOOL: &str = "execute_tool";
    /// [`ERROR_TYPE`] of a tool result carrying `isError: true`.
    pub const TOOL_ERROR: &str = "tool_error";
    /// The histogram of operation durations, in seconds.
    pub const MCP_SERVER_OPERATION_DURATION: &str = "mcp.server.operation.duration";
}

use semconv::{
    ERROR_TYPE, EXECUTE_TOOL, GEN_AI_OPERATION_NAME, GEN_AI_PROMPT_NAME,
    GEN_AI_TOOL_CALL_ARGUMENTS, GEN_AI_TOOL_CALL_RESULT, GEN_AI_TOOL_NAME,
    JSONRPC_PROTOCOL_VERSION, JSONRPC_REQUEST_ID, MCP_METHOD_NAME, MCP_PROTOCOL_VERSION,
    MCP_SERVER_OPERATION_DURATION, MCP_SESSION_ID, RPC_RESPONSE_STATUS_CODE, TOOL_ERROR,
};

/// Instrumentation scope of the meter.
const INSTRUMENTATION_NAME: &str = "dravr-tronc";

/// Bucket boundaries the MCP semantic conventions advise for
/// [`MCP_SERVER_OPERATION_DURATION`], in seconds.
const OPERATION_DURATION_BUCKETS: [f64; 14] = [
    0.01, 0.02, 0.05, 0.1, 0.2, 0.5, 1.0, 2.0, 5.0, 10.0, 30.0, 60.0, 120.0, 300.0,
];

/// Records each dispatched MCP message as `OpenTelemetry` sees MCP.
///
/// - A `SERVER` span named `"{mcp.method.name} {target}"` (`tools/call
///   get_sleep`), or the method alone when there is no target. A valid
///   `params._meta.traceparent` makes it a child of that remote span;
///   otherwise it is a child of the span it was dispatched in — the HTTP
///   `request` span, which continues the `traceparent` header.
/// - [`MCP_METHOD_NAME`], [`JSONRPC_PROTOCOL_VERSION`],
///   [`MCP_PROTOCOL_VERSION`], [`GEN_AI_TOOL_NAME`] with
///   [`GEN_AI_OPERATION_NAME`] = [`EXECUTE_TOOL`] for a tool call or
///   [`GEN_AI_PROMPT_NAME`] for a prompt, on the span and the histogram;
///   [`JSONRPC_REQUEST_ID`] and [`MCP_SESSION_ID`] on the span only.
/// - On a JSON-RPC error, [`RPC_RESPONSE_STATUS_CODE`] and [`ERROR_TYPE`]
///   set to the error code, and an error status; on a tool result with
///   `isError`, [`ERROR_TYPE`] = [`TOOL_ERROR`] and an error status.
/// - The captured arguments and result, when the server's
///   [`PayloadCapturePolicy`](super::PayloadCapturePolicy) took them, as
///   [`GEN_AI_TOOL_CALL_ARGUMENTS`] and [`GEN_AI_TOOL_CALL_RESULT`].
/// - One [`MCP_SERVER_OPERATION_DURATION`] measurement, in seconds.
///
/// The span's attributes go to `OpenTelemetry` directly, not through
/// `tracing` fields, so a payload never reaches a log line. The span is a
/// `tracing` span at INFO, so `RUST_LOG` filtering it out disables it.
pub struct OtelObserver {
    duration: OnceLock<Histogram<f64>>,
}

impl OtelObserver {
    /// An observer that records through the global meter provider, which
    /// [`tracing_init`](crate::server::tracing_init) installs.
    ///
    /// The histogram is created on the first operation, so the observer may
    /// be built before the provider is installed.
    #[must_use]
    pub fn new() -> Self {
        Self {
            duration: OnceLock::new(),
        }
    }

    /// An observer that records through `meter` instead of the global
    /// provider.
    #[must_use]
    pub fn with_meter(meter: &Meter) -> Self {
        let duration = OnceLock::new();
        let _ = duration.set(build_duration_histogram(meter));
        Self { duration }
    }

    fn duration(&self) -> &Histogram<f64> {
        self.duration
            .get_or_init(|| build_duration_histogram(&global::meter(INSTRUMENTATION_NAME)))
    }
}

impl Default for OtelObserver {
    fn default() -> Self {
        Self::new()
    }
}

fn build_duration_histogram(meter: &Meter) -> Histogram<f64> {
    meter
        .f64_histogram(MCP_SERVER_OPERATION_DURATION)
        .with_unit("s")
        .with_description("MCP request or notification duration as observed on the receiver")
        .with_boundaries(OPERATION_DURATION_BUCKETS.to_vec())
        .build()
}

/// The target the span name and the `gen_ai` attributes carry.
fn target(info: &OperationInfo) -> Option<&str> {
    match info.method.as_str() {
        TOOLS_CALL | PROMPTS_GET => info.target.as_deref(),
        _ => None,
    }
}

/// Attributes the span and the histogram share; all low-cardinality.
fn shared_attributes(info: &OperationInfo) -> Vec<KeyValue> {
    let mut attributes = vec![
        KeyValue::new(MCP_METHOD_NAME, info.method.clone()),
        KeyValue::new(JSONRPC_PROTOCOL_VERSION, JSONRPC_VERSION),
    ];
    if let Some(target) = target(info) {
        if info.is_tool_call() {
            attributes.push(KeyValue::new(GEN_AI_TOOL_NAME, target.to_owned()));
            attributes.push(KeyValue::new(GEN_AI_OPERATION_NAME, EXECUTE_TOOL));
        } else {
            attributes.push(KeyValue::new(GEN_AI_PROMPT_NAME, target.to_owned()));
        }
    }
    if let Some(version) = &info.protocol_version {
        attributes.push(KeyValue::new(MCP_PROTOCOL_VERSION, version.clone()));
    }
    attributes
}

/// A JSON-RPC id as the span records it: a string id as itself, a number as
/// its digits.
fn request_id_text(id: &Value) -> String {
    id.as_str().map_or_else(|| id.to_string(), str::to_owned)
}

fn record_payload(span: &Span, key: &'static str, payload: Option<&CapturedPayload>) {
    if let Some(json) = payload.and_then(CapturedPayload::as_json) {
        span.set_attribute(key, json.to_owned());
    }
}

impl Observer for OtelObserver {
    fn start(&self, info: &OperationInfo) -> Span {
        let name = target(info).map_or_else(
            || Cow::Borrowed(info.method.as_str()),
            |target| Cow::Owned(format!("{} {target}", info.method)),
        );
        let span = info_span!("mcp.server.operation", otel.name = %name, otel.kind = "server");
        if let Some(context) = &info.trace_context {
            join_trace(&span, context);
        }
        for attribute in shared_attributes(info) {
            span.set_attribute(attribute.key, attribute.value);
        }
        if let Some(id) = &info.request_id {
            span.set_attribute(JSONRPC_REQUEST_ID, request_id_text(id));
        }
        if let Some(session) = &info.session_id {
            span.set_attribute(MCP_SESSION_ID, session.clone());
        }
        record_payload(&span, GEN_AI_TOOL_CALL_ARGUMENTS, info.arguments.as_ref());
        span
    }

    fn complete(
        &self,
        info: &OperationInfo,
        span: &Span,
        outcome: &OperationOutcome,
        elapsed: Duration,
    ) {
        let mut metric_attributes = shared_attributes(info);
        match outcome {
            OperationOutcome::Completed { result } => {
                record_payload(span, GEN_AI_TOOL_CALL_RESULT, result.as_ref());
            }
            OperationOutcome::ToolError { result } => {
                record_payload(span, GEN_AI_TOOL_CALL_RESULT, result.as_ref());
                span.set_attribute(ERROR_TYPE, TOOL_ERROR);
                metric_attributes.push(KeyValue::new(ERROR_TYPE, TOOL_ERROR));
                span.set_status(Status::error("Tool call returned an error result"));
            }
            OperationOutcome::Failed { code, message } => {
                let code = code.to_string();
                span.set_attribute(RPC_RESPONSE_STATUS_CODE, code.clone());
                span.set_attribute(ERROR_TYPE, code.clone());
                metric_attributes.push(KeyValue::new(RPC_RESPONSE_STATUS_CODE, code.clone()));
                metric_attributes.push(KeyValue::new(ERROR_TYPE, code));
                span.set_status(Status::error(message.clone()));
            }
            OperationOutcome::TaskCreated
            | OperationOutcome::NotificationAccepted
            | OperationOutcome::Cancelled => {}
        }
        self.duration()
            .record(elapsed.as_secs_f64(), &metric_attributes);
    }
}
