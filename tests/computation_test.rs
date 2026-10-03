// ABOUTME: Drives a Computation through McpServer's JSON-RPC path end to end
// ABOUTME: Pins both generated schemas, the structured result's float precision and each error's wording
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

#![cfg(feature = "computation")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use dravr_tronc::mcp::schema::{Tool, ToolResponse};
use dravr_tronc::testkit::assert::{
    assert_structured_content, assert_tool_error, assert_tool_success, tool_text,
};
use dravr_tronc::testkit::McpTestClient;
use dravr_tronc::{Computation, McpServer, ToolRegistry};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// Arguments of the test tool.
#[derive(Deserialize, JsonSchema)]
struct Readings {
    /// Temperatures in °C.
    celsius: Vec<f32>,
}

/// The warmest reading and how many were compared.
#[derive(Serialize, JsonSchema)]
struct Warmest {
    celsius: f32,
    count: usize,
}

struct WarmestReading;

impl Computation for WarmestReading {
    type Input = Readings;
    type Output = Warmest;
    const NAME: &'static str = "warmest_reading";
    const TITLE: &'static str = "Warmest reading";
    const DESCRIPTION: &'static str = "The highest of the given temperatures.";

    fn compute(&self, input: Readings) -> Result<Warmest, String> {
        let count = input.celsius.len();
        input
            .celsius
            .into_iter()
            .reduce(f32::max)
            .map(|celsius| Warmest { celsius, count })
            .ok_or_else(|| "celsius is empty".to_owned())
    }
}

fn client() -> McpTestClient {
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(WarmestReading));
    McpTestClient::in_process(Arc::new(McpServer::new(
        "computation-test",
        "0",
        registry,
        Arc::new(()),
    )))
}

async fn listed() -> Tool {
    let mut tools = client().list_tools().await.expect("tools/list");
    assert_eq!(tools.len(), 1);
    tools.remove(0)
}

async fn call(arguments: Value) -> ToolResponse {
    client()
        .call_tool("warmest_reading", arguments)
        .await
        .expect("a tool result")
}

#[tokio::test]
async fn the_definition_is_generated_from_the_input_type() {
    let tool = listed().await;
    assert_eq!(tool.name, "warmest_reading");
    assert_eq!(tool.description, "The highest of the given temperatures.");
    assert_eq!(tool.input_schema["type"], "object");
    assert_eq!(tool.input_schema["required"], json!(["celsius"]));
    assert_eq!(
        tool.input_schema["properties"]["celsius"]["description"],
        "Temperatures in °C."
    );
    let annotations = tool.annotations.expect("annotations");
    assert_eq!(annotations.title.as_deref(), Some("Warmest reading"));
    assert_eq!(annotations.read_only_hint, Some(true));
    assert_eq!(annotations.open_world_hint, Some(false));
}

#[tokio::test]
async fn the_output_schema_is_generated_from_the_output_type() {
    let output = listed().await.output_schema.expect("an outputSchema");
    assert_eq!(output["type"], "object");
    assert_eq!(output["title"], "Warmest");
    assert_eq!(output["required"], json!(["celsius", "count"]));
    assert_eq!(output["properties"]["celsius"]["type"], "number");
}

#[tokio::test]
async fn the_result_is_structured_content_with_the_same_json_as_text() {
    let result = call(json!({ "celsius": [9.5, 12.8, 11.0] })).await;
    // The structured f32 is 12.8, not its widened f64.
    assert_structured_content(&result, &json!({ "celsius": 12.8, "count": 3 }));
    let reparsed: Value =
        serde_json::from_str(&tool_text(&result)).expect("the text block is JSON");
    assert_eq!(Some(reparsed), result.structured_content);
}

#[tokio::test]
async fn an_f32_result_is_written_at_its_own_precision() {
    let result = call(json!({ "celsius": [9.5, 12.8, 11.0] })).await;
    assert_eq!(
        assert_tool_success(&result),
        r#"{"celsius":12.8,"count":3}"#
    );
}

#[tokio::test]
async fn malformed_arguments_name_the_tool() {
    let result = call(json!({ "celsius": "warm" })).await;
    // With schema validation the generated inputSchema refuses the call before
    // serde reads it; without, the parse does. Either way the tool is named.
    let expected = if cfg!(feature = "schema-validation") {
        r#"warmest_reading: invalid arguments: /celsius: "warm" is not of type "array""#
    } else {
        "warmest_reading: invalid arguments: invalid type: string"
    };
    assert_tool_error(&result, expected);
}

#[tokio::test]
async fn a_computation_error_is_a_tool_error_naming_the_tool() {
    let result = call(json!({ "celsius": [] })).await;
    assert!(result.is_error);
    assert_eq!(tool_text(&result), "warmest_reading: celsius is empty");
}

/// A computation whose output is not an object.
struct Celsius;

impl Computation for Celsius {
    type Input = Readings;
    type Output = Vec<f32>;
    const NAME: &'static str = "celsius";
    const TITLE: &'static str = "Celsius";
    const DESCRIPTION: &'static str = "The readings, unchanged.";

    fn compute(&self, input: Readings) -> Result<Vec<f32>, String> {
        Ok(input.celsius)
    }
}

#[tokio::test]
async fn an_output_that_is_not_an_object_is_a_tool_error_naming_the_tool() {
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(Celsius));
    let client = McpTestClient::in_process(Arc::new(McpServer::new(
        "computation-test",
        "0",
        registry,
        Arc::new(()),
    )));
    let result = client
        .call_tool("celsius", json!({ "celsius": [1.0] }))
        .await
        .expect("a tool result");
    assert!(result.is_error);
    assert_eq!(
        tool_text(&result),
        "celsius: could not render the result: structured content must be a JSON object, \
         but the result is an array"
    );
}
