// ABOUTME: Tests a running tool's channel to its client over Streamable HTTP, through the testkit
// ABOUTME: Progress, logs, sampling and elicitation as streamed events; sessions mint, scope and end
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::str_to_string
)]

use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use dravr_tronc::error::{INTERNAL_ERROR, INVALID_PARAMS, INVALID_REQUEST, METHOD_NOT_FOUND};
use dravr_tronc::mcp::auth::{AuthError, AuthHook};
use dravr_tronc::mcp::client_channel::ClientRequestError;
use dravr_tronc::mcp::elicitation::{
    ElicitRequest, ElicitationSchema, PrimitiveSchema, StringSchema,
};
use dravr_tronc::mcp::logging::LogLevel;
use dravr_tronc::mcp::modern::meta_keys;
use dravr_tronc::mcp::observe::{Observer, OperationInfo, OperationOutcome};
use dravr_tronc::mcp::protocol::JsonRpcRequest;
use dravr_tronc::mcp::schema::{
    CreateMessageRequest, LoggingCapability, ServerCapabilities, Tool, ToolResponse,
};
use dravr_tronc::mcp::server::{McpServer, DEFAULT_SESSION_TTL};
use dravr_tronc::mcp::tool::{McpTool, ToolContext, ToolRegistry};
use dravr_tronc::mcp::transport::http::MCP_SESSION_ID_HEADER;
use dravr_tronc::protocol::JsonRpcError;
use dravr_tronc::testkit::assert::{assert_rpc_error, assert_tool_error, assert_tool_success};
use dravr_tronc::testkit::{McpTestClient, McpTestServer};
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

fn sessionful() -> McpServer<State> {
    McpServer::new("channel-test", "0.1.0", registry(), Arc::new(State))
        .with_capabilities(capabilities())
        .with_http_sessions(DEFAULT_SESSION_TTL)
}

fn sessionless() -> McpServer<State> {
    McpServer::new("channel-test", "0.1.0", registry(), Arc::new(State))
        .with_capabilities(capabilities())
}

fn call_params(name: &str, meta: &Value) -> Value {
    json!({ "name": name, "arguments": {}, "_meta": meta })
}

