// ABOUTME: Tests a running tool's channel to its client over Streamable HTTP, through the testkit
// ABOUTME: Progress and logs stream as events before the response; requests need a session HTTP lacks
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::str_to_string
)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use dravr_tronc::error::{INTERNAL_ERROR, INVALID_PARAMS, INVALID_REQUEST, METHOD_NOT_FOUND};
use dravr_tronc::mcp::client_channel::ClientRequestError;
use dravr_tronc::mcp::elicitation::{
    ElicitRequest, ElicitationSchema, PrimitiveSchema, StringSchema,
};
use dravr_tronc::mcp::logging::LogLevel;
use dravr_tronc::mcp::modern::meta_keys;
use dravr_tronc::mcp::observe::{Observer, OperationInfo, OperationOutcome};
use dravr_tronc::mcp::schema::{
    CreateMessageRequest, LoggingCapability, ServerCapabilities, Tool, ToolResponse,
};
use dravr_tronc::mcp::server::McpServer;
use dravr_tronc::mcp::tool::{McpTool, ToolContext, ToolRegistry};
use dravr_tronc::testkit::assert::{assert_rpc_error, assert_tool_error};
use dravr_tronc::testkit::McpTestClient;
use futures::future::BoxFuture;
use serde_json::{json, Value};
use tokio::task::yield_now;
use tokio::time::sleep;
use tracing::Span;

struct State;

/// A tool whose behaviour is a closure over its context.
struct Fixture {
    name: &'static str,
    run: fn(ToolContext) -> BoxFuture<'static, ToolResponse>,
}

#[async_trait]
impl McpTool<State> for Fixture {
    fn definition(&self) -> Tool {
        Tool {
            name: self.name.to_owned(),
            description: self.name.to_owned(),
            input_schema: json!({ "type": "object" }),
            output_schema: None,
            annotations: None,
            execution: None,
        }
    }

    async fn execute(&self, _state: &Arc<State>, ctx: &ToolContext, _args: Value) -> ToolResponse {
        (self.run)(ctx.clone()).await
    }
}

fn sampling_request() -> CreateMessageRequest {
    serde_json::from_value(json!({
        "messages": [{ "role": "user", "content": { "type": "text", "text": "Say hi" } }],
        "maxTokens": 100
    }))
    .unwrap()
}

fn name_form() -> ElicitRequest {
    ElicitRequest {
        message: "Who are you?".to_owned(),
        requested_schema: ElicitationSchema {
            properties: [(
                "name".to_owned(),
                PrimitiveSchema::String(StringSchema::default()),
            )]
            .into(),
            required: vec!["name".to_owned()],
            ..ElicitationSchema::default()
        },
    }
}

fn registry() -> ToolRegistry<State> {
    let fixtures = [
        Fixture {
            name: "progress",
            run: |ctx| {
                Box::pin(async move {
                    for step in [0.0, 50.0, 100.0] {
                        ctx.client.progress(step, Some(100.0), None);
                        sleep(Duration::from_millis(5)).await;
                    }
                    ToolResponse::text("done".to_owned())
                })
            },
        },
        Fixture {
            name: "logs",
            run: |ctx| {
                Box::pin(async move {
                    ctx.client.log(LogLevel::Debug, None, json!("debug"));
                    ctx.client
                        .log(LogLevel::Info, Some("fixture"), json!("info"));
                    ctx.client
                        .log(LogLevel::Error, None, json!({ "error": true }));
                    ToolResponse::text("logged".to_owned())
                })
            },
        },
        Fixture {
            name: "sample",
            run: |ctx| {
                Box::pin(async move {
                    match ctx.client.create_message(&sampling_request()).await {
                        Ok(result) => ToolResponse::text(format!("LLM: {}", result.content.text)),
                        Err(e) => ToolResponse::error(e.to_string()),
                    }
                })
            },
        },
        Fixture {
            name: "ask",
            run: |ctx| {
                Box::pin(async move {
                    match ctx.client.elicit(&name_form()).await {
                        Ok(answer) => ToolResponse::text(format!(
                            "{:?} {}",
                            answer.action,
                            answer.content.map(Value::Object).unwrap_or_default()
                        )),
                        Err(ClientRequestError::Rpc(error)) => {
                            ToolResponse::error(format!("client error {}", error.code))
                        }
                        Err(e) => ToolResponse::error(e.to_string()),
                    }
                })
            },
        },
        Fixture {
            name: "boom",
            run: |ctx| {
                Box::pin(async move {
                    ctx.client.progress(1.0, None, None);
                    yield_now().await;
                    panic!("the tool broke mid-stream");
                })
            },
        },
    ];
    let mut registry = ToolRegistry::new();
    for fixture in fixtures {
        registry.register(Box::new(fixture));
    }
    registry
}

fn capabilities() -> ServerCapabilities {
    ServerCapabilities {
        logging: Some(LoggingCapability {}),
        ..ServerCapabilities::tools_only()
    }
}

fn sessionless() -> McpServer<State> {
    McpServer::new("channel-test", "0.1.0", registry(), Arc::new(State))
        .with_capabilities(capabilities())
}

fn call_params(name: &str, meta: &Value) -> Value {
    json!({ "name": name, "arguments": {}, "_meta": meta })
}

