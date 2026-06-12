// ABOUTME: Generic MCP server that routes JSON-RPC requests to protocol handlers and tools
// ABOUTME: Implements initialize, tools/list, tools/call, and ping — parameterized over state S
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use std::sync::Arc;

use serde::Serialize;
use serde_json::Value;
use tokio::sync::RwLock;
use tracing::debug;

use crate::error::{
    INTERNAL_ERROR, INVALID_PARAMS, INVALID_REQUEST, METHOD_NOT_FOUND, PARSE_ERROR,
    UNSUPPORTED_PROTOCOL_VERSION,
};
use crate::mcp::modern::{
    DiscoverResult, ModernMeta, ModernRequestMeta, PROTOCOL_VERSION_2026_07_28,
};
use crate::mcp::protocol::{JsonRpcRequest, JsonRpcResponse, JSONRPC_VERSION, PROTOCOL_VERSION};
use crate::mcp::schema::{
    InitializeRequest, InitializeResponse, ServerCapabilities, ServerInfo, ToolCall,
};
use crate::mcp::tool::ToolRegistry;

/// The protocol revisions a default [`McpServer`] advertises, in preference
/// order: the modern stateless era first, then the current legacy revision.
fn default_supported_versions() -> Vec<String> {
    vec![
        PROTOCOL_VERSION_2026_07_28.to_owned(),
        PROTOCOL_VERSION.to_owned(),
    ]
}

/// MCP server that dispatches JSON-RPC requests to the appropriate handler
///
/// Generic over `S` — the project-specific server state type.
/// Owns the shared state and tool registry. Transport layers feed parsed
/// requests into `handle_request` and send the returned responses.
pub struct McpServer<S: Send + Sync> {
    name: String,
    version: String,
    state: Arc<RwLock<S>>,
    tools: ToolRegistry<S>,
    capabilities: ServerCapabilities,
    instructions: Option<String>,
    supported_versions: Vec<String>,
}

