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
//! today: text, image, audio, embedded-resource, mixed and error results, the
//! JSON Schema 2020-12 input schema, and the progress, logging, sampling and
//! elicitation a running call sends its client. A scenario that needs more —
//! completion, resources, prompts, which a host serves — fails, and is
//! listed in the expected-failure baselines under `conformance/`, which is
//! where a fix shows up as a line removed.
//!
//! ```text
//! cargo run --example conformance_server -- --port 3001
//! ```

use std::collections::BTreeMap;
use std::env;
use std::error::Error;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use dravr_tronc::mcp::elicitation::{
    BooleanSchema, ElicitRequest, ElicitResult, ElicitationSchema, EnumOption,
    LegacyTitledEnumSchema, NumberSchema, NumberType, PrimitiveSchema, StringSchema,
    TitledEnumItems, TitledMultiSelectSchema, TitledSingleSelectSchema, UntitledEnumItems,
    UntitledMultiSelectSchema, UntitledSingleSelectSchema,
};
use dravr_tronc::mcp::logging::LogLevel;
use dravr_tronc::mcp::schema::{Content, ResourceContents, Tool, ToolResponse};
use dravr_tronc::mcp::schema::{CreateMessageRequest, LoggingCapability, ServerCapabilities};
use dravr_tronc::mcp::server::DEFAULT_SESSION_TTL;
use dravr_tronc::mcp::transport::http::serve;
use dravr_tronc::{McpServer, McpTool, ToolContext, ToolRegistry};
use serde_json::{json, Number, Value};
use tokio::time::sleep;

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
    for fixture in CHANNEL_FIXTURES {
        registry.register(Box::new(fixture));
    }
    registry
}

/// The pause the progress and logging scenarios ask for between messages.
const STEP: Duration = Duration::from_millis(50);

/// What a fixture that talks to its client during the call does.
#[derive(Clone, Copy)]
enum Conversation {
    /// Report progress 0, 50 and 100 out of 100.
    Progress,
    /// Log three info messages.
    Logging,
    /// Ask the client's model to answer the `prompt` argument.
    Sampling,
    /// Ask for a username and an e-mail, showing the `message` argument.
    Elicitation,
    /// Ask with a default for every primitive type (SEP-1034).
    ElicitationDefaults,
    /// Ask with each of the five enum shapes (SEP-1330).
    ElicitationEnums,
}

/// A fixture tool whose answer comes out of a conversation with the client,
/// held over [`ToolContext::client`].
struct ChannelFixture {
    name: &'static str,
    description: &'static str,
    conversation: Conversation,
}

/// The fixtures of the progress, logging, sampling and elicitation scenarios.
const CHANNEL_FIXTURES: [ChannelFixture; 6] = [
    ChannelFixture {
        name: "test_tool_with_progress",
        description: "Reports progress notifications",
        conversation: Conversation::Progress,
    },
    ChannelFixture {
        name: "test_tool_with_logging",
        description: "Sends log messages during execution",
        conversation: Conversation::Logging,
    },
    ChannelFixture {
        name: "test_sampling",
        description: "Requests LLM sampling from the client",
        conversation: Conversation::Sampling,
    },
    ChannelFixture {
        name: "test_elicitation",
        description: "Requests user input from the client",
        conversation: Conversation::Elicitation,
    },
    ChannelFixture {
        name: "test_elicitation_sep1034_defaults",
        description: "Requests input with a default for every primitive type",
        conversation: Conversation::ElicitationDefaults,
    },
    ChannelFixture {
        name: "test_elicitation_sep1330_enums",
        description: "Requests input with every enum schema shape",
        conversation: Conversation::ElicitationEnums,
    },
];

#[async_trait]
impl McpTool<()> for ChannelFixture {
    fn definition(&self) -> Tool {
        let input_schema = match self.conversation {
            Conversation::Sampling => string_argument("prompt", "The prompt to send to the LLM"),
            Conversation::Elicitation => string_argument("message", "The message to show the user"),
            Conversation::Progress
            | Conversation::Logging
            | Conversation::ElicitationDefaults
            | Conversation::ElicitationEnums => no_arguments(),
        };
        Tool {
            name: self.name.to_owned(),
            description: self.description.to_owned(),
            input_schema,
            output_schema: None,
            annotations: None,
            execution: None,
        }
    }

