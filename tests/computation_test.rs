// ABOUTME: Drives a Computation through McpServer's JSON-RPC path end to end
// ABOUTME: Pins the generated schema, the rendered result's float precision and each error's wording
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

#[derive(Serialize)]
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

async fn call(arguments: Value) -> (bool, String) {
    let result = rpc(json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": { "name": "warmest_reading", "arguments": arguments }
    }))
    .await;
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
