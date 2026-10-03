// ABOUTME: Drives a Computation through McpServer's JSON-RPC path end to end
// ABOUTME: Pins both generated schemas, the structured result's float precision and each error's wording
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

#![cfg(feature = "computation")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

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

fn server() -> McpServer<()> {
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(WarmestReading));
    McpServer::new("computation-test", "0", registry, Arc::new(()))
}

async fn rpc(body: Value) -> Value {
    server()
        .handle_raw(&body.to_string())
        .await
        .expect("response")
        .result
        .expect("result")
}

async fn call_result(arguments: Value) -> Value {
    rpc(json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": { "name": "warmest_reading", "arguments": arguments }
    }))
    .await
}

async fn call(arguments: Value) -> (bool, String) {
    let result = call_result(arguments).await;
    (
        result["isError"].as_bool().unwrap_or(false),
        result["content"][0]["text"]
            .as_str()
            .expect("text")
            .to_owned(),
    )
}

#[tokio::test]
async fn the_definition_is_generated_from_the_input_type() {
    let listed = rpc(json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" })).await;
    let tool = &listed["tools"][0];
    assert_eq!(tool["name"], "warmest_reading");
    assert_eq!(
        tool["description"],
        "The highest of the given temperatures."
    );
    assert_eq!(tool["inputSchema"]["type"], "object");
    assert_eq!(tool["inputSchema"]["required"], json!(["celsius"]));
    assert_eq!(
        tool["inputSchema"]["properties"]["celsius"]["description"],
        "Temperatures in °C."
    );
    assert_eq!(tool["annotations"]["title"], "Warmest reading");
    assert_eq!(tool["annotations"]["readOnlyHint"], true);
    assert_eq!(tool["annotations"]["openWorldHint"], false);
}

#[tokio::test]
async fn the_output_schema_is_generated_from_the_output_type() {
    let listed = rpc(json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" })).await;
    let output = &listed["tools"][0]["outputSchema"];
    assert_eq!(output["type"], "object");
    assert_eq!(output["title"], "Warmest");
    assert_eq!(output["required"], json!(["celsius", "count"]));
    assert_eq!(output["properties"]["celsius"]["type"], "number");
}

#[tokio::test]
async fn the_result_is_structured_content_with_the_same_json_as_text() {
    let result = call_result(json!({ "celsius": [9.5, 12.8, 11.0] })).await;
    assert_eq!(
        result["structuredContent"],
        json!({ "celsius": 12.8, "count": 3 }),
        "the structured f32 is 12.8, not its widened f64"
    );
    let text = result["content"][0]["text"].as_str().expect("text");
    let reparsed: Value = serde_json::from_str(text).expect("the text block is JSON");
    assert_eq!(reparsed, result["structuredContent"]);
}

#[tokio::test]
async fn an_f32_result_is_written_at_its_own_precision() {
    let (is_error, text) = call(json!({ "celsius": [9.5, 12.8, 11.0] })).await;
    assert!(!is_error, "{text}");
    assert_eq!(text, r#"{"celsius":12.8,"count":3}"#);
}

#[tokio::test]
async fn malformed_arguments_name_the_tool() {
    let (is_error, text) = call(json!({ "celsius": "warm" })).await;
    assert!(is_error);
    assert!(
        text.starts_with("warmest_reading: invalid arguments: invalid type: string"),
        "{text}"
    );
}

#[tokio::test]
async fn a_computation_error_is_a_tool_error_naming_the_tool() {
    let (is_error, text) = call(json!({ "celsius": [] })).await;
    assert!(is_error);
    assert_eq!(text, "warmest_reading: celsius is empty");
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
    let server = McpServer::new("computation-test", "0", registry, Arc::new(()));
    let result = server
        .handle_raw(
            &json!({
                "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                "params": { "name": "celsius", "arguments": { "celsius": [1.0] } }
            })
            .to_string(),
        )
        .await
        .expect("response")
        .result
        .expect("result");
    assert_eq!(result["isError"], true);
    assert_eq!(
        result["content"][0]["text"],
        "celsius: could not render the result: structured content must be a JSON object, \
         but the result is an array"
    );
}
