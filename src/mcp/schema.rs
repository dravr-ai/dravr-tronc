// ABOUTME: MCP wire-protocol schema types (initialize, tools, capabilities, content)
// ABOUTME: Layered on the JSON-RPC foundation in protocol.rs; generic over any MCP server
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

//! MCP wire-frame schema types.
//!
//! These layer the Model Context Protocol message shapes (initialize handshake,
//! `tools/list`, `tools/call`, capabilities, content) on top of the
//! protocol-agnostic JSON-RPC envelope in [`crate::mcp::protocol`]. They are
//! free of any project-specific coupling so every `dravr-*` MCP server shares a
//! single canonical wire vocabulary.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::mcp::protocol::{JsonRpcError, JsonRpcRequest, JsonRpcResponse, JSONRPC_VERSION};

/// `notifications/progress` method string.
const METHOD_PROGRESS: &str = "notifications/progress";
/// `notifications/cancelled` method string.
const METHOD_CANCELLED: &str = "notifications/cancelled";
/// `notifications/oauth_completed` method string.
const METHOD_OAUTH_COMPLETED: &str = "notifications/oauth_completed";

/// MCP request wire frame (alias for the canonical JSON-RPC request).
pub type McpRequest = JsonRpcRequest;
/// MCP response wire frame (alias for the canonical JSON-RPC response).
pub type McpResponse = JsonRpcResponse;
/// MCP error object (alias for the canonical JSON-RPC error).
pub type McpError = JsonRpcError;

/// MCP protocol information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProtocolInfo {
    /// MCP protocol version (e.g. `"2025-11-25"`).
    #[serde(rename = "protocolVersion")]
    pub protocol_version: String,
}

/// Server information per the MCP spec.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerInfo {
    /// Server name identifier (machine-readable).
    pub name: String,
    /// Server version string.
    pub version: String,
    /// Human-readable display title (MCP 2025-11-25).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Human-readable server description (MCP 2025-11-25).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Server website URL (MCP 2025-11-25).
    #[serde(rename = "websiteUrl", skip_serializing_if = "Option::is_none")]
    pub website_url: Option<String>,
}

impl ServerInfo {
    /// Minimal server identity from a name and version (no display metadata).
    #[must_use]
    pub fn new(name: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            version: version.into(),
            title: None,
            description: None,
            website_url: None,
        }
    }
}

/// Behavioral annotations for an MCP tool (MCP 2025-11-25).
///
/// Hints to clients about tool behavior, enabling better UX decisions such as
/// confirmation prompts for destructive operations.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ToolAnnotations {
    /// Human-readable display title.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Whether the tool only reads data without side effects.
    #[serde(rename = "readOnlyHint", skip_serializing_if = "Option::is_none")]
    pub read_only_hint: Option<bool>,
    /// Whether the tool may perform destructive operations (delete, overwrite).
    #[serde(rename = "destructiveHint", skip_serializing_if = "Option::is_none")]
    pub destructive_hint: Option<bool>,
    /// Whether repeated calls with the same args have no additional effect.
    #[serde(rename = "idempotentHint", skip_serializing_if = "Option::is_none")]
    pub idempotent_hint: Option<bool>,
    /// Whether the tool interacts with external entities beyond the server.
    #[serde(rename = "openWorldHint", skip_serializing_if = "Option::is_none")]
    pub open_world_hint: Option<bool>,
}

/// Tool definition exposed via `tools/list`.
///
/// `input_schema` is a raw JSON Schema value so each tool can describe arbitrary
/// inputs; it serializes as the spec-mandated `inputSchema`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tool {
    /// Unique tool name.
    pub name: String,
    /// Human-readable tool description.
    pub description: String,
    /// JSON Schema describing the tool's input.
    #[serde(rename = "inputSchema")]
    pub input_schema: serde_json::Value,
    /// Optional behavioral annotations (MCP 2025-11-25).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub annotations: Option<ToolAnnotations>,
}

