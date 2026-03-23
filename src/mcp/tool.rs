// ABOUTME: Generic McpTool trait and ToolRegistry for MCP tool discovery and dispatch
// ABOUTME: Parameterized over state type S so each project provides its own ServerState

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::RwLock;

use crate::mcp::protocol::{CallToolResult, ToolDefinition};

/// Trait implemented by each MCP tool exposed by a server
///
/// Generic over `S` — the project-specific server state type.
/// Each project defines its own `ServerState` and implements tools against it.
#[async_trait]
pub trait McpTool<S: Send + Sync>: Send + Sync {
    /// Return the tool's MCP definition (name, description, input schema)
    fn definition(&self) -> ToolDefinition;

    /// Execute the tool with the given arguments against the shared server state
    async fn execute(&self, state: &Arc<RwLock<S>>, arguments: Value) -> CallToolResult;
}

/// Registry mapping tool names to their handler implementations
///
/// Tools are registered at server startup and looked up by name
/// when `tools/call` requests arrive from the MCP client.
pub struct ToolRegistry<S: Send + Sync> {
    tools: HashMap<String, Box<dyn McpTool<S>>>,
}

impl<S: Send + Sync> Default for ToolRegistry<S> {
    fn default() -> Self {
        Self::new()
    }
}

impl<S: Send + Sync> ToolRegistry<S> {
    /// Create an empty registry
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
        }
    }

    /// Register a tool handler, keyed by its definition name
    pub fn register(&mut self, tool: Box<dyn McpTool<S>>) {
        let name = tool.definition().name;
        self.tools.insert(name, tool);
    }

    /// Return the number of registered tools
    pub fn len(&self) -> usize {
        self.tools.len()
    }

    /// Return true if no tools are registered
    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    /// List all registered tool definitions for `tools/list` responses
    pub fn list_definitions(&self) -> Vec<ToolDefinition> {
        self.tools.values().map(|t| t.definition()).collect()
    }

    /// Dispatch a `tools/call` to the named tool handler
    pub async fn execute(
        &self,
        name: &str,
        state: &Arc<RwLock<S>>,
        arguments: Value,
    ) -> CallToolResult {
        match self.tools.get(name) {
            Some(tool) => tool.execute(state, arguments).await,
            None => CallToolResult::error(format!("Unknown tool: {name}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct DummyState {
        counter: i32,
    }

    struct EchoTool;

    #[async_trait]
    impl McpTool<DummyState> for EchoTool {
        fn definition(&self) -> ToolDefinition {
            ToolDefinition {
                name: "echo".to_owned(),
                description: "Echoes the input".to_owned(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "message": { "type": "string" }
                    }
                }),
            }
        }

        async fn execute(
            &self,
            _state: &Arc<RwLock<DummyState>>,
            arguments: Value,
        ) -> CallToolResult {
            let msg = arguments
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("(empty)");
            CallToolResult::text(format!("echo: {msg}"))
        }
    }

    struct CounterTool;

    #[async_trait]
    impl McpTool<DummyState> for CounterTool {
        fn definition(&self) -> ToolDefinition {
            ToolDefinition {
                name: "counter".to_owned(),
                description: "Returns the counter value".to_owned(),
                input_schema: json!({"type": "object"}),
            }
        }

        async fn execute(
            &self,
            state: &Arc<RwLock<DummyState>>,
            _arguments: Value,
        ) -> CallToolResult {
            let guard = state.read().await;
            CallToolResult::text(format!("counter: {}", guard.counter))
        }
    }

    fn make_state() -> Arc<RwLock<DummyState>> {
        Arc::new(RwLock::new(DummyState { counter: 42 }))
    }

    #[test]
    fn empty_registry() {
        let registry = ToolRegistry::<DummyState>::new();
        assert!(registry.is_empty());
        assert_eq!(registry.len(), 0);
        assert!(registry.list_definitions().is_empty());
    }

    #[test]
    fn default_is_empty() {
        let registry = ToolRegistry::<DummyState>::default();
        assert!(registry.is_empty());
    }

    #[test]
    fn register_and_list() {
        let mut registry = ToolRegistry::<DummyState>::new();
        registry.register(Box::new(EchoTool));
        registry.register(Box::new(CounterTool));

        assert_eq!(registry.len(), 2);
        assert!(!registry.is_empty());

        let defs = registry.list_definitions();
        assert_eq!(defs.len(), 2);

        let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
        assert!(names.contains(&"echo"));
        assert!(names.contains(&"counter"));
    }

    #[test]
    fn register_replaces_duplicate_name() {
        let mut registry = ToolRegistry::<DummyState>::new();
        registry.register(Box::new(EchoTool));
        registry.register(Box::new(EchoTool));
        assert_eq!(registry.len(), 1);
    }

    #[tokio::test]
    async fn execute_known_tool() {
        let mut registry = ToolRegistry::<DummyState>::new();
        registry.register(Box::new(EchoTool));

        let state = make_state();
        let result = registry
            .execute("echo", &state, json!({"message": "hello"}))
            .await;
        assert!(result.is_error.is_none());
        assert_eq!(result.content[0].text, "echo: hello");
    }

    #[tokio::test]
    async fn execute_reads_state() {
        let mut registry = ToolRegistry::<DummyState>::new();
        registry.register(Box::new(CounterTool));

        let state = make_state();
        let result = registry.execute("counter", &state, json!({})).await;
        assert_eq!(result.content[0].text, "counter: 42");
    }

    #[tokio::test]
    async fn execute_unknown_tool_returns_error() {
        let registry = ToolRegistry::<DummyState>::new();
        let state = make_state();
        let result = registry.execute("nonexistent", &state, json!({})).await;
        assert_eq!(result.is_error, Some(true));
        assert!(result.content[0].text.contains("Unknown tool"));
    }

    #[test]
    fn tool_definitions_have_required_fields() {
        let tool = EchoTool;
        let def = tool.definition();
        assert!(!def.name.is_empty());
        assert!(!def.description.is_empty());
        assert!(def.input_schema.is_object());
    }
}
