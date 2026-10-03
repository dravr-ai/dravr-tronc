// ABOUTME: The testkit against a real McpServer — in-process and on a port-0 socket
// ABOUTME: Pins what the client sends (bearer, headers, _meta, era), what it reads back, and each assertion
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::env;
use std::fs;
use std::process;
use std::sync::Arc;

use async_trait::async_trait;
use dravr_tronc::error::{INVALID_PARAMS, UNAUTHORIZED};
use dravr_tronc::mcp::auth::{AuthError, AuthHook};
use dravr_tronc::mcp::protocol::JsonRpcRequest;
use dravr_tronc::mcp::schema::{Tool, ToolResponse};
use dravr_tronc::testkit::assert::{
    assert_rpc_error, assert_rpc_success, assert_structured_content, assert_tool_error,
    assert_tool_success, assert_tools_snapshot,
};
use dravr_tronc::testkit::{McpTestClient, McpTestServer, TestClientError, TESTKIT_CLIENT_NAME};
use dravr_tronc::{McpServer, McpTool, ToolContext, ToolRegistry};
use serde_json::{json, Value};

/// The bearer token the test hook admits.
const KEY: &str = "s3cret";

/// Reports what reached the tool: caller, `_meta` and declared capabilities.
struct Whoami;

#[async_trait]
impl McpTool<()> for Whoami {
    fn definition(&self) -> Tool {
        Tool {
            name: "whoami".to_owned(),
            description: "Who the server thinks is calling".to_owned(),
            input_schema: json!({ "type": "object" }),
            output_schema: Some(json!({ "type": "object" })),
            annotations: None,
            execution: None,
        }
    }

    async fn execute(
        &self,
        _state: &Arc<()>,
        ctx: &ToolContext,
        _arguments: Value,
    ) -> ToolResponse {
        ToolResponse::structured(&json!({
            "user": ctx.user_id,
            "trace": ctx.meta.get("ai.dravr/trace"),
            "client": ctx.meta.client_info().map(|info| info.name),
            "modern": ctx.client_capabilities.is_some(),
        }))
        .expect("an object")
    }
}

/// Always fails.
struct Broken;

#[async_trait]
impl McpTool<()> for Broken {
    fn definition(&self) -> Tool {
        Tool {
            name: "broken".to_owned(),
            description: "Always fails".to_owned(),
            input_schema: json!({ "type": "object" }),
            output_schema: None,
            annotations: None,
            execution: None,
        }
    }

    async fn execute(
        &self,
        _state: &Arc<()>,
        _ctx: &ToolContext,
        _arguments: Value,
    ) -> ToolResponse {
        ToolResponse::error("the sensor is unplugged".to_owned())
    }
}

/// Admits the bearer [`KEY`] as user `athlete`, refuses anything else.
struct KeyHook;

#[async_trait]
impl AuthHook<()> for KeyHook {
    async fn authenticate(
        &self,
        request: &JsonRpcRequest,
        _state: &Arc<()>,
    ) -> Result<ToolContext, AuthError> {
        match request.auth_token.as_deref() {
            Some(KEY) => Ok(ToolContext::new().with_user("athlete")),
            _ => Err(AuthError::Unauthorized {
                www_authenticate: "Bearer".to_owned(),
            }),
        }
    }
}

fn server() -> Arc<McpServer<()>> {
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(Whoami));
    registry.register(Box::new(Broken));
    Arc::new(
        McpServer::new("testkit-test", "1.0.0", registry, Arc::new(()))
            .with_auth_hook(Arc::new(KeyHook)),
    )
}

fn client() -> McpTestClient {
    McpTestClient::in_process(server()).with_bearer(KEY)
}

#[tokio::test]
async fn initialize_lists_and_calls_in_process() {
    let client = client();
    let init = client.initialize().await.expect("initialize");
    assert_eq!(init["serverInfo"]["name"], "testkit-test");

    let mut names: Vec<String> = client
        .list_tools()
        .await
        .expect("tools/list")
        .into_iter()
        .map(|tool| tool.name)
        .collect();
    names.sort_unstable();
    assert_eq!(names, ["broken", "whoami"]);

    let result = client
        .call_tool("whoami", json!({}))
        .await
        .expect("tools/call");
    assert_structured_content(
        &result,
        &json!({ "user": "athlete", "trace": null, "client": null, "modern": false }),
    );
}