/// Parameters for a `tools/call` request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    /// Name of the tool to invoke.
    pub name: String,
    /// Tool arguments as JSON.
    #[serde(default)]
    pub arguments: Option<serde_json::Value>,
}

/// Result of a `tools/call` invocation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResponse {
    /// Response content items.
    pub content: Vec<Content>,
    /// Whether the tool execution resulted in an error.
    #[serde(rename = "isError")]
    pub is_error: bool,
    /// Structured response data (MCP 2025-11-25 `structuredContent`).
    #[serde(rename = "structuredContent", skip_serializing_if = "Option::is_none")]
    pub structured_content: Option<serde_json::Value>,
}

impl ToolResponse {
    /// Build a successful text result.
    #[must_use]
    pub fn text(content: String) -> Self {
        Self {
            content: vec![Content::Text { text: content }],
            is_error: false,
            structured_content: None,
        }
    }

    /// Build an error result carrying the given message.
    #[must_use]
    pub fn error(message: String) -> Self {
        Self {
            content: vec![Content::Text { text: message }],
            is_error: true,
            structured_content: None,
        }
    }
}

/// Content item within an MCP message or tool result.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Content {
    /// Plain text content.
    #[serde(rename = "text")]
    Text {
        /// Text content string.
        text: String,
    },
    /// Image content with base64 data.
    #[serde(rename = "image")]
    Image {
        /// Base64-encoded image data.
        data: String,
        /// MIME type of the image (e.g. `"image/png"`).
        #[serde(rename = "mimeType")]
        mime_type: String,
    },
    /// Resource reference with URI.
    #[serde(rename = "resource")]
    Resource {
        /// URI of the resource.
        uri: String,
        /// Optional text description of the resource.
        #[serde(skip_serializing_if = "Option::is_none")]
        text: Option<String>,
        /// MIME type of the resource.
        #[serde(rename = "mimeType", skip_serializing_if = "Option::is_none")]
        mime_type: Option<String>,
    },
    /// Progress update for a long-running operation.
    #[serde(rename = "progress")]
    Progress {
        /// Token identifying the operation.
        #[serde(rename = "progressToken")]
        progress_token: String,
        /// Current progress value.
        progress: f64,
        /// Optional total for computing a percentage.
        total: Option<f64>,
    },
}

impl Content {
    /// Borrow the inner string when this is a [`Content::Text`].
    #[must_use]
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text { text } => Some(text),
            _ => None,
        }
    }
}

/// MCP server capability declarations.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ServerCapabilities {
    /// Experimental capabilities not in the MCP spec.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub experimental: Option<HashMap<String, serde_json::Value>>,
    /// Server logging capability.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logging: Option<LoggingCapability>,
    /// Server prompts capability.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompts: Option<PromptsCapability>,
    /// Server resources capability.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resources: Option<ResourcesCapability>,
    /// Server tools capability.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<ToolsCapability>,
    /// Server authentication capability.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth: Option<AuthCapability>,
    /// Server OAuth 2.0 capability.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oauth2: Option<OAuth2Capability>,
    /// Server completion (auto-complete) capability.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completion: Option<CompletionCapability>,
    /// Server sampling (LLM calls) capability.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sampling: Option<SamplingCapability>,
}

impl ServerCapabilities {
    /// Capabilities advertising tool support only (no list-changed notifications).
    #[must_use]
    pub fn tools_only() -> Self {
        Self {
            tools: Some(ToolsCapability {
                list_changed: Some(false),
            }),
            ..Self::default()
        }
    }
}

/// Tools capability.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolsCapability {
    /// Whether the server emits `tools/list_changed` notifications.
    #[serde(rename = "listChanged", skip_serializing_if = "Option::is_none")]
    pub list_changed: Option<bool>,
}

/// Logging capability.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoggingCapability {}

