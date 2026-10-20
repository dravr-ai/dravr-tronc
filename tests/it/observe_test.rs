// ABOUTME: Tests MCP dispatch observation: every message starts and completes once, with the outcome it ended in
// ABOUTME: Pins payload capture (off by default, redacted, truncated) and the span a tool handler runs in
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::str_to_string
)]

use std::sync::{Arc, Mutex, Once};
use std::time::Duration;

use async_trait::async_trait;
use dravr_tronc::error::{INVALID_PARAMS, METHOD_NOT_FOUND};
use dravr_tronc::mcp::observe::{
    CapturedPayload, Observer, OmissionReason, OperationInfo, OperationOutcome,
    PayloadCapturePolicy, PayloadKind, PayloadSite, TRUNCATION_MARKER,
};
use dravr_tronc::mcp::schema::{Tool, ToolResponse};
use dravr_tronc::mcp::server::McpServer;
use dravr_tronc::mcp::tool::{McpTool, ToolContext, ToolRegistry};
use serde_json::{json, Value};
use tokio::time;
use tracing::subscriber::set_global_default;
use tracing::{info_span, Span};
use tracing_subscriber::registry;

struct State;

/// A process-wide registry, so spans are recorded on every test thread.
///
/// Process-wide rather than per-test: a span's callsite caches whether any
/// subscriber wants it, and a thread-local one installed after another test
/// thread ran with none can find that cache already saying no.
fn span_registry() {
    static INSTALLED: Once = Once::new();
    INSTALLED.call_once(|| {
        set_global_default(registry()).unwrap();
    });
}

/// Answers with the name of the span it runs in, or `isError` when asked.
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
            return ToolResponse::error("no sleep recorded".to_owned());
        }
        if let Some(ms) = arguments.get("delay_ms").and_then(Value::as_u64) {
            time::sleep(Duration::from_millis(ms)).await;
        }
        let span = Span::current()
            .metadata()
            .map_or("none", |metadata| metadata.name());
        ToolResponse::text(format!("hrv=42 span={span}"))
    }
}

/// Records every start and complete.
#[derive(Default)]
struct Recorder {
    started: Mutex<Vec<OperationInfo>>,
    completed: Mutex<Vec<(String, OperationOutcome)>>,
}

impl Observer for Recorder {
    fn start(&self, info: &OperationInfo) -> Span {
        self.started.lock().unwrap().push(info.clone());
        info_span!("observed")
    }

    fn complete(
        &self,
        info: &OperationInfo,
        _span: &Span,
        outcome: &OperationOutcome,
        _elapsed: Duration,
    ) {
        self.completed
            .lock()
            .unwrap()
            .push((info.method.clone(), outcome.clone()));
    }
}

fn observed_server(policy: PayloadCapturePolicy) -> (McpServer<State>, Arc<Recorder>) {
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(SleepTool));
    let recorder = Arc::new(Recorder::default());
    let server = McpServer::new("test", "0.1.0", registry, Arc::new(State))
        .with_observer(Arc::clone(&recorder) as Arc<dyn Observer>)
        .with_payload_capture(policy);
    (server, recorder)
}

fn call(arguments: &Value) -> String {
    json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": { "name": "get_sleep", "arguments": arguments }
    })
    .to_string()
}

fn outcomes(recorder: &Recorder) -> Vec<(String, OperationOutcome)> {
    recorder.completed.lock().unwrap().clone()
}

#[tokio::test]
async fn a_tool_call_starts_and_completes_once_inside_the_observer_span() {
    span_registry();
    let (server, recorder) = observed_server(PayloadCapturePolicy::default());

    let response = server.handle_raw(&call(&json!({}))).await.unwrap();

    let text = response.result.unwrap()["content"][0]["text"].clone();
    assert_eq!(
        text, "hrv=42 span=observed",
        "the tool runs in the observer's span"
    );
    let started = recorder.started.lock().unwrap().clone();
    assert_eq!(started.len(), 1);
    assert_eq!(started[0].method, "tools/call");
    assert_eq!(started[0].target.as_deref(), Some("get_sleep"));
    assert_eq!(started[0].request_id, Some(json!(1)));
    assert_eq!(started[0].arguments, None, "capture is off by default");
    assert_eq!(
        outcomes(&recorder),
        vec![(
            "tools/call".to_owned(),
            OperationOutcome::Completed { result: None }
        )]
    );
}

