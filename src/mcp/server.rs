// ABOUTME: Generic MCP server that routes JSON-RPC requests to protocol handlers and tools
// ABOUTME: Implements initialize, tools/list, tools/call, and ping — parameterized over state S
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use std::sync::Arc;

use serde::Serialize;
use serde_json::Value;
use tracing::debug;

use crate::error::{
    INTERNAL_ERROR, INVALID_PARAMS, INVALID_REQUEST, METHOD_NOT_FOUND, PARSE_ERROR,
    UNSUPPORTED_PROTOCOL_VERSION,
};
use crate::mcp::auth::{AuthError, AuthHook};
use crate::mcp::host::{MethodHandler, ToolDispatcher};
use crate::mcp::modern::{
    DiscoverResult, ModernMeta, ModernRequestMeta, PROTOCOL_VERSION_2026_07_28,
};
use crate::mcp::protocol::{JsonRpcRequest, JsonRpcResponse, JSONRPC_VERSION, PROTOCOL_VERSION};
use crate::mcp::schema::{
    InitializeRequest, InitializeResponse, ServerCapabilities, ServerInfo, ToolCall, ToolResponse,
};
use crate::mcp::tool::{ToolContext, ToolRegistry};

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
/// Generic over `S` — the project-specific server state type, shared as
/// `Arc<S>`. `S` is `?Sized`, so a host may parameterize it with a resource
/// façade trait object (`dyn HostRuntime`); a host needing interior mutability
/// parameterizes `S` with it (e.g. `RwLock<Inner>`).
/// Owns the shared state and tool registry. Transport layers feed parsed
/// requests into `handle_request` and send the returned responses.
pub struct McpServer<S: Send + Sync + ?Sized> {
    name: String,
    version: String,
    state: Arc<S>,
    tools: ToolRegistry<S>,
    capabilities: ServerCapabilities,
    instructions: Option<String>,
    supported_versions: Vec<String>,
    auth_hook: Option<Arc<dyn AuthHook<S>>>,
    allowed_origins: Vec<String>,
    tool_dispatcher: Option<Arc<dyn ToolDispatcher<S>>>,
    method_handler: Option<Arc<dyn MethodHandler<S>>>,
}

