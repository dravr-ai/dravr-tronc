// ABOUTME: Verified caller identity for MCP — a platform-minted JWT checked against the issuer's JWKS
// ABOUTME: CallerTokenVerifier checks the token; CallerAuthHook turns it into the call's ToolContext
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

//! # Who is calling, not merely "someone holding the key"
//!
//! A shared bearer key ([`ApiKeyAuthHook`]) proves the caller holds the key and
//! nothing else: no user, no tenant. A tool that reads or writes one user's
//! rows in one tenant cannot run on that, and taking the user and tenant as
//! arguments would hand every key holder every tenant.
//!
//! This module carries the identity on the credential instead. The platform's
//! authorization server mints an RS256 JWT addressed to one resource server
//! (RFC 8707: the token's `aud` is that server's resource identifier), and the
//! server checks it here:
//!
//! - the signature, against the issuer's published JWK set, cached by
//!   [`GoogleKeySet`] — the one key-set cache in this crate, which serves any
//!   RS256 issuer that publishes `kid`/`n`/`e` keys, not only Google;
//! - `iss` equal to the configured issuer, `aud` equal to this server's own
//!   resource identifier (a token minted for another server is refused, so a
//!   token captured on one hop cannot be replayed at another), `exp`, and `nbf`
//!   when the token carries one;
//! - then `sub` becomes [`ToolContext::user_id`], the tenant claim
//!   ([`PLATFORM_TENANT_CLAIM`] unless configured otherwise) becomes
//!   [`ToolContext::tenant_id`], the space-delimited `scope` claim (RFC 9068
//!   §2.2.3) becomes [`ToolContext::scopes`], and `client_id` becomes
//!   [`ToolContext::client_id`].
//!
//! A tool that needs the identity declares
//! [`ToolCapabilities::REQUIRES_AUTH`](crate::mcp::tool::ToolCapabilities::REQUIRES_AUTH)
//! and, for tenant rows,
//! [`ToolCapabilities::REQUIRES_TENANT`](crate::mcp::tool::ToolCapabilities::REQUIRES_TENANT);
//! [`ToolRegistry::execute`](crate::mcp::tool::ToolRegistry::execute) refuses
//! it on a context without them, so no tool depends on its own discipline to
//! stay closed. The tool then reads the identity off its context, never off
//! its arguments.
//!
//! ## What a refusal says
//!
//! A token that fails any check is answered `401` with the configured
//! challenge, and the reason goes to the server's log only: a verifier that
//! says which claim failed is a claim-guessing oracle. When the issuer's keys
//! cannot be fetched the answer is `500` ([`AuthError::Internal`]), because
//! that outage says nothing about the token, and a `401` would send a client
//! to discard a good one. Neither outcome ever yields an identity.

use std::env;
use std::error::Error;
use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use jsonwebtoken::{decode, Algorithm, Validation};
use reqwest::Client;
use serde::Deserialize;
use serde_json::{Map, Value};
use tracing::{error, warn};

use crate::iam::{GoogleKeySet, IamError};
use crate::mcp::auth::{ApiKeyAuthHook, AuthError, AuthHook};
use crate::mcp::protocol::JsonRpcRequest;
use crate::mcp::resource_metadata::{InvalidResource, ProtectedResourceMetadata};
use crate::mcp::tool::ToolContext;

/// The claim the dravr platform carries the caller's active tenant in.
///
/// The platform's tokens name it `active_tenant_id` because a user may belong
/// to several tenants and the token says which one this session acts in.
pub const PLATFORM_TENANT_CLAIM: &str = "active_tenant_id";

/// `auth_method` recorded on the [`ToolContext`] of a caller whose token
/// [`CallerTokenVerifier`] verified.
pub const VERIFIED_CALLER_AUTH_METHOD: &str = "verified_caller";

/// The spec claims a token must carry; a token missing one is refused before
/// any of its values are read.
const REQUIRED_CLAIMS: [&str; 4] = ["exp", "iss", "aud", "sub"];

