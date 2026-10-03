// ABOUTME: MCP conformance-suite server — McpServer over Streamable HTTP with the suite's fixture tools
// ABOUTME: Run by scripts/ci/conformance.sh; serves both protocol eras on one /mcp endpoint
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

//! The server `@modelcontextprotocol/conformance` is pointed at.
//!
//! It is a plain [`McpServer`] with no host extensions — no dispatcher, no
//! method handler — so the suite measures the engine as every host gets it.
//! It registers the fixture tools whose specified answer the engine can give
//! today: text, image, audio, embedded-resource, mixed and error results, and
//! the JSON Schema 2020-12 input schema. A scenario that needs more —
//! notifications sent during a call, resources, prompts — fails, and is
//! listed in the expected-failure baselines under `conformance/`, which is
//! where a fix shows up as a line removed.
//!
//! ```text
//! cargo run --example conformance_server -- --port 3001
//! ```

use std::env;
use std::error::Error;
use std::sync::Arc;

use async_trait::async_trait;
use dravr_tronc::mcp::schema::{Content, ResourceContents, Tool, ToolResponse};
use dravr_tronc::mcp::transport::http::serve;
use dravr_tronc::{McpServer, McpTool, ToolContext, ToolRegistry};
use serde_json::{json, Value};

/// Port served when `--port` is not given.
const DEFAULT_PORT: u16 = 3001;

/// A 1x1 PNG, base64.
const MINI_PNG_BASE64: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8/5+hHgAHggJ/PchI7wAAAABJRU5ErkJggg==";

/// A WAV of two silent samples, 8 kHz mono 8-bit PCM, base64.
const MINI_WAV_BASE64: &str = "UklGRiYAAABXQVZFZm10IBAAAAABAAEAQB8AAEAfAAABAAgAZGF0YQIAAACAgA==";

/// What a fixture tool answers.
#[derive(Clone)]
enum Answer {
    Text(&'static str),
    Image,
    Audio,
    EmbeddedResource,
    Mixed,
    Error(&'static str),
}

/// One of the suite's fixture tools: a fixed name, schema and answer.
struct Fixture {
    name: &'static str,
    description: &'static str,
    input_schema: Value,
    answer: Answer,
}

#[async_trait]
impl McpTool<()> for Fixture {
    fn definition(&self) -> Tool {
        Tool {
            name: self.name.to_owned(),
            description: self.description.to_owned(),
            input_schema: self.input_schema.clone(),
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
        match &self.answer {
            Answer::Text(text) => ToolResponse::text((*text).to_owned()),
            Answer::Error(message) => ToolResponse::error((*message).to_owned()),
            Answer::Image => ToolResponse::image(MINI_PNG_BASE64, "image/png"),
            Answer::Audio => ToolResponse::audio(MINI_WAV_BASE64, "audio/wav"),
            Answer::EmbeddedResource => ToolResponse::resource(
                ResourceContents::text(
                    "test://embedded-resource",
                    "This is an embedded resource content.",
                )
                .with_mime_type("text/plain"),
            ),
            Answer::Mixed => ToolResponse::blocks(vec![
                Content::text("Multiple content types test:"),
                Content::image(MINI_PNG_BASE64, "image/png"),
                Content::resource(
                    ResourceContents::text(
                        "test://mixed-content-resource",
                        r#"{"test":"data","value":123}"#,
                    )
                    .with_mime_type("application/json"),
                ),
            ]),
        }
    }
}

/// The input schema of a fixture that takes no arguments.
fn no_arguments() -> Value {
    json!({ "type": "object", "properties": {}, "additionalProperties": false })
}

/// The schema the `json-schema-2020-12` scenario expects to find unaltered
/// in `tools/list`.
fn json_schema_2020_12() -> Value {
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "$defs": {
            "address": {
                "$anchor": "addressDef",
                "type": "object",
                "properties": {
                    "street": { "type": "string" },
                    "city": { "type": "string" }
                }
            }
        },
        "properties": {
            "name": { "type": "string" },
            "address": { "$ref": "#/$defs/address" },
            "contactMethod": { "type": "string", "enum": ["phone", "email"] },
            "phone": { "type": "string" },
            "email": { "type": "string" }
        },
        "allOf": [{ "anyOf": [{ "required": ["phone"] }, { "required": ["email"] }] }],
        "if": {
            "properties": { "contactMethod": { "const": "phone" } },
            "required": ["contactMethod"]
        },
        "then": { "required": ["phone"] },
        "else": { "required": ["email"] },
        "additionalProperties": false
    })
}

fn registry() -> ToolRegistry<()> {
    let fixtures = [
        Fixture {
            name: "test_simple_text",
            description: "Returns simple text",
            input_schema: no_arguments(),
            answer: Answer::Text("This is a simple text response for testing."),
        },
        Fixture {
            name: "test_image_content",
            description: "Returns image content",
            input_schema: no_arguments(),
            answer: Answer::Image,
        },
        Fixture {
            name: "test_audio_content",
            description: "Returns audio content",
            input_schema: no_arguments(),
            answer: Answer::Audio,
        },
        Fixture {
            name: "test_embedded_resource",
            description: "Returns embedded resource content",
            input_schema: no_arguments(),
            answer: Answer::EmbeddedResource,
        },
        Fixture {
            name: "test_multiple_content_types",
            description: "Returns multiple content types",
            input_schema: no_arguments(),
            answer: Answer::Mixed,
        },
        Fixture {
            name: "test_error_handling",
            description: "Always returns an error",
            input_schema: no_arguments(),
            answer: Answer::Error("This tool intentionally returns an error for testing"),
        },
        Fixture {
            name: "json_schema_2020_12_tool",
            description: "Tool with JSON Schema 2020-12 features",
            input_schema: json_schema_2020_12(),
            answer: Answer::Text("JSON Schema 2020-12 tool called"),
        },
    ];
    let mut registry = ToolRegistry::new();
    for fixture in fixtures {
        registry.register(Box::new(fixture));
    }
    registry
}

/// The port named by `--port <n>`, or [`DEFAULT_PORT`].
fn port() -> Result<u16, Box<dyn Error + Send + Sync>> {
    let args: Vec<String> = env::args().collect();
    match args.iter().position(|a| a == "--port") {
        None => Ok(DEFAULT_PORT),
        Some(i) => {
            let value = args.get(i + 1).ok_or("--port needs a value")?;
            Ok(value.parse()?)
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    let server = McpServer::new(
        "dravr-tronc-conformance",
        env!("CARGO_PKG_VERSION"),
        registry(),
        Arc::new(()),
    );
    serve(Arc::new(server), "127.0.0.1", port()?).await
}
