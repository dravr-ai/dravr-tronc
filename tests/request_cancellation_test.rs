// ABOUTME: Tests for notifications/cancelled reaching an in-flight request
// ABOUTME: Pins that the tool's token fires, no response is sent, and only the sender can cancel
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use dravr_tronc::mcp::protocol::JsonRpcRequest;
use dravr_tronc::mcp::schema::{Tool, ToolResponse};
use dravr_tronc::mcp::server::McpServer;
use dravr_tronc::mcp::tasks::CancellationToken;
use dravr_tronc::mcp::tool::{McpTool, ToolContext, ToolRegistry};
use serde_json::{json, Value};
use tokio::sync::{mpsc, Notify};
use tokio::time::timeout;

struct TestState;

/// A tool that reports its cancellation token as it starts, then waits for the
/// test to release it.
struct WaitingTool {
    started: mpsc::UnboundedSender<CancellationToken>,
    release: Arc<Notify>,
}

#[async_trait]
impl McpTool<TestState> for WaitingTool {
    fn definition(&self) -> Tool {
        Tool {
            name: "wait".to_owned(),
            description: "Waits until released".to_owned(),
            input_schema: json!({"type": "object"}),
            output_schema: None,
            annotations: None,
            execution: None,
        }
    }

    async fn execute(
        &self,
        _state: &Arc<TestState>,
        ctx: &ToolContext,
        _arguments: Value,
    ) -> ToolResponse {
        self.started
            .send(ctx.cancellation.clone())
            .expect("the test listens");
        self.release.notified().await;
        ToolResponse::text("released".to_owned())
    }
}

fn server() -> (
    Arc<McpServer<TestState>>,
    mpsc::UnboundedReceiver<CancellationToken>,
    Arc<Notify>,
) {
    let (started, tokens) = mpsc::unbounded_channel();
    let release = Arc::new(Notify::new());
    let mut tools = ToolRegistry::new();
    tools.register(Box::new(WaitingTool {
        started,
        release: Arc::clone(&release),
    }));
    let server = McpServer::new("test-server", "0.1.0", tools, Arc::new(TestState));
    (Arc::new(server), tokens, release)
}

fn request(raw: &Value) -> JsonRpcRequest {
    serde_json::from_value(raw.clone()).expect("a request")
}

fn call(id: &Value) -> JsonRpcRequest {
    request(&json!({
        "jsonrpc": "2.0", "id": id, "method": "tools/call",
        "params": { "name": "wait", "arguments": {} }
    }))
}

fn cancelled(id: &Value) -> JsonRpcRequest {
    request(&json!({
        "jsonrpc": "2.0", "method": "notifications/cancelled",
        "params": { "requestId": id, "reason": "user gave up" }
    }))
}

/// The notification fires the running tool's token and the cancelled request
/// is answered with nothing, since the client will not read it.
#[tokio::test]
async fn notifications_cancelled_stops_an_in_flight_tool_call() {
    let (server, mut tokens, _release) = server();
    let ctx = ToolContext::new().with_user("alice");

    let in_flight = {
        let server = Arc::clone(&server);
        let ctx = ctx.clone();
        tokio::spawn(async move {
            server
                .handle_request_with_context(call(&json!(5)), &ctx)
                .await
        })
    };
    let token = timeout(Duration::from_secs(1), tokens.recv())
        .await
        .expect("the tool starts")
        .expect("a token");
    assert!(!token.is_cancelled());

    let ack = server
        .handle_request_with_context(cancelled(&json!(5)), &ctx)
        .await;
    assert!(ack.is_none(), "a notification is never answered");

    timeout(Duration::from_secs(1), token.cancelled())
        .await
        .expect("the tool's token fires");
    let response = timeout(Duration::from_secs(1), in_flight)
        .await
        .expect("the call stops waiting on the tool")
        .expect("joined");
    assert!(
        response.is_none(),
        "a cancelled request gets no response: {response:?}"
    );
}

/// JSON-RPC ids are unique per client only: another caller naming the same id,
/// or the same caller naming `"5"` for `5`, cancels nothing.
#[tokio::test]
async fn only_the_sender_can_cancel_its_request() {
    let (server, mut tokens, release) = server();
    let alice = ToolContext::new().with_user("alice");
    let mallory = ToolContext::new().with_user("mallory");

    let in_flight = {
        let server = Arc::clone(&server);
        let alice = alice.clone();
        tokio::spawn(async move {
            server
                .handle_request_with_context(call(&json!(5)), &alice)
                .await
        })
    };
    let token = timeout(Duration::from_secs(1), tokens.recv())
        .await
        .expect("the tool starts")
        .expect("a token");

    server
        .handle_request_with_context(cancelled(&json!(5)), &mallory)
        .await;
    server
        .handle_request_with_context(cancelled(&json!("5")), &alice)
        .await;
    assert!(!token.is_cancelled(), "neither notification names the call");

    release.notify_one();
    let response = timeout(Duration::from_secs(1), in_flight)
        .await
        .expect("the call completes")
        .expect("joined")
        .expect("an uncancelled call is answered");
    assert_eq!(
        response.result.expect("success")["content"][0]["text"],
        "released"
    );
}

/// A cancellation for a request that already finished — the notification can
/// always cross the response — is harmless.
#[tokio::test]
async fn cancelling_a_finished_request_is_a_no_op() {
    let (server, _tokens, _release) = server();
    let ping = request(&json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" }));
    assert!(server.handle_request(ping).await.is_some());
    assert!(server.handle_request(cancelled(&json!(1))).await.is_none());
}