    async fn execute(&self, _state: &Arc<()>, ctx: &ToolContext, arguments: Value) -> ToolResponse {
        let argument = |name: &str| arguments[name].as_str().unwrap_or_default().to_owned();
        match self.conversation {
            Conversation::Progress => {
                for (step, progress) in [0.0, 50.0, 100.0].into_iter().enumerate() {
                    if step > 0 {
                        sleep(STEP).await;
                    }
                    ctx.client.progress(progress, Some(100.0), None);
                }
                ToolResponse::text("Progress reporting completed".to_owned())
            }
            Conversation::Logging => {
                let messages = [
                    "Tool execution started",
                    "Tool processing data",
                    "Tool execution completed",
                ];
                for (step, message) in messages.into_iter().enumerate() {
                    if step > 0 {
                        sleep(STEP).await;
                    }
                    ctx.client.log(LogLevel::Info, None, json!(message));
                }
                ToolResponse::text("Logging completed".to_owned())
            }
            Conversation::Sampling => sample(ctx, &argument("prompt")).await,
            Conversation::Elicitation => {
                let request = ElicitRequest {
                    message: argument("message"),
                    requested_schema: user_schema(),
                };
                elicit(ctx, &request, "User response").await
            }
            Conversation::ElicitationDefaults => {
                let request = ElicitRequest {
                    message: "Please review and update the form fields with defaults".to_owned(),
                    requested_schema: defaults_schema(),
                };
                elicit(ctx, &request, "Elicitation completed").await
            }
            Conversation::ElicitationEnums => {
                let request = ElicitRequest {
                    message: "Please select options from the enum fields".to_owned(),
                    requested_schema: enums_schema(),
                };
                elicit(ctx, &request, "Elicitation completed").await
            }
        }
    }
}

/// The input schema of a fixture taking one required string argument.
fn string_argument(name: &str, description: &str) -> Value {
    json!({
        "type": "object",
        "properties": { name: { "type": "string", "description": description } },
        "required": [name]
    })
}

/// Ask the client's model to answer `prompt`, and answer with what it said.
async fn sample(ctx: &ToolContext, prompt: &str) -> ToolResponse {
    let request: CreateMessageRequest = match serde_json::from_value(json!({
        "messages": [{ "role": "user", "content": { "type": "text", "text": prompt } }],
        "maxTokens": 100
    })) {
        Ok(request) => request,
        Err(e) => return ToolResponse::error(format!("Sampling request: {e}")),
    };
    match ctx.client.create_message(&request).await {
        Ok(result) => result.content.as_text().map_or_else(
            || ToolResponse::error("Sampling answered with a non-text block".to_owned()),
            |text| ToolResponse::text(format!("LLM response: {text}")),
        ),
        Err(e) => ToolResponse::error(format!("Sampling failed: {e}")),
    }
}

/// Ask the person `request`, and answer with what they did, after `label`.
async fn elicit(ctx: &ToolContext, request: &ElicitRequest, label: &str) -> ToolResponse {
    match ctx.client.elicit(request).await {
        Ok(ElicitResult { action, content }) => ToolResponse::text(format!(
            "{label}: action={}, content={}",
            json!(action).as_str().unwrap_or_default(),
            content.map(Value::Object).unwrap_or_default()
        )),
        Err(e) => ToolResponse::error(format!("Elicitation failed: {e}")),
    }
}

/// A described free-text field.
fn text_field(description: &str) -> PrimitiveSchema {
    PrimitiveSchema::String(StringSchema {
        description: Some(description.to_owned()),
        ..StringSchema::default()
    })
}

/// The `test_elicitation` form: a username and an e-mail address.
fn user_schema() -> ElicitationSchema {
    ElicitationSchema {
        properties: BTreeMap::from([
            ("username".to_owned(), text_field("User's response")),
            ("email".to_owned(), text_field("User's email address")),
        ]),
        required: vec!["username".to_owned(), "email".to_owned()],
        ..ElicitationSchema::default()
    }
}

