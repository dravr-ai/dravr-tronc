// ABOUTME: Tests OtelObserver against in-memory exporters: MCP semantic-convention span attributes, status and duration
// ABOUTME: Pins the span name, the error classification, the _meta trace parent and the opt-in payload attributes
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

#![cfg(feature = "otel")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::str_to_string
)]

use std::sync::Arc;

use async_trait::async_trait;
use dravr_tronc::mcp::observe::semconv::{
    ERROR_TYPE, EXECUTE_TOOL, GEN_AI_OPERATION_NAME, GEN_AI_TOOL_CALL_ARGUMENTS,
    GEN_AI_TOOL_CALL_RESULT, GEN_AI_TOOL_NAME, JSONRPC_PROTOCOL_VERSION, JSONRPC_REQUEST_ID,
    MCP_METHOD_NAME, MCP_PROTOCOL_VERSION, MCP_SERVER_OPERATION_DURATION, RPC_RESPONSE_STATUS_CODE,
    TOOL_ERROR,
};
use dravr_tronc::mcp::observe::{Observer, OtelObserver, PayloadCapturePolicy};
use dravr_tronc::mcp::schema::{Tool, ToolResponse};
use dravr_tronc::mcp::server::McpServer;
use dravr_tronc::mcp::tool::{McpTool, ToolContext, ToolRegistry};
use opentelemetry::global;
use opentelemetry::metrics::MeterProvider as _;
use opentelemetry::trace::{SpanId, SpanKind, Status, TraceId, TracerProvider as _};
use opentelemetry::{Key, KeyValue, Value as OtelValue};
use opentelemetry_sdk::metrics::data::{AggregatedMetrics, MetricData};
use opentelemetry_sdk::metrics::{InMemoryMetricExporter, SdkMeterProvider};
use opentelemetry_sdk::propagation::TraceContextPropagator;
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider, SpanData};
use serde_json::{json, Value};
use tracing::subscriber::{set_default, DefaultGuard};
use tracing_subscriber::layer::SubscriberExt;

struct State;

struct SleepTool;

#[async_trait]
impl McpTool<State> for SleepTool {
    fn definition(&self) -> Tool {
        Tool {
            name: "get_sleep".to_owned(),
            description: "Last night's sleep".to_owned(),
            input_schema: json!({"type": "object"}),
            output_schema: None,
            annotations: None,
            execution: None,
        }
    }

    async fn execute(
        &self,
        _state: &Arc<State>,
        _ctx: &ToolContext,
        arguments: Value,
    ) -> ToolResponse {
        if arguments.get("fail").and_then(Value::as_bool) == Some(true) {
            ToolResponse::error("no sleep recorded".to_owned())
        } else {
            ToolResponse::text("hrv=42".to_owned())
        }
    }
}

/// An observed server exporting spans and metrics to memory.
struct Harness {
    server: McpServer<State>,
    spans: InMemorySpanExporter,
    metrics: InMemoryMetricExporter,
    meters: SdkMeterProvider,
    _tracer: SdkTracerProvider,
    _subscriber: DefaultGuard,
}

impl Harness {
    fn new(policy: PayloadCapturePolicy) -> Self {
        global::set_text_map_propagator(TraceContextPropagator::new());
        let spans = InMemorySpanExporter::default();
        let tracer = SdkTracerProvider::builder()
            .with_simple_exporter(spans.clone())
            .build();
        let subscriber = set_default(
            tracing_subscriber::registry()
                .with(tracing_opentelemetry::layer().with_tracer(tracer.tracer("test"))),
        );
        let metrics = InMemoryMetricExporter::default();
        let meters = SdkMeterProvider::builder()
            .with_periodic_exporter(metrics.clone())
            .build();
        let observer = OtelObserver::with_meter(&meters.meter("test"));

        let mut registry = ToolRegistry::new();
        registry.register(Box::new(SleepTool));
        let server = McpServer::new("test", "0.1.0", registry, Arc::new(State))
            .with_observer(Arc::new(observer) as Arc<dyn Observer>)
            .with_payload_capture(policy);
        Self {
            server,
            spans,
            metrics,
            meters,
            _tracer: tracer,
            _subscriber: subscriber,
        }
    }

    fn operation_span(&self) -> SpanData {
        let spans = self.spans.get_finished_spans().unwrap();
        spans
            .into_iter()
            .find(|span| span.span_kind == SpanKind::Server)
            .expect("one server span per operation")
    }

    /// The attributes of every `mcp.server.operation.duration` data point.
    fn duration_points(&self) -> Vec<Vec<KeyValue>> {
        self.meters.force_flush().unwrap();
        let mut points = Vec::new();
        for resource in self.metrics.get_finished_metrics().unwrap() {
            for scope in resource.scope_metrics() {
                for metric in scope.metrics() {
                    if metric.name() != MCP_SERVER_OPERATION_DURATION {
                        continue;
                    }
                    assert_eq!(metric.unit(), "s");
                    if let AggregatedMetrics::F64(MetricData::Histogram(histogram)) = metric.data()
                    {
                        for point in histogram.data_points() {
                            assert_eq!(point.count(), 1);
                            points.push(point.attributes().cloned().collect());
                        }
                    }
                }
            }
        }
        points
    }
}

fn attribute<'a>(attributes: &'a [KeyValue], key: &str) -> Option<&'a OtelValue> {
    attributes
        .iter()
        .find(|kv| kv.key == Key::from(key.to_owned()))
        .map(|kv| &kv.value)
}

fn text(attributes: &[KeyValue], key: &str) -> Option<String> {
    attribute(attributes, key).map(OtelValue::to_string)
}

