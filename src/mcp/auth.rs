// ABOUTME: Host-supplied authentication seam for the HTTP transport (RFC 9728 resource server)
// ABOUTME: AuthHook resolves a per-call ToolContext; ApiKeyAuthHook and shared_key_gate gate on one key
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
//! **Which mechanism?** This crate ships six, and the choice is made by what
//! the caller can present, per route — not by which one has no feature flag.
//! The table is in the crate README under "Choosing an auth mechanism"; read it
//! before reaching for a shared bearer key, which is the wrong answer whenever
//! the caller is another Google workload, a browser session, or a webhook.
//!
//! When a shared key *is* the answer — our own binary calling over the wire —
//! [`ApiKeyAuthHook`] is that hook, so no service writes its own.

use std::sync::Arc;

use async_trait::async_trait;
use subtle::ConstantTimeEq;

use crate::mcp::protocol::JsonRpcRequest;
use crate::mcp::tool::ToolContext;
use crate::server::auth::{configured_key, startup_auth, AuthMode, InsecureBindError};

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
///
/// The hook also authenticates what reaches `/mcp` that is not a JSON-RPC
/// request: a client's response to a request a tool sent it, and a `DELETE`
/// ending a session. It is handed those with an empty `method` (and the
/// response's `id`), carrying the credential and headers as a request does.
/// The caller it resolves must be the one the request or session belongs to,
/// so a hook authenticates them exactly as it would any request.
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
/// - **It fails closed.** With the variable unset, empty or blank it admits
///   nobody, where `require_auth` admits everybody. Attach it only when the
///   key is set — [`startup_auth`] answering [`AuthMode::Enforced`], or
///   [`shared_key_gate`] answering `Some` — so an absent key is decided once,
///   at startup, and never becomes an open door.
/// - **Its refusal is the MCP one:** a `401` with an RFC 6750 §3 bearer
///   challenge (`Bearer realm="…"`) and a JSON-RPC error body, rendered by the
///   HTTP transport, instead of `require_auth`'s REST error.
///
/// The key is read on every request, so rotating it needs no restart, by the
/// rule `require_auth` and `startup_auth` share (surrounding whitespace
/// trimmed, blank means unset), and it is compared in constant time. An admitted request runs as a caller that
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

impl ApiKeyAuthHook {
    /// The context of `request` when `presented` is the key, or `None`.
    ///
    /// The admitted caller holds the key and nothing more: no user, no
    /// tenant, never admin. Shared with `mcp::caller::CallerAuthHook`
    /// (feature `google-iam`), which opens the same door beside a verified
    /// caller token.
    pub(crate) fn admit(&self, presented: &str, request: &JsonRpcRequest) -> Option<ToolContext> {
        // Read by the rule `require_auth` and `startup_auth` apply: a blank
        // value is no key (so it admits nobody), and the key is compared with
        // its surrounding whitespace trimmed.
        let expected = configured_key(&self.env_var)?;
        // `ct_eq` on slices of different lengths is false without comparing
        // bytes; the length of a key is not a secret.
        let admitted = bool::from(expected.as_bytes().ct_eq(presented.as_bytes()));
        admitted.then(|| {
            let mut ctx = ToolContext::new().with_auth_method(API_KEY_AUTH_METHOD);
            ctx.request_id.clone_from(&request.id);
            ctx
        })
    }
}

#[async_trait]
impl<S: Send + Sync + ?Sized> AuthHook<S> for ApiKeyAuthHook {
    async fn authenticate(
        &self,
        request: &JsonRpcRequest,
        _state: &Arc<S>,
    ) -> Result<ToolContext, AuthError> {
        let presented = request.auth_token.as_deref().unwrap_or_default();
        self.admit(presented, request)
            .ok_or_else(|| AuthError::Unauthorized {
                www_authenticate: self.challenge.clone(),
            })
    }
}

/// The gate for an `/mcp` route guarded by the shared key in `env_var`,
/// resolved for a bind on `host`: the hook to install, or none.
///
/// This is [`startup_auth`] — the posture logged, a reachable bind with no
/// key refused — turned into what an [`McpServer`](crate::mcp::server::McpServer)
/// takes:
///
/// - key set → `Some` [`ApiKeyAuthHook`] answering refusals with a
///   `Bearer realm="<realm>"` challenge, on any host;
/// - no key, loopback host → `None`: unauthenticated local development, which
///   [`startup_auth`] has already logged at WARN;
/// - no key, reachable host → [`InsecureBindError`] naming `env_var`, and the
///   server must not start.
///
/// It gates `/mcp` with the hook, which fails closed and answers the MCP
/// `401`, rather than with [`require_auth`](crate::server::auth::require_auth),
/// which fails open and answers the REST one. A REST route beside it still
/// takes `require_auth`: both read `env_var` by the same rule, so the two
/// gates agree on the key.
///
/// ```rust,ignore
/// let gate = shared_key_gate("RIVIERE_API_TOKEN", "dravr-riviere", &args.host)?;
/// let server = McpServer::new("dravr-riviere", VERSION, registry, state)
///     .with_optional_auth_hook(gate.map(|hook| Arc::new(hook) as _));
/// ```
///
/// It judges an HTTP bind, so a binary that also serves stdio calls it only
/// when the transport is
/// [`HTTP_TRANSPORT`](crate::mcp::transport::HTTP_TRANSPORT): stdio opens no
/// socket, and resolving the gate there would log a posture that does not
/// apply and refuse a keyless reachable `--host` stdio never binds. See
/// [`transport::run`](crate::mcp::transport::run).
///
/// # Errors
///
/// [`InsecureBindError`], its `gate` naming `env_var`, when the key is unset
/// or blank and `host` is not loopback.
pub fn shared_key_gate(
    env_var: &str,
    realm: &str,
    host: &str,
) -> Result<Option<ApiKeyAuthHook>, InsecureBindError> {
    Ok(match startup_auth(env_var, host)? {
        AuthMode::Enforced => Some(ApiKeyAuthHook::new(env_var, realm)),
        AuthMode::LoopbackDev => None,
    })
}

