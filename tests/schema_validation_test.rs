// ABOUTME: tools/call checks arguments against inputSchema and structuredContent against outputSchema
// ABOUTME: Pins each refusal's wording, the 2020-12 default dialect, and what is deliberately not checked
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

#![cfg(feature = "schema-validation")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use dravr_tronc::mcp::schema::{Tool, ToolResponse};
use dravr_tronc::mcp::validation::{SchemaRole, ToolSchemaValidator};
use dravr_tronc::testkit::assert::{
    assert_structured_content, assert_tool_error, assert_tool_success, tool_text,
};
use dravr_tronc::testkit::McpTestClient;
use dravr_tronc::{McpServer, McpTool, ToolContext, ToolRegistry};
use serde_json::{json, Value};

/// Counts how many calls reached a handler.
#[derive(Default)]
struct Calls(AtomicUsize);

/// A tool that answers with whatever response it was built with.
struct Fixed {
    name: &'static str,
    input_schema: Value,
    output_schema: Option<Value>,
    response: ToolResponse,
}

#[async_trait]
impl McpTool<Calls> for Fixed {
    fn definition(&self) -> Tool {
        Tool {
            name: self.name.to_owned(),
            description: "A fixed answer".to_owned(),
            input_schema: self.input_schema.clone(),
            output_schema: self.output_schema.clone(),
            annotations: None,
            execution: None,
        }
    }

    async fn execute(&self, state: &Arc<Calls>, _ctx: &ToolContext, _args: Value) -> ToolResponse {
        state.0.fetch_add(1, Ordering::SeqCst);
        self.response.clone()
    }
}

fn distance_input() -> Value {
    json!({
        "type": "object",
        "properties": { "km": { "type": "number", "minimum": 0 } },
        "required": ["km"],
        "additionalProperties": false,
    })
}

fn pace_output() -> Value {
    json!({
        "type": "object",
        "properties": { "pace": { "type": "number" } },
        "required": ["pace"],
    })
}

fn tool(response: ToolResponse) -> Fixed {
    Fixed {
        name: "pace",
        input_schema: distance_input(),
        output_schema: Some(pace_output()),
        response,
    }
}

async fn call(tool: Fixed, arguments: Value) -> (ToolResponse, usize) {
    let mut registry = ToolRegistry::new();
    let name = tool.name;
    registry.register(Box::new(tool));
    let state = Arc::new(Calls::default());
    let server = McpServer::new("validation-test", "0", registry, Arc::clone(&state));
    let result = McpTestClient::in_process(Arc::new(server))
        .call_tool(name, arguments)
        .await
        .expect("a tool result, not a protocol error");
    (result, state.0.load(Ordering::SeqCst))
}

fn structured(value: &Value) -> ToolResponse {
    ToolResponse::structured(value).expect("an object")
}

#[tokio::test]
async fn conforming_arguments_and_result_pass_through() {
    let (result, calls) = call(
        tool(structured(&json!({ "pace": 5.2 }))),
        json!({ "km": 10 }),
    )
    .await;
    assert_eq!(calls, 1);
    assert_structured_content(&result, &json!({ "pace": 5.2 }));
}

#[tokio::test]
async fn arguments_that_violate_the_input_schema_never_reach_the_handler() {
    let (result, calls) = call(
        tool(structured(&json!({ "pace": 5.2 }))),
        json!({ "km": -1 }),
    )
    .await;
    assert_eq!(calls, 0);
    assert!(result.is_error, "a tool error the model can correct");
    assert_eq!(
        tool_text(&result),
        "pace: invalid arguments: /km: -1 is less than the minimum of 0"
    );
}

#[tokio::test]
async fn every_violation_is_reported() {
    let (result, _) = call(
        tool(structured(&json!({ "pace": 5.2 }))),
        json!({ "miles": 3 }),
    )
    .await;
    let text = tool_text(&result);
    assert!(text.starts_with("pace: invalid arguments: "), "{text}");
    assert!(text.contains("\"km\" is a required property"), "{text}");
    assert!(
        text.contains("Additional properties are not allowed"),
        "{text}"
    );
}

#[tokio::test]
async fn a_result_that_violates_the_output_schema_is_refused() {
    let (result, calls) = call(
        tool(structured(&json!({ "pace": "fast" }))),
        json!({ "km": 1 }),
    )
    .await;
    assert_eq!(calls, 1);
    assert!(result.is_error);
    assert_eq!(
        tool_text(&result),
        "pace: the result does not match the tool's outputSchema: \
         /pace: \"fast\" is not of type \"number\""
    );
    assert!(result.structured_content.is_none());
}

#[tokio::test]
async fn a_success_without_structured_content_is_refused_when_an_output_schema_is_declared() {
    let (result, _) = call(
        tool(ToolResponse::text("5.2".to_owned())),
        json!({ "km": 1 }),
    )
    .await;
    assert!(result.is_error);
    assert_eq!(
        tool_text(&result),
        "pace: the result does not match the tool's outputSchema: \
         the tool declares an outputSchema but returned no structuredContent"
    );
}

#[tokio::test]
async fn an_error_result_is_not_checked_against_the_output_schema() {
    let (result, _) = call(
        tool(ToolResponse::error("no GPS".to_owned())),
        json!({ "km": 1 }),
    )
    .await;
    assert_tool_error(&result, "no GPS");
}

#[tokio::test]
async fn a_tool_without_an_output_schema_returns_text_unchecked() {
    let mut text_only = tool(ToolResponse::text("5.2".to_owned()));
    text_only.output_schema = None;
    let (result, _) = call(text_only, json!({ "km": 1 })).await;
    assert_eq!(assert_tool_success(&result), "5.2");
}

#[tokio::test]
async fn a_tool_whose_schema_does_not_compile_is_refused_every_call() {
    let mut broken = tool(structured(&json!({ "pace": 1 })));
    broken.input_schema = json!({ "type": "object", "minProperties": "two" });
    let (result, calls) = call(broken, json!({})).await;
    assert_eq!(calls, 0);
    assert!(result.is_error);
    let text = tool_text(&result);
    assert!(
        text.starts_with("pace: its inputSchema is not a usable JSON Schema: "),
        "{text}"
    );
}

#[test]
fn a_schema_without_a_dialect_is_read_as_2020_12() {
    // `prefixItems` exists only from 2020-12; an earlier draft ignores it.
    let validator = ToolSchemaValidator::compile(&Tool {
        name: "t".to_owned(),
        description: String::new(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "pair": { "type": "array", "prefixItems": [{ "type": "string" }] }
            }
        }),
        output_schema: None,
        annotations: None,
        execution: None,
    })
    .expect("compiles");
    assert!(validator.check_arguments(&json!({ "pair": ["a"] })).is_ok());
    assert!(validator.check_arguments(&json!({ "pair": [1] })).is_err());
}

#[test]
fn a_remote_reference_is_not_fetched() {
    let refused = ToolSchemaValidator::compile(&Tool {
        name: "t".to_owned(),
        description: String::new(),
        input_schema: json!({ "type": "object" }),
        output_schema: Some(json!({ "$ref": "https://example.com/schema.json" })),
        annotations: None,
        execution: None,
    })
    .expect_err("an external document is never resolved");
    assert_eq!(refused.schema, SchemaRole::Output);
}