/// Answers sampling with a fixed message and elicitation with a fixed name.
fn answering_client(server: McpServer<State>) -> McpTestClient {
    McpTestClient::in_process(Arc::new(server))
        .with_capabilities(json!({ "sampling": {}, "elicitation": {} }))
        .on_server_request(|method, params| match method {
            "sampling/createMessage" => {
                assert_eq!(params["maxTokens"], 100);
                Ok(
                    json!({ "role": "assistant", "content": { "type": "text", "text": "hi" },
                           "model": "test", "stopReason": "endTurn" }),
                )
            }
            "elicitation/create" => {
                assert_eq!(params["requestedSchema"]["required"], json!(["name"]));
                Ok(json!({ "action": "accept", "content": { "name": "Ada" } }))
            }
            other => Err(JsonRpcError::new(METHOD_NOT_FOUND, other.to_owned())),
        })
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
async fn a_session_carries_the_log_level_to_later_calls() {
    let client = answering_client(sessionful());
    client.initialize().await.unwrap();
    assert!(client.session_id().is_some(), "initialize minted a session");

    let unasked = client
        .exchange("tools/call", Some(call_params("logs", &json!({}))))
        .await
        .unwrap();
    assert!(unasked.messages.is_empty(), "no level asked, no logs");

    client
        .result("logging/setLevel", Some(json!({ "level": "info" })))
        .await
        .unwrap();
    let logged = client
        .exchange("tools/call", Some(call_params("logs", &json!({}))))
        .await
        .unwrap();
    let messages = logged.notifications("notifications/message");
    let levels: Vec<&str> = messages
        .iter()
        .map(|m| m["params"]["level"].as_str().unwrap())
        .collect();
    assert_eq!(levels, vec!["info", "error"], "debug is below the level");
    assert_eq!(messages[0]["params"]["logger"], "fixture");
}

#[tokio::test]
async fn a_bad_level_is_invalid_params_and_a_sessionless_one_is_refused() {
    let client = answering_client(sessionful());
    client.initialize().await.unwrap();
    let bad = client
        .request("logging/setLevel", Some(json!({ "level": "loud" })))
        .await
        .unwrap();
    assert_rpc_error(&bad, INVALID_PARAMS);

    let sessionless = McpTestClient::in_process(Arc::new(sessionless()));
    sessionless.initialize().await.unwrap();
    assert_eq!(sessionless.session_id(), None, "sessions are opt-in");
    let refused = sessionless
        .request("logging/setLevel", Some(json!({ "level": "info" })))
        .await
        .unwrap();
    assert_rpc_error(&refused, INVALID_REQUEST);
}

#[tokio::test]
async fn set_level_is_unknown_on_a_server_without_logging() {
    let server = McpServer::new("no-logging", "0.1.0", registry(), Arc::new(State))
        .with_http_sessions(DEFAULT_SESSION_TTL);
    let client = McpTestClient::in_process(Arc::new(server));
    client.initialize().await.unwrap();
    let unknown = client
        .request("logging/setLevel", Some(json!({ "level": "info" })))
        .await
        .unwrap();
    assert_rpc_error(&unknown, METHOD_NOT_FOUND);
}

#[tokio::test]
async fn a_tool_samples_and_elicits_from_a_client_that_declared_both() {
    let client = answering_client(sessionful());
    client.initialize().await.unwrap();

    let sampled = client
        .exchange("tools/call", Some(call_params("sample", &json!({}))))
        .await
        .unwrap();
    assert_eq!(sampled.server_requests().len(), 1);
    assert_eq!(
        sampled.server_requests()[0]["method"],
        "sampling/createMessage"
    );
    let result: ToolResponse = serde_json::from_value(sampled.response.result.unwrap()).unwrap();
    assert_eq!(assert_tool_success(&result), "LLM: hi");

    let asked = client.call_tool("ask", json!({})).await.unwrap();
    assert_eq!(assert_tool_success(&asked), r#"Accept {"name":"Ada"}"#);
}

#[tokio::test]
async fn a_request_needs_the_capability_declared_on_the_session() {
    let undeclared = McpTestClient::in_process(Arc::new(sessionful()));
    undeclared.initialize().await.unwrap();
    let refused = undeclared.call_tool("sample", json!({})).await.unwrap();
    assert_tool_error(&refused, "sampling");

    // Declared at initialize, but on a server that keeps no session to
    // remember it.
    let forgotten = answering_client(sessionless());
    forgotten.initialize().await.unwrap();
    let refused = forgotten.call_tool("ask", json!({})).await.unwrap();
    assert_tool_error(&refused, "elicitation");
}

#[tokio::test]
async fn a_client_error_reaches_the_tool() {
    let client = McpTestClient::in_process(Arc::new(sessionful()))
        .with_capabilities(json!({ "elicitation": {} }))
        .on_server_request(|_, _| Err(JsonRpcError::new(-1, "no form today")));
    client.initialize().await.unwrap();
    let answer = client.call_tool("ask", json!({})).await.unwrap();
    assert_tool_error(&answer, "client error -1");
}

#[tokio::test]
async fn a_modern_call_logs_at_its_meta_level_and_never_sends_a_request() {
    let client = McpTestClient::in_process(Arc::new(sessionful())).modern();
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
    assert_eq!(client.session_id(), None, "the modern era has no sessions");
}

#[tokio::test]
async fn a_session_belongs_to_its_caller_and_ends_on_delete() {
    let server = McpTestServer::start(Arc::new(
        sessionful().with_auth_hook(Arc::new(BearerIsUser)),
    ))
    .await
    .unwrap();
    let alice = server.client().with_bearer("alice");
    alice.initialize().await.unwrap();
    let session = alice.session_id().unwrap();

    // Bob presents Alice's session id: the server will not confirm it exists.
    let bob = McpTestClient::http(server.url())
        .with_bearer("bob")
        .with_header(MCP_SESSION_ID_HEADER, session.clone());
    let refused = bob
        .raw(r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#)
        .await
        .unwrap();
    assert_eq!(refused.status, 404);

    let unknown = McpTestClient::http(server.url())
        .with_bearer("alice")
        .with_header(MCP_SESSION_ID_HEADER, "not-a-session");
    let gone = unknown
        .raw(r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#)
        .await
        .unwrap();
    assert_eq!(gone.status, 404);
    assert_eq!(gone.json().unwrap()["id"], 1);

    let ended = alice.end_session().await.unwrap();
    assert_eq!(ended.status, 204);
    let after = McpTestClient::http(server.url())
        .with_bearer("alice")
        .with_header(MCP_SESSION_ID_HEADER, session);
    let gone = after
        .raw(r#"{"jsonrpc":"2.0","id":2,"method":"ping"}"#)
        .await
        .unwrap();
    assert_eq!(gone.status, 404, "an ended session is gone");

    // Requests without a session are still served.
    let anonymous = server.client().with_bearer("alice");
    assert!(anonymous.result("ping", None).await.is_ok());
}

#[tokio::test]
async fn a_server_without_sessions_mints_none_and_refuses_delete() {
    let server = McpTestServer::start(Arc::new(sessionless())).await.unwrap();
    let client = server.client();
    client.initialize().await.unwrap();
    assert_eq!(client.session_id(), None);
    let delete = client
        .with_header(MCP_SESSION_ID_HEADER, "whatever")
        .end_session()
        .await
        .unwrap();
    assert_eq!(delete.status, 405);
}

#[tokio::test]
async fn a_response_post_carries_no_mirror_and_is_accepted() {
    let client = McpTestClient::in_process(Arc::new(sessionless()));
    let accepted = client
        .raw(r#"{"jsonrpc":"2.0","id":"nobody-waits","result":{}}"#)
        .await
        .unwrap();
    assert_eq!(accepted.status, 202, "{}", accepted.body);
    assert!(accepted.body.is_empty());

    let mirrored = client
        .with_header("mcp-method", "tools/call")
        .raw(r#"{"jsonrpc":"2.0","id":"x","result":{}}"#)
        .await
        .unwrap();
    assert_eq!(mirrored.status, 400);
    assert_eq!(mirrored.json().unwrap()["error"]["code"], -32_020);
}

#[tokio::test]
async fn an_earlier_legacy_revision_header_is_served() {
    for version in ["2025-03-26", "2025-06-18"] {
        let client = McpTestClient::in_process(Arc::new(sessionless()))
            .with_header("mcp-protocol-version", version);
        let answer = client
            .raw(r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#)
            .await
            .unwrap();
        assert_eq!(answer.status, 200, "{version}: {}", answer.body);
    }
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
    let server = sessionful().with_observer(observer.clone());
    let client = answering_client(server);
    client.initialize().await.unwrap();
    let before = observer.0.load(Ordering::SeqCst);
    let streamed = client
        .exchange(
            "tools/call",
            Some(call_params("sample", &json!({ "progressToken": "o" }))),
        )
        .await
        .unwrap();
    assert_eq!(streamed.server_requests().len(), 1);
    assert_eq!(
        observer.0.load(Ordering::SeqCst) - before,
        1,
        "the call completes once; the client's answering POST is no operation"
    );
}

/// Authenticates `Bearer <name>` as the user `<name>`.
struct BearerIsUser;

#[async_trait]
impl AuthHook<State> for BearerIsUser {
    async fn authenticate(
        &self,
        request: &JsonRpcRequest,
        _state: &Arc<State>,
    ) -> Result<ToolContext, AuthError> {
        request.auth_token.as_deref().map_or_else(
            || {
                Err(AuthError::Unauthorized {
                    www_authenticate: "Bearer".to_owned(),
                })
            },
            |user| Ok(ToolContext::new().with_user(user)),
        )
    }
}

#[tokio::test]
async fn an_answer_counts_only_from_the_caller_the_request_went_to() {
    let server = McpTestServer::start(Arc::new(
        sessionful().with_auth_hook(Arc::new(BearerIsUser)),
    ))
    .await
    .unwrap();
    let alice = server
        .client()
        .with_bearer("alice")
        .with_capabilities(json!({ "elicitation": {} }));
    alice.initialize().await.unwrap();
    let session = alice.session_id().unwrap();

    let http = reqwest::Client::new();
    let post = |bearer: &str, session: Option<&str>, body: &Value| {
        let mut request = http
            .post(server.url())
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream")
            .header("authorization", format!("Bearer {bearer}"))
            .body(body.to_string());
        if let Some(session) = session {
            request = request.header(MCP_SESSION_ID_HEADER, session);
        }
        request.send()
    };

    let call = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                       "params": { "name": "ask", "arguments": {} } });
    let mut stream = post("alice", Some(&session), &call).await.unwrap();
    let mut text = String::new();
    while !text.contains("\n\n") {
        text.push_str(&String::from_utf8_lossy(
            &stream.chunk().await.unwrap().unwrap(),
        ));
    }
    let ask: Value =
        serde_json::from_str(text.lines().find_map(|l| l.strip_prefix("data: ")).unwrap()).unwrap();
    assert_eq!(ask["method"], "elicitation/create");
    let answer =
        |action: &str| json!({ "jsonrpc": "2.0", "id": ask["id"], "result": { "action": action } });

    // Bob cannot answer in Alice's session, and outside it reaches nothing.
    let in_her_session = post("bob", Some(&session), &answer("accept"))
        .await
        .unwrap();
    assert_eq!(in_her_session.status(), 404);
    let outside = post("bob", None, &answer("accept")).await.unwrap();
    assert_eq!(outside.status(), 202, "accepted, and routed to nothing");

    let hers = post("alice", Some(&session), &answer("decline"))
        .await
        .unwrap();
    assert_eq!(hers.status(), 202);
    let rest = stream.text().await.unwrap();
    assert!(
        rest.contains("Decline"),
        "Alice's answer is the one used: {rest}"
    );
}

/// POST `body` to `/mcp` in `session`, as an anonymous client.
async fn post_in(server: &McpTestServer, session: &str, body: &Value) -> reqwest::Response {
    reqwest::Client::new()
        .post(server.url())
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .header(MCP_SESSION_ID_HEADER, session)
        .body(body.to_string())
        .send()
        .await
        .unwrap()
}

/// Read a call's event stream up to its first event, the elicitation it sent.
async fn first_event(stream: &mut reqwest::Response) -> Value {
    let mut text = String::new();
    while !text.contains("\n\n") {
        text.push_str(&String::from_utf8_lossy(
            &stream.chunk().await.unwrap().unwrap(),
        ));
    }
    serde_json::from_str(text.lines().find_map(|l| l.strip_prefix("data: ")).unwrap()).unwrap()
}

#[tokio::test]
async fn a_cancellation_reaches_only_the_requests_of_its_own_session() {
    let server = McpTestServer::start(Arc::new(sessionful())).await.unwrap();
    let ada = server
        .client()
        .with_capabilities(json!({ "elicitation": {} }));
    ada.initialize().await.unwrap();
    let hers = ada.session_id().unwrap();
    let bob = server.client();
    bob.initialize().await.unwrap();
    let his = bob.session_id().unwrap();

    // Both anonymous, both counting from 1: Bob's cancellation names the id
    // of Ada's call, but in his own session.
    let call = |id: u64| {
        json!({ "jsonrpc": "2.0", "id": id, "method": "tools/call",
                "params": { "name": "ask", "arguments": {} } })
    };
    let cancel = |id: u64| {
        json!({ "jsonrpc": "2.0", "method": "notifications/cancelled",
                "params": { "requestId": id } })
    };
    let mut stream = post_in(&server, &hers, &call(1)).await;
    let ask = first_event(&mut stream).await;
    assert_eq!(ask["method"], "elicitation/create");
    let ignored = post_in(&server, &his, &cancel(1)).await;
    assert_eq!(ignored.status(), 202);
    let answer = json!({ "jsonrpc": "2.0", "id": ask["id"], "result": { "action": "decline" } });
    assert_eq!(post_in(&server, &hers, &answer).await.status(), 202);
    let rest = stream.text().await.unwrap();
    assert!(
        rest.contains("Decline"),
        "Ada's call ran to its end: {rest}"
    );

    // The same notification in her own session does cancel her call.
    let mut stream = post_in(&server, &hers, &call(2)).await;
    first_event(&mut stream).await;
    assert_eq!(post_in(&server, &hers, &cancel(2)).await.status(), 202);
    let rest = stream.text().await.unwrap();
    assert!(
        !rest.contains("\"id\":2"),
        "a cancelled call is answered with nothing: {rest}"
    );
}

#[tokio::test]
async fn the_session_store_is_bounded_per_caller_and_in_total() {
    let one = NonZeroUsize::new(1).unwrap();
    let server = McpTestServer::start(Arc::new(
        sessionful()
            .with_http_session_limits(NonZeroUsize::new(2).unwrap(), one)
            .with_auth_hook(Arc::new(BearerIsUser)),
    ))
    .await
    .unwrap();

    // A caller at its limit gives up its idle session for the new one.
    let first = server.client().with_bearer("alice");
    first.initialize().await.unwrap();
    let again = server.client().with_bearer("alice");
    again.initialize().await.unwrap();
    assert!(again.result("ping", None).await.is_ok());
    let evicted = first
        .raw(r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#)
        .await
        .unwrap();
    assert_eq!(evicted.status, 404, "told to initialize again");

    // A full server ends no other caller's session to make room.
    server
        .client()
        .with_bearer("bob")
        .initialize()
        .await
        .unwrap();
    let refused = server
        .client()
        .with_bearer("carol")
        .raw(
            r#"{"jsonrpc":"2.0","id":7,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"c","version":"1"}}}"#,
        )
        .await
        .unwrap();
    assert_eq!(refused.status, 503, "{}", refused.body);
    assert_eq!(refused.json().unwrap()["id"], 7);
    assert!(again.result("ping", None).await.is_ok());
}

/// A client accepting only JSON gets one JSON response for a call that talks
/// to its client first: the call's notifications are dropped, and its request
/// fails with the typed error naming why.
#[tokio::test]
async fn a_client_accepting_only_json_gets_one_json_response() {
    let client = answering_client(sessionful()).with_header("accept", "application/json");
    client.initialize().await.unwrap();
    client
        .result("logging/setLevel", Some(json!({ "level": "debug" })))
        .await
        .unwrap();

    for name in ["progress", "logs"] {
        let answered = client
            .exchange(
                "tools/call",
                Some(call_params(name, &json!({ "progressToken": "p-1" }))),
            )
            .await
            .unwrap();
        assert!(
            answered.messages.is_empty(),
            "{name}: {:?}",
            answered.messages
        );
        assert!(answered.response.result.is_some(), "{name}");
    }

    let asked = client.call_tool("ask", json!({})).await.unwrap();
    assert_tool_error(&asked, &ClientRequestError::NoEventStream.to_string());
}

/// No `Accept` is `*/*`: a single response is JSON, and a call that talks to
/// its client first is still answered as the event stream it needs.
#[tokio::test]
async fn a_client_sending_no_accept_is_served() {
    let server = McpTestServer::start(Arc::new(sessionless())).await.unwrap();
    let post = |body: Value| {
        reqwest::Client::new()
            .post(server.url())
            .header("content-type", "application/json")
            .body(body.to_string())
            .send()
    };

    let ping = post(json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" }))
        .await
        .unwrap();
    assert_eq!(ping.status(), 200);
    assert!(ping.headers()["content-type"]
        .to_str()
        .unwrap()
        .starts_with("application/json"));

    let streamed = post(json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/call",
                                "params": call_params("progress", &json!({ "progressToken": "p" })) }))
    .await
    .unwrap();
    assert_eq!(streamed.status(), 200);
    assert!(streamed.headers()["content-type"]
        .to_str()
        .unwrap()
        .starts_with("text/event-stream"));
    let events = streamed.text().await.unwrap();
    assert_eq!(
        events.matches("notifications/progress").count(),
        3,
        "{events}"
    );
    assert!(events.contains("\"id\":2"), "{events}");
}