impl<S: Send + Sync + 'static> McpServer<S> {
    /// Create a server with the given name, version, tool registry, and shared state
    ///
    /// Defaults to advertising tool support only, no instructions, and the
    /// modern + current legacy protocol revisions. Use the `with_*` builders to
    /// override the advertised capabilities, instructions, or supported versions.
    pub fn new(
        name: impl Into<String>,
        version: impl Into<String>,
        tools: ToolRegistry<S>,
        state: Arc<RwLock<S>>,
    ) -> Self {
        Self {
            name: name.into(),
            version: version.into(),
            state,
            tools,
            capabilities: ServerCapabilities::tools_only(),
            instructions: None,
            supported_versions: default_supported_versions(),
        }
    }

    /// Override the capabilities advertised in `initialize` and `server/discover`.
    #[must_use]
    pub fn with_capabilities(mut self, capabilities: ServerCapabilities) -> Self {
        self.capabilities = capabilities;
        self
    }

    /// Set the natural-language instructions advertised to clients.
    #[must_use]
    pub fn with_instructions(mut self, instructions: impl Into<String>) -> Self {
        self.instructions = Some(instructions.into());
        self
    }

    /// Override the protocol revisions the server advertises and accepts, in
    /// preference order.
    #[must_use]
    pub fn with_supported_versions(mut self, versions: Vec<String>) -> Self {
        self.supported_versions = versions;
        self
    }

    /// Route a raw JSON string to the appropriate MCP handler
    ///
    /// Parses the string as a `JsonRpcRequest`, dispatches it, and returns
    /// the serialized response. Returns `None` for notifications.
    pub async fn handle_raw(&self, raw: &str) -> Option<JsonRpcResponse> {
        let request: JsonRpcRequest = match serde_json::from_str(raw) {
            Ok(req) => req,
            Err(e) => {
                return Some(JsonRpcResponse::error(
                    None,
                    PARSE_ERROR,
                    format!("Parse error: {e}"),
                ));
            }
        };
        self.handle_request(request).await
    }

    /// Route a parsed JSON-RPC request to the appropriate MCP handler
    ///
    /// Performs era detection on each request: one carrying modern per-request
    /// `_meta` (revision 2026-07-28) is served statelessly via
    /// [`Self::process_modern`]; otherwise it follows the legacy
    /// `initialize`/session path. Returns `None` for notifications (no id).
    pub async fn handle_request(&self, request: JsonRpcRequest) -> Option<JsonRpcResponse> {
        // Validate JSON-RPC protocol version
        if request.jsonrpc != JSONRPC_VERSION {
            return Some(JsonRpcResponse::error(
                request.id,
                INVALID_REQUEST,
                format!("Unsupported JSON-RPC version: {}", request.jsonrpc),
            ));
        }

        // Notifications have no id and expect no response
        if request.id.is_none() {
            debug!(method = %request.method, "Received notification, no response");
            return None;
        }

        // Era detection — see `mcp::modern` + the dual-era spec.
        let response = match ModernRequestMeta::from_params(request.params.as_ref()) {
            ModernMeta::Malformed(reason) => {
                JsonRpcResponse::error(request.id, INVALID_PARAMS, reason)
            }
            ModernMeta::Modern(meta) => self.process_modern(request, *meta).await,
            ModernMeta::Legacy => self.process_legacy(request).await,
        };

        Some(response)
    }

    /// Dispatch a legacy (`initialize`/session) request.
    async fn process_legacy(&self, request: JsonRpcRequest) -> JsonRpcResponse {
        match request.method.as_str() {
            "initialize" => self.handle_initialize(request.id, request.params.as_ref()),
            "tools/list" => self.handle_tools_list(request.id),
            "tools/call" => self.handle_tools_call(request.id, request.params).await,
            "server/discover" => self.handle_server_discover(request.id),
            "ping" => JsonRpcResponse::success(request.id, Value::Object(serde_json::Map::new())),
            method => {
                debug!(method, "Unknown MCP method");
                JsonRpcResponse::error(
                    request.id,
                    METHOD_NOT_FOUND,
                    format!("Method not found: {method}"),
                )
            }
        }
    }

    /// Dispatch a modern (2026-07-28, stateless per-request `_meta`) request.
    ///
    /// Rejects unsupported protocol versions with `UnsupportedProtocolVersionError`
    /// (-32004), routes the operation through the shared handlers, and frames a
    /// successful result with `resultType`. Legacy-only lifecycle methods
    /// (`initialize`, `ping`) are not valid here and fall through to
    /// method-not-found.
    async fn process_modern(
        &self,
        request: JsonRpcRequest,
        meta: ModernRequestMeta,
    ) -> JsonRpcResponse {
        if !self.supports_version(&meta.protocol_version) {
            return self.unsupported_version_error(request.id, &meta.protocol_version);
        }

        let response = match request.method.as_str() {
            "server/discover" => self.handle_server_discover(request.id),
            "tools/list" => self.handle_tools_list(request.id),
            "tools/call" => self.handle_tools_call(request.id, request.params).await,
            method => {
                debug!(method, "Unknown modern MCP method");
                JsonRpcResponse::error(
                    request.id,
                    METHOD_NOT_FOUND,
                    format!("Method not found: {method}"),
                )
            }
        };

        Self::frame_modern_result(response)
    }

    /// Whether the server advertises and accepts the given protocol revision.
    fn supports_version(&self, version: &str) -> bool {
        self.supported_versions.iter().any(|v| v == version)
    }

    /// Handle `initialize` — negotiate the protocol version and advertise the
    /// server's identity, capabilities, and instructions.
    ///
    /// Echoes the client's requested version when supported; otherwise responds
    /// with the server's current legacy revision (the spec lets the client then
    /// decide whether to proceed).
    fn handle_initialize(&self, id: Option<Value>, params: Option<&Value>) -> JsonRpcResponse {
        let init = params.and_then(|p| serde_json::from_value::<InitializeRequest>(p.clone()).ok());

        if let Some(req) = &init {
            debug!(
                client = %req.client_info.name,
                version = %req.client_info.version,
                protocol = %req.protocol_version,
                "MCP client connected"
            );
        }

        let negotiated_version = match &init {
            Some(req) if self.supports_version(&req.protocol_version) => {
                req.protocol_version.clone()
            }
            _ => PROTOCOL_VERSION.to_owned(),
        };

        let result = InitializeResponse::new(
            negotiated_version,
            ServerInfo::new(self.name.clone(), self.version.clone()),
            self.capabilities.clone(),
            self.instructions.clone(),
        );

        Self::success_or_error(id, &result)
    }

    /// Handle the modern `server/discover` RPC — advertise the supported
    /// protocol versions, capabilities, and identity. Answerable on either era
    /// and carries no session state.
    fn handle_server_discover(&self, id: Option<Value>) -> JsonRpcResponse {
        let discover = DiscoverResult::new(
            self.supported_versions.clone(),
            self.capabilities.clone(),
            ServerInfo::new(self.name.clone(), self.version.clone()),
            self.instructions.clone(),
        );
        Self::success_or_error(id, &discover)
    }

    /// Build an `UnsupportedProtocolVersionError` (-32004) listing the versions
    /// the server supports and echoing the requested one.
    fn unsupported_version_error(&self, id: Option<Value>, requested: &str) -> JsonRpcResponse {
        let data = serde_json::json!({
            "supported": self.supported_versions,
            "requested": requested,
        });
        JsonRpcResponse::error_with_data(
            id,
            UNSUPPORTED_PROTOCOL_VERSION,
            "Unsupported protocol version",
            data,
        )
    }

    /// Frame a modern response's successful result with a `resultType` (defaults
    /// to `"complete"`), as required by revision 2026-07-28. Error responses and
    /// non-object results pass through unchanged.
    fn frame_modern_result(mut response: JsonRpcResponse) -> JsonRpcResponse {
        if let Some(obj) = response.result.as_mut().and_then(Value::as_object_mut) {
            obj.entry("resultType")
                .or_insert_with(|| Value::String("complete".to_owned()));
        }
        response
    }

    /// Serialize a result value into a success response, or an internal error.
    fn success_or_error<T: Serialize>(id: Option<Value>, value: &T) -> JsonRpcResponse {
        match serde_json::to_value(value) {
            Ok(val) => JsonRpcResponse::success(id, val),
            Err(e) => {
                JsonRpcResponse::error(id, INTERNAL_ERROR, format!("Serialization error: {e}"))
            }
        }
    }

    /// Handle `tools/list` — return all registered tool definitions
    fn handle_tools_list(&self, id: Option<Value>) -> JsonRpcResponse {
        match serde_json::to_value(self.tools.list_definitions()) {
            Ok(tools) => {
                let mut result = serde_json::Map::new();
                result.insert("tools".to_owned(), tools);
                JsonRpcResponse::success(id, Value::Object(result))
            }
            Err(e) => {
                JsonRpcResponse::error(id, INTERNAL_ERROR, format!("Serialization error: {e}"))
            }
        }
    }

    /// Handle `tools/call` — dispatch to the named tool handler
    async fn handle_tools_call(&self, id: Option<Value>, params: Option<Value>) -> JsonRpcResponse {
        let call: ToolCall = match params {
            Some(p) => match serde_json::from_value(p) {
                Ok(cp) => cp,
                Err(e) => {
                    return JsonRpcResponse::error(
                        id,
                        INVALID_PARAMS,
                        format!("Invalid params: {e}"),
                    );
                }
            },
            None => {
                return JsonRpcResponse::error(
                    id,
                    INVALID_PARAMS,
                    "Missing params for tools/call".to_owned(),
                );
            }
        };

        let arguments = call
            .arguments
            .unwrap_or_else(|| Value::Object(serde_json::Map::new()));

        let result = self.tools.execute(&call.name, &self.state, arguments).await;

        match serde_json::to_value(result) {
            Ok(val) => JsonRpcResponse::success(id, val),
            Err(e) => JsonRpcResponse::error(
                id,
                INTERNAL_ERROR,
                format!("Result serialization error: {e}"),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::schema::{Tool, ToolResponse};
    use crate::mcp::tool::McpTool;
    use serde_json::json;

    struct TestState;

    struct PingTool;

    #[async_trait::async_trait]
    impl McpTool<TestState> for PingTool {
        fn definition(&self) -> Tool {
            Tool {
                name: "ping_tool".to_owned(),
                description: "Returns pong".to_owned(),
                input_schema: json!({"type": "object"}),
                annotations: None,
            }
        }

        async fn execute(
            &self,
            _state: &Arc<RwLock<TestState>>,
            _arguments: Value,
        ) -> ToolResponse {
            ToolResponse::text("pong".to_owned())
        }
    }

    fn make_server() -> McpServer<TestState> {
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(PingTool));
        let state = Arc::new(RwLock::new(TestState));
        McpServer::new("test-server", "0.1.0", registry, state)
    }

    #[tokio::test]
    async fn handle_initialize() {
        let server = make_server();
        let raw = r#"{
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": { "name": "test-client" }
            }
        }"#;
        let resp = server.handle_raw(raw).await.expect("response"); // Safe: test assertion
        let result = resp.result.expect("result"); // Safe: test assertion
        assert_eq!(result["protocolVersion"], PROTOCOL_VERSION);
        assert_eq!(result["serverInfo"]["name"], "test-server");
        assert_eq!(result["serverInfo"]["version"], "0.1.0");
    }

    #[tokio::test]
    async fn handle_initialize_without_params() {
        let server = make_server();
        let raw = r#"{"jsonrpc": "2.0", "id": 1, "method": "initialize"}"#;
        let resp = server.handle_raw(raw).await.expect("response"); // Safe: test assertion
        assert!(resp.result.is_some());
        assert!(resp.error.is_none());
    }

    #[tokio::test]
    async fn handle_tools_list() {
        let server = make_server();
        let raw = r#"{"jsonrpc": "2.0", "id": 2, "method": "tools/list"}"#;
        let resp = server.handle_raw(raw).await.expect("response"); // Safe: test assertion
        let result = resp.result.expect("result"); // Safe: test assertion
        let tools = result["tools"].as_array().expect("tools array"); // Safe: test assertion
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["name"], "ping_tool");
    }

    #[tokio::test]
    async fn handle_tools_call() {
        let server = make_server();
        let raw = r#"{
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": { "name": "ping_tool", "arguments": {} }
        }"#;
        let resp = server.handle_raw(raw).await.expect("response"); // Safe: test assertion
        let result = resp.result.expect("result"); // Safe: test assertion
        assert_eq!(result["content"][0]["text"], "pong");
    }

    #[tokio::test]
    async fn handle_tools_call_unknown_tool() {
        let server = make_server();
        let raw = r#"{
            "jsonrpc": "2.0",
            "id": 4,
            "method": "tools/call",
            "params": { "name": "nonexistent" }
        }"#;
        let resp = server.handle_raw(raw).await.expect("response"); // Safe: test assertion
        let result = resp.result.expect("result"); // Safe: test assertion
        assert_eq!(result["isError"], true);
        assert!(result["content"][0]["text"]
            .as_str()
            .expect("text") // Safe: test assertion
            .contains("Unknown tool"));
    }

    #[tokio::test]
    async fn handle_tools_call_missing_params() {
        let server = make_server();
        let raw = r#"{"jsonrpc": "2.0", "id": 5, "method": "tools/call"}"#;
        let resp = server.handle_raw(raw).await.expect("response"); // Safe: test assertion
        let err = resp.error.expect("error"); // Safe: test assertion
        assert_eq!(err.code, INVALID_PARAMS);
    }

    #[tokio::test]
    async fn handle_ping() {
        let server = make_server();
        let raw = r#"{"jsonrpc": "2.0", "id": 6, "method": "ping"}"#;
        let resp = server.handle_raw(raw).await.expect("response"); // Safe: test assertion
        assert!(resp.result.is_some());
        assert!(resp.error.is_none());
    }

    #[tokio::test]
    async fn handle_unknown_method() {
        let server = make_server();
        let raw = r#"{"jsonrpc": "2.0", "id": 7, "method": "bogus/method"}"#;
        let resp = server.handle_raw(raw).await.expect("response"); // Safe: test assertion
        let err = resp.error.expect("error"); // Safe: test assertion
        assert_eq!(err.code, METHOD_NOT_FOUND);
        assert!(err.message.contains("bogus/method"));
    }

    #[tokio::test]
    async fn handle_invalid_json() {
        let server = make_server();
        let resp = server
            .handle_raw("not json at all")
            .await
            .expect("response"); // Safe: test assertion
        let err = resp.error.expect("error"); // Safe: test assertion
        assert_eq!(err.code, PARSE_ERROR);
    }

    #[tokio::test]
    async fn handle_wrong_jsonrpc_version() {
        let server = make_server();
        let raw = r#"{"jsonrpc": "1.0", "id": 8, "method": "ping"}"#;
        let resp = server.handle_raw(raw).await.expect("response"); // Safe: test assertion
        let err = resp.error.expect("error"); // Safe: test assertion
        assert_eq!(err.code, INVALID_REQUEST);
    }

    #[tokio::test]
    async fn notification_returns_none() {
        let server = make_server();
        let raw = r#"{"jsonrpc": "2.0", "method": "notifications/cancelled"}"#;
        let resp = server.handle_raw(raw).await;
        assert!(resp.is_none());
    }

    #[tokio::test]
    async fn response_id_matches_request_id() {
        let server = make_server();
        let raw = r#"{"jsonrpc": "2.0", "id": 999, "method": "ping"}"#;
        let resp = server.handle_raw(raw).await.expect("response"); // Safe: test assertion
        assert_eq!(resp.id, Some(Value::from(999)));
    }

    #[tokio::test]
    async fn tools_call_with_no_arguments_defaults_to_empty_object() {
        let server = make_server();
        let raw = r#"{
            "jsonrpc": "2.0",
            "id": 10,
            "method": "tools/call",
            "params": { "name": "ping_tool" }
        }"#;
        let resp = server.handle_raw(raw).await.expect("response"); // Safe: test assertion
        let result = resp.result.expect("result"); // Safe: test assertion
        assert_eq!(result["content"][0]["text"], "pong");
    }

    #[tokio::test]
    async fn tools_call_with_invalid_params_structure() {
        let server = make_server();
        let raw = r#"{
            "jsonrpc": "2.0",
            "id": 11,
            "method": "tools/call",
            "params": "not an object"
        }"#;
        let resp = server.handle_raw(raw).await.expect("response"); // Safe: test assertion
        let err = resp.error.expect("error"); // Safe: test assertion
        assert_eq!(err.code, INVALID_PARAMS);
    }

    #[tokio::test]
    async fn server_discover_advertises_versions_and_tools() {
        let server = make_server();
        let raw = r#"{"jsonrpc": "2.0", "id": 20, "method": "server/discover"}"#;
        let resp = server.handle_raw(raw).await.expect("response"); // Safe: test assertion
        let result = resp.result.expect("result"); // Safe: test assertion
        assert_eq!(result["resultType"], "complete");
        let versions: Vec<&str> = result["supportedVersions"]
            .as_array()
            .expect("versions") // Safe: test assertion
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert!(versions.contains(&"2026-07-28"));
        assert!(versions.contains(&"2025-11-25"));
        assert!(result["capabilities"]["tools"].is_object());
        assert_eq!(result["serverInfo"]["name"], "test-server");
    }

    #[tokio::test]
    async fn initialize_echoes_supported_client_version() {
        let server = make_server();
        let raw = r#"{
            "jsonrpc": "2.0",
            "id": 21,
            "method": "initialize",
            "params": {
                "protocolVersion": "2026-07-28",
                "capabilities": {},
                "clientInfo": { "name": "c", "version": "1" }
            }
        }"#;
        let resp = server.handle_raw(raw).await.expect("response"); // Safe: test assertion
        let result = resp.result.expect("result"); // Safe: test assertion
        assert_eq!(result["protocolVersion"], "2026-07-28");
    }

    #[tokio::test]
    async fn modern_tools_list_frames_result_type() {
        let server = make_server();
        let raw = r#"{
            "jsonrpc": "2.0",
            "id": 22,
            "method": "tools/list",
            "params": {
                "_meta": {
                    "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                    "io.modelcontextprotocol/clientInfo": { "name": "c", "version": "1" },
                    "io.modelcontextprotocol/clientCapabilities": {}
                }
            }
        }"#;
        let resp = server.handle_raw(raw).await.expect("response"); // Safe: test assertion
        let result = resp.result.expect("result"); // Safe: test assertion
        assert_eq!(result["resultType"], "complete");
        assert_eq!(result["tools"][0]["name"], "ping_tool");
    }

    #[tokio::test]
    async fn modern_unsupported_version_returns_minus_32004() {
        let server = make_server();
        let raw = r#"{
            "jsonrpc": "2.0",
            "id": 23,
            "method": "tools/list",
            "params": {
                "_meta": {
                    "io.modelcontextprotocol/protocolVersion": "1999-01-01",
                    "io.modelcontextprotocol/clientInfo": { "name": "c", "version": "1" },
                    "io.modelcontextprotocol/clientCapabilities": {}
                }
            }
        }"#;
        let resp = server.handle_raw(raw).await.expect("response"); // Safe: test assertion
        let err = resp.error.expect("error"); // Safe: test assertion
        assert_eq!(err.code, UNSUPPORTED_PROTOCOL_VERSION);
        let data = err.data.expect("data"); // Safe: test assertion
        assert_eq!(data["requested"], "1999-01-01");
        assert!(data["supported"]
            .as_array()
            .expect("supported") // Safe: test assertion
            .iter()
            .any(|v| v == "2026-07-28"));
    }

    #[tokio::test]
    async fn modern_malformed_meta_returns_invalid_params() {
        let server = make_server();
        // protocolVersion present (modern) but clientInfo missing => malformed.
        let raw = r#"{
            "jsonrpc": "2.0",
            "id": 24,
            "method": "tools/list",
            "params": {
                "_meta": {
                    "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                    "io.modelcontextprotocol/clientCapabilities": {}
                }
            }
        }"#;
        let resp = server.handle_raw(raw).await.expect("response"); // Safe: test assertion
        let err = resp.error.expect("error"); // Safe: test assertion
        assert_eq!(err.code, INVALID_PARAMS);
    }
}
