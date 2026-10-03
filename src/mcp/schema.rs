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

#[cfg(feature = "computation")]
use schemars::JsonSchema as DeriveJsonSchema;
use serde::de::Error as DeError;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::error::Error as StdError;
use std::fmt;

use crate::mcp::protocol::{JsonRpcError, JsonRpcRequest, JsonRpcResponse, JSONRPC_VERSION};

/// `notifications/progress` method string.
const METHOD_PROGRESS: &str = "notifications/progress";
/// `notifications/oauth_completed` method string.
const METHOD_OAUTH_COMPLETED: &str = "notifications/oauth_completed";
/// Key under [`ServerCapabilities::experimental`] carrying an [`OAuth2Capability`].
const EXPERIMENTAL_OAUTH2: &str = "oauth2";

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

/// Task-execution declaration for one tool (SEP-2663).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolExecution {
    /// Whether calls to this tool may be answered with a task handle.
    pub task_support: TaskSupport,
}

/// The three task-support levels a tool can declare.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TaskSupport {
    /// The tool never returns a task handle.
    Forbidden,
    /// The tool may return a handle to a declaring client, and answers
    /// inline otherwise.
    Optional,
    /// Every call to this tool is answered with a task handle (for a
    /// declaring client; a non-declaring client's call is refused).
    Required,
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
    /// JSON Schema describing the tool's structured output, when it declares
    /// one. A tool that sets this SHOULD return `structuredContent` matching it.
    #[serde(
        rename = "outputSchema",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub output_schema: Option<serde_json::Value>,
    /// Optional behavioral annotations (MCP 2025-11-25).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub annotations: Option<ToolAnnotations>,
    /// Task-execution declaration (SEP-2663): whether a call to this tool may
    /// be answered with a `resultType: "task"` handle. Absent means the tool
    /// never returns one — the safe reading for every pre-Tasks client.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution: Option<ToolExecution>,
}

/// Parameters for a `tools/call` request.
///
/// Built with [`Self::new`] and the `with_` methods, never as a literal: it
/// is `#[non_exhaustive]`, so a parameter a later revision adds to
/// `tools/call` is not a breaking change.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct ToolCall {
    /// Name of the tool to invoke.
    pub name: String,
    /// Tool arguments as JSON.
    #[serde(default)]
    pub arguments: Option<serde_json::Value>,
    /// The client's answers to the `inputRequests` of an earlier
    /// [`InputRequiredResult`], when this call retries one (SEP-2322), keyed
    /// like the requests they answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_responses: Option<serde_json::Map<String, serde_json::Value>>,
    /// The opaque `requestState` of an earlier [`InputRequiredResult`],
    /// echoed back by the client on retry (SEP-2322).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_state: Option<String>,
}

impl ToolCall {
    /// A call of the tool `name` with `arguments`.
    #[must_use]
    pub fn new(name: impl Into<String>, arguments: serde_json::Value) -> Self {
        Self {
            name: name.into(),
            arguments: Some(arguments),
            input_responses: None,
            request_state: None,
        }
    }

    /// This call retrying an [`InputRequiredResult`], carrying the client's
    /// answers to its `inputRequests`.
    #[must_use]
    pub fn with_input_responses(
        mut self,
        input_responses: serde_json::Map<String, serde_json::Value>,
    ) -> Self {
        self.input_responses = Some(input_responses);
        self
    }

    /// This call echoing the `requestState` of an [`InputRequiredResult`].
    #[must_use]
    pub fn with_request_state(mut self, request_state: impl Into<String>) -> Self {
        self.request_state = Some(request_state.into());
        self
    }
}

/// SEP-2322 `InputRequiredResult`: the server needs more from the client
/// before it can answer, and says so instead of a result.
///
/// The client fulfils `inputRequests` (elicitation, sampling, roots) and
/// retries the original request carrying `inputResponses` under the same keys,
/// plus `requestState` verbatim. Serializes flat beside
/// `resultType: "input_required"`. At least one of the two fields is always
/// present, which the constructors guarantee.
///
/// `requestState` round-trips through the client, which must treat it as
/// opaque but can change it: a server that puts anything it relies on there
/// authenticates it (a MAC, an AEAD) before trusting it on retry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InputRequiredResult {
    /// Always `"input_required"`.
    result_type: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    input_requests: Option<serde_json::Map<String, serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    request_state: Option<String>,
}

impl InputRequiredResult {
    /// Ask the client to fulfil `input_requests`, keyed by identifiers it
    /// echoes back in `inputResponses`.
    #[must_use]
    pub const fn new(input_requests: serde_json::Map<String, serde_json::Value>) -> Self {
        Self {
            result_type: "input_required",
            input_requests: Some(input_requests),
            request_state: None,
        }
    }

    /// Ask the client only to retry with `request_state` — no input needed,
    /// the shape a server shedding load uses to resume work later.
    #[must_use]
    pub fn retry_with_state(request_state: impl Into<String>) -> Self {
        Self {
            result_type: "input_required",
            input_requests: None,
            request_state: Some(request_state.into()),
        }
    }

    /// Attach state the client echoes back on retry.
    #[must_use]
    pub fn with_request_state(mut self, request_state: impl Into<String>) -> Self {
        self.request_state = Some(request_state.into());
        self
    }

    /// The outstanding requests, if any.
    #[must_use]
    pub const fn input_requests(&self) -> Option<&serde_json::Map<String, serde_json::Value>> {
        self.input_requests.as_ref()
    }

    /// The state the client echoes back, if any.
    #[must_use]
    pub fn request_state(&self) -> Option<&str> {
        self.request_state.as_deref()
    }
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
    /// Build a successful result of the given content blocks, in order.
    #[must_use]
    pub fn blocks(content: Vec<Content>) -> Self {
        Self {
            content,
            is_error: false,
            structured_content: None,
        }
    }

    /// Build a successful text result.
    #[must_use]
    pub fn text(content: String) -> Self {
        Self::blocks(vec![Content::text(content)])
    }

    /// Build a successful result of one image, from base64-encoded `data` of
    /// type `mime_type`.
    #[must_use]
    pub fn image(data: impl Into<String>, mime_type: impl Into<String>) -> Self {
        Self::blocks(vec![Content::image(data, mime_type)])
    }

    /// Build a successful result of one audio clip, from base64-encoded
    /// `data` of type `mime_type`.
    #[must_use]
    pub fn audio(data: impl Into<String>, mime_type: impl Into<String>) -> Self {
        Self::blocks(vec![Content::audio(data, mime_type)])
    }