/// The identity a verified token carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedCaller {
    /// The token's `sub`: the user the token acts for.
    pub user_id: String,
    /// The tenant claim, when the token carries one.
    pub tenant_id: Option<String>,
    /// The grant, from the space-delimited `scope` claim.
    pub scopes: Vec<String>,
    /// The OAuth client the token was issued to, from `client_id`.
    pub client_id: Option<String>,
}

impl VerifiedCaller {
    /// The per-call context of a request this caller made: the identity, the
    /// grant and the client, under [`VERIFIED_CALLER_AUTH_METHOD`]. Never
    /// admin: a token carries no operator authority this crate could check.
    #[must_use]
    pub fn into_context(self, request_id: Option<Value>) -> ToolContext {
        let mut ctx = ToolContext::new()
            .with_user(self.user_id)
            .with_auth_method(VERIFIED_CALLER_AUTH_METHOD);
        ctx.tenant_id = self.tenant_id;
        ctx.scopes = self.scopes;
        ctx.client_id = self.client_id;
        ctx.request_id = request_id;
        ctx
    }
}

/// The claims the verifier reads after the library has checked the spec ones.
#[derive(Deserialize)]
struct CallerClaims {
    sub: String,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    client_id: Option<String>,
    #[serde(flatten)]
    rest: Map<String, Value>,
}

/// Verifies the caller tokens one issuer mints for one resource server.
pub struct CallerTokenVerifier {
    issuer: String,
    audience: String,
    tenant_claim: String,
    keys: GoogleKeySet,
}

impl fmt::Debug for CallerTokenVerifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CallerTokenVerifier")
            .field("issuer", &self.issuer)
            .field("audience", &self.audience)
            .field("tenant_claim", &self.tenant_claim)
            .finish_non_exhaustive()
    }
}

impl CallerTokenVerifier {
    /// Verify tokens `issuer` mints for `audience`, signed by a key `keys`
    /// serves.
    ///
    /// `issuer` is compared verbatim with the token's `iss`, and `audience`
    /// with its `aud`: this server's resource identifier exactly as the
    /// authorization server binds tokens to it.
    #[must_use]
    pub fn new(issuer: impl Into<String>, audience: impl Into<String>, keys: GoogleKeySet) -> Self {
        Self {
            issuer: issuer.into(),
            audience: audience.into(),
            tenant_claim: PLATFORM_TENANT_CLAIM.to_owned(),
            keys,
        }
    }

    /// Read the tenant from `claim` instead of [`PLATFORM_TENANT_CLAIM`].
    #[must_use]
    pub fn with_tenant_claim(mut self, claim: impl Into<String>) -> Self {
        self.tenant_claim = claim.into();
        self
    }

    /// The audience a token must name.
    #[must_use]
    pub fn audience(&self) -> &str {
        &self.audience
    }

    /// Check one bearer token and read the identity it carries.
    ///
    /// # Errors
    ///
    /// [`IamError::Rejected`] when the token is malformed, not RS256, signed
    /// by a key the issuer does not publish, from another issuer, for another
    /// audience, expired, not yet valid, missing `exp`/`iss`/`aud`/`sub`, or
    /// carrying an empty `sub` or a tenant claim that is not a non-empty
    /// string. [`IamError::JwksUnavailable`] when the issuer's keys could not
    /// be fetched — kept apart because it says nothing about the token.
    pub async fn verify(&self, token: &str) -> Result<VerifiedCaller, IamError> {
        let decoding_key = self.keys.decoding_key_for(token).await?;

        let mut validation = Validation::new(Algorithm::RS256);
        validation.set_audience(&[self.audience.as_str()]);
        validation.set_issuer(&[self.issuer.as_str()]);
        validation.set_required_spec_claims(&REQUIRED_CLAIMS);
        validation.validate_nbf = true;

        let claims = decode::<CallerClaims>(token, &decoding_key, &validation)
            .map_err(|e| IamError::Rejected(e.to_string()))?
            .claims;

        if claims.sub.trim().is_empty() {
            return Err(IamError::Rejected("token has an empty sub".to_owned()));
        }
        let tenant_id = match claims.rest.get(&self.tenant_claim) {
            None | Some(Value::Null) => None,
            Some(Value::String(tenant)) if !tenant.trim().is_empty() => Some(tenant.clone()),
            Some(_) => {
                return Err(IamError::Rejected(format!(
                    "claim {} is not a tenant id",
                    self.tenant_claim
                )))
            }
        };
        let scopes = claims
            .scope
            .as_deref()
            .unwrap_or_default()
            .split_whitespace()
            .map(str::to_owned)
            .collect();

        Ok(VerifiedCaller {
            user_id: claims.sub,
            tenant_id,
            scopes,
            client_id: claims.client_id.filter(|id| !id.is_empty()),
        })
    }
}