#[cfg(test)]
mod tests {
    use std::env;

    use serde_json::Value;

    use super::*;

    fn request(token: Option<&str>) -> JsonRpcRequest {
        let mut request: JsonRpcRequest =
            serde_json::from_str(r#"{"jsonrpc":"2.0","id":3,"method":"ping"}"#)
                .expect("a ping parses"); // Safe: test assertion
        request.auth_token = token.map(str::to_owned);
        request
    }

    async fn authenticate(
        hook: &ApiKeyAuthHook,
        token: Option<&str>,
    ) -> Result<ToolContext, AuthError> {
        AuthHook::<()>::authenticate(hook, &request(token), &Arc::new(())).await
    }

    #[test]
    fn no_key_on_loopback_is_no_gate() {
        const ENV: &str = "TRONC_SHARED_KEY_GATE_TEST_LOOPBACK";
        env::remove_var(ENV);

        for host in ["127.0.0.1", "localhost", "::1"] {
            assert_eq!(
                shared_key_gate(ENV, "svc", host),
                Ok(None),
                "{host} with no key serves unauthenticated local development"
            );
        }
    }

    #[test]
    fn no_key_on_a_reachable_bind_is_refused_naming_the_key() {
        const ENV: &str = "TRONC_SHARED_KEY_GATE_TEST_REACHABLE";
        env::set_var(ENV, " \n");

        let refused = shared_key_gate(ENV, "svc", "0.0.0.0");
        env::remove_var(ENV);

        assert_eq!(
            refused,
            Err(InsecureBindError {
                host: "0.0.0.0".to_owned(),
                gate: Some(ENV.to_owned()),
            }),
            "a blank key on a reachable bind is no gate, and the refusal names the key to set"
        );
    }

    #[tokio::test]
    async fn a_set_key_gates_every_host_with_a_hook_that_fails_closed() {
        const ENV: &str = "TRONC_SHARED_KEY_GATE_TEST_ENFORCED";
        env::set_var(ENV, "k-42\n");

        let reachable = shared_key_gate(ENV, "dravr-svc", "0.0.0.0")
            .expect("a key arms the gate") // Safe: test assertion
            .expect("an armed gate is a hook"); // Safe: test assertion
        let loopback = shared_key_gate(ENV, "dravr-svc", "127.0.0.1");

        let admitted = authenticate(&reachable, Some("k-42")).await;
        let wrong = authenticate(&reachable, Some("k-43")).await;
        let missing = authenticate(&reachable, None).await;
        env::remove_var(ENV);

        assert_eq!(
            loopback,
            Ok(Some(ApiKeyAuthHook::new(ENV, "dravr-svc"))),
            "a key gates a loopback bind too"
        );
        assert_eq!(
            reachable.challenge(),
            "Bearer realm=\"dravr-svc\"",
            "the hook challenges with the realm it was given"
        );
        let ctx = admitted.expect("the key admits its holder"); // Safe: test assertion
        assert_eq!(
            ctx.auth_method.as_deref(),
            Some(API_KEY_AUTH_METHOD),
            "the key admits through the api-key method"
        );
        assert_eq!(
            ctx.request_id,
            Some(Value::from(3)),
            "the context carries the request's id"
        );
        assert_eq!(ctx.user_id, None, "the key names no user");
        assert!(!ctx.is_admin, "the key is never admin");
        for (label, refused) in [("a wrong key", wrong), ("no key", missing)] {
            match refused {
                Err(AuthError::Unauthorized { www_authenticate }) => assert_eq!(
                    www_authenticate, "Bearer realm=\"dravr-svc\"",
                    "{label} is answered with the realm challenge"
                ),
                other => panic!("{label} must be refused with a 401, got {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn the_hook_admits_nobody_once_the_key_is_removed() {
        const ENV: &str = "TRONC_SHARED_KEY_GATE_TEST_REMOVED";
        env::set_var(ENV, "k-7");
        let hook = shared_key_gate(ENV, "svc", "0.0.0.0")
            .expect("a key arms the gate") // Safe: test assertion
            .expect("an armed gate is a hook"); // Safe: test assertion

        env::remove_var(ENV);
        let refused = authenticate(&hook, Some("k-7")).await;

        assert!(
            matches!(refused, Err(AuthError::Unauthorized { .. })),
            "a gate armed at startup stays closed when its key goes away, got {refused:?}"
        );
    }
}
