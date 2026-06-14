// ABOUTME: Host-supplied authentication seam for the HTTP transport (RFC 9728 resource server)
// ABOUTME: AuthHook resolves a per-call ToolContext from a request; AuthError maps to 401/403
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

//! Authentication seam for the MCP HTTP transport.
//!
//! The generic engine knows nothing about how a host authenticates callers — it
//! only knows how to ask. A host that needs auth implements [`AuthHook`] to turn
//! a request (whose bearer token + headers the transport has populated) into a
//! per-call [`crate::mcp::tool::ToolContext`], or to reject it. The HTTP
//! transport renders an [`AuthError`] as the matching status code: `401` with a
//! `WWW-Authenticate` challenge (RFC 9728) or `403`.

use std::sync::Arc;

use async_trait::async_trait;

use crate::mcp::protocol::JsonRpcRequest;
use crate::mcp::tool::ToolContext;

/// Why an [`AuthHook`] rejected a request. The HTTP transport maps each variant
/// to its status code.
#[derive(Debug, Clone)]
pub enum AuthError {
    /// `401 Unauthorized` with a `WWW-Authenticate` challenge header (RFC 9728).
    /// The string is the full header value, e.g.
    /// `Bearer resource_metadata="https://api.example.com/.well-known/oauth-protected-resource"`.
    Unauthorized {
        /// The `WWW-Authenticate` header value to return.
        www_authenticate: String,
    },
    /// `403 Forbidden` — authenticated but not permitted. Carries a reason.
    Forbidden {
        /// Human-readable reason (returned in the response body).
        reason: String,
    },
}

/// Host-supplied authentication for the HTTP transport.
///
/// Given the parsed request — whose `auth_token` and `headers` the transport has
/// populated from the HTTP request — resolve the per-call [`ToolContext`] or
/// reject with an [`AuthError`]. A server configured with no hook authenticates
/// every request as the default (anonymous) context, which suits stdio or a
/// trusted-network deployment.
#[async_trait]
pub trait AuthHook<S: Send + Sync + ?Sized>: Send + Sync {
    /// Authenticate a request, yielding its per-call context or a rejection.
    async fn authenticate(
        &self,
        request: &JsonRpcRequest,
        state: &Arc<S>,
    ) -> Result<ToolContext, AuthError>;
}
