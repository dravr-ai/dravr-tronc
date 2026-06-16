// ABOUTME: Host-integration seams letting a host extend the generic MCP engine
// ABOUTME: tool-list filtering, non-tool method handling, and tool-call pre/post hooks
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

//! Host-integration seams for the MCP engine.
//!
//! The generic [`crate::mcp::server::McpServer`] natively serves `initialize`,
//! `server/discover`, `ping`, `tools/list`, and `tools/call`. A host with richer
//! needs installs these optional seams to extend that behaviour without the
//! engine knowing anything host-specific:
//!
//! - [`ToolListProvider`] — replace the `tools/list` result with a per-caller
//!   view (e.g. tenant-scoped or feature-flagged tool sets).
//! - [`MethodHandler`] — serve JSON-RPC methods the engine doesn't (e.g.
//!   `resources/*`, `prompts/*`, `sampling/*`, `completion/*`, `roots/*`).
//! - [`ToolCallHooks`] — run host logic around every `tools/call` (e.g. quota
//!   gating before, usage recording / notification appending after).
//!
//! Each seam is optional; a server with none behaves exactly as before.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use crate::mcp::protocol::JsonRpcResponse;
use crate::mcp::schema::{Tool, ToolResponse};
use crate::mcp::tool::ToolContext;

/// Host-supplied `tools/list` view.
///
/// When installed, the engine asks the provider for the tool definitions
/// visible to a caller instead of listing the whole registry. This lets a host
/// scope the advertised tools per [`ToolContext`] — for example tenant-enabled
/// subsets or feature-flagged tools the registry alone can't express.
#[async_trait]
pub trait ToolListProvider<S: Send + Sync + ?Sized>: Send + Sync {
    /// Return the tool definitions to advertise for this caller.
    async fn list_tools(&self, state: &Arc<S>, ctx: &ToolContext) -> Vec<Tool>;
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

/// Host hooks that run around every `tools/call`.
///
/// [`Self::before`] runs prior to executing the named tool and may short-circuit
/// the call (e.g. when a quota is exceeded) by returning `Err(response)` — that
/// [`ToolResponse`] is returned to the caller and the tool never runs.
/// [`Self::after`] runs once the tool produces a response and may augment it
/// (e.g. record usage, append pending notifications) before it's sent.
#[async_trait]
pub trait ToolCallHooks<S: Send + Sync + ?Sized>: Send + Sync {
    /// Run before the tool executes. `Ok(())` proceeds; `Err(response)`
    /// short-circuits the call with the given response.
    ///
    /// # Errors
    /// Returns the short-circuit [`ToolResponse`] when the call must not proceed.
    async fn before(
        &self,
        name: &str,
        state: &Arc<S>,
        ctx: &ToolContext,
        arguments: &Value,
    ) -> Result<(), ToolResponse>;

    /// Run after the tool produces `response`; returns the response to send,
    /// optionally augmented.
    async fn after(
        &self,
        name: &str,
        state: &Arc<S>,
        ctx: &ToolContext,
        response: ToolResponse,
    ) -> ToolResponse;
}
