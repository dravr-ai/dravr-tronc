// ABOUTME: Integration tests exercising the full MCP stack end-to-end
// ABOUTME: Tests protocol compliance, tool dispatch, HTTP transport, and auth middleware together
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::str_to_string
)]

use std::sync::Arc;

use async_trait::async_trait;
use dravr_tronc::error::ErrorResponse;
use dravr_tronc::mcp::schema::{Tool, ToolResponse};
use dravr_tronc::mcp::server::McpServer;
use dravr_tronc::mcp::tool::{McpTool, ToolContext, ToolRegistry};
use dravr_tronc::server::health::HealthResponse;
use dravr_tronc::testkit::assert::{assert_rpc_error, assert_rpc_success, assert_tool_success};
use dravr_tronc::testkit::{McpTestClient, McpTestServer};
use serde_json::{json, Value};

// ============================================================================
// Test fixtures
// ============================================================================

struct AppState {
    greeting: String,
}

struct GreetTool;

#[async_trait]
impl McpTool<AppState> for GreetTool {
    fn definition(&self) -> Tool {
        Tool {
            name: "greet".to_owned(),
            description: "Greet a person".to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "Person to greet" }
                },
                "required": ["name"]
            }),
            output_schema: None,
            annotations: None,
            execution: None,
        }
    }

    async fn execute(
        &self,
        state: &Arc<AppState>,
        _ctx: &ToolContext,
        arguments: Value,
    ) -> ToolResponse {
        let name = arguments
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("stranger");
        ToolResponse::text(format!("{} {name}", state.greeting))
    }
}

struct UppercaseTool;

#[async_trait]
impl McpTool<AppState> for UppercaseTool {
    fn definition(&self) -> Tool {
        Tool {
            name: "uppercase".to_owned(),
            description: "Convert text to uppercase".to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "text": { "type": "string" }
                }
            }),
            output_schema: None,
            annotations: None,
            execution: None,
        }
    }

    async fn execute(
        &self,
        _state: &Arc<AppState>,
        _ctx: &ToolContext,
        arguments: Value,
    ) -> ToolResponse {
        let text = arguments.get("text").and_then(|v| v.as_str()).unwrap_or("");
        ToolResponse::text(text.to_uppercase())
    }
}

fn make_server() -> Arc<McpServer<AppState>> {
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(GreetTool));
    registry.register(Box::new(UppercaseTool));
    let state = Arc::new(AppState {
        greeting: "Hello".to_owned(),
    });
    Arc::new(McpServer::new("integration-test", "0.0.1", registry, state))
}

// ============================================================================
// MCP protocol compliance tests
// ============================================================================

#[tokio::test]
async fn full_mcp_handshake_sequence() {
    let client = McpTestClient::in_process(make_server());

    // Step 1: Initialize, with a revision the server answers with its own
    let init = client
        .result(
            "initialize",
            Some(json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": { "name": "test-client", "version": "1.0" }
            })),
        )
        .await
        .expect("initialize");
    assert_eq!(init["protocolVersion"], "2025-11-25");
    assert_eq!(init["serverInfo"]["name"], "integration-test");
    assert!(init["capabilities"]["tools"].is_object());

    // Step 2: List tools
    let tools = client.list_tools().await.expect("tools/list");
    assert_eq!(tools.len(), 2);
    let tool_names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
    assert!(tool_names.contains(&"greet"));
    assert!(tool_names.contains(&"uppercase"));

    // Step 3: Call a tool
    let greeted = client
        .call_tool("greet", json!({ "name": "Pierre" }))
        .await
        .expect("tools/call");
    assert_eq!(assert_tool_success(&greeted), "Hello Pierre");

    // Step 4: Ping
    let ping = client.request("ping", None).await.expect("ping");
    assert_rpc_success(&ping);
}

#[tokio::test]
async fn tool_reads_shared_state() {
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(GreetTool));
    let state = Arc::new(AppState {
        greeting: "Bonjour".to_owned(),
    });
    let client =
        McpTestClient::in_process(Arc::new(McpServer::new("test", "0.1", registry, state)));

    let greeted = client
        .call_tool("greet", json!({ "name": "Jean" }))
        .await
        .expect("tools/call");
    assert_eq!(assert_tool_success(&greeted), "Bonjour Jean");
}

#[tokio::test]
async fn unknown_tool_is_a_protocol_error() {
    let client = McpTestClient::in_process(make_server());
    let response = client
        .request("tools/call", Some(json!({ "name": "bogus" })))
        .await
        .expect("a JSON-RPC response");
    assert!(response.result.is_none(), "not an isError tool result");
    assert_eq!(assert_rpc_error(&response, -32602), "Unknown tool: bogus");
}

#[tokio::test]
async fn notification_is_silently_ignored() {
    let accepted = McpTestClient::in_process(make_server())
        .notify("notifications/initialized", None)
        .await
        .expect("an answer");
    assert_eq!(accepted.status, 202);
    assert!(accepted.body.is_empty());
}

#[tokio::test]
async fn multiple_sequential_requests_maintain_state() {
    let client = McpTestClient::in_process(make_server());

    for _ in 1..=5 {
        let shouted = client
            .call_tool("uppercase", json!({ "text": "hello" }))
            .await
            .expect("tools/call");
        assert_eq!(assert_tool_success(&shouted), "HELLO");
    }
}

#[tokio::test]
async fn response_id_matches_request_id() {
    let client = McpTestClient::in_process(make_server());
    for i in 1..=3 {
        let answer = client
            .raw(format!(r#"{{"jsonrpc":"2.0","id":{i},"method":"ping"}}"#))
            .await
            .expect("an answer");
        assert_eq!(answer.rpc().expect("JSON-RPC").id, Some(Value::from(i)));
    }
}

// ============================================================================
// HTTP transport integration tests
// ============================================================================

#[tokio::test]
async fn http_full_handshake() {
    let server = McpTestServer::start(make_server()).await.expect("bind");
    let client = server.client();

    client.initialize().await.expect("initialize");
    assert_eq!(client.list_tools().await.expect("tools/list").len(), 2);
    let greeted = client
        .call_tool("greet", json!({ "name": "HTTP" }))
        .await
        .expect("tools/call");
    assert_eq!(assert_tool_success(&greeted), "Hello HTTP");
}

// ============================================================================
// Health response tests
// ============================================================================

#[test]
fn health_response_builder_chain() {
    let resp = HealthResponse::ok("my-service", "2.0.0")
        .with_detail("strava", "connected")
        .with_detail("garmin", "disconnected");

    assert_eq!(resp.status, "ok");
    assert_eq!(resp.service, "my-service");
    assert_eq!(resp.version, "2.0.0");
    assert_eq!(resp.details.len(), 2);
    assert_eq!(resp.details["strava"], "connected");
}

// ============================================================================
// Error response tests
// ============================================================================

#[test]
fn error_response_structure() {
    let resp = ErrorResponse::new("quota_exceeded", "daily limit reached");
    let json = serde_json::to_value(&resp).expect("serialize");
    assert_eq!(json["error"]["type"], "quota_exceeded");
    assert_eq!(json["error"]["message"], "daily limit reached");
}