    /// Build a successful result of one embedded resource.
    #[must_use]
    pub fn resource(contents: ResourceContents) -> Self {
        Self::blocks(vec![Content::resource(contents)])
    }

    /// Build a successful result of one resource link.
    #[must_use]
    pub fn resource_link(link: ResourceLink) -> Self {
        Self::blocks(vec![Content::resource_link(link)])
    }

    /// Build an error result carrying the given message.
    #[must_use]
    pub fn error(message: String) -> Self {
        Self {
            content: vec![Content::text(message)],
            is_error: true,
            structured_content: None,
        }
    }

    /// Build a successful result whose `structuredContent` is `value`, for a
    /// tool that declares an `outputSchema`.
    ///
    /// The same JSON is also the result's text block: the specification asks a
    /// tool returning structured content to serialise it into a text block as
    /// well, for a client that reads only `content`.
    ///
    /// The value is written once with `serde_json::to_string` and the
    /// structured form is read back from that text, never built with
    /// `serde_json::to_value`. `to_value` widens every `f32` to its nearest
    /// `f64`, so 12.8 would reach the structured reader as
    /// 12.800000190734863 while the text said 12.8; read back from the text,
    /// both forms carry the number at its own precision.
    ///
    /// # Errors
    ///
    /// [`StructuredContentError::Render`] when `value` cannot be serialised,
    /// and [`StructuredContentError::NotAnObject`] when it serialises to
    /// anything but a JSON object — `structuredContent` is an object in every
    /// revision of the specification, and so is the root of an
    /// `outputSchema`.
    pub fn structured<T: Serialize + ?Sized>(value: &T) -> Result<Self, StructuredContentError> {
        let text = serde_json::to_string(value).map_err(StructuredContentError::Render)?;
        let structured: serde_json::Value =
            serde_json::from_str(&text).map_err(StructuredContentError::Render)?;
        if !structured.is_object() {
            return Err(StructuredContentError::NotAnObject(json_kind(&structured)));
        }
        Ok(Self {
            content: vec![Content::text(text)],
            is_error: false,
            structured_content: Some(structured),
        })
    }
}

/// A list result, as the JSON object `{"items": [...]}`.
///
/// `structuredContent` and the root of an `outputSchema` are JSON objects in
/// every revision of the specification, so a tool whose result is a list
/// cannot return the bare array: [`ToolResponse::structured`] refuses it, and
/// a [`Computation`](crate::mcp::computation::Computation) whose `Output` is
/// a `Vec` would declare an array schema. Wrapping the list in `Listed` gives
/// every server the same shape for it — one `items` member — instead of each
/// naming its own wrapper.
///
/// With the `computation` feature it derives `schemars::JsonSchema`, so it
/// is a `Computation::Output` as it stands: `type Output = Listed<Row>;`,
/// and `Ok(rows.into())` or `.collect()` to build it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "computation", derive(DeriveJsonSchema))]
pub struct Listed<T> {
    /// The items, in order.
    pub items: Vec<T>,
}

impl<T> Listed<T> {
    /// A list result of `items`.
    #[must_use]
    pub const fn new(items: Vec<T>) -> Self {
        Self { items }
    }
}

impl<T> From<Vec<T>> for Listed<T> {
    fn from(items: Vec<T>) -> Self {
        Self::new(items)
    }
}

impl<T> FromIterator<T> for Listed<T> {
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        Self::new(iter.into_iter().collect())
    }
}

/// Why a value could not become a result's `structuredContent`.
#[derive(Debug)]
#[non_exhaustive]
pub enum StructuredContentError {
    /// The value's `Serialize` implementation failed.
    Render(serde_json::Error),
    /// The value serialised to this kind of JSON value, not an object.
    NotAnObject(&'static str),
}

impl fmt::Display for StructuredContentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Render(e) => write!(f, "could not serialise the result: {e}"),
            Self::NotAnObject(kind) => write!(
                f,
                "structured content must be a JSON object, but the result is {kind}"
            ),
        }
    }
}

impl StdError for StructuredContentError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            Self::Render(e) => Some(e),
            Self::NotAnObject(_) => None,
        }
    }
}

/// The JSON kind of `value`, with its article, for an error message.
const fn json_kind(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "a boolean",
        serde_json::Value::Number(_) => "a number",
        serde_json::Value::String(_) => "a string",
        serde_json::Value::Array(_) => "an array",
        serde_json::Value::Object(_) => "an object",
    }
}

/// A content block within a tool result or prompt message, in its spec shape:
/// the `ContentBlock` union of revisions 2025-11-25 and 2026-07-28, which
/// share it.
///
/// Every block carries optional [`Annotations`] and an optional `_meta`
/// object, written only when set. The constructors build a bare block;
/// [`Self::with_annotations`] adds the hints to any of them.
///
/// `#[non_exhaustive]`: the union grows with the specification (revision
/// 2025-11-25 adds tool-use and tool-result blocks to sampling), so a match
/// outside this crate keeps a wildcard arm, or reads a block through
/// [`Self::as_text`] and [`Self::annotations`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
#[non_exhaustive]
pub enum Content {
    /// Plain text (`"type": "text"`).
    #[serde(rename = "text")]
    Text(TextContent),
    /// A base64-encoded image (`"type": "image"`).
    #[serde(rename = "image")]
    Image(ImageContent),
    /// Base64-encoded audio (`"type": "audio"`).
    #[serde(rename = "audio")]
    Audio(AudioContent),
    /// A link to a resource the client may read (`"type": "resource_link"`).
    #[serde(rename = "resource_link")]
    ResourceLink(ResourceLink),
    /// A resource's contents carried in the block (`"type": "resource"`).
    #[serde(rename = "resource")]
    Resource(EmbeddedResource),
}

