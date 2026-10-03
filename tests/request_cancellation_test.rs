// ABOUTME: Tests for notifications/cancelled reaching an in-flight request
// ABOUTME: Pins that the tool's token fires, no response is sent, and only the sender can cancel, over HTTP too
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use dravr_tronc::mcp::auth::{AuthError, AuthHook};
use dravr_tronc::mcp::protocol::JsonRpcRequest;
use dravr_tronc::mcp::schema::{Tool, ToolResponse};
use dravr_tronc::mcp::server::McpServer;
use dravr_tronc::mcp::tasks::CancellationToken;
use dravr_tronc::mcp::tool::{McpTool, ToolContext, ToolRegistry};
use dravr_tronc::testkit::assert::assert_tool_success;
use dravr_tronc::testkit::McpTestClient;
use serde_json::{json, Value};
use tokio::sync::{mpsc, Notify};
use tokio::task::JoinHandle;
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

/// Resolves `user:<name>:<client>` to the user `<name>`, any other bearer to
/// no principal — what a shared API key resolves to — and no bearer to the
/// anonymous caller.
struct BearerHook;

#[async_trait]
impl AuthHook<TestState> for BearerHook {
    async fn authenticate(
        &self,
        request: &JsonRpcRequest,
        _state: &Arc<TestState>,
    ) -> Result<ToolContext, AuthError> {
        let user = request
            .auth_token
            .as_deref()
            .and_then(|token| token.strip_prefix("user:"))
            .and_then(|rest| rest.split(':').next());
        Ok(user.map_or_else(ToolContext::new, |user| ToolContext::new().with_user(user)))
    }
}

/// Two sessionless HTTP clients of one server: `first` starts a call — its
/// id 1, as the second client's first call would be too — and the test
/// learns its token.
struct TwoClients {
    first: McpTestClient,
    second: McpTestClient,
    token: CancellationToken,
    call: JoinHandle<()>,
    release: Arc<Notify>,
}

async fn two_clients(first: Option<&str>, second: Option<&str>) -> TwoClients {
    let (started, mut tokens) = mpsc::unbounded_channel();
    let release = Arc::new(Notify::new());
    let mut tools = ToolRegistry::new();
    tools.register(Box::new(WaitingTool {
        started,
        release: Arc::clone(&release),
    }));
    let server = Arc::new(
        McpServer::new("test-server", "0.1.0", tools, Arc::new(TestState))
            .with_auth_hook(Arc::new(BearerHook)),
    );
    let client = |bearer: Option<&str>| {
        let client = McpTestClient::in_process(Arc::clone(&server));
        match bearer {
            Some(bearer) => client.with_bearer(bearer),
            None => client,
        }
    };
    let (first, second) = (client(first), client(second));
    let call = {
        let first = first.clone();
        tokio::spawn(async move {
            // A cancelled call is answered with nothing, which the client
            // reads as an error; an uncancelled one says it was released.
            if let Ok(result) = first.call_tool("wait", json!({})).await {
                assert_eq!(assert_tool_success(&result), "released");
            }
        })
    };
    let token = timeout(Duration::from_secs(1), tokens.recv())
        .await
        .expect("the tool starts")
        .expect("a token");
    TwoClients {
        first,
        second,
        token,
        call,
        release,
    }
}

impl TwoClients {
    /// `client` sends `notifications/cancelled` for id 1; returns whether the
    /// first client's call was cancelled by it.
    async fn cancel_from(&self, client: &McpTestClient) -> bool {
        let accepted = client
            .notify(
                "notifications/cancelled",
                Some(json!({ "requestId": 1, "reason": "gave up" })),
            )
            .await
            .expect("an answer");
        assert_eq!(accepted.status, 202);
        self.token.is_cancelled()
    }

    async fn finish(self) {
        self.release.notify_one();
        timeout(Duration::from_secs(1), self.call)
            .await
            .expect("the call ends")
            .expect("joined");
    }
}

/// Two anonymous sessionless clients are one identity presenting nothing:
/// neither can be told from the other, so neither notification is acted on —
/// not even the one from the client that sent the call.
#[tokio::test]
async fn anonymous_sessionless_callers_cannot_cancel_each_other() {
    let clients = two_clients(None, None).await;
    assert!(!clients.cancel_from(&clients.second).await);
    assert!(!clients.cancel_from(&clients.first).await);
    clients.finish().await;
}

/// Two clients sharing one API key resolve to no principal and present the
/// same credential: as indistinguishable as two anonymous ones.
#[tokio::test]
async fn callers_sharing_an_api_key_cannot_cancel_each_other() {
    let clients = two_clients(Some("shared-key"), Some("shared-key")).await;
    assert!(!clients.cancel_from(&clients.second).await);
    assert!(!clients.cancel_from(&clients.first).await);
    clients.finish().await;
}

/// Two clients of one user, each with its own token, are told apart by the
/// credential: the other client's notification leaves the call running, and
/// the caller's own cancels it.
#[tokio::test]
async fn one_user_s_two_clients_cancel_only_their_own_calls() {
    let clients = two_clients(Some("user:ada:laptop"), Some("user:ada:phone")).await;
    assert!(!clients.cancel_from(&clients.second).await);
    assert!(clients.cancel_from(&clients.first).await);
    clients.finish().await;
}