/// Prompts capability.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptsCapability {
    /// Whether the server emits `prompts/list_changed` notifications.
    #[serde(rename = "listChanged", skip_serializing_if = "Option::is_none")]
    pub list_changed: Option<bool>,
}

/// Resources capability.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourcesCapability {
    /// Whether the server supports resource subscriptions.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subscribe: Option<bool>,
    /// Whether the server emits `resources/list_changed` notifications.
    #[serde(rename = "listChanged", skip_serializing_if = "Option::is_none")]
    pub list_changed: Option<bool>,
}

/// Authentication capability.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthCapability {
    /// OAuth 2.0 authentication details.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oauth2: Option<OAuth2Capability>,
}

/// OAuth 2.0 capability.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuth2Capability {
    /// OAuth 2.0 discovery URL.
    #[serde(rename = "discoveryUrl")]
    pub discovery_url: String,
    /// OAuth 2.0 authorization endpoint.
    #[serde(rename = "authorizationEndpoint")]
    pub authorization_endpoint: String,
    /// OAuth 2.0 token endpoint.
    #[serde(rename = "tokenEndpoint")]
    pub token_endpoint: String,
    /// OAuth 2.0 client registration endpoint (RFC 7591).
    #[serde(rename = "registrationEndpoint")]
    pub registration_endpoint: String,
}

/// Completion (auto-complete) capability.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionCapability {}

/// Client capabilities sent in an `initialize` request.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ClientCapabilities {
    /// Experimental client capabilities.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub experimental: Option<HashMap<String, serde_json::Value>>,
    /// Client sampling capability.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sampling: Option<SamplingCapability>,
    /// Client roots capability.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub roots: Option<RootsCapability>,
}

/// Sampling capability (declared by either client or server).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SamplingCapability {}

/// Roots capability.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RootsCapability {
    /// Whether the client emits `roots/list_changed` notifications.
    #[serde(rename = "listChanged", skip_serializing_if = "Option::is_none")]
    pub list_changed: Option<bool>,
}

/// MCP `initialize` request from a client (legacy handshake).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitializeRequest {
    /// Client's requested protocol version.
    #[serde(rename = "protocolVersion")]
    pub protocol_version: String,
    /// Client information.
    #[serde(rename = "clientInfo")]
    pub client_info: ClientInfo,
    /// Client capabilities.
    pub capabilities: ClientCapabilities,
    /// Optional client-supplied OAuth application credentials, kept as raw JSON
    /// so server implementations interpret their own credential shape.
    #[serde(
        rename = "oauthCredentials",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub oauth_credentials: Option<HashMap<String, serde_json::Value>>,
}

/// Client information per the MCP spec.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientInfo {
    /// Client name identifier (machine-readable).
    pub name: String,
    /// Client version string.
    pub version: String,
    /// Human-readable display title (MCP 2025-11-25).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Human-readable client description (MCP 2025-11-25).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Client website URL (MCP 2025-11-25).
    #[serde(
        default,
        rename = "websiteUrl",
        skip_serializing_if = "Option::is_none"
    )]
    pub website_url: Option<String>,
}

/// MCP `initialize` response from the server (legacy handshake).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitializeResponse {
    /// Negotiated protocol version.
    #[serde(rename = "protocolVersion")]
    pub protocol_version: String,
    /// Server information.
    #[serde(rename = "serverInfo")]
    pub server_info: ServerInfo,
    /// Server capabilities.
    pub capabilities: ServerCapabilities,
    /// Optional natural-language instructions for the client.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
}

impl InitializeResponse {
    /// Assemble an `initialize` response from its parts. Callers resolve the
    /// negotiated version, identity, advertised capabilities, and instructions.
    #[must_use]
    pub fn new(
        protocol_version: String,
        server_info: ServerInfo,
        capabilities: ServerCapabilities,
        instructions: Option<String>,
    ) -> Self {
        Self {
            protocol_version,
            server_info,
            capabilities,
            instructions,
        }
    }
}