impl Content {
    /// A text block.
    #[must_use]
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text(TextContent {
            text: text.into(),
            annotations: None,
            meta: None,
        })
    }

    /// An image block from base64-encoded `data` of type `mime_type`.
    #[must_use]
    pub fn image(data: impl Into<String>, mime_type: impl Into<String>) -> Self {
        Self::Image(ImageContent {
            data: data.into(),
            mime_type: mime_type.into(),
            annotations: None,
            meta: None,
        })
    }

    /// An audio block from base64-encoded `data` of type `mime_type`.
    #[must_use]
    pub fn audio(data: impl Into<String>, mime_type: impl Into<String>) -> Self {
        Self::Audio(AudioContent {
            data: data.into(),
            mime_type: mime_type.into(),
            annotations: None,
            meta: None,
        })
    }

    /// An embedded-resource block carrying `contents`.
    #[must_use]
    pub fn resource(contents: ResourceContents) -> Self {
        Self::Resource(EmbeddedResource {
            resource: contents,
            annotations: None,
            meta: None,
        })
    }

    /// A resource-link block pointing at `link`.
    #[must_use]
    pub fn resource_link(link: ResourceLink) -> Self {
        Self::ResourceLink(link)
    }

    /// This block with its annotations set to `annotations`.
    #[must_use]
    pub fn with_annotations(mut self, annotations: Annotations) -> Self {
        let slot = match &mut self {
            Self::Text(block) => &mut block.annotations,
            Self::Image(block) => &mut block.annotations,
            Self::Audio(block) => &mut block.annotations,
            Self::ResourceLink(block) => &mut block.annotations,
            Self::Resource(block) => &mut block.annotations,
        };
        *slot = Some(annotations);
        self
    }

    /// The block's annotations, if it carries any.
    #[must_use]
    pub const fn annotations(&self) -> Option<&Annotations> {
        match self {
            Self::Text(block) => block.annotations.as_ref(),
            Self::Image(block) => block.annotations.as_ref(),
            Self::Audio(block) => block.annotations.as_ref(),
            Self::ResourceLink(block) => block.annotations.as_ref(),
            Self::Resource(block) => block.annotations.as_ref(),
        }
    }

    /// Borrow the inner string when this is a [`Content::Text`].
    #[must_use]
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text(block) => Some(&block.text),
            Self::Image(_) | Self::Audio(_) | Self::ResourceLink(_) | Self::Resource(_) => None,
        }
    }
}

/// The spec's `TextContent`: a text block.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TextContent {
    /// The text.
    pub text: String,
    /// Hints on how the client should use the block.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub annotations: Option<Annotations>,
    /// The block's `_meta` object.
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<serde_json::Map<String, serde_json::Value>>,
}

/// The spec's `ImageContent`: a base64-encoded image.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImageContent {
    /// The base64-encoded image data.
    pub data: String,
    /// The image's MIME type (e.g. `"image/png"`).
    #[serde(rename = "mimeType")]
    pub mime_type: String,
    /// Hints on how the client should use the block.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub annotations: Option<Annotations>,
    /// The block's `_meta` object.
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<serde_json::Map<String, serde_json::Value>>,
}

/// The spec's `AudioContent`: base64-encoded audio.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AudioContent {
    /// The base64-encoded audio data.
    pub data: String,
    /// The audio's MIME type (e.g. `"audio/wav"`).
    #[serde(rename = "mimeType")]
    pub mime_type: String,
    /// Hints on how the client should use the block.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub annotations: Option<Annotations>,
    /// The block's `_meta` object.
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<serde_json::Map<String, serde_json::Value>>,
}

/// The spec's `ResourceLink`: a resource the server can read, named by its
/// URI, which the client fetches with `resources/read` if it wants it.
///
/// It carries every field of the spec's `Resource`; a link need not appear
/// in `resources/list`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResourceLink {
    /// The resource's URI.
    pub uri: String,
    /// The resource's programmatic name.
    pub name: String,
    /// A human-readable display name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// What the resource is.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The resource's MIME type, if known.
    #[serde(rename = "mimeType", skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    /// The resource's raw size in bytes, before any base64 encoding, if known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    /// Icons a client may show for the resource.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icons: Option<Vec<Icon>>,
    /// Hints on how the client should use the block.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub annotations: Option<Annotations>,
    /// The block's `_meta` object.
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<serde_json::Map<String, serde_json::Value>>,
}

impl ResourceLink {
    /// A link to `uri` named `name`, with no optional field set.
    #[must_use]
    pub fn new(uri: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            uri: uri.into(),
            name: name.into(),
            title: None,
            description: None,
            mime_type: None,
            size: None,
            icons: None,
            annotations: None,
            meta: None,
        }
    }
}

/// The spec's `EmbeddedResource`: a resource's contents carried in the block.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EmbeddedResource {
    /// The resource's contents, text or binary.
    pub resource: ResourceContents,
    /// Hints on how the client should use the block.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub annotations: Option<Annotations>,
    /// The block's `_meta` object.
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<serde_json::Map<String, serde_json::Value>>,
}

/// The contents of a resource, as the spec's
/// `TextResourceContents | BlobResourceContents` union: the two carry no
/// tag, and are told apart by holding `text` or `blob`.
///
/// Contents holding both are refused rather than read as one kind with the
/// other field dropped, so a relayed block never loses its text or bytes.
///
/// `#[non_exhaustive]`, like [`Content`]: [`Self::uri`] and
/// [`Self::mime_type`] read what every kind carries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(untagged)]
#[non_exhaustive]
pub enum ResourceContents {
    /// Contents representable as text.
    Text(TextResourceContents),
    /// Binary contents, base64-encoded.
    Blob(BlobResourceContents),
}

/// Every field either kind of [`ResourceContents`] may carry, read before
/// the kind is chosen by which of `text` and `blob` is present.
#[derive(Deserialize)]
struct ResourceContentsWire {
    uri: String,
    #[serde(rename = "mimeType")]
    mime_type: Option<String>,
    text: Option<String>,
    blob: Option<String>,
    #[serde(rename = "_meta")]
    meta: Option<serde_json::Map<String, serde_json::Value>>,
}

impl<'de> Deserialize<'de> for ResourceContents {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = ResourceContentsWire::deserialize(deserializer)?;
        match (wire.text, wire.blob) {
            (Some(text), None) => Ok(Self::Text(TextResourceContents {
                uri: wire.uri,
                mime_type: wire.mime_type,
                text,
                meta: wire.meta,
            })),
            (None, Some(blob)) => Ok(Self::Blob(BlobResourceContents {
                uri: wire.uri,
                mime_type: wire.mime_type,
                blob,
                meta: wire.meta,
            })),
            (Some(_), Some(_)) => Err(DeError::custom(
                "resource contents hold both `text` and `blob`",
            )),
            (None, None) => Err(DeError::custom(
                "resource contents hold neither `text` nor `blob`",
            )),
        }
    }
}

impl ResourceContents {
    /// The text contents of the resource at `uri`.
    #[must_use]
    pub fn text(uri: impl Into<String>, text: impl Into<String>) -> Self {
        Self::Text(TextResourceContents {
            uri: uri.into(),
            mime_type: None,
            text: text.into(),
            meta: None,
        })
    }

