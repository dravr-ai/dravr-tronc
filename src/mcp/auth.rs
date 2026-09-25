// ABOUTME: Host-supplied authentication seam for the HTTP transport (RFC 9728 resource server)
// ABOUTME: AuthHook resolves a per-call ToolContext; ApiKeyAuthHook is the shared-bearer-key hook
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
//! `WWW-Authenticate` challenge (RFC 9728), `403`, `429` with `Retry-After`, or
//! `500`.
//!
//! **Which mechanism?** This crate ships five, and the choice is made by what
//! the caller can present, per route — not by which one has no feature flag.
//! The table is in the crate README under "Choosing an auth mechanism"; read it
//! before reaching for a shared bearer key, which is the wrong answer whenever
//! the caller is another Google workload, a browser session, or a webhook.
//!
//! When a shared key *is* the answer — our own binary calling over the wire —
//! [`ApiKeyAuthHook`] is that hook, so no service writes its own.

use std::env;
use std::sync::Arc;

use async_trait::async_trait;
use subtle::ConstantTimeEq;

use crate::mcp::protocol::JsonRpcRequest;
use crate::mcp::tool::ToolContext;

/// Why an [`AuthHook`] rejected a request. The HTTP transport maps each variant
/// to its status code.
#[derive(Debug, Clone)]
#[non_exhaustive]
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
    /// `403 Forbidden` with an RFC 6750 §3.1 `insufficient_scope` challenge —
    /// the caller authenticated, but the credential's grant does not cover
    /// what they asked for.
    ///
    /// Distinct from [`Self::Forbidden`] because the remedy is different and
    /// machine-actionable: a client that reads `scope="…"` off the challenge
    /// knows exactly which grant to request, and can re-authorize instead of
    /// giving up. A bare 403 tells it only that it lost.
    ///
    /// Kept as its own variant rather than a field on [`Self::Forbidden`] so
    /// that adding it breaks no host that *constructs* a `Forbidden` — the
    /// common case across consumers — only ones that match exhaustively, which
    /// `#[non_exhaustive]` now prevents from recurring.
    InsufficientScope {
        /// The full `WWW-Authenticate` value, e.g.
        /// `Bearer error="insufficient_scope", scope="fitness:write"`.
        www_authenticate: String,
        /// Human-readable reason (returned in the response body).
        reason: String,
    },
    /// `429 Too Many Requests` with a `Retry-After` header — the credential is
    /// valid, but its request budget is spent until its window frees capacity.
    ///
    /// Distinct from [`Self::Unauthorized`] because the remedies are opposite:
    /// a client told its token is invalid refreshes it and re-authorizes, which
    /// a spent budget refuses again, while a client told to slow down waits.
    /// The transport renders the wait both as the `Retry-After` header and as
    /// `data.retry_after_secs` on the JSON-RPC error, so the two never differ.
    RateLimited {
        /// Seconds until the same request can succeed, floored at one when
        /// rendered: a refusal still in force never reads as "retry now".
        retry_after_secs: u64,
        /// Human-readable reason (returned in the response body).
        reason: String,
    },
    /// `500 Internal Server Error` — a failure on the host's side (a database
    /// read, a dependency it calls) interrupted authentication before it could
    /// decide.
    ///
    /// It says nothing about the credential, so it must not be rendered as a
    /// `401`, which sends a client to discard a good token and re-authorize.
    Internal {
        /// Human-readable reason (returned in the response body). Never the
        /// underlying error's text, which is the host's to log.
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

/// `auth_method` recorded on the [`ToolContext`] of a request [`ApiKeyAuthHook`]
/// admitted.
pub const API_KEY_AUTH_METHOD: &str = "api_key";

/// Requires every MCP request to carry, as its bearer token, the shared key
/// held in an environment variable.
///
/// It is the MCP-route counterpart of
/// [`require_auth`](crate::server::auth::require_auth), and it differs in the
/// two ways an MCP route needs:
///
/// - **It fails closed.** With the variable unset or empty it admits nobody,
///   where `require_auth` admits everybody. Attach it only when the key is set
///   — [`startup_auth`](crate::server::auth::startup_auth) answering
///   [`AuthMode::Enforced`](crate::server::auth::AuthMode::Enforced) — so an
///   absent key is decided once, at startup, and never becomes an open door.
/// - **Its refusal is the MCP one:** a `401` with an RFC 6750 §3 bearer
///   challenge (`Bearer realm="…"`) and a JSON-RPC error body, rendered by the
///   HTTP transport, instead of `require_auth`'s REST error.
///
/// The key is read on every request, so rotating it needs no restart, and it
/// is compared in constant time. An admitted request runs as a caller that
/// holds the key and nothing more: no user, no tenant, never admin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiKeyAuthHook {
    env_var: String,
    challenge: String,
}

impl ApiKeyAuthHook {
    /// A hook enforcing the key in `env_var`, answering refusals with a
    /// `Bearer realm="<realm>"` challenge.
    ///
    /// Name the realm after the service. Two surfaces of one service that share
    /// a key share a realm, so a client can reuse one credential for both.
    #[must_use]
    pub fn new(env_var: impl Into<String>, realm: &str) -> Self {
        Self {
            env_var: env_var.into(),
            challenge: format!("Bearer realm=\"{}\"", quote_escape(realm)),
        }
    }

    /// The `WWW-Authenticate` value every refusal carries.
    #[must_use]
    pub fn challenge(&self) -> &str {
        &self.challenge
    }
}

/// `value` with `\` and `"` escaped, so it can sit inside an RFC 9110
/// quoted-string.
fn quote_escape(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for c in value.chars() {
        if matches!(c, '\\' | '"') {
            escaped.push('\\');
        }
        escaped.push(c);
    }
    escaped
}

#[async_trait]
impl<S: Send + Sync + ?Sized> AuthHook<S> for ApiKeyAuthHook {
    async fn authenticate(
        &self,
        request: &JsonRpcRequest,
        _state: &Arc<S>,
    ) -> Result<ToolContext, AuthError> {
        let expected = env::var(&self.env_var).unwrap_or_default();
        let presented = request.auth_token.as_deref().unwrap_or_default();
        // `ct_eq` on slices of different lengths is false without comparing
        // bytes; the length of a key is not a secret.
        let admitted =
            !expected.is_empty() && bool::from(expected.as_bytes().ct_eq(presented.as_bytes()));

        if admitted {
            let mut ctx = ToolContext::new().with_auth_method(API_KEY_AUTH_METHOD);
            ctx.request_id.clone_from(&request.id);
            Ok(ctx)
        } else {
            Err(AuthError::Unauthorized {
                www_authenticate: self.challenge.clone(),
            })
        }
    }
}