#[tokio::test]
async fn each_way_an_operation_ends_is_its_own_outcome() {
    let (server, recorder) = observed_server(PayloadCapturePolicy::default());

    server.handle_raw(&call(&json!({"fail": true}))).await;
    server
        .handle_raw(r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"nope"}}"#)
        .await;
    server
        .handle_raw(r#"{"jsonrpc":"2.0","id":3,"method":"resources/list"}"#)
        .await;
    server
        .handle_raw(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
        .await;
    server
        .handle_raw(r#"{"jsonrpc":"2.0","id":4,"method":"ping"}"#)
        .await;

    let outcomes = outcomes(&recorder);
    assert_eq!(outcomes.len(), 5, "{outcomes:?}");
    assert_eq!(outcomes[0].1, OperationOutcome::ToolError { result: None });
    assert!(
        matches!(&outcomes[1].1, OperationOutcome::Failed { code, message }
            if *code == INVALID_PARAMS && message.contains("nope")),
        "{:?}",
        outcomes[1]
    );
    assert!(
        matches!(&outcomes[2].1, OperationOutcome::Failed { code, .. } if *code == METHOD_NOT_FOUND),
        "{:?}",
        outcomes[2]
    );
    assert_eq!(
        outcomes[3],
        (
            "notifications/initialized".to_owned(),
            OperationOutcome::NotificationAccepted
        )
    );
    assert_eq!(outcomes[4].1, OperationOutcome::Completed { result: None });
}

#[tokio::test]
async fn a_dispatch_dropped_before_it_answered_completes_as_cancelled() {
    let (server, recorder) = observed_server(PayloadCapturePolicy::default());

    let abandoned = time::timeout(
        Duration::from_millis(20),
        server.handle_raw(&call(&json!({"delay_ms": 5_000}))),
    )
    .await;

    assert!(abandoned.is_err());
    assert_eq!(
        outcomes(&recorder),
        vec![("tools/call".to_owned(), OperationOutcome::Cancelled)]
    );
}

#[tokio::test]
async fn captured_payloads_are_redacted_then_truncated() {
    let policy = PayloadCapturePolicy::disabled()
        .with_request_arguments(true)
        .with_response_content(true)
        .with_max_bytes(40)
        .with_redactor(Arc::new(|site: &PayloadSite<'_>, payload: Value| {
            match site.kind {
                // The athlete id goes; the rest stays.
                PayloadKind::Arguments => {
                    let mut payload = payload;
                    payload.as_object_mut()?.remove("athlete_id");
                    Some(payload)
                }
                PayloadKind::Result => Some(payload),
            }
        }));
    let (server, recorder) = observed_server(policy);

    server
        .handle_raw(&call(&json!({"athlete_id": "a-123", "night": "mon"})))
        .await;

    let started = recorder.started.lock().unwrap().clone();
    assert_eq!(
        started[0].arguments,
        Some(CapturedPayload::Json(r#"{"night":"mon"}"#.to_owned()))
    );
    let OperationOutcome::Completed {
        result: Some(CapturedPayload::Json(result)),
    } = &outcomes(&recorder)[0].1
    else {
        panic!("the result is captured: {:?}", outcomes(&recorder));
    };
    assert!(result.len() <= 40, "{result}");
    assert!(result.ends_with(TRUNCATION_MARKER), "{result}");
    assert!(result.starts_with(r#"{"content":"#), "{result}");
}

#[tokio::test]
async fn a_redactor_that_keeps_nothing_leaves_only_the_reason() {
    let policy = PayloadCapturePolicy::disabled()
        .with_request_arguments(true)
        .with_response_content(true)
        .with_redactor(Arc::new(|_: &PayloadSite<'_>, _: Value| None));
    let (server, recorder) = observed_server(policy);

    server.handle_raw(&call(&json!({"hrv": 42}))).await;

    let started = recorder.started.lock().unwrap().clone();
    assert_eq!(
        started[0].arguments,
        Some(CapturedPayload::Omitted(OmissionReason::Redacted))
    );
    assert_eq!(
        outcomes(&recorder)[0].1,
        OperationOutcome::Completed {
            result: Some(CapturedPayload::Omitted(OmissionReason::Redacted))
        }
    );
}

#[tokio::test]
async fn an_unobserved_server_dispatches_unchanged() {
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(SleepTool));
    let server = McpServer::new("test", "0.1.0", registry, Arc::new(State));

    let response = server.handle_raw(&call(&json!({}))).await.unwrap();

    assert_eq!(
        response.result.unwrap()["content"][0]["text"],
        "hrv=42 span=none"
    );
}