#[tokio::test]
async fn progress_streams_before_the_response_and_needs_a_token() {
    let client = McpTestClient::in_process(Arc::new(sessionless()));
    let streamed = client
        .exchange(
            "tools/call",
            Some(call_params("progress", &json!({ "progressToken": "p-1" }))),
        )
        .await
        .unwrap();
    let progress = streamed.notifications("notifications/progress");
    assert_eq!(progress.len(), 3, "{:?}", streamed.messages);
    let values: Vec<f64> = progress
        .iter()
        .map(|n| n["params"]["progress"].as_f64().unwrap())
        .collect();
    assert_eq!(values, vec![0.0, 50.0, 100.0]);
    assert!(progress
        .iter()
        .all(|n| n["params"]["progressToken"] == "p-1"));
    assert_eq!(
        streamed.response.result.unwrap()["content"][0]["text"],
        "done"
    );

    // No token: nothing to report under, so the answer is plain JSON.
    let quiet = client
        .exchange("tools/call", Some(call_params("progress", &json!({}))))
        .await
        .unwrap();
    assert!(quiet.messages.is_empty());
}

#[tokio::test]
async fn a_bad_level_is_invalid_params_and_a_sessionless_one_is_refused() {
    let client = McpTestClient::in_process(Arc::new(sessionless()));
    client.initialize().await.unwrap();
    let bad = client
        .request("logging/setLevel", Some(json!({ "level": "loud" })))
        .await
        .unwrap();
    assert_rpc_error(&bad, INVALID_PARAMS);

    let refused = client
        .request("logging/setLevel", Some(json!({ "level": "info" })))
        .await
        .unwrap();
    assert_rpc_error(&refused, INVALID_REQUEST);
}

#[tokio::test]
async fn set_level_is_unknown_on_a_server_without_logging() {
    let server = McpServer::new("no-logging", "0.1.0", registry(), Arc::new(State));
    let client = McpTestClient::in_process(Arc::new(server));
    client.initialize().await.unwrap();
    let unknown = client
        .request("logging/setLevel", Some(json!({ "level": "info" })))
        .await
        .unwrap();
    assert_rpc_error(&unknown, METHOD_NOT_FOUND);
}

#[tokio::test]
async fn a_request_needs_a_session_holding_the_declared_capability() {
    // HTTP keeps no session, so whatever initialize declared is not known
    // to a later call.
    let client = McpTestClient::in_process(Arc::new(sessionless()));
    client.initialize().await.unwrap();
    let refused = client.call_tool("sample", json!({})).await.unwrap();
    assert_tool_error(&refused, "sampling");
    let refused = client.call_tool("ask", json!({})).await.unwrap();
    assert_tool_error(&refused, "elicitation");
}

#[tokio::test]
async fn a_modern_call_logs_at_its_meta_level_and_never_sends_a_request() {
    let client = McpTestClient::in_process(Arc::new(sessionless())).modern();
    let silent = client
        .exchange("tools/call", Some(call_params("logs", &json!({}))))
        .await
        .unwrap();
    assert!(silent.messages.is_empty(), "no logLevel in _meta, no logs");

    let logged = client
        .exchange(
            "tools/call",
            Some(call_params(
                "logs",
                &json!({ meta_keys::LOG_LEVEL: "error" }),
            )),
        )
        .await
        .unwrap();
    assert_eq!(logged.notifications("notifications/message").len(), 1);

    let client = client.with_meta(
        meta_keys::CLIENT_CAPABILITIES,
        json!({ "sampling": {}, "elicitation": {} }),
    );
    let sampled = client
        .exchange("tools/call", Some(call_params("sample", &json!({}))))
        .await
        .unwrap();
    assert!(sampled.server_requests().is_empty());
    let result: ToolResponse = serde_json::from_value(sampled.response.result.unwrap()).unwrap();
    assert_tool_error(&result, "input-required");
}

#[tokio::test]
async fn a_tool_panicking_mid_stream_ends_with_an_internal_error() {
    let client = McpTestClient::in_process(Arc::new(sessionless()));
    let streamed = client
        .exchange(
            "tools/call",
            Some(call_params("boom", &json!({ "progressToken": 7 }))),
        )
        .await
        .unwrap();
    assert_eq!(streamed.notifications("notifications/progress").len(), 1);
    assert_rpc_error(&streamed.response, INTERNAL_ERROR);
}

/// Counts every completed operation.
#[derive(Default)]
struct Completions(AtomicUsize);

impl Observer for Completions {
    fn complete(
        &self,
        _info: &OperationInfo,
        _span: &Span,
        _outcome: &OperationOutcome,
        _elapsed: Duration,
    ) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[tokio::test]
async fn a_streamed_call_is_observed_once() {
    let observer = Arc::new(Completions::default());
    let server = sessionless().with_observer(observer.clone());
    let client = McpTestClient::in_process(Arc::new(server));
    let streamed = client
        .exchange(
            "tools/call",
            Some(call_params("progress", &json!({ "progressToken": "o" }))),
        )
        .await
        .unwrap();
    assert_eq!(streamed.notifications("notifications/progress").len(), 3);
    assert_eq!(
        observer.0.load(Ordering::SeqCst),
        1,
        "a streamed call completes once"
    );
}