    /// The binary contents of the resource at `uri`, from base64-encoded
    /// `blob`.
    #[must_use]
    pub fn blob(uri: impl Into<String>, blob: impl Into<String>) -> Self {
        Self::Blob(BlobResourceContents {
            uri: uri.into(),
            mime_type: None,
            blob: blob.into(),
            meta: None,
        })
    }

    /// These contents with their MIME type set to `mime_type`.
    #[must_use]
    pub fn with_mime_type(mut self, mime_type: impl Into<String>) -> Self {
        let slot = match &mut self {
            Self::Text(contents) => &mut contents.mime_type,
            Self::Blob(contents) => &mut contents.mime_type,
        };
        *slot = Some(mime_type.into());
        self
    }

    /// The URI of the resource these contents are from.
    #[must_use]
    pub fn uri(&self) -> &str {
        match self {
            Self::Text(contents) => &contents.uri,
            Self::Blob(contents) => &contents.uri,
        }
    }

    /// The contents' MIME type, if set.
    #[must_use]
    pub fn mime_type(&self) -> Option<&str> {
        match self {
            Self::Text(contents) => contents.mime_type.as_deref(),
            Self::Blob(contents) => contents.mime_type.as_deref(),
        }
    }
}

/// The spec's `TextResourceContents`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextResourceContents {
    /// The URI of the resource.
    pub uri: String,
    /// The MIME type of the resource, if known.
    #[serde(rename = "mimeType", skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    /// The resource's text.
    pub text: String,
    /// The contents' `_meta` object.
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<serde_json::Map<String, serde_json::Value>>,
}

/// The spec's `BlobResourceContents`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlobResourceContents {
    /// The URI of the resource.
    pub uri: String,
    /// The MIME type of the resource, if known.
    #[serde(rename = "mimeType", skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    /// The resource's bytes, base64-encoded.
    pub blob: String,
    /// The contents' `_meta` object.
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<serde_json::Map<String, serde_json::Value>>,
}

/// The spec's `Annotations`: hints a client reads to decide how to use or
/// show a block. Every field is optional and written only when set.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Annotations {
    /// Who the block is meant for; both roles when it is for both.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audience: Option<Vec<Role>>,
    /// How important the block is, from 0 (optional) to 1 (required).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<f64>,
    /// When the underlying data last changed, as an ISO 8601 timestamp
    /// (e.g. `"2025-01-12T15:00:58Z"`), kept as written.
    #[serde(rename = "lastModified", skip_serializing_if = "Option::is_none")]
    pub last_modified: Option<String>,
}

/// The spec's `Role`: the sender or recipient of a message or block.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// The user.
    User,
    /// The assistant.
    Assistant,
}

/// The spec's `Icon`: an image a client may show for an item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Icon {
    /// The icon's URI: an `https:` URL or a `data:` URI.
    pub src: String,
    /// The icon's MIME type, when `src` does not say.
    #[serde(rename = "mimeType", skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    /// The sizes the icon is available at (e.g. `["48x48"]`, `["any"]`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sizes: Option<Vec<String>>,
    /// The UI theme the icon is drawn for.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub theme: Option<IconTheme>,
}

/// The UI theme an [`Icon`] is drawn for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IconTheme {
    /// A light background.
    Light,
    /// A dark background.
    Dark,
}

/// MCP server capability declarations.
///
/// Exactly the keys the specification defines for a server (2025-06-18 schema
/// `ServerCapabilities`: `experimental`, `logging`, `completions`, `prompts`,
/// `resources`, `tools`) plus the revision `2026-07-28` `extensions` map. A
/// client negotiates only what it finds under these names, so anything else a
/// server wants to state goes under [`Self::experimental`] — which is where
/// [`Self::with_oauth2`] puts the OAuth 2.0 endpoints.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ServerCapabilities {
    /// Non-standard capabilities, keyed by name. The one place the
    /// specification lets a server advertise something it does not define.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub experimental: Option<HashMap<String, serde_json::Value>>,
    /// Server logging capability.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logging: Option<LoggingCapability>,
    /// Argument auto-completion: the server answers `completion/complete`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completions: Option<CompletionCapability>,
    /// Server prompts capability.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompts: Option<PromptsCapability>,
    /// Server resources capability.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resources: Option<ResourcesCapability>,
    /// Server tools capability.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<ToolsCapability>,
    /// Extension capabilities, keyed by reverse-DNS identifier (revision
    /// `2026-07-28`). An extension advertises support with an empty object,
    /// e.g. `{"io.modelcontextprotocol/tasks": {}}`.
    ///
    /// Distinct from [`Self::experimental`]: extensions are specified and
    /// negotiated, experimental capabilities are neither.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extensions: Option<HashMap<String, serde_json::Value>>,
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

    /// Advertise the server's OAuth 2.0 endpoints, under
    /// `experimental["oauth2"]`.
    ///
    /// MCP defines no capability for them — a client finds the authorization
    /// server through RFC 9728 protected-resource metadata — so they are an
    /// experimental capability, not a top-level key a conforming client would
    /// read as a negotiated one.
    #[must_use]
    pub fn with_oauth2(mut self, oauth2: OAuth2Capability) -> Self {
        self.experimental
            .get_or_insert_with(HashMap::new)
            .insert(EXPERIMENTAL_OAUTH2.to_owned(), oauth2.into_value());
        self
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

/// OAuth 2.0 endpoints a server advertises through
/// [`ServerCapabilities::with_oauth2`].
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

impl OAuth2Capability {
    /// The capability as the JSON object its serde derive writes.
    ///
    /// Built by hand because every field is a string, so the conversion
    /// cannot fail, and `serde_json::to_value` would still hand back a
    /// `Result` to discard.
    fn into_value(self) -> serde_json::Value {
        serde_json::json!({
            "discoveryUrl": self.discovery_url,
            "authorizationEndpoint": self.authorization_endpoint,
            "tokenEndpoint": self.token_endpoint,
            "registrationEndpoint": self.registration_endpoint,
        })
    }
}

/// Completion (auto-complete) capability, advertised as `completions`.
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

/// Sampling capability: the client can answer `sampling/createMessage`.
///
/// A client capability only. A server does not declare it.
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
    /// JSON Schema for the tool's structured output, when it declares one.
    ///
    /// Held as a document rather than as [`JsonSchema`], which models the
    /// subset a hand-written input schema needs. An output schema is
    /// typically derived from the result type — schemars writes
    /// `"type": ["string", "null"]` for an optional field, and `title` on
    /// every definition — so the typed form cannot round-trip one: it
    /// refuses the union outright and drops what it does not model. Nothing
    /// here inspects the document; it is carried to the client verbatim,
    /// which is also what [`Tool::output_schema`] does.
    #[serde(
        rename = "outputSchema",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub output_schema: Option<serde_json::Value>,
    /// Optional behavioral annotations (MCP 2025-11-25).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub annotations: Option<ToolAnnotations>,
    /// Task-execution declaration (SEP-2663): whether a call to this tool may
    /// be answered with a `resultType: "task"` handle. Absent means the tool
    /// never returns one — the safe reading for every pre-Tasks client.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution: Option<ToolExecution>,
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
            output_schema: None,
            annotations: None,
            execution: None,
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
            output_schema: None,
            annotations: Some(annotations),
            execution: None,
        }
    }

    /// Declare the schema of this tool's structured output.
    #[must_use]
    pub fn with_output_schema(mut self, output_schema: serde_json::Value) -> Self {
        self.output_schema = Some(output_schema);
        self
    }
}

