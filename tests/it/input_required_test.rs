// ABOUTME: Tests for SEP-2322 multi round-trip on tools/call, outside the tasks extension
// ABOUTME: Pins the input_required wire shape and that the retry's answers and state reach the tool
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use async_trait::async_trait;
use dravr_tronc::mcp::host::{CallToolOutcome, ToolDispatcher};
use dravr_tronc::mcp::schema::{InputRequiredResult, Tool, ToolCall, ToolResponse};
use dravr_tronc::mcp::server::McpServer;
use dravr_tronc::mcp::tool::{ToolContext, ToolRegistry};
use serde_json::{json, Map, Value};

struct TestState;

/// Asks for confirmation on the first attempt; on the retry, answers with what
/// the client sent and the state it echoed.
struct ConfirmingDispatcher;

#[async_trait]
impl ToolDispatcher<TestState> for ConfirmingDispatcher {
    async fn list_tools(&self, _state: &Arc<TestState>, _ctx: &ToolContext) -> Vec<Tool> {
        Vec::new()
    }

    async fn call_tool(
        &self,
        _name: &str,
        _state: &Arc<TestState>,
        ctx: &ToolContext,
        _arguments: Value,
    ) -> CallToolOutcome {
        let Some(responses) = &ctx.input_responses else {
            let mut requests = Map::new();
            requests.insert(
                "confirm".to_owned(),
                json!({ "method": "elicitation/create", "params": { "message": "Delete?" } }),
            );
            return CallToolOutcome::InputRequired(Box::new(
                InputRequiredResult::new(requests).with_request_state("step-1"),
            ));
        };
        let answer = responses["confirm"]["action"].as_str().unwrap_or_default();
        let state = ctx.request_state.as_deref().unwrap_or_default();
        CallToolOutcome::from(ToolResponse::text(format!("{answer} at {state}")))
    }
}

fn server() -> McpServer<TestState> {
    McpServer::new(
        "test-server",
        "0.1.0",
        ToolRegistry::new(),
        Arc::new(TestState),
    )
    .with_tool_dispatcher(Arc::new(ConfirmingDispatcher))
}

fn modern_meta() -> Value {
    json!({
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientCapabilities": { "elicitation": {} }
    })
}

#[test]
fn input_required_result_serializes_flat_with_its_discriminator() {
    let mut requests = Map::new();
    requests.insert("a".to_owned(), json!({ "method": "roots/list" }));
    let value =
        serde_json::to_value(InputRequiredResult::new(requests).with_request_state("opaque"))
            .expect("serializes");
    assert_eq!(value["resultType"], "input_required");
    assert_eq!(value["inputRequests"]["a"]["method"], "roots/list");
    assert_eq!(value["requestState"], "opaque");

    // Load shedding: state only, no requests field at all.
    let value =
        serde_json::to_value(InputRequiredResult::retry_with_state("later")).expect("serializes");
    assert_eq!(value["resultType"], "input_required");
    assert!(value.get("inputRequests").is_none());
}

#[test]
fn tool_call_reads_input_responses_and_request_state() {
    let call: ToolCall = serde_json::from_value(json!({
        "name": "t",
        "inputResponses": { "confirm": { "action": "accept" } },
        "requestState": "step-1"
    }))
    .expect("deserializes");
    assert_eq!(
        call.input_responses.expect("present")["confirm"]["action"],
        "accept"
    );
    assert_eq!(call.request_state.as_deref(), Some("step-1"));
}

/// The full round trip: the first call is answered `input_required`, and the
/// retry's answers and echoed state reach the tool.
#[tokio::test]
async fn a_tool_call_asks_for_input_and_the_retry_carries_the_answers() {
    let server = server();
    let first = json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": { "name": "delete", "arguments": {}, "_meta": modern_meta() }
    })
    .to_string();
    let result = server
        .handle_raw(&first)
        .await
        .expect("response")
        .result
        .expect("success");
    assert_eq!(result["resultType"], "input_required");
    assert_eq!(
        result["inputRequests"]["confirm"]["method"],
        "elicitation/create"
    );
    let state = result["requestState"].as_str().expect("state").to_owned();

    let retry = json!({
        "jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": {
            "name": "delete",
            "arguments": {},
            "inputResponses": { "confirm": { "action": "accept" } },
            "requestState": state,
            "_meta": modern_meta()
        }
    })
    .to_string();
    let result = server
        .handle_raw(&retry)
        .await
        .expect("response")
        .result
        .expect("success");
    assert_eq!(result["resultType"], "complete");
    assert_eq!(result["content"][0]["text"], "accept at step-1");
}

/// A legacy result has no `resultType`, so a legacy client cannot be asked for
/// input this way; the engine refuses rather than send a shape it would
/// misread as a tool result.
#[tokio::test]
async fn a_legacy_call_is_never_answered_input_required() {
    let server = server();
    let raw = json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": { "name": "delete", "arguments": {} }
    })
    .to_string();
    let response = server.handle_raw(&raw).await.expect("response");
    assert_eq!(response.error.expect("refused").code, -32_603);
}