/// A typed tool schema (`tools/list` entry with a structured input schema).
///
/// The raw-`Value` [`Tool`] suits arbitrary tools; `ToolSchema` is the typed
/// variant for servers that describe inputs with [`JsonSchema`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSchema {
    /// Tool name identifier.
    pub name: String,
    /// Human-readable tool description.
    pub description: String,
    /// JSON Schema for the tool's input parameters.
    #[serde(rename = "inputSchema")]
    pub input_schema: JsonSchema,
    /// Optional behavioral annotations (MCP 2025-11-25).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub annotations: Option<ToolAnnotations>,
}

impl ToolSchema {
    /// Create a tool schema without annotations.
    #[must_use]
    pub fn without_annotations(
        name: String,
        description: String,
        input_schema: JsonSchema,
    ) -> Self {
        Self {
            name,
            description,
            input_schema,
            annotations: None,
        }
    }

    /// Create a tool schema with behavioral annotations (MCP 2025-11-25).
    #[must_use]
    pub fn with_annotations(
        name: String,
        description: String,
        input_schema: JsonSchema,
        annotations: ToolAnnotations,
    ) -> Self {
        Self {
            name,
            description,
            input_schema,
            annotations: Some(annotations),
        }
    }
}

/// A (typed) JSON Schema definition for tool inputs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonSchema {
    /// Schema type (e.g. `"object"`, `"string"`).
    #[serde(rename = "type")]
    pub schema_type: String,
    /// Property definitions for object schemas.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub properties: Option<HashMap<String, PropertySchema>>,
    /// Names of required properties.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required: Option<Vec<String>>,
}

/// A JSON Schema property definition.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PropertySchema {
    /// Property type (e.g. `"string"`, `"number"`, `"boolean"`).
    #[serde(rename = "type")]
    pub property_type: String,
    /// Human-readable property description.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Item schema for array-type properties (JSON Schema `items`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub items: Option<Box<Self>>,
    /// Nested property definitions for object-type properties.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub properties: Option<HashMap<String, Self>>,
    /// Required fields for object-type properties.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required: Option<Vec<String>>,
}

/// Notification for progress on a long-running operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProgressNotification {
    /// JSON-RPC version (`"2.0"`).
    pub jsonrpc: String,
    /// Method name (`notifications/progress`).
    pub method: String,
    /// Progress parameters.
    pub params: ProgressParams,
}

/// Parameters for a progress notification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProgressParams {
    /// Token identifying the operation being tracked.
    #[serde(rename = "progressToken")]
    pub progress_token: String,
    /// Current progress value.
    pub progress: f64,
    /// Optional total for percentage calculation.
    pub total: Option<f64>,
    /// Optional human-readable progress message.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl ProgressNotification {
    /// Create a progress notification.
    #[must_use]
    pub fn new(
        progress_token: String,
        progress: f64,
        total: Option<f64>,
        message: Option<String>,
    ) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_owned(),
            method: METHOD_PROGRESS.to_owned(),
            params: ProgressParams {
                progress_token,
                progress,
                total,
                message,
            },
        }
    }

    /// Create a cancellation notification.
    #[must_use]
    pub fn cancelled(progress_token: String, message: Option<String>) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_owned(),
            method: METHOD_CANCELLED.to_owned(),
            params: ProgressParams {
                progress_token,
                progress: 0.0,
                total: None,
                message,
            },
        }
    }
}

/// Notification that an OAuth flow completed, for MCP clients.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthCompletedNotification {
    /// JSON-RPC version (`"2.0"`).
    pub jsonrpc: String,
    /// Method name (`notifications/oauth_completed`).
    pub method: String,
    /// OAuth completion parameters.
    pub params: OAuthCompletedParams,
}