/// The SEP-1034 form: a default for a string, an integer, a number, an enum
/// and a boolean.
fn defaults_schema() -> ElicitationSchema {
    let number = |kind, default| {
        PrimitiveSchema::Number(NumberSchema {
            kind,
            default: Some(default),
            ..NumberSchema::default()
        })
    };
    ElicitationSchema {
        properties: BTreeMap::from([
            (
                "name".to_owned(),
                PrimitiveSchema::String(StringSchema {
                    default: Some("John Doe".to_owned()),
                    ..StringSchema::default()
                }),
            ),
            (
                "age".to_owned(),
                number(NumberType::Integer, Number::from(30)),
            ),
            (
                "score".to_owned(),
                number(
                    NumberType::Number,
                    Number::from_f64(95.5).unwrap_or_else(|| Number::from(95)),
                ),
            ),
            (
                "status".to_owned(),
                PrimitiveSchema::UntitledSingleSelect(UntitledSingleSelectSchema {
                    values: strings(&["active", "inactive", "pending"]),
                    default: Some("active".to_owned()),
                    ..UntitledSingleSelectSchema::default()
                }),
            ),
            (
                "verified".to_owned(),
                PrimitiveSchema::Boolean(BooleanSchema {
                    default: Some(true),
                    ..BooleanSchema::default()
                }),
            ),
        ]),
        ..ElicitationSchema::default()
    }
}

/// The SEP-1330 form: each of the five enum shapes.
fn enums_schema() -> ElicitationSchema {
    let options = |pairs: &[(&str, &str)]| -> Vec<EnumOption> {
        pairs
            .iter()
            .map(|(value, title)| EnumOption {
                value: (*value).to_owned(),
                title: (*title).to_owned(),
            })
            .collect()
    };
    ElicitationSchema {
        properties: BTreeMap::from([
            (
                "untitledSingle".to_owned(),
                PrimitiveSchema::UntitledSingleSelect(UntitledSingleSelectSchema {
                    values: strings(&["option1", "option2", "option3"]),
                    ..UntitledSingleSelectSchema::default()
                }),
            ),
            (
                "titledSingle".to_owned(),
                PrimitiveSchema::TitledSingleSelect(TitledSingleSelectSchema {
                    options: options(&[
                        ("value1", "First Option"),
                        ("value2", "Second Option"),
                        ("value3", "Third Option"),
                    ]),
                    ..TitledSingleSelectSchema::default()
                }),
            ),
            (
                "legacyEnum".to_owned(),
                PrimitiveSchema::LegacyTitledEnum(LegacyTitledEnumSchema {
                    values: strings(&["opt1", "opt2", "opt3"]),
                    names: strings(&["Option One", "Option Two", "Option Three"]),
                    ..LegacyTitledEnumSchema::default()
                }),
            ),
            (
                "untitledMulti".to_owned(),
                PrimitiveSchema::UntitledMultiSelect(UntitledMultiSelectSchema {
                    items: UntitledEnumItems {
                        values: strings(&["option1", "option2", "option3"]),
                        ..UntitledEnumItems::default()
                    },
                    ..UntitledMultiSelectSchema::default()
                }),
            ),
            (
                "titledMulti".to_owned(),
                PrimitiveSchema::TitledMultiSelect(TitledMultiSelectSchema {
                    items: TitledEnumItems {
                        options: options(&[
                            ("value1", "First Choice"),
                            ("value2", "Second Choice"),
                            ("value3", "Third Choice"),
                        ]),
                    },
                    ..TitledMultiSelectSchema::default()
                }),
            ),
        ]),
        ..ElicitationSchema::default()
    }
}

/// Owned copies of `values`.
fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
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
    // Logging, so the logging fixture may log; sessions, so a client's
    // initialize-time capabilities and log level reach its later calls.
    let capabilities = ServerCapabilities {
        logging: Some(LoggingCapability {}),
        ..ServerCapabilities::tools_only()
    };
    let server = McpServer::new(
        "dravr-tronc-conformance",
        env!("CARGO_PKG_VERSION"),
        registry(),
        Arc::new(()),
    )
    .with_capabilities(capabilities)
    .with_http_sessions(DEFAULT_SESSION_TTL);
    serve(Arc::new(server), "127.0.0.1", port()?).await
}
