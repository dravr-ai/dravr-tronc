// ABOUTME: Generic MCP server that routes JSON-RPC requests to protocol handlers and tools
// ABOUTME: Implements initialize, tools/list, tools/call, and ping — parameterized over state S
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use std::sync::Arc;

use serde_json::Value;
use tokio::sync::RwLock;
use tracing::debug;

use crate::error::{
    INTERNAL_ERROR, INVALID_PARAMS, INVALID_REQUEST, METHOD_NOT_FOUND, PARSE_ERROR,
};
use crate::mcp::protocol::{
    CallToolParams, InitializeParams, InitializeResult, JsonRpcRequest, JsonRpcResponse,
    ServerCapabilities, ServerInfo, ToolsCapability, ToolsListResult, PROTOCOL_VERSION,
};
use crate::mcp::tool::ToolRegistry;

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
}

impl<S: Send + Sync + 'static> McpServer<S> {
    /// Create a server with the given name, version, tool registry, and shared state
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

    /// Route a parsed JSON-RPC request to the appropriate MCP handler
    ///
    /// Returns `None` for notifications (requests without an id).
    pub async fn handle_request(&self, request: JsonRpcRequest) -> Option<JsonRpcResponse> {
        // Validate JSON-RPC protocol version
        if request.jsonrpc != "2.0" {
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

        let response = match request.method.as_str() {
            "initialize" => self.handle_initialize(request.id, request.params),
            "tools/list" => self.handle_tools_list(request.id),
            "tools/call" => self.handle_tools_call(request.id, request.params).await,
            "ping" => JsonRpcResponse::success(request.id, Value::Object(serde_json::Map::new())),
            method => {
                debug!(method, "Unknown MCP method");
                JsonRpcResponse::error(
                    request.id,
                    METHOD_NOT_FOUND,
                    format!("Method not found: {method}"),
                )
            }
        };

        Some(response)
    }

    /// Handle `initialize` — parse client info and return server capabilities
    fn handle_initialize(&self, id: Option<Value>, params: Option<Value>) -> JsonRpcResponse {
        if let Some(params) = params {
            if let Ok(init) = serde_json::from_value::<InitializeParams>(params) {
                debug!(
                    client = %init.client_info.name,
                    version = ?init.client_info.version,
                    protocol = %init.protocol_version,
                    "MCP client connected"
                );
            }
        }

        let result = InitializeResult {
            protocol_version: PROTOCOL_VERSION.to_owned(),
            capabilities: ServerCapabilities {
                tools: Some(ToolsCapability {}),
            },
            server_info: ServerInfo {
                name: self.name.clone(),
                version: self.version.clone(),
            },
        };

        match serde_json::to_value(result) {
            Ok(val) => JsonRpcResponse::success(id, val),
            Err(e) => {
                JsonRpcResponse::error(id, INTERNAL_ERROR, format!("Serialization error: {e}"))
            }
        }
    }

    /// Handle `tools/list` — return all registered tool definitions
    fn handle_tools_list(&self, id: Option<Value>) -> JsonRpcResponse {
        let result = ToolsListResult {
            tools: self.tools.list_definitions(),
        };

        match serde_json::to_value(result) {
            Ok(val) => JsonRpcResponse::success(id, val),
            Err(e) => {
                JsonRpcResponse::error(id, INTERNAL_ERROR, format!("Serialization error: {e}"))
            }
        }
    }

    /// Handle `tools/call` — dispatch to the named tool handler
    async fn handle_tools_call(&self, id: Option<Value>, params: Option<Value>) -> JsonRpcResponse {
        let call_params: CallToolParams = match params {
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

        let arguments = call_params
            .arguments
            .unwrap_or_else(|| Value::Object(serde_json::Map::new()));

        let result = self
            .tools
            .execute(&call_params.name, &self.state, arguments)
            .await;

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
    use crate::mcp::protocol::{CallToolResult, ToolDefinition};
    use crate::mcp::tool::McpTool;
    use serde_json::json;

    struct TestState;

    struct PingTool;

    #[async_trait::async_trait]
    impl McpTool<TestState> for PingTool {
        fn definition(&self) -> ToolDefinition {
            ToolDefinition {
                name: "ping_tool".to_owned(),
                description: "Returns pong".to_owned(),
                input_schema: json!({"type": "object"}),
            }
        }

        async fn execute(
            &self,
            _state: &Arc<RwLock<TestState>>,
            _arguments: Value,
        ) -> CallToolResult {
            CallToolResult::text("pong".to_owned())
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
        assert_eq!(result["protocolVersion"], "2024-11-05");
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
}