/// The MCP [`AuthHook`] that admits a caller by a token
/// [`CallerTokenVerifier`] verifies, and resolves the call's identity from it.
///
/// Two other ways in can be opened beside it, each yielding a context with
/// **no** identity, so a tool that requires one is still refused by the
/// registry:
///
/// - [`Self::with_shared_key`]: the holder of the service's shared key, for
///   the operations that touch no tenant's data. A token that is not the key
///   is verified as a caller token; the key never stands in for one.
/// - [`Self::allowing_anonymous`]: a request carrying no credential at all,
///   for loopback development only. A request that presents a credential which
///   fails is refused even then — a bad token is never quietly downgraded.
pub struct CallerAuthHook {
    verifier: Arc<CallerTokenVerifier>,
    challenge: String,
    shared_key: Option<ApiKeyAuthHook>,
    anonymous: bool,
}

impl fmt::Debug for CallerAuthHook {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CallerAuthHook")
            .field("verifier", &self.verifier)
            .field("challenge", &self.challenge)
            .field("shared_key", &self.shared_key.is_some())
            .field("anonymous", &self.anonymous)
            .finish()
    }
}

impl CallerAuthHook {
    /// Admit callers whose token `verifier` accepts, answering every refusal
    /// with the `WWW-Authenticate` value `challenge` — the server's
    /// [`ProtectedResourceMetadata::www_authenticate`], so a client learns
    /// where to obtain a token.
    #[must_use]
    pub fn new(verifier: Arc<CallerTokenVerifier>, challenge: impl Into<String>) -> Self {
        Self {
            verifier,
            challenge: challenge.into(),
            shared_key: None,
            anonymous: false,
        }
    }

    /// Also admit the holder of the shared key `hook` enforces, with no
    /// identity.
    #[must_use]
    pub fn with_shared_key(mut self, hook: ApiKeyAuthHook) -> Self {
        self.shared_key = Some(hook);
        self
    }

    /// Also admit a request that carries no credential, with no identity.
    ///
    /// For loopback development, where the service would otherwise serve
    /// unauthenticated: the tools that need no identity keep working without
    /// ceremony, and the ones that need one still need a verified token.
    #[must_use]
    pub fn allowing_anonymous(mut self) -> Self {
        self.anonymous = true;
        self
    }

    fn unauthorized(&self) -> AuthError {
        AuthError::Unauthorized {
            www_authenticate: self.challenge.clone(),
        }
    }
}

#[async_trait]
impl<S: Send + Sync + ?Sized> AuthHook<S> for CallerAuthHook {
    async fn authenticate(
        &self,
        request: &JsonRpcRequest,
        _state: &Arc<S>,
    ) -> Result<ToolContext, AuthError> {
        let presented = request
            .auth_token
            .as_deref()
            .filter(|token| !token.is_empty());
        let Some(token) = presented else {
            if self.anonymous {
                let mut ctx = ToolContext::new();
                ctx.request_id.clone_from(&request.id);
                return Ok(ctx);
            }
            return Err(self.unauthorized());
        };

        if let Some(shared_key) = &self.shared_key {
            if let Some(ctx) = shared_key.admit(token, request) {
                return Ok(ctx);
            }
        }

        match self.verifier.verify(token).await {
            Ok(caller) => Ok(caller.into_context(request.id.clone())),
            Err(IamError::JwksUnavailable(why)) => {
                error!(error = %why, "could not fetch the caller token issuer's keys");
                Err(AuthError::Internal {
                    reason: "The caller token could not be checked".to_owned(),
                })
            }
            Err(why) => {
                // Logged with the reason, answered without it.
                warn!(error = %why, "rejected a caller token");
                Err(self.unauthorized())
            }
        }
    }
}

