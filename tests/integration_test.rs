// ABOUTME: Integration tests exercising the full MCP stack end-to-end
// ABOUTME: Tests protocol compliance, tool dispatch, HTTP transport, and auth middleware together
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use std::sync::Arc;

use async_trait::async_trait;
use dravr_tronc::error::ErrorResponse;
use dravr_tronc::mcp::protocol::{CallToolResult, ToolDefinition};
use dravr_tronc::mcp::server::McpServer;
use dravr_tronc::mcp::tool::{McpTool, ToolRegistry};
use dravr_tronc::mcp::transport::http::mcp_router;
use dravr_tronc::server::health::HealthResponse;
use http::Request;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tokio::sync::RwLock;
use tower::ServiceExt;

// ============================================================================
// Test fixtures
// ============================================================================

struct AppState {
    greeting: String,
}

struct GreetTool;

#[async_trait]
impl McpTool<AppState> for GreetTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "greet".to_owned(),
            description: "Greet a person".to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "Person to greet" }
                },
                "required": ["name"]
            }),
        }
    }

    async fn execute(&self, state: &Arc<RwLock<AppState>>, arguments: Value) -> CallToolResult {
        let name = arguments
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("stranger");
        let guard = state.read().await;
        CallToolResult::text(format!("{} {name}", guard.greeting))
    }
}

struct UppercaseTool;

#[async_trait]
impl McpTool<AppState> for UppercaseTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "uppercase".to_owned(),
            description: "Convert text to uppercase".to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "text": { "type": "string" }
                }
            }),
        }
    }

    async fn execute(&self, _state: &Arc<RwLock<AppState>>, arguments: Value) -> CallToolResult {
        let text = arguments.get("text").and_then(|v| v.as_str()).unwrap_or("");
        CallToolResult::text(text.to_uppercase())
    }
}

fn make_server() -> Arc<McpServer<AppState>> {
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(GreetTool));
    registry.register(Box::new(UppercaseTool));
    let state = Arc::new(RwLock::new(AppState {
        greeting: "Hello".to_owned(),
    }));
    Arc::new(McpServer::new("integration-test", "0.0.1", registry, state))
}

// ============================================================================
// MCP protocol compliance tests
// ============================================================================

#[tokio::test]
async fn full_mcp_handshake_sequence() {
    let server = make_server();

    // Step 1: Initialize
    let init_resp = server
        .handle_raw(
            r#"{
            "jsonrpc":"2.0",
            "id":1,
            "method":"initialize",
            "params":{
                "protocolVersion":"2024-11-05",
                "capabilities":{},
                "clientInfo":{"name":"test-client","version":"1.0"}
            }
        }"#,
        )
        .await
        .expect("response");
    let init_result = init_resp.result.expect("result");
    assert_eq!(init_result["protocolVersion"], "2024-11-05");
    assert_eq!(init_result["serverInfo"]["name"], "integration-test");
    assert!(init_result["capabilities"]["tools"].is_object());

    // Step 2: List tools
    let list_resp = server
        .handle_raw(r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#)
        .await
        .expect("response");
    let tools = list_resp.result.expect("result")["tools"]
        .as_array()
        .expect("array")
        .clone();
    assert_eq!(tools.len(), 2);
    let tool_names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
    assert!(tool_names.contains(&"greet"));
    assert!(tool_names.contains(&"uppercase"));

    // Step 3: Call a tool
    let call_resp = server
        .handle_raw(
            r#"{
            "jsonrpc":"2.0",
            "id":3,
            "method":"tools/call",
            "params":{"name":"greet","arguments":{"name":"Pierre"}}
        }"#,
        )
        .await
        .expect("response");
    let call_result = call_resp.result.expect("result");
    assert_eq!(call_result["content"][0]["text"], "Hello Pierre");

    // Step 4: Ping
    let ping_resp = server
        .handle_raw(r#"{"jsonrpc":"2.0","id":4,"method":"ping"}"#)
        .await
        .expect("response");
    assert!(ping_resp.result.is_some());
    assert!(ping_resp.error.is_none());
}

#[tokio::test]
async fn tool_reads_shared_state() {
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(GreetTool));
    let state = Arc::new(RwLock::new(AppState {
        greeting: "Bonjour".to_owned(),
    }));
    let server = Arc::new(McpServer::new("test", "0.1", registry, state));

    let resp = server
        .handle_raw(
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"greet","arguments":{"name":"Jean"}}}"#,
        )
        .await
        .expect("response");
    let result = resp.result.expect("result");
    assert_eq!(result["content"][0]["text"], "Bonjour Jean");
}

#[tokio::test]
async fn unknown_tool_returns_error_in_result() {
    let server = make_server();
    let resp = server
        .handle_raw(r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"bogus"}}"#)
        .await
        .expect("response");
    let result = resp.result.expect("result");
    assert_eq!(result["isError"], true);
}

#[tokio::test]
async fn notification_is_silently_ignored() {
    let server = make_server();
    let resp = server
        .handle_raw(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
        .await;
    assert!(resp.is_none());
}

#[tokio::test]
async fn multiple_sequential_requests_maintain_state() {
    let server = make_server();

    for i in 1..=5 {
        let raw = format!(
            r#"{{"jsonrpc":"2.0","id":{i},"method":"tools/call","params":{{"name":"uppercase","arguments":{{"text":"hello"}}}}}}"#
        );
        let resp = server.handle_raw(&raw).await.expect("response");
        let result = resp.result.expect("result");
        assert_eq!(result["content"][0]["text"], "HELLO");
        assert_eq!(resp.id, Some(Value::from(i)));
    }
}

// ============================================================================
// HTTP transport integration tests
// ============================================================================

#[tokio::test]
async fn http_full_handshake() {
    let server = make_server();
    let app = mcp_router(server);

    // Initialize
    let init_body = r#"{
        "jsonrpc":"2.0","id":1,"method":"initialize",
        "params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"http-test"}}
    }"#;
    let req = Request::builder()
        .method("POST")
        .uri("/mcp")
        .header("content-type", "application/json")
        .body(init_body.to_owned())
        .expect("request");
    let resp = app.clone().oneshot(req).await.expect("response");
    assert_eq!(resp.status(), 200);

    // Tools/list
    let list_body = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#;
    let req = Request::builder()
        .method("POST")
        .uri("/mcp")
        .header("content-type", "application/json")
        .body(list_body.to_owned())
        .expect("request");
    let resp = app.clone().oneshot(req).await.expect("response");
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    let json: Value = serde_json::from_slice(&bytes).expect("json");
    assert_eq!(json["result"]["tools"].as_array().expect("tools").len(), 2);

    // Tools/call
    let call_body = r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"greet","arguments":{"name":"HTTP"}}}"#;
    let req = Request::builder()
        .method("POST")
        .uri("/mcp")
        .header("content-type", "application/json")
        .body(call_body.to_owned())
        .expect("request");
    let resp = app.oneshot(req).await.expect("response");
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    let json: Value = serde_json::from_slice(&bytes).expect("json");
    assert_eq!(json["result"]["content"][0]["text"], "Hello HTTP");
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