impl<S: Send + Sync + ?Sized + 'static> McpServer<S> {
    /// Create a server with the given name, version, tool registry, and shared state
    ///
    /// Defaults to advertising tool support only, no instructions, and the
    /// modern + current legacy protocol revisions. Use the `with_*` builders to
    /// override the advertised capabilities, instructions, or supported versions.
    pub fn new(
        name: impl Into<String>,
        version: impl Into<String>,
        tools: ToolRegistry<S>,
        state: Arc<S>,
    ) -> Self {
        Self {
            name: name.into(),
            version: version.into(),
            state,
            tools,
            capabilities: ServerCapabilities::tools_only(),
            instructions: None,
            supported_versions: default_supported_versions(),
            auth_hook: None,
            allowed_origins: Vec::new(),
            tool_dispatcher: None,
            method_handler: None,
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

    /// Install a host authentication hook for the HTTP transport. With a hook,
    /// the transport authenticates every request (rejecting with 401/403); with
    /// none, every request runs as the default anonymous context.
    #[must_use]
    pub fn with_auth_hook(mut self, auth_hook: Arc<dyn AuthHook<S>>) -> Self {
        self.auth_hook = Some(auth_hook);
        self
    }

    /// Restrict the `Origin`s the HTTP transport accepts. An empty list (the
    /// default) or one containing `"*"` allows any origin; a request whose
    /// `Origin` header is present and not listed is rejected with 403.
    #[must_use]
    pub fn with_allowed_origins(mut self, origins: Vec<String>) -> Self {
        self.allowed_origins = origins;
        self
    }

    /// The `Origin` allowlist the HTTP transport enforces.
    pub fn allowed_origins(&self) -> &[String] {
        &self.allowed_origins
    }

    /// Install a host [`ToolDispatcher`] that owns `tools/list` and `tools/call`
    /// (per-caller views, quota, execution, usage). When installed it replaces
    /// the built-in registry for both tool methods.
    #[must_use]
    pub fn with_tool_dispatcher(mut self, dispatcher: Arc<dyn ToolDispatcher<S>>) -> Self {
        self.tool_dispatcher = Some(dispatcher);
        self
    }

    /// Install a host [`MethodHandler`] for methods the engine doesn't natively
    /// serve (`resources/*`, `prompts/*`, `sampling/*`, …). Unknown methods are
    /// offered to it before falling through to method-not-found.
    #[must_use]
    pub fn with_method_handler(mut self, handler: Arc<dyn MethodHandler<S>>) -> Self {
        self.method_handler = Some(handler);
        self
    }

    /// Authenticate a request via the configured [`AuthHook`], or yield the
    /// default anonymous [`ToolContext`] when no hook is installed.
    ///
    /// # Errors
    /// Returns the hook's [`AuthError`] (401/403) when authentication fails.
    pub async fn authenticate(&self, request: &JsonRpcRequest) -> Result<ToolContext, AuthError> {
        match &self.auth_hook {
            Some(hook) => hook.authenticate(request, &self.state).await,
            None => Ok(ToolContext::default()),
        }
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

    /// Route a parsed JSON-RPC request under the default anonymous context.
    ///
    /// Convenience for transports without authentication (e.g. stdio). See
    /// [`Self::handle_request_with_context`] for the authenticated path.
    pub async fn handle_request(&self, request: JsonRpcRequest) -> Option<JsonRpcResponse> {
        self.handle_request_with_context(request, &ToolContext::default())
            .await
    }

    /// Route a parsed JSON-RPC request, dispatching tool calls under the given
    /// per-call [`ToolContext`] (resolved by the transport's auth hook).
    ///
    /// Performs era detection on each request: one carrying modern per-request
    /// `_meta` (revision 2026-07-28) is served statelessly via
    /// [`Self::process_modern`]; otherwise it follows the legacy
    /// `initialize`/session path. Returns `None` for notifications (no id).
    pub async fn handle_request_with_context(
        &self,
        request: JsonRpcRequest,
        ctx: &ToolContext,
    ) -> Option<JsonRpcResponse> {
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
            ModernMeta::Modern(meta) => self.process_modern(request, *meta, ctx).await,
            ModernMeta::Legacy => self.process_legacy(request, ctx).await,
        };

        Some(response)
    }

    /// Dispatch a legacy (`initialize`/session) request.
    async fn process_legacy(&self, request: JsonRpcRequest, ctx: &ToolContext) -> JsonRpcResponse {
        match request.method.as_str() {
            "initialize" => self.handle_initialize(request.id, request.params.as_ref()),
            "tools/list" => self.handle_tools_list(request.id, ctx).await,
            "tools/call" => {
                self.handle_tools_call(request.id, request.params, ctx)
                    .await
            }
            "server/discover" => self.handle_server_discover(request.id),
            "ping" => JsonRpcResponse::success(request.id, Value::Object(serde_json::Map::new())),
            other => {
                self.handle_unknown_method(other, request.id, request.params, ctx)
                    .await
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
        ctx: &ToolContext,
    ) -> JsonRpcResponse {
        if !self.supports_version(&meta.protocol_version) {
            return self.unsupported_version_error(request.id, &meta.protocol_version);
        }

        let response = match request.method.as_str() {
            "server/discover" => self.handle_server_discover(request.id),
            "tools/list" => self.handle_tools_list(request.id, ctx).await,
            "tools/call" => {
                self.handle_tools_call(request.id, request.params, ctx)
                    .await
            }
            other => {
                self.handle_unknown_method(other, request.id, request.params, ctx)
                    .await
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

    /// Offer a method the engine doesn't natively serve to the host
    /// [`MethodHandler`], falling through to method-not-found when there is no
    /// handler or it declines the method.
    async fn handle_unknown_method(
        &self,
        method: &str,
        id: Option<Value>,
        params: Option<Value>,
        ctx: &ToolContext,
    ) -> JsonRpcResponse {
        if let Some(handler) = &self.method_handler {
            if let Some(response) = handler
                .handle(method, id.clone(), params, &self.state, ctx)
                .await
            {
                return response;
            }
        }
        debug!(method, "Unknown MCP method");
        JsonRpcResponse::error(id, METHOD_NOT_FOUND, format!("Method not found: {method}"))
    }

    /// Handle `tools/list` — return the tool definitions visible to the caller.
    ///
    /// Uses the host [`ToolListProvider`] when installed (e.g. for tenant
    /// scoping); otherwise lists every registered tool.
    async fn handle_tools_list(&self, id: Option<Value>, ctx: &ToolContext) -> JsonRpcResponse {
        let definitions = match &self.tool_dispatcher {
            Some(dispatcher) => dispatcher.list_tools(&self.state, ctx).await,
            None => self.tools.list_definitions(),
        };
        match serde_json::to_value(definitions) {
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

    /// Handle `tools/call` — dispatch to the named tool handler under `ctx`
    async fn handle_tools_call(
        &self,
        id: Option<Value>,
        params: Option<Value>,
        ctx: &ToolContext,
    ) -> JsonRpcResponse {
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

        // A host dispatcher owns the whole call (quota, exec, usage); otherwise
        // execute against the built-in registry.
        let result = match &self.tool_dispatcher {
            Some(dispatcher) => {
                dispatcher
                    .call_tool(&call.name, &self.state, ctx, arguments)
                    .await
            }
            None => {
                self.tools
                    .execute(&call.name, &self.state, ctx, arguments)
                    .await
            }
        };

        Self::tool_response_result(id, &result)
    }

    /// Serialize a [`ToolResponse`] into a `tools/call` success response, or an
    /// internal error if serialization fails.
    fn tool_response_result(id: Option<Value>, result: &ToolResponse) -> JsonRpcResponse {
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
            _state: &Arc<TestState>,
            _ctx: &ToolContext,
            _arguments: Value,
        ) -> ToolResponse {
            ToolResponse::text("pong".to_owned())
        }
    }

    fn make_server() -> McpServer<TestState> {
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(PingTool));
        let state = Arc::new(TestState);
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

    // ---- Host-integration seams (host.rs) ----

    use crate::mcp::host::{MethodHandler, ToolDispatcher};

    /// Host dispatcher that owns both tool methods: a tenant-scoped `tools/list`
    /// view and a `tools/call` that routes entirely host-side (no registry).
    struct ScopedDispatcher;

    #[async_trait::async_trait]
    impl ToolDispatcher<TestState> for ScopedDispatcher {
        async fn list_tools(&self, _state: &Arc<TestState>, ctx: &ToolContext) -> Vec<Tool> {
            if ctx.tenant_id.is_some() {
                vec![Tool {
                    name: "scoped_tool".to_owned(),
                    description: "tenant-scoped".to_owned(),
                    input_schema: json!({"type": "object"}),
                    annotations: None,
                }]
            } else {
                Vec::new()
            }
        }

        async fn call_tool(
            &self,
            name: &str,
            _state: &Arc<TestState>,
            _ctx: &ToolContext,
            _arguments: Value,
        ) -> ToolResponse {
            match name {
                "scoped_tool" => ToolResponse::text("dispatched".to_owned()),
                other => ToolResponse::error(format!("quota exceeded for {other}")),
            }
        }
    }

    /// Serves `resources/list` and declines everything else (returns `None`).
    struct ResourcesMethodHandler;

    #[async_trait::async_trait]
    impl MethodHandler<TestState> for ResourcesMethodHandler {
        async fn handle(
            &self,
            method: &str,
            id: Option<Value>,
            _params: Option<Value>,
            _state: &Arc<TestState>,
            _ctx: &ToolContext,
        ) -> Option<JsonRpcResponse> {
            if method == "resources/list" {
                Some(JsonRpcResponse::success(id, json!({ "resources": [] })))
            } else {
                None
            }
        }
    }

    #[tokio::test]
    async fn dispatcher_list_empty_without_tenant() {
        let server = make_server().with_tool_dispatcher(Arc::new(ScopedDispatcher));
        // No tenant in the default context → dispatcher returns an empty list.
        let raw = r#"{"jsonrpc": "2.0", "id": 30, "method": "tools/list"}"#;
        let resp = server.handle_raw(raw).await.expect("response"); // Safe: test assertion
        let result = resp.result.expect("result"); // Safe: test assertion
        let tools = result["tools"].as_array().expect("tools array"); // Safe: test assertion
        assert!(tools.is_empty(), "no tenant → dispatcher yields no tools");
    }

    #[tokio::test]
    async fn dispatcher_list_scoped_with_tenant() {
        let server = make_server().with_tool_dispatcher(Arc::new(ScopedDispatcher));
        let request: JsonRpcRequest =
            serde_json::from_str(r#"{"jsonrpc": "2.0", "id": 31, "method": "tools/list"}"#)
                .expect("request"); // Safe: test assertion
        let ctx = ToolContext::new().with_tenant("tenant-1");
        let resp = server
            .handle_request_with_context(request, &ctx)
            .await
            .expect("response"); // Safe: test assertion
        let result = resp.result.expect("result"); // Safe: test assertion
        let tools = result["tools"].as_array().expect("tools array"); // Safe: test assertion
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["name"], "scoped_tool");
    }

    #[tokio::test]
    async fn method_handler_serves_non_tool_method() {
        let server = make_server().with_method_handler(Arc::new(ResourcesMethodHandler));
        let raw = r#"{"jsonrpc": "2.0", "id": 32, "method": "resources/list"}"#;
        let resp = server.handle_raw(raw).await.expect("response"); // Safe: test assertion
        let result = resp.result.expect("result"); // Safe: test assertion
        assert!(result["resources"].is_array());
        assert!(resp.error.is_none());
    }

    #[tokio::test]
    async fn method_handler_declines_falls_through_to_method_not_found() {
        let server = make_server().with_method_handler(Arc::new(ResourcesMethodHandler));
        let raw = r#"{"jsonrpc": "2.0", "id": 33, "method": "prompts/list"}"#;
        let resp = server.handle_raw(raw).await.expect("response"); // Safe: test assertion
        let err = resp.error.expect("error"); // Safe: test assertion
        assert_eq!(err.code, METHOD_NOT_FOUND);
        assert!(err.message.contains("prompts/list"));
    }

    #[tokio::test]
    async fn dispatcher_call_routes_host_side() {
        // With a dispatcher installed, tools/call bypasses the registry entirely
        // and runs the host's call_tool (here: echo for the known tool).
        let server = make_server().with_tool_dispatcher(Arc::new(ScopedDispatcher));
        let raw = r#"{
            "jsonrpc": "2.0",
            "id": 34,
            "method": "tools/call",
            "params": { "name": "scoped_tool", "arguments": {} }
        }"#;
        let resp = server.handle_raw(raw).await.expect("response"); // Safe: test assertion
        let result = resp.result.expect("result"); // Safe: test assertion
        assert_eq!(result["content"][0]["text"], "dispatched");
    }

    #[tokio::test]
    async fn dispatcher_call_reports_host_error() {
        // The dispatcher decides errors host-side (e.g. quota); even the registry's
        // own `ping_tool` is invisible to the dispatcher path.
        let server = make_server().with_tool_dispatcher(Arc::new(ScopedDispatcher));
        let raw = r#"{
            "jsonrpc": "2.0",
            "id": 35,
            "method": "tools/call",
            "params": { "name": "ping_tool", "arguments": {} }
        }"#;
        let resp = server.handle_raw(raw).await.expect("response"); // Safe: test assertion
        let result = resp.result.expect("result"); // Safe: test assertion
        assert_eq!(result["isError"], true);
        assert!(result["content"][0]["text"]
            .as_str()
            .expect("text") // Safe: test assertion
            .contains("quota exceeded"));
    }
}
