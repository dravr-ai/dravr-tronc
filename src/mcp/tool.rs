// ABOUTME: Generic McpTool trait + ToolRegistry, with a per-call ToolContext and capability gating
// ABOUTME: Parameterized over state type S so each project provides its own ServerState
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use bitflags::bitflags;
use serde_json::Value;

use crate::mcp::schema::{Tool, ToolResponse};

bitflags! {
    /// Host-agnostic classification flags a tool declares for discovery + gating.
    ///
    /// These are the generic capabilities the registry and transports reason
    /// about (auth/tenant/provider requirements, read vs. write, admin gating).
    /// Domain taxonomy (e.g. fitness "goals"/"recipes" groupings) belongs in the
    /// registry's string categories, not here.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct ToolCapabilities: u16 {
        /// The tool requires an authenticated caller.
        const REQUIRES_AUTH = 0b0000_0001;
        /// The tool requires a resolved tenant context.
        const REQUIRES_TENANT = 0b0000_0010;
        /// The tool requires a connected upstream provider.
        const REQUIRES_PROVIDER = 0b0000_0100;
        /// The tool only reads data (no side effects).
        const READS_DATA = 0b0000_1000;
        /// The tool writes or mutates data.
        const WRITES_DATA = 0b0001_0000;
        /// The tool may only be invoked by an admin caller.
        const ADMIN_ONLY = 0b0010_0000;
    }
}

/// Per-call context threaded into a tool's `execute`.
///
/// Carries the request-scoped identity the host resolved (caller, tenant, how
/// they authenticated, the request id) plus the precomputed admin flag the
/// registry uses to gate `ADMIN_ONLY` tools. All identity fields are optional
/// and host-agnostic (ids as strings) so a server without users/tenants — or a
/// stdio server with no auth — can pass [`ToolContext::default`].
#[derive(Debug, Clone, Default)]
pub struct ToolContext {
    /// Authenticated caller id, if any (host-defined; e.g. a UUID string).
    pub user_id: Option<String>,
    /// Resolved tenant id, if any.
    pub tenant_id: Option<String>,
    /// How the caller authenticated (host-defined label, e.g. `"jwt_bearer"`).
    pub auth_method: Option<String>,
    /// Correlation id for tracing/logging.
    pub request_id: Option<Value>,
    /// Whether the caller holds admin privileges (resolved by the host).
    pub is_admin: bool,
}

impl ToolContext {
    /// An empty context — no identity, not admin. Equivalent to [`Self::default`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the authenticated caller id.
    #[must_use]
    pub fn with_user(mut self, user_id: impl Into<String>) -> Self {
        self.user_id = Some(user_id.into());
        self
    }

    /// Set the resolved tenant id.
    #[must_use]
    pub fn with_tenant(mut self, tenant_id: impl Into<String>) -> Self {
        self.tenant_id = Some(tenant_id.into());
        self
    }

    /// Set the authentication method label.
    #[must_use]
    pub fn with_auth_method(mut self, auth_method: impl Into<String>) -> Self {
        self.auth_method = Some(auth_method.into());
        self
    }

    /// Set the request correlation id.
    #[must_use]
    pub fn with_request_id(mut self, request_id: Value) -> Self {
        self.request_id = Some(request_id);
        self
    }

    /// Mark whether the caller holds admin privileges.
    #[must_use]
    pub const fn as_admin(mut self, is_admin: bool) -> Self {
        self.is_admin = is_admin;
        self
    }
}

/// Trait implemented by each MCP tool exposed by a server
///
/// Generic over `S` — the project-specific server state type, shared as
/// `Arc<S>`. `S` is `?Sized`, so a host may parameterize it with an unsized
/// trait object (e.g. a resource façade `dyn HostRuntime`) rather than a
/// concrete struct. The shared state is handed to `execute` immutably; a host
/// that needs interior mutability parameterizes `S` with it (e.g.
/// `S = RwLock<Inner>`, yielding `Arc<RwLock<Inner>>`).
#[async_trait]
pub trait McpTool<S: Send + Sync + ?Sized>: Send + Sync {
    /// Return the tool's MCP definition (name, description, input schema)
    fn definition(&self) -> Tool;

    /// Declare the tool's host-agnostic capabilities (auth/tenant/admin/...).
    ///
    /// Defaults to no capabilities. The registry uses [`ToolCapabilities::ADMIN_ONLY`]
    /// to gate execution; transports may use the rest for discovery filtering.
    fn capabilities(&self) -> ToolCapabilities {
        ToolCapabilities::empty()
    }