/// Parameters for an OAuth completion notification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthCompletedParams {
    /// Provider name (e.g. `"strava"`).
    pub provider: String,
    /// Whether the flow completed successfully.
    pub success: bool,
    /// Human-readable status message.
    pub message: String,
    /// User id when authentication succeeded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
}

impl OAuthCompletedNotification {
    /// Create an OAuth completion notification.
    #[must_use]
    pub fn new(provider: String, success: bool, message: String, user_id: Option<String>) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_owned(),
            method: METHOD_OAUTH_COMPLETED.to_owned(),
            params: OAuthCompletedParams {
                provider,
                success,
                message,
                user_id,
            },
        }
    }
}

/// Request to create a message via the client's LLM (MCP sampling).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateMessageRequest {
    /// Messages to send to the LLM.
    pub messages: Vec<PromptMessage>,
    /// Optional model preferences.
    #[serde(rename = "modelPreferences", skip_serializing_if = "Option::is_none")]
    pub model_preferences: Option<ModelPreferences>,
    /// Optional system prompt.
    #[serde(rename = "systemPrompt", skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    /// Whether to include context from MCP servers.
    #[serde(rename = "includeContext", skip_serializing_if = "Option::is_none")]
    pub include_context: Option<String>,
    /// Maximum tokens to generate.
    #[serde(rename = "maxTokens")]
    pub max_tokens: i32,
    /// Sampling temperature.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    /// Stop sequences.
    #[serde(rename = "stopSequences", skip_serializing_if = "Option::is_none")]
    pub stop_sequences: Option<Vec<String>>,
    /// Additional metadata.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<HashMap<String, serde_json::Value>>,
}

/// Result of a create-message (sampling) request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateMessageResult {
    /// Role of the message (usually `"assistant"`).
    pub role: String,
    /// Generated message content.
    pub content: MessageContent,
    /// Model that was used.
    pub model: String,
    /// Stop reason for completion.
    #[serde(rename = "stopReason", skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<String>,
}

/// Message content wrapper for sampling results.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageContent {
    /// Content type (usually `"text"`).
    #[serde(rename = "type")]
    pub content_type: String,
    /// Text content.
    pub text: String,
}

/// Model preferences for sampling.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelPreferences {
    /// Model hints in preference order.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hints: Option<Vec<ModelHint>>,
    /// Cost priority (0.0–1.0; 1.0 prefers cheaper models).
    #[serde(rename = "costPriority", skip_serializing_if = "Option::is_none")]
    pub cost_priority: Option<f64>,
    /// Speed priority (0.0–1.0; 1.0 prefers faster models).
    #[serde(rename = "speedPriority", skip_serializing_if = "Option::is_none")]
    pub speed_priority: Option<f64>,
    /// Intelligence priority (0.0–1.0; 1.0 prefers more capable models).
    #[serde(
        rename = "intelligencePriority",
        skip_serializing_if = "Option::is_none"
    )]
    pub intelligence_priority: Option<f64>,
}

/// A hint for model selection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelHint {
    /// Model name (e.g. `"claude-3-5-sonnet"`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// A prompt message for the LLM.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptMessage {
    /// Role of the sender.
    pub role: String,
    /// Message content.
    pub content: Content,
}

impl PromptMessage {
    /// Create a user message.
    #[must_use]
    pub fn user(content: Content) -> Self {
        Self {
            role: "user".to_owned(),
            content,
        }
    }

    /// Create an assistant message.
    #[must_use]
    pub fn assistant(content: Content) -> Self {
        Self {
            role: "assistant".to_owned(),
            content,
        }
    }
}

/// Request for completion (auto-complete) suggestions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompleteRequest {
    /// Reference to the item being completed.
    #[serde(rename = "ref")]
    pub ref_: CompletionReference,
    /// The argument currently being completed.
    pub argument: ArgumentValue,
}

/// A reference to the completion context.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionReference {
    /// Type of reference.
    #[serde(rename = "type")]
    pub type_: String,
    /// Name of the tool/resource/prompt.
    pub name: String,
}