/// A (typed) JSON Schema definition for tool inputs and outputs.
///
/// Revision `2026-07-28` requires tool schemas to be JSON Schema 2020-12, which
/// admits composition (`oneOf`/`anyOf`/`allOf`), references (`$ref`/`$defs`) and
/// the validation vocabulary — not just flat `properties`/`required`.
///
/// A schema that is *only* a reference or a composition carries no `type`; leave
/// [`Self::schema_type`] empty and it is omitted from the wire.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct JsonSchema {
    /// Dialect declaration, e.g.
    /// `"https://json-schema.org/draft/2020-12/schema"`.
    #[serde(rename = "$schema", default, skip_serializing_if = "Option::is_none")]
    pub schema_dialect: Option<String>,
    /// Schema type (e.g. `"object"`, `"string"`). Empty means "no `type`".
    #[serde(rename = "type", default, skip_serializing_if = "String::is_empty")]
    pub schema_type: String,
    /// Human-readable schema description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Property definitions for object schemas.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub properties: Option<BTreeMap<String, PropertySchema>>,
    /// Names of required properties.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required: Option<Vec<String>>,
    /// Whether properties beyond those declared are permitted.
    #[serde(
        rename = "additionalProperties",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub additional_properties: Option<bool>,
    /// Reusable subschemas referenced by `$ref`.
    #[serde(rename = "$defs", default, skip_serializing_if = "Option::is_none")]
    pub defs: Option<BTreeMap<String, PropertySchema>>,
    /// Exactly one of these subschemas must validate.
    #[serde(rename = "oneOf", default, skip_serializing_if = "Option::is_none")]
    pub one_of: Option<Vec<PropertySchema>>,
    /// At least one of these subschemas must validate.
    #[serde(rename = "anyOf", default, skip_serializing_if = "Option::is_none")]
    pub any_of: Option<Vec<PropertySchema>>,
    /// All of these subschemas must validate.
    #[serde(rename = "allOf", default, skip_serializing_if = "Option::is_none")]
    pub all_of: Option<Vec<PropertySchema>>,
}

/// A JSON Schema property definition (JSON Schema 2020-12 subset).
///
/// As with [`JsonSchema`], an empty [`Self::property_type`] omits `type`, which
/// is what a `$ref`-only or composition-only subschema needs.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PropertySchema {
    /// Property type (e.g. `"string"`, `"number"`, `"boolean"`). Empty means
    /// "no `type`".
    #[serde(rename = "type", default, skip_serializing_if = "String::is_empty")]
    pub property_type: String,
    /// Human-readable property description.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Item schema for array-type properties (JSON Schema `items`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub items: Option<Box<Self>>,
    /// Nested property definitions for object-type properties.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub properties: Option<BTreeMap<String, Self>>,
    /// Required fields for object-type properties.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required: Option<Vec<String>>,
    /// Whether properties beyond those declared are permitted.
    #[serde(
        rename = "additionalProperties",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub additional_properties: Option<bool>,
    /// Permitted values (JSON Schema `enum`).
    #[serde(rename = "enum", default, skip_serializing_if = "Option::is_none")]
    pub enum_values: Option<Vec<serde_json::Value>>,
    /// The single permitted value (JSON Schema `const`).
    #[serde(rename = "const", default, skip_serializing_if = "Option::is_none")]
    pub const_value: Option<serde_json::Value>,
    /// Default value advertised to clients.
    #[serde(rename = "default", default, skip_serializing_if = "Option::is_none")]
    pub default_value: Option<serde_json::Value>,
    /// Semantic format annotation (e.g. `"date-time"`, `"uri"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    /// Regular expression a string value must match.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
    /// Inclusive lower bound for numeric values.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minimum: Option<f64>,
    /// Inclusive upper bound for numeric values.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub maximum: Option<f64>,
    /// Minimum length of a string value.
    #[serde(rename = "minLength", default, skip_serializing_if = "Option::is_none")]
    pub min_length: Option<u64>,
    /// Maximum length of a string value.
    #[serde(rename = "maxLength", default, skip_serializing_if = "Option::is_none")]
    pub max_length: Option<u64>,
    /// Minimum number of array items.
    #[serde(rename = "minItems", default, skip_serializing_if = "Option::is_none")]
    pub min_items: Option<u64>,
    /// Maximum number of array items.
    #[serde(rename = "maxItems", default, skip_serializing_if = "Option::is_none")]
    pub max_items: Option<u64>,
    /// Reference to a subschema, typically into `$defs`.
    #[serde(rename = "$ref", default, skip_serializing_if = "Option::is_none")]
    pub ref_path: Option<String>,
    /// Exactly one of these subschemas must validate.
    #[serde(rename = "oneOf", default, skip_serializing_if = "Option::is_none")]
    pub one_of: Option<Vec<Self>>,
    /// At least one of these subschemas must validate.
    #[serde(rename = "anyOf", default, skip_serializing_if = "Option::is_none")]
    pub any_of: Option<Vec<Self>>,
    /// All of these subschemas must validate.
    #[serde(rename = "allOf", default, skip_serializing_if = "Option::is_none")]
    pub all_of: Option<Vec<Self>>,
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

/// The token a client puts in a request's `_meta.progressToken` to ask for
/// progress on it, echoed back in every `notifications/progress` about it.
///
/// The specification types it `string | number` and requires it unique across
/// the client's active requests; a server only ever echoes the value it was
/// given, so it keeps the variant it arrived as.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ProgressToken {
    /// A string token.
    String(String),
    /// An integer token.
    Integer(i64),
}