/// A caller-token configuration that is incomplete or does not hold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallerConfigError {
    /// Some of the variables are set and these are not. Caller tokens are
    /// configured by all of them or none, so a typo in one cannot leave the
    /// service believing it verifies callers it cannot.
    Incomplete {
        /// The unset or blank variables.
        missing: Vec<String>,
    },
    /// The resource identifier is not one RFC 9728 allows.
    InvalidResource(InvalidResource),
}

impl fmt::Display for CallerConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Incomplete { missing } => write!(
                f,
                "caller tokens are configured by every variable or none; unset: {}",
                missing.join(", ")
            ),
            Self::InvalidResource(invalid) => invalid.fmt(f),
        }
    }
}

impl Error for CallerConfigError {}

/// Where a service's caller tokens come from, read from its environment.
///
/// One prefix per service, so `from_env("COMMERE")` reads:
///
/// | Variable | Meaning |
/// |---|---|
/// | `COMMERE_RESOURCE_URL` | this server's resource identifier: the audience its tokens name, and the `resource` its metadata publishes |
/// | `COMMERE_CALLER_ISSUER` | the `iss` its tokens carry |
/// | `COMMERE_CALLER_JWKS_URL` | where the issuer publishes its signing keys |
/// | `COMMERE_AUTHORIZATION_SERVER` | the authorization server a client obtains a token from, as the metadata names it |
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallerTokenConfig {
    /// This server's resource identifier, which is the audience.
    pub resource: String,
    /// The issuer tokens must name.
    pub issuer: String,
    /// The issuer's JWK set URL.
    pub jwks_url: String,
    /// The authorization server the metadata names.
    pub authorization_server: String,
}

impl CallerTokenConfig {
    /// The variable names for `prefix`, in the order the fields are declared.
    fn variables(prefix: &str) -> [String; 4] {
        [
            format!("{prefix}_RESOURCE_URL"),
            format!("{prefix}_CALLER_ISSUER"),
            format!("{prefix}_CALLER_JWKS_URL"),
            format!("{prefix}_AUTHORIZATION_SERVER"),
        ]
    }

    /// Read the configuration under `prefix`, or `None` when none of its
    /// variables is set.
    ///
    /// # Errors
    ///
    /// [`CallerConfigError::Incomplete`] when some are set and others are
    /// not; [`CallerConfigError::InvalidResource`] when the resource
    /// identifier is not an `https` URL (`http` on loopback) without a query
    /// or fragment.
    pub fn from_env(prefix: &str) -> Result<Option<Self>, CallerConfigError> {
        let names = Self::variables(prefix);
        let values: Vec<Option<String>> = names
            .iter()
            .map(|name| {
                env::var(name)
                    .ok()
                    .map(|v| v.trim().to_owned())
                    .filter(|v| !v.is_empty())
            })
            .collect();
        if values.iter().all(Option::is_none) {
            return Ok(None);
        }
        let missing: Vec<String> = names
            .iter()
            .zip(&values)
            .filter(|(_, value)| value.is_none())
            .map(|(name, _)| name.clone())
            .collect();
        let mut values = values.into_iter().flatten();
        let (Some(resource), Some(issuer), Some(jwks_url), Some(authorization_server)) =
            (values.next(), values.next(), values.next(), values.next())
        else {
            return Err(CallerConfigError::Incomplete { missing });
        };
        let config = Self {
            resource,
            issuer,
            jwks_url,
            authorization_server,
        };
        config
            .resource_metadata()
            .map_err(CallerConfigError::InvalidResource)?;
        Ok(Some(config))
    }

    /// The verifier for this configuration, fetching keys with `http`.
    #[must_use]
    pub fn verifier(&self, http: Client) -> CallerTokenVerifier {
        CallerTokenVerifier::new(
            &self.issuer,
            &self.resource,
            GoogleKeySet::new(&self.jwks_url, http),
        )
    }