/// The argument value being completed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArgumentValue {
    /// Name of the argument.
    pub name: String,
    /// Current value being typed.
    pub value: String,
}

/// Result of a completion request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompleteResult {
    /// Completion suggestions.
    pub completion: Completion,
}

impl Default for CompleteResult {
    fn default() -> Self {
        Self {
            completion: Completion {
                values: vec![],
                total: Some(0),
                has_more: Some(false),
            },
        }
    }
}

/// A list of completion suggestions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Completion {
    /// Suggested completion values.
    pub values: Vec<String>,
    /// Total number of possible completions.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total: Option<usize>,
    /// Whether more completions are available.
    #[serde(rename = "hasMore", skip_serializing_if = "Option::is_none")]
    pub has_more: Option<bool>,
}

/// A root directory entry (MCP roots).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Root {
    /// URI of the root directory.
    pub uri: String,
    /// Human-readable name.
    pub name: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::protocol::PROTOCOL_VERSION;
    use serde_json::json;

    #[test]
    fn tool_serializes_input_schema_as_camel_case() {
        let tool = Tool {
            name: "test_tool".to_owned(),
            description: "A test".to_owned(),
            input_schema: json!({"type": "object"}),
            annotations: None,
        };
        let json = serde_json::to_value(&tool).expect("serialize"); // Safe: test assertion
        assert_eq!(json["name"], "test_tool");
        assert!(json.get("inputSchema").is_some());
        assert!(json.get("input_schema").is_none());
        assert!(json.get("annotations").is_none());
    }

    #[test]
    fn tool_call_arguments_default_to_none() {
        let raw = r#"{"name": "my_tool"}"#;
        let call: ToolCall = serde_json::from_str(raw).expect("deserialize"); // Safe: test assertion
        assert_eq!(call.name, "my_tool");
        assert!(call.arguments.is_none());
    }

    #[test]
    fn tool_response_text_is_not_error() {
        let resp = ToolResponse::text("hello".to_owned());
        assert!(!resp.is_error);
        assert_eq!(resp.content.len(), 1);
        assert_eq!(resp.content[0].as_text(), Some("hello"));
    }

    #[test]
    fn tool_response_error_sets_is_error() {
        let resp = ToolResponse::error("oops".to_owned());
        assert!(resp.is_error);
        let json = serde_json::to_value(&resp).expect("serialize"); // Safe: test assertion
        assert_eq!(json["isError"], true);
        assert_eq!(json["content"][0]["type"], "text");
        assert_eq!(json["content"][0]["text"], "oops");
    }

    #[test]
    fn initialize_response_serializes_camel_case() {
        let result = InitializeResponse::new(
            PROTOCOL_VERSION.to_owned(),
            ServerInfo::new("test", "0.1.0"),
            ServerCapabilities::tools_only(),
            None,
        );
        let json = serde_json::to_value(&result).expect("serialize"); // Safe: test assertion
        assert!(json.get("protocolVersion").is_some());
        assert!(json.get("serverInfo").is_some());
        assert_eq!(json["capabilities"]["tools"]["listChanged"], false);
        assert!(json.get("protocol_version").is_none());
    }

    #[test]
    fn initialize_request_deserializes_camel_case() {
        let raw = r#"{
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": { "name": "test-client", "version": "1.0" }
        }"#;
        let req: InitializeRequest = serde_json::from_str(raw).expect("deserialize"); // Safe: test assertion
        assert_eq!(req.protocol_version, "2025-11-25");
        assert_eq!(req.client_info.name, "test-client");
        assert_eq!(req.client_info.version, "1.0");
    }

    #[test]
    fn content_as_text_only_matches_text_variant() {
        let img = Content::Image {
            data: "AAAA".to_owned(),
            mime_type: "image/png".to_owned(),
        };
        assert!(img.as_text().is_none());
    }
}