fn call(id: u64, arguments: &Value) -> String {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "tools/call",
        "params": { "name": "get_sleep", "arguments": arguments }
    })
    .to_string()
}

#[tokio::test]
async fn a_tool_call_is_a_server_span_named_for_its_tool() {
    let harness = Harness::new(PayloadCapturePolicy::default());

    harness
        .server
        .handle_raw(&call(9, &json!({"hrv": 1})))
        .await;

    let span = harness.operation_span();
    assert_eq!(span.name, "tools/call get_sleep");
    let attributes = &span.attributes;
    assert_eq!(
        text(attributes, MCP_METHOD_NAME).as_deref(),
        Some("tools/call")
    );
    assert_eq!(
        text(attributes, GEN_AI_TOOL_NAME).as_deref(),
        Some("get_sleep")
    );
    assert_eq!(
        text(attributes, GEN_AI_OPERATION_NAME).as_deref(),
        Some(EXECUTE_TOOL)
    );
    assert_eq!(
        text(attributes, JSONRPC_PROTOCOL_VERSION).as_deref(),
        Some("2.0")
    );
    assert_eq!(text(attributes, JSONRPC_REQUEST_ID).as_deref(), Some("9"));
    assert_eq!(attribute(attributes, ERROR_TYPE), None);
    assert_eq!(
        attribute(attributes, GEN_AI_TOOL_CALL_ARGUMENTS),
        None,
        "payloads are opt-in"
    );
    assert_eq!(attribute(attributes, GEN_AI_TOOL_CALL_RESULT), None);
    assert_eq!(span.status, Status::Unset);

    let points = harness.duration_points();
    assert_eq!(points.len(), 1);
    assert_eq!(
        text(&points[0], GEN_AI_TOOL_NAME).as_deref(),
        Some("get_sleep")
    );
    assert_eq!(
        attribute(&points[0], JSONRPC_REQUEST_ID),
        None,
        "a request id never reaches a metric"
    );
}

#[tokio::test]
async fn a_tool_result_with_is_error_is_a_tool_error() {
    let harness = Harness::new(PayloadCapturePolicy::default());

    harness
        .server
        .handle_raw(&call(1, &json!({"fail": true})))
        .await;

    let span = harness.operation_span();
    assert_eq!(
        text(&span.attributes, ERROR_TYPE).as_deref(),
        Some(TOOL_ERROR)
    );
    assert_eq!(attribute(&span.attributes, RPC_RESPONSE_STATUS_CODE), None);
    assert!(
        matches!(span.status, Status::Error { .. }),
        "{:?}",
        span.status
    );
    let points = harness.duration_points();
    assert_eq!(text(&points[0], ERROR_TYPE).as_deref(), Some(TOOL_ERROR));
}

#[tokio::test]
async fn a_json_rpc_error_is_classified_by_its_code() {
    let harness = Harness::new(PayloadCapturePolicy::default());

    harness
        .server
        .handle_raw(r#"{"jsonrpc":"2.0","id":"x","method":"nope/nothing"}"#)
        .await;

    let span = harness.operation_span();
    assert_eq!(span.name, "nope/nothing");
    assert_eq!(
        text(&span.attributes, JSONRPC_REQUEST_ID).as_deref(),
        Some("x")
    );
    assert_eq!(
        text(&span.attributes, RPC_RESPONSE_STATUS_CODE).as_deref(),
        Some("-32601")
    );
    assert_eq!(
        text(&span.attributes, ERROR_TYPE).as_deref(),
        Some("-32601")
    );
    assert!(matches!(&span.status, Status::Error { description } if description.contains("nope")));
    let points = harness.duration_points();
    assert_eq!(
        text(&points[0], RPC_RESPONSE_STATUS_CODE).as_deref(),
        Some("-32601")
    );
}

#[tokio::test]
async fn a_meta_traceparent_parents_the_span_and_names_the_revision() {
    let harness = Harness::new(PayloadCapturePolicy::default());
    let request = json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "tools/list",
        "params": { "_meta": {
            "traceparent": "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
            "io.modelcontextprotocol/protocolVersion": "2026-07-28",
            "io.modelcontextprotocol/clientCapabilities": {}
        }}
    });

    harness.server.handle_raw(&request.to_string()).await;

    let span = harness.operation_span();
    assert_eq!(
        span.span_context.trace_id(),
        TraceId::from_hex("4bf92f3577b34da6a3ce929d0e0e4736").unwrap()
    );
    assert_eq!(
        span.parent_span_id,
        SpanId::from_hex("00f067aa0ba902b7").unwrap()
    );
    assert!(span.parent_span_is_remote);
    assert_eq!(
        text(&span.attributes, MCP_PROTOCOL_VERSION).as_deref(),
        Some("2026-07-28")
    );
}

#[tokio::test]
async fn captured_payloads_become_the_gen_ai_attributes() {
    let harness = Harness::new(
        PayloadCapturePolicy::disabled()
            .with_request_arguments(true)
            .with_response_content(true),
    );

    harness
        .server
        .handle_raw(&call(1, &json!({"night": "mon"})))
        .await;

    let span = harness.operation_span();
    assert_eq!(
        text(&span.attributes, GEN_AI_TOOL_CALL_ARGUMENTS).as_deref(),
        Some(r#"{"night":"mon"}"#)
    );
    let result = text(&span.attributes, GEN_AI_TOOL_CALL_RESULT).unwrap();
    assert!(result.contains("hrv=42"), "{result}");
    let points = harness.duration_points();
    assert_eq!(
        attribute(&points[0], GEN_AI_TOOL_CALL_ARGUMENTS),
        None,
        "a payload never reaches a metric"
    );
}
