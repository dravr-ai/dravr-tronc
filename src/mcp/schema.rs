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

use crate::mcp::protocol::{JsonRpcError, JsonRpcRequest, JsonRpcResponse};

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