    /// Execute the tool against the shared server state and per-call context
    async fn execute(&self, state: &Arc<S>, ctx: &ToolContext, arguments: Value) -> ToolResponse;
}

/// Registry mapping tool names to their handler implementations
///
/// Tools are registered at server startup and looked up by name
/// when `tools/call` requests arrive from the MCP client.
pub struct ToolRegistry<S: Send + Sync + ?Sized> {
    tools: HashMap<String, Box<dyn McpTool<S>>>,
    categories: HashMap<String, Vec<String>>,
}

impl<S: Send + Sync + ?Sized> Default for ToolRegistry<S> {
    fn default() -> Self {
        Self::new()
    }
}

impl<S: Send + Sync + ?Sized> ToolRegistry<S> {
    /// Create an empty registry
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
            categories: HashMap::new(),
        }
    }

    /// Register a tool handler, keyed by its definition name
    pub fn register(&mut self, tool: Box<dyn McpTool<S>>) {
        let name = tool.definition().name;
        self.tools.insert(name, tool);
    }

    /// Register a tool handler and record it under the given category
    pub fn register_with_category(&mut self, tool: Box<dyn McpTool<S>>, category: &str) {
        let name = tool.definition().name;
        self.categories
            .entry(category.to_owned())
            .or_default()
            .push(name.clone());
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
    pub fn list_definitions(&self) -> Vec<Tool> {
        self.tools.values().map(|t| t.definition()).collect()
    }

    /// List tool definitions visible to a non-admin caller (excludes
    /// `ADMIN_ONLY` tools). Pass `is_admin = true` to include everything.
    pub fn list_definitions_for(&self, is_admin: bool) -> Vec<Tool> {
        self.tools
            .values()
            .filter(|t| is_admin || !t.capabilities().contains(ToolCapabilities::ADMIN_ONLY))
            .map(|t| t.definition())
            .collect()
    }

    /// Look up a registered tool's declared capabilities.
    pub fn capabilities_of(&self, name: &str) -> Option<ToolCapabilities> {
        self.tools.get(name).map(|t| t.capabilities())
    }

    /// Names of the categories tools have been registered under.
    pub fn categories(&self) -> Vec<&str> {
        self.categories.keys().map(String::as_str).collect()
    }

    /// Tool names registered under the given category.
    pub fn tools_in_category(&self, category: &str) -> Vec<&str> {
        self.categories
            .get(category)
            .map(|names| names.iter().map(String::as_str).collect())
            .unwrap_or_default()
    }

    /// Dispatch a `tools/call` to the named tool handler
    ///
    /// Gates `ADMIN_ONLY` tools on `ctx.is_admin` before dispatching.
    pub async fn execute(
        &self,
        name: &str,
        state: &Arc<S>,
        ctx: &ToolContext,
        arguments: Value,
    ) -> ToolResponse {
        match self.tools.get(name) {
            Some(tool) => {
                if tool.capabilities().contains(ToolCapabilities::ADMIN_ONLY) && !ctx.is_admin {
                    return ToolResponse::error(format!("Tool '{name}' requires admin privileges"));
                }
                tool.execute(state, ctx, arguments).await
            }
            None => ToolResponse::error(format!("Unknown tool: {name}")),
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
        fn definition(&self) -> Tool {
            Tool {
                name: "echo".to_owned(),
                description: "Echoes the input".to_owned(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "message": { "type": "string" }
                    }
                }),
                annotations: None,
            }
        }

        fn capabilities(&self) -> ToolCapabilities {
            ToolCapabilities::READS_DATA
        }

        async fn execute(
            &self,
            _state: &Arc<DummyState>,
            _ctx: &ToolContext,
            arguments: Value,
        ) -> ToolResponse {
            let msg = arguments
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("(empty)");
            ToolResponse::text(format!("echo: {msg}"))
        }
    }

    struct CounterTool;

    #[async_trait]
    impl McpTool<DummyState> for CounterTool {
        fn definition(&self) -> Tool {
            Tool {
                name: "counter".to_owned(),
                description: "Returns the counter value".to_owned(),
                input_schema: json!({"type": "object"}),
                annotations: None,
            }
        }

        async fn execute(
            &self,
            state: &Arc<DummyState>,
            _ctx: &ToolContext,
            _arguments: Value,
        ) -> ToolResponse {
            ToolResponse::text(format!("counter: {}", state.counter))
        }
    }

    struct AdminTool;

    #[async_trait]
    impl McpTool<DummyState> for AdminTool {
        fn definition(&self) -> Tool {
            Tool {
                name: "admin_reset".to_owned(),
                description: "Admin-only reset".to_owned(),
                input_schema: json!({"type": "object"}),
                annotations: None,
            }
        }

        fn capabilities(&self) -> ToolCapabilities {
            ToolCapabilities::ADMIN_ONLY | ToolCapabilities::WRITES_DATA
        }

        async fn execute(
            &self,
            _state: &Arc<DummyState>,
            _ctx: &ToolContext,
            _arguments: Value,
        ) -> ToolResponse {
            ToolResponse::text("reset".to_owned())
        }
    }

    fn make_state() -> Arc<DummyState> {
        Arc::new(DummyState { counter: 42 })
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

    #[test]
    fn register_with_category_tracks_membership() {
        let mut registry = ToolRegistry::<DummyState>::new();
        registry.register_with_category(Box::new(EchoTool), "data");
        registry.register_with_category(Box::new(CounterTool), "data");

        assert!(registry.categories().contains(&"data"));
        let mut in_data = registry.tools_in_category("data");
        in_data.sort_unstable();
        assert_eq!(in_data, vec!["counter", "echo"]);
        assert!(registry.tools_in_category("missing").is_empty());
    }

    #[test]
    fn capabilities_are_reported() {
        let mut registry = ToolRegistry::<DummyState>::new();
        registry.register(Box::new(EchoTool));
        assert_eq!(
            registry.capabilities_of("echo"),
            Some(ToolCapabilities::READS_DATA)
        );
        assert!(registry.capabilities_of("missing").is_none());
    }

    #[test]
    fn admin_only_tools_hidden_from_non_admins() {
        let mut registry = ToolRegistry::<DummyState>::new();
        registry.register(Box::new(EchoTool));
        registry.register(Box::new(AdminTool));

        let user_visible = registry.list_definitions_for(false);
        assert_eq!(user_visible.len(), 1);
        assert_eq!(user_visible[0].name, "echo");

        let admin_visible = registry.list_definitions_for(true);
        assert_eq!(admin_visible.len(), 2);
    }

    #[tokio::test]
    async fn execute_known_tool() {
        let mut registry = ToolRegistry::<DummyState>::new();
        registry.register(Box::new(EchoTool));

        let state = make_state();
        let ctx = ToolContext::new();
        let result = registry
            .execute("echo", &state, &ctx, json!({"message": "hello"}))
            .await;
        assert!(!result.is_error);
        assert_eq!(result.content[0].as_text(), Some("echo: hello"));
    }

    #[tokio::test]
    async fn execute_reads_state() {
        let mut registry = ToolRegistry::<DummyState>::new();
        registry.register(Box::new(CounterTool));

        let state = make_state();
        let ctx = ToolContext::new();
        let result = registry.execute("counter", &state, &ctx, json!({})).await;
        assert_eq!(result.content[0].as_text(), Some("counter: 42"));
    }

    #[tokio::test]
    async fn execute_unknown_tool_returns_error() {
        let registry = ToolRegistry::<DummyState>::new();
        let state = make_state();
        let ctx = ToolContext::new();
        let result = registry
            .execute("nonexistent", &state, &ctx, json!({}))
            .await;
        assert!(result.is_error);
        assert!(result.content[0]
            .as_text()
            .expect("text") // Safe: test assertion
            .contains("Unknown tool"));
    }

    #[tokio::test]
    async fn admin_only_tool_rejects_non_admin() {
        let mut registry = ToolRegistry::<DummyState>::new();
        registry.register(Box::new(AdminTool));
        let state = make_state();

        let non_admin = ToolContext::new();
        let denied = registry
            .execute("admin_reset", &state, &non_admin, json!({}))
            .await;
        assert!(denied.is_error);
        assert!(denied.content[0]
            .as_text()
            .expect("text") // Safe: test assertion
            .contains("admin"));

        let admin = ToolContext::new().as_admin(true);
        let allowed = registry
            .execute("admin_reset", &state, &admin, json!({}))
            .await;
        assert!(!allowed.is_error);
        assert_eq!(allowed.content[0].as_text(), Some("reset"));
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
