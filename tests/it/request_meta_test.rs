// ABOUTME: A tool reads its request's _meta off ToolContext, typed, in both protocol eras
// ABOUTME: Pins the progress token's two wire forms, the modern keys and an extension key's absent/malformed split
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use async_trait::async_trait;
use dravr_tronc::mcp::schema::{ProgressNotification, ProgressToken, Tool, ToolResponse};
use dravr_tronc::testkit::McpTestClient;
use dravr_tronc::{McpServer, McpTool, RequestMeta, ToolContext, ToolRegistry};
use serde::Deserialize;
use serde_json::{json, Value};

/// A vendor `_meta` extension the test tool reads.
#[derive(Debug, Deserialize, PartialEq, Eq)]
struct Trace {
    span: String,
}

const TRACE_KEY: &str = "ai.dravr/trace";

/// Reports what it read from `ctx.meta` as structured content.
struct ReadMeta;

#[async_trait]
impl McpTool<()> for ReadMeta {
    fn definition(&self) -> Tool {
        Tool {
            name: "read_meta".to_owned(),
            description: "Report the request's _meta".to_owned(),
            input_schema: json!({ "type": "object" }),
            output_schema: None,
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
        let trace = match ctx.meta.get_as::<Trace>(TRACE_KEY) {
            None => json!("absent"),
            Some(Ok(trace)) => json!(trace.span),
            Some(Err(_)) => json!("malformed"),
        };
        ToolResponse::structured(&json!({
            "progressToken": ctx.meta.progress_token(),
            "protocolVersion": ctx.meta.protocol_version(),
            "client": ctx.meta.client_info().map(|info| info.name),
            "logLevel": ctx.meta.log_level(),
            "trace": trace,
        }))
        .expect("an object")
    }
}

async fn read(meta: Option<Value>) -> Value {
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(ReadMeta));
    let client = McpTestClient::in_process(Arc::new(McpServer::new(
        "meta-test",
        "0",
        registry,
        Arc::new(()),
    )));
    let mut params = json!({ "name": "read_meta", "arguments": {} });
    if let Some(meta) = meta {
        params["_meta"] = meta;
    }
    let result = client
        .result("tools/call", Some(params))
        .await
        .expect("a tool result");
    result["structuredContent"].clone()
}

#[tokio::test]
async fn a_legacy_call_hands_its_progress_token_and_extension_key_to_the_tool() {
    let seen = read(Some(json!({
        "progressToken": "tok-7",
        TRACE_KEY: { "span": "abc" },
    })))
    .await;
    assert_eq!(seen["progressToken"], "tok-7");
    assert_eq!(seen["trace"], "abc");
    assert_eq!(seen["protocolVersion"], Value::Null);
}

#[tokio::test]
async fn an_integer_progress_token_keeps_its_type() {
    let seen = read(Some(json!({ "progressToken": 42 }))).await;
    assert_eq!(seen["progressToken"], 42);
}

#[tokio::test]
async fn a_modern_call_hands_its_per_request_keys_to_the_tool() {
    let seen = read(Some(json!({
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientCapabilities": {},
        "io.modelcontextprotocol/clientInfo": { "name": "probe", "version": "1" },
        "io.modelcontextprotocol/logLevel": "debug",
    })))
    .await;
    assert_eq!(seen["protocolVersion"], "2026-07-28");
    assert_eq!(seen["client"], "probe");
    assert_eq!(seen["logLevel"], "debug");
}

#[tokio::test]
async fn an_extension_key_tells_absent_from_malformed() {
    assert_eq!(read(None).await["trace"], "absent");
    assert_eq!(
        read(Some(json!({ TRACE_KEY: "not an object" }))).await["trace"],
        "malformed"
    );
}

#[test]
fn a_meta_that_is_not_an_object_is_empty() {
    let meta = RequestMeta::from_params(Some(&json!({ "_meta": [1, 2] })));
    assert!(meta.is_empty());
    assert!(RequestMeta::from_params(None).is_empty());
}

#[test]
fn a_progress_token_of_another_type_is_not_one() {
    let meta = RequestMeta::from_params(Some(&json!({ "_meta": { "progressToken": 1.5 } })));
    assert_eq!(meta.progress_token(), None);
}

#[test]
fn a_progress_notification_echoes_the_token_as_it_arrived() {
    let meta = RequestMeta::from_params(Some(&json!({ "_meta": { "progressToken": 9 } })));
    let token = meta.progress_token().expect("a token");
    assert_eq!(token, ProgressToken::Integer(9));
    let wire = serde_json::to_value(ProgressNotification::new(token, 1.0, Some(2.0), None))
        .expect("serializes");
    assert_eq!(wire["params"]["progressToken"], 9);
}
