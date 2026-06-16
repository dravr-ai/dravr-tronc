// ABOUTME: Host-integration seams letting a host extend the generic MCP engine
// ABOUTME: full tool dispatch (list + call) and non-tool method handling
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

//! Host-integration seams for the MCP engine.
//!
//! The generic [`crate::mcp::server::McpServer`] natively serves `initialize`,
//! `server/discover`, `ping`, `tools/list`, and `tools/call` from its own
//! [`crate::mcp::tool::ToolRegistry`]. A host with richer needs installs these
//! optional seams to take over without the engine knowing anything
//! host-specific:
//!
//! - [`ToolDispatcher`] — own the entire tool surface. When installed it
//!   replaces the built-in registry for both `tools/list` (e.g. per-tenant or
//!   feature-flagged views) and `tools/call` (the host runs the whole call:
//!   authorization beyond auth, quota, execution, usage, notifications).
//! - [`MethodHandler`] — serve JSON-RPC methods the engine doesn't (e.g.
//!   `resources/*`, `prompts/*`, `sampling/*`, `completion/*`, `roots/*`).
//!
//! Each seam is optional; a server with none behaves exactly as a registry-only
//! server. See also [`crate::mcp::auth::AuthHook`] for the authentication seam.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use crate::mcp::protocol::JsonRpcResponse;
use crate::mcp::schema::{Tool, ToolResponse};
use crate::mcp::tool::ToolContext;

/// Host-owned tool surface — replaces the built-in registry for `tools/list`
/// and `tools/call` when installed.
///
/// A host whose tool listing or execution needs more than the generic registry
/// offers (per-caller views, quota gating, usage accounting, notification
/// fan-out) implements this and installs it via
/// [`crate::mcp::server::McpServer::with_tool_dispatcher`]. The engine then
/// routes both tool methods here instead of to its own
/// [`crate::mcp::tool::ToolRegistry`], keeping the host's tool catalog the
/// single source of truth.
#[async_trait]
pub trait ToolDispatcher<S: Send + Sync + ?Sized>: Send + Sync {
    /// Return the tool definitions to advertise for this caller.
    async fn list_tools(&self, state: &Arc<S>, ctx: &ToolContext) -> Vec<Tool>;

    /// Execute a tool call end-to-end and return its response. The host owns
    /// everything inside: authorization beyond authentication, quota checks,
    /// the actual execution, usage recording, and response augmentation. An
    /// unknown tool or a refusal is reported as an error [`ToolResponse`]
    /// (`is_error = true`), not a transport error.
    async fn call_tool(
        &self,
        name: &str,
        state: &Arc<S>,
        ctx: &ToolContext,
        arguments: Value,
    ) -> ToolResponse;
}

/// Host-supplied handler for methods the engine doesn't natively serve.
///
/// The engine dispatches `initialize`/`discover`/`ping`/`tools/*` itself and
/// routes every other method here first. Returning `Some(response)` answers the
/// request; returning `None` lets the engine fall through to its
/// method-not-found response. This is where a host serves `resources/*`,
/// `prompts/*`, `sampling/*`, and similar protocol areas the engine leaves open.
#[async_trait]
pub trait MethodHandler<S: Send + Sync + ?Sized>: Send + Sync {
    /// Handle a non-tool method, or decline it by returning `None`.
    async fn handle(
        &self,
        method: &str,
        id: Option<Value>,
        params: Option<Value>,
        state: &Arc<S>,
        ctx: &ToolContext,
    ) -> Option<JsonRpcResponse>;
}