impl From<String> for ProgressToken {
    fn from(token: String) -> Self {
        Self::String(token)
    }
}

impl From<&str> for ProgressToken {
    fn from(token: &str) -> Self {
        Self::String(token.to_owned())
    }
}

impl From<i64> for ProgressToken {
    fn from(token: i64) -> Self {
        Self::Integer(token)
    }
}

/// Parameters for a progress notification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProgressParams {
    /// Token identifying the operation being tracked.
    #[serde(rename = "progressToken")]
    pub progress_token: ProgressToken,
    /// Current progress value.
    pub progress: f64,
    /// Total the progress counts toward, when known. Omitted when unknown:
    /// the spec types it as an optional number, and `null` is not one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total: Option<f64>,
    /// Optional human-readable progress message.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl ProgressNotification {
    /// Create a progress notification.
    #[must_use]
    pub fn new(
        progress_token: impl Into<ProgressToken>,
        progress: f64,
        total: Option<f64>,
        message: Option<String>,
    ) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_owned(),
            method: METHOD_PROGRESS.to_owned(),
            params: ProgressParams {
                progress_token: progress_token.into(),
                progress,
                total,
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

/// The client's answer to a `sampling/createMessage`: the message its model
/// produced.
///
/// `content` is a [`Content`] block, so an image or audio answer reads as one
/// rather than failing to parse, and its annotations and `_meta` survive. A
/// sampled message is text, image or audio in the specification; [`Content`]
/// also holds the resource blocks, which a client does not send here.
///
/// Read off the wire, never built by a server, so it is `#[non_exhaustive]`:
/// a field the specification adds to the result is not a breaking change.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct CreateMessageResult {
    /// Who sent the message — the assistant, for a model's answer.
    pub role: Role,
    /// The message the model produced.
    pub content: Content,
    /// The name of the model that produced it.
    pub model: String,
    /// Why the model stopped (`"endTurn"`, `"stopSequence"`, `"maxTokens"`,
    /// or the client's own reason).
    #[serde(rename = "stopReason", skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<String>,
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
    pub role: Role,
    /// Message content.
    pub content: Content,
}

impl PromptMessage {
    /// Create a user message.
    #[must_use]
    pub fn user(content: Content) -> Self {
        Self {
            role: Role::User,
            content,
        }
    }