    /// The RFC 9728 metadata this server publishes, naming the authorization
    /// server a client obtains its token from.
    ///
    /// # Errors
    ///
    /// [`InvalidResource`] when the resource identifier is not one RFC 9728
    /// allows.
    pub fn resource_metadata(&self) -> Result<ProtectedResourceMetadata, InvalidResource> {
        ProtectedResourceMetadata::new(&self.resource, vec![self.authorization_server.clone()])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    const PREFIX: &str = "TRONC_CALLER_UNIT";

    fn clear() {
        for name in CallerTokenConfig::variables(PREFIX) {
            env::remove_var(name);
        }
    }

    #[test]
    #[serial]
    fn no_variable_set_is_no_configuration() {
        clear();
        assert_eq!(CallerTokenConfig::from_env(PREFIX), Ok(None));
    }

    #[test]
    #[serial]
    fn a_partial_configuration_names_what_is_missing() {
        clear();
        env::set_var(
            format!("{PREFIX}_RESOURCE_URL"),
            "https://svc.example.com/mcp",
        );
        env::set_var(format!("{PREFIX}_CALLER_ISSUER"), "  ");
        let refused = CallerTokenConfig::from_env(PREFIX);
        clear();
        assert_eq!(
            refused,
            Err(CallerConfigError::Incomplete {
                missing: vec![
                    format!("{PREFIX}_CALLER_ISSUER"),
                    format!("{PREFIX}_CALLER_JWKS_URL"),
                    format!("{PREFIX}_AUTHORIZATION_SERVER"),
                ]
            })
        );
    }

    #[test]
    #[serial]
    fn a_complete_configuration_reads_every_variable() {
        clear();
        env::set_var(
            format!("{PREFIX}_RESOURCE_URL"),
            "https://svc.example.com/mcp",
        );
        env::set_var(format!("{PREFIX}_CALLER_ISSUER"), "pierre-mcp-server");
        env::set_var(
            format!("{PREFIX}_CALLER_JWKS_URL"),
            "https://api.example.com/.well-known/jwks.json",
        );
        env::set_var(
            format!("{PREFIX}_AUTHORIZATION_SERVER"),
            "https://api.example.com",
        );
        let config = CallerTokenConfig::from_env(PREFIX);
        clear();
        let config = config.unwrap().unwrap();
        assert_eq!(config.resource, "https://svc.example.com/mcp");
        assert_eq!(config.issuer, "pierre-mcp-server");
        assert_eq!(
            config.resource_metadata().unwrap().authorization_servers,
            vec!["https://api.example.com".to_owned()]
        );
        assert_eq!(
            config.verifier(Client::new()).audience(),
            "https://svc.example.com/mcp"
        );
    }

    #[test]
    #[serial]
    fn a_plain_http_resource_off_loopback_is_refused() {
        clear();
        env::set_var(
            format!("{PREFIX}_RESOURCE_URL"),
            "http://svc.example.com/mcp",
        );
        env::set_var(format!("{PREFIX}_CALLER_ISSUER"), "pierre-mcp-server");
        env::set_var(format!("{PREFIX}_CALLER_JWKS_URL"), "https://a/jwks");
        env::set_var(format!("{PREFIX}_AUTHORIZATION_SERVER"), "https://a");
        let refused = CallerTokenConfig::from_env(PREFIX);
        clear();
        assert!(matches!(
            refused,
            Err(CallerConfigError::InvalidResource(_))
        ));
    }

    #[test]
    fn a_verified_caller_becomes_a_context_with_its_identity_and_never_admin() {
        let ctx = VerifiedCaller {
            user_id: "user-1".to_owned(),
            tenant_id: Some("tenant-1".to_owned()),
            scopes: vec!["profile:read".to_owned()],
            client_id: Some("client-1".to_owned()),
        }
        .into_context(Some(Value::from(7)));
        assert_eq!(ctx.user_id.as_deref(), Some("user-1"));
        assert_eq!(ctx.tenant_id.as_deref(), Some("tenant-1"));
        assert_eq!(ctx.scopes, vec!["profile:read".to_owned()]);
        assert_eq!(ctx.client_id.as_deref(), Some("client-1"));
        assert_eq!(
            ctx.auth_method.as_deref(),
            Some(VERIFIED_CALLER_AUTH_METHOD)
        );
        assert_eq!(ctx.request_id, Some(Value::from(7)));
        assert!(!ctx.is_admin);
    }
}