#[tokio::test]
async fn the_bearer_is_what_the_auth_hook_reads() {
    let refused = McpTestClient::in_process(server())
        .with_bearer("wrong")
        .raw(r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#)
        .await
        .expect("an answer");
    assert_eq!(refused.status, 401);
    assert_rpc_error(&refused.rpc().expect("a JSON-RPC body"), UNAUTHORIZED);
}

#[tokio::test]
async fn client_meta_reaches_the_tool_and_a_call_s_own_meta_wins() {
    let client = client().with_meta("ai.dravr/trace", json!("from-client"));
    let result = client
        .call_tool("whoami", json!({}))
        .await
        .expect("tools/call");
    assert_eq!(
        result.structured_content.as_ref().expect("structured")["trace"],
        "from-client"
    );

    let own = client
        .result(
            "tools/call",
            Some(json!({
                "name": "whoami", "arguments": {},
                "_meta": { "ai.dravr/trace": "from-call" }
            })),
        )
        .await
        .expect("tools/call");
    assert_eq!(own["structuredContent"]["trace"], "from-call");
}

#[tokio::test]
async fn a_modern_client_is_served_statelessly() {
    let client = client().modern();
    let listed = client.result("tools/list", None).await.expect("tools/list");
    assert_eq!(listed["resultType"], "complete");

    let result = client
        .call_tool("whoami", json!({}))
        .await
        .expect("tools/call");
    let seen = result.structured_content.expect("structured");
    assert_eq!(seen["modern"], true);
    assert_eq!(seen["client"], TESTKIT_CLIENT_NAME);
}

#[tokio::test]
async fn a_tool_error_is_a_result_and_an_unknown_tool_a_protocol_error() {
    let client = client();
    let failed = client
        .call_tool("broken", json!({}))
        .await
        .expect("a result");
    assert_tool_error(&failed, "unplugged");

    let unknown = client.call_tool("missing", json!({})).await;
    assert!(
        matches!(&unknown, Err(TestClientError::Rpc(e)) if e.code == INVALID_PARAMS),
        "{unknown:?}"
    );

    let response = client
        .request("tools/call", Some(json!({ "name": "missing" })))
        .await
        .expect("a JSON-RPC response");
    assert_eq!(
        assert_rpc_error(&response, INVALID_PARAMS),
        "Unknown tool: missing"
    );
}

#[tokio::test]
async fn raw_sends_what_the_typed_calls_never_would() {
    let answer = client().raw("not json").await.expect("an answer");
    assert_eq!(answer.status, 400);
    assert_rpc_error(&answer.rpc().expect("a JSON-RPC body"), -32700);

    let accepted = client()
        .notify("notifications/initialized", None)
        .await
        .expect("an answer");
    assert_eq!(accepted.status, 202);
}

#[tokio::test]
async fn an_event_stream_answer_is_read_as_its_event() {
    // The client accepts both renderings, as Streamable HTTP requires, so
    // the server answers this one as SSE.
    let answer = client()
        .raw(r#"{"jsonrpc":"2.0","id":7,"method":"ping"}"#)
        .await
        .expect("an answer");
    assert!(
        answer.headers["content-type"]
            .to_str()
            .unwrap()
            .starts_with("text/event-stream"),
        "{answer:?}"
    );
    let response = answer.rpc().expect("the event's JSON-RPC response");
    assert_eq!(response.id, Some(json!(7)));
    assert_rpc_success(&response);
}

#[tokio::test]
async fn a_port_zero_server_answers_over_a_real_socket() {
    let first = McpTestServer::start(server()).await.expect("bind");
    let second = McpTestServer::start(server()).await.expect("bind");
    assert_ne!(first.addr().port(), 0);
    assert_ne!(first.addr(), second.addr());

    let result = first
        .client()
        .with_bearer(KEY)
        .call_tool("whoami", json!({}))
        .await
        .expect("tools/call over HTTP");
    assert_tool_success(&result);

    let refused = second.client().call_tool("whoami", json!({})).await;
    assert!(
        matches!(&refused, Err(TestClientError::Rpc(e)) if e.code == UNAUTHORIZED),
        "{refused:?}"
    );
}

#[tokio::test]
async fn the_tools_snapshot_pins_the_catalog() {
    let tools = client().list_tools().await.expect("tools/list");
    assert_tools_snapshot(
        &tools,
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/snapshots/testkit_tools.json"
        ),
    );
}

#[tokio::test]
#[should_panic(expected = "tools/list differs from")]
async fn a_drifted_catalog_fails_the_snapshot() {
    let path = env::temp_dir().join(format!("tronc-snapshot-{}.json", process::id()));
    fs::write(&path, "[]\n").unwrap();
    let tools = client().list_tools().await.expect("tools/list");
    assert_tools_snapshot(&tools, &path);
}

#[tokio::test]
#[should_panic(expected = "no tools snapshot at")]
async fn a_missing_snapshot_fails_rather_than_passing() {
    let tools = client().list_tools().await.expect("tools/list");
    assert_tools_snapshot(
        &tools,
        env::temp_dir().join("tronc-snapshot-never-written.json"),
    );
}