    /// Create an assistant message.
    #[must_use]
    pub fn assistant(content: Content) -> Self {
        Self {
            role: Role::Assistant,
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
            output_schema: None,
            annotations: None,
            execution: None,
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

    fn oauth2() -> OAuth2Capability {
        OAuth2Capability {
            discovery_url: "https://auth.example/.well-known/oauth-authorization-server".to_owned(),
            authorization_endpoint: "https://auth.example/authorize".to_owned(),
            token_endpoint: "https://auth.example/token".to_owned(),
            registration_endpoint: "https://auth.example/register".to_owned(),
        }
    }

    /// A fully populated set writes only keys the specification defines for a
    /// server, and completion support under its spec name, `completions`.
    #[test]
    fn server_capabilities_write_only_spec_keys() {
        let caps = ServerCapabilities {
            experimental: Some(HashMap::new()),
            logging: Some(LoggingCapability {}),
            completions: Some(CompletionCapability {}),
            prompts: Some(PromptsCapability {
                list_changed: Some(false),
            }),
            resources: Some(ResourcesCapability {
                subscribe: Some(false),
                list_changed: Some(false),
            }),
            tools: Some(ToolsCapability {
                list_changed: Some(false),
            }),
            extensions: Some(HashMap::new()),
        }
        .with_oauth2(oauth2());
        let json = serde_json::to_value(&caps).expect("serialize"); // Safe: test assertion
        let mut keys: Vec<&str> = json
            .as_object()
            .expect("object") // Safe: test assertion
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            vec![
                "completions",
                "experimental",
                "extensions",
                "logging",
                "prompts",
                "resources",
                "tools"
            ]
        );
        assert_eq!(json["completions"], json!({}));
    }

    #[test]
    fn a_client_reading_spec_completions_sees_them() {
        let caps: ServerCapabilities =
            serde_json::from_value(json!({ "completions": {}, "tools": {} })).expect("deserialize"); // Safe: test assertion
        assert!(caps.completions.is_some());
    }

    #[test]
    fn oauth2_endpoints_travel_under_experimental() {
        let caps = ServerCapabilities::tools_only().with_oauth2(oauth2());
        let json = serde_json::to_value(&caps).expect("serialize"); // Safe: test assertion
        assert!(json.get("oauth2").is_none());
        assert!(json.get("auth").is_none());
        assert_eq!(
            json["experimental"]["oauth2"]["tokenEndpoint"],
            "https://auth.example/token"
        );
        // The hand-built object is exactly what the serde derive writes.
        assert_eq!(
            json["experimental"]["oauth2"],
            serde_json::to_value(oauth2()).expect("serialize") // Safe: test assertion
        );
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

    /// Blocks that are not spec content, or not in the spec's shape, are
    /// refused rather than read: `progress` is a notification, never a
    /// content type, and an embedded resource nests under `resource`.
    #[test]
    fn non_spec_content_blocks_are_refused() {
        let progress = json!({
            "type": "progress", "progressToken": "t", "progress": 1.0, "total": null
        });
        assert!(serde_json::from_value::<Content>(progress).is_err());
        let flattened_resource = json!({
            "type": "resource", "uri": "file:///a.txt", "text": "a", "mimeType": "text/plain"
        });
        assert!(serde_json::from_value::<Content>(flattened_resource).is_err());
    }

    #[test]
    fn progress_without_a_total_omits_it() {
        let json = serde_json::to_value(ProgressNotification::new(
            "tok-1".to_owned(),
            0.5,
            None,
            None,
        ))
        .expect("serialize"); // Safe: test assertion
        assert_eq!(json["method"], "notifications/progress");
        assert_eq!(
            json["params"],
            json!({ "progressToken": "tok-1", "progress": 0.5 })
        );
    }

    #[test]
    fn progress_with_a_total_carries_it() {
        let json = serde_json::to_value(ProgressNotification::new(
            "tok-2".to_owned(),
            3.0,
            Some(10.0),
            Some("reading activities".to_owned()),
        ))
        .expect("serialize"); // Safe: test assertion
        assert_eq!(
            json["params"],
            json!({
                "progressToken": "tok-2",
                "progress": 3.0,
                "total": 10.0,
                "message": "reading activities"
            })
        );
    }

    #[test]
    fn content_as_text_only_matches_text_variant() {
        let img = Content::image("AAAA", "image/png");
        assert!(img.as_text().is_none());
    }

    /// `wire` read as a [`Content`] and written back is `wire`, unchanged.
    fn assert_round_trip(wire: &serde_json::Value) -> Content {
        let block: Content = serde_json::from_value(wire.clone()).expect("deserialize"); // Safe: test assertion
        assert_eq!(&serde_json::to_value(&block).expect("serialize"), wire); // Safe: test assertion
        block
    }

    fn every_annotation() -> serde_json::Value {
        json!({
            "audience": ["user", "assistant"],
            "priority": 0.8,
            "lastModified": "2025-01-12T15:00:58Z"
        })
    }

    #[test]
    fn every_content_block_round_trips_in_its_spec_shape() {
        let blocks = [
            json!({ "type": "text", "text": "hi" }),
            json!({
                "type": "image", "data": "iVBORw0K", "mimeType": "image/png",
                "annotations": every_annotation(), "_meta": { "example.com/k": 1 }
            }),
            json!({ "type": "audio", "data": "UklGRg==", "mimeType": "audio/wav" }),
            json!({
                "type": "audio", "data": "UklGRg==", "mimeType": "audio/wav",
                "annotations": { "priority": 0.0 }
            }),
            json!({
                "type": "resource",
                "resource": { "uri": "file:///notes.txt", "mimeType": "text/plain", "text": "a" },
                "annotations": { "audience": ["user"] }
            }),
            json!({
                "type": "resource",
                "resource": { "uri": "file:///a.bin", "blob": "AAEC", "_meta": { "k": "v" } },
                "_meta": { "k": true }
            }),
            json!({ "type": "resource_link", "uri": "file:///a.rs", "name": "a.rs" }),
            json!({
                "type": "resource_link",
                "uri": "file:///project/src/main.rs",
                "name": "main.rs",
                "title": "Main entry point",
                "description": "Primary application entry point",
                "mimeType": "text/x-rust",
                "size": 2048,
                "icons": [
                    { "src": "https://example.com/rust.png", "mimeType": "image/png",
                      "sizes": ["48x48", "any"], "theme": "dark" },
                    { "src": "data:image/svg+xml;base64,PHN2Zz4=" }
                ],
                "annotations": every_annotation(),
                "_meta": { "example.com/k": [1, 2] }
            }),
        ];
        for wire in &blocks {
            assert_round_trip(wire);
        }
    }

    #[test]
    fn a_text_block_with_every_annotation_reads_into_typed_fields() {
        let wire = json!({ "type": "text", "text": "hi", "annotations": every_annotation() });
        let block = assert_round_trip(&wire);
        let annotations = block.annotations().expect("annotated"); // Safe: test assertion
        assert_eq!(
            annotations.audience.as_deref(),
            Some(&[Role::User, Role::Assistant][..])
        );
        assert_eq!(annotations.priority, Some(0.8));
        assert_eq!(
            annotations.last_modified.as_deref(),
            Some("2025-01-12T15:00:58Z")
        );
        assert_eq!(block.as_text(), Some("hi"));
    }

    #[test]
    fn constructors_write_the_spec_shape_and_nothing_unset() {
        let cases = [
            (Content::text("hi"), json!({ "type": "text", "text": "hi" })),
            (
                Content::image("iVBORw0K", "image/png"),
                json!({ "type": "image", "data": "iVBORw0K", "mimeType": "image/png" }),
            ),
            (
                Content::audio("UklGRg==", "audio/wav"),
                json!({ "type": "audio", "data": "UklGRg==", "mimeType": "audio/wav" }),
            ),
            (
                Content::resource(
                    ResourceContents::text("test://embedded", "body").with_mime_type("text/plain"),
                ),
                json!({
                    "type": "resource",
                    "resource": { "uri": "test://embedded", "mimeType": "text/plain", "text": "body" }
                }),
            ),
            (
                Content::resource(ResourceContents::blob("test://bin", "AAEC")),
                json!({ "type": "resource", "resource": { "uri": "test://bin", "blob": "AAEC" } }),
            ),
            (
                Content::resource_link(ResourceLink::new("file:///a.rs", "a.rs")),
                json!({ "type": "resource_link", "uri": "file:///a.rs", "name": "a.rs" }),
            ),
        ];
        for (block, wire) in cases {
            assert_eq!(serde_json::to_value(&block).expect("serialize"), wire); // Safe: test assertion
            assert_eq!(assert_round_trip(&wire), block);
        }
    }

    #[test]
    fn with_annotations_sets_the_hints_on_every_kind_of_block() {
        let hints = Annotations {
            audience: Some(vec![Role::Assistant]),
            priority: Some(1.0),
            last_modified: None,
        };
        let blocks = [
            Content::text("t"),
            Content::image("AA==", "image/png"),
            Content::audio("AA==", "audio/wav"),
            Content::resource(ResourceContents::blob("test://b", "AA==")),
            Content::resource_link(ResourceLink::new("test://l", "l")),
        ];
        for block in blocks {
            assert!(block.annotations().is_none());
            let annotated = block.with_annotations(hints.clone());
            assert_eq!(annotated.annotations(), Some(&hints));
            let wire = serde_json::to_value(&annotated).expect("serialize"); // Safe: test assertion
            assert_eq!(
                wire["annotations"],
                json!({ "audience": ["assistant"], "priority": 1.0 })
            );
        }
    }

    /// The two kinds of resource contents carry no tag: `text` makes one a
    /// text resource, `blob` a binary one, and neither is no resource.
    #[test]
    fn resource_contents_are_told_apart_by_text_or_blob() {
        let text: ResourceContents =
            serde_json::from_value(json!({ "uri": "test://t", "text": "a" })).expect("text"); // Safe: test assertion
        assert!(matches!(text, ResourceContents::Text(_)));
        assert_eq!(text.uri(), "test://t");
        assert_eq!(text.mime_type(), None);

        let blob: ResourceContents = serde_json::from_value(
            json!({ "uri": "test://b", "mimeType": "image/png", "blob": "AA==" }),
        )
        .expect("blob"); // Safe: test assertion
        assert!(matches!(blob, ResourceContents::Blob(_)));
        assert_eq!(blob.mime_type(), Some("image/png"));

        let neither = json!({ "type": "resource", "resource": { "uri": "test://n" } });
        assert!(serde_json::from_value::<Content>(neither).is_err());
    }

    /// Contents holding both `text` and `blob` are refused: read as text,
    /// they would come back out without their bytes.
    #[test]
    fn resource_contents_holding_text_and_blob_are_refused() {
        let both = json!({ "uri": "x://a", "text": "t", "blob": "AA==" });
        let refused =
            serde_json::from_value::<ResourceContents>(both.clone()).expect_err("both fields"); // Safe: test assertion
        assert!(refused.to_string().contains("both `text` and `blob`"));

        let block = json!({ "type": "resource", "resource": both });
        assert!(serde_json::from_value::<Content>(block).is_err());
    }

    /// Every field of either kind survives a read and a write back.
    #[test]
    fn resource_contents_round_trip_every_field() {
        for wire in [
            json!({ "uri": "x://t", "mimeType": "text/plain", "text": "t", "_meta": { "k": 1 } }),
            json!({ "uri": "x://b", "mimeType": "image/png", "blob": "AA==", "_meta": { "k": 2 } }),
        ] {
            let contents: ResourceContents =
                serde_json::from_value(wire.clone()).expect("contents"); // Safe: test assertion
            assert_eq!(serde_json::to_value(&contents).expect("json"), wire); // Safe: test assertion
        }
    }

    #[test]
    fn blocks_outside_the_spec_shape_are_refused() {
        let refused = [
            json!({ "type": "audio", "data": "AA==" }),
            json!({ "type": "image", "mimeType": "image/png" }),
            json!({ "type": "resource_link", "uri": "file:///a" }),
            json!({ "type": "resource_link", "name": "a" }),
            json!({ "type": "resource", "resource": { "text": "no uri" } }),
            json!({ "type": "text", "text": "a", "annotations": { "audience": ["system"] } }),
            json!({ "type": "video", "data": "AA==", "mimeType": "video/mp4" }),
        ];
        for wire in refused {
            assert!(
                serde_json::from_value::<Content>(wire.clone()).is_err(),
                "{wire} was read"
            );
        }
    }

    #[test]
    fn tool_response_constructors_hold_one_block_each() {
        let link = ResourceLink {
            mime_type: Some("text/x-rust".to_owned()),
            ..ResourceLink::new("file:///a.rs", "a.rs")
        };
        let cases = [
            (ToolResponse::image("AA==", "image/png"), "image"),
            (ToolResponse::audio("AA==", "audio/wav"), "audio"),
            (
                ToolResponse::resource(ResourceContents::text("test://r", "r")),
                "resource",
            ),
            (ToolResponse::resource_link(link), "resource_link"),
        ];
        for (response, kind) in cases {
            assert!(!response.is_error);
            let wire = serde_json::to_value(&response).expect("serialize"); // Safe: test assertion
            assert_eq!(wire["content"].as_array().map(Vec::len), Some(1));
            assert_eq!(wire["content"][0]["type"], kind);
            assert!(wire.get("structuredContent").is_none());
        }
    }

    #[test]
    fn tool_response_blocks_keeps_their_order() {
        let response = ToolResponse::blocks(vec![
            Content::text("first"),
            Content::image("AA==", "image/png"),
            Content::resource(
                ResourceContents::text("test://r", "{}").with_mime_type("application/json"),
            ),
        ]);
        let wire = serde_json::to_value(&response).expect("serialize"); // Safe: test assertion
        let kinds: Vec<&str> = wire["content"]
            .as_array()
            .expect("content array") // Safe: test assertion
            .iter()
            .filter_map(|block| block["type"].as_str())
            .collect();
        assert_eq!(kinds, ["text", "image", "resource"]);
        assert_eq!(wire["isError"], false);
    }

    #[test]
    fn prompt_message_roles_are_the_spec_role_strings() {
        let user =
            serde_json::to_value(PromptMessage::user(Content::text("q"))).expect("serialize"); // Safe: test assertion
        assert_eq!(
            user,
            json!({ "role": "user", "content": { "type": "text", "text": "q" } })
        );
        let assistant: PromptMessage = serde_json::from_value(
            json!({ "role": "assistant", "content": { "type": "audio", "data": "AA==", "mimeType": "audio/wav" } }),
        )
        .expect("deserialize"); // Safe: test assertion
        assert_eq!(assistant.role, Role::Assistant);
    }

    #[test]
    fn a_tool_call_is_built_without_a_literal_and_writes_the_wire_names() {
        let first =
            serde_json::to_value(ToolCall::new("confirm", json!({ "x": 1 }))).expect("serialize"); // Safe: test assertion
        assert_eq!(first, json!({ "name": "confirm", "arguments": { "x": 1 } }));

        let mut answers = serde_json::Map::new();
        answers.insert("ok".to_owned(), json!({ "action": "accept" }));
        let retry = ToolCall::new("confirm", json!({}))
            .with_input_responses(answers)
            .with_request_state("s1");
        let wire = serde_json::to_value(&retry).expect("serialize"); // Safe: test assertion
        assert_eq!(wire["inputResponses"]["ok"]["action"], "accept");
        assert_eq!(wire["requestState"], "s1");
        let read: ToolCall = serde_json::from_value(wire).expect("deserialize"); // Safe: test assertion
        assert_eq!(read.request_state.as_deref(), Some("s1"));

        let empty = ToolCall::default();
        assert!(empty.name.is_empty() && empty.arguments.is_none());
    }

    #[test]
    fn a_sampled_message_is_a_content_block_under_a_spec_role() {
        let text: CreateMessageResult = serde_json::from_value(json!({
            "role": "assistant",
            "content": { "type": "text", "text": "hi" },
            "model": "m",
            "stopReason": "endTurn"
        }))
        .expect("deserialize"); // Safe: test assertion
        assert_eq!(text.role, Role::Assistant);
        assert_eq!(text.content.as_text(), Some("hi"));
        assert_eq!(text.stop_reason.as_deref(), Some("endTurn"));

        let image: CreateMessageResult = serde_json::from_value(json!({
            "role": "assistant",
            "content": { "type": "image", "data": "iVBO", "mimeType": "image/png" },
            "model": "m"
        }))
        .expect("an image answer parses"); // Safe: test assertion
        assert_eq!(image.content, Content::image("iVBO", "image/png"));

        let unknown_role = serde_json::from_value::<CreateMessageResult>(json!({
            "role": "system",
            "content": { "type": "text", "text": "hi" },
            "model": "m"
        }));
        assert!(unknown_role.is_err(), "a role is user or assistant");
    }
}
