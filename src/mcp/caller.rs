// ABOUTME: Verified caller identity for MCP — a platform-minted JWT checked against the issuer's JWKS
// ABOUTME: CallerTokenVerifier checks the token, CallerAuthHook admits it; resolve_mcp_auth picks /mcp's gate
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
use tracing::{error, info, warn};

use crate::iam::{GoogleKeySet, IamError};
use crate::mcp::auth::{shared_key_gate, ApiKeyAuthHook, AuthError, AuthHook};
use crate::mcp::protocol::JsonRpcRequest;
use crate::mcp::resource_metadata::{InvalidResource, ProtectedResourceMetadata};
use crate::mcp::tool::ToolContext;
use crate::server::auth::{api_key_configured, AuthMode, InsecureBindError};

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

/// How a service's `/mcp` authenticates, resolved once at startup by
/// [`resolve_mcp_auth`] and applied with
/// [`McpServer::with_mcp_auth`](crate::mcp::server::McpServer::with_mcp_auth).
///
/// Non-exhaustive: it is produced by [`resolve_mcp_auth`], so a field added
/// later is not a breaking change.
#[non_exhaustive]
pub struct McpAuth<S: Send + Sync + ?Sized> {
    /// The startup posture.
    pub mode: AuthMode,
    /// The hook gating `/mcp`; `None` only for unauthenticated loopback
    /// development.
    pub hook: Option<Arc<dyn AuthHook<S>>>,
    /// The RFC 9728 metadata naming where a caller obtains a token, when
    /// caller tokens are configured.
    pub metadata: Option<ProtectedResourceMetadata>,
}

impl<S: Send + Sync + ?Sized> McpAuth<S> {
    /// Whether a request can carry a verified caller — what a tool declaring
    /// [`ToolCapabilities::REQUIRES_AUTH`](crate::mcp::tool::ToolCapabilities::REQUIRES_AUTH)
    /// needs. A service registers its per-user tools only when this holds:
    /// without it no request could ever run them.
    #[must_use]
    pub const fn verifies_callers(&self) -> bool {
        self.metadata.is_some()
    }
}

impl<S: Send + Sync + ?Sized> fmt::Debug for McpAuth<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("McpAuth")
            .field("mode", &self.mode)
            .field("hook", &self.hook.is_some())
            .field("metadata", &self.metadata)
            .finish()
    }
}

/// Why [`resolve_mcp_auth`] refused to resolve a gate.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum McpAuthError {
    /// Neither the shared key nor caller tokens are configured and the bind
    /// is reachable: nothing would authenticate requests, so the server must
    /// not start. Its `gate` names both ways to arm it.
    InsecureBind(InsecureBindError),
    /// The caller-token variables are partly set, or name a resource
    /// identifier RFC 9728 does not allow.
    Caller(CallerConfigError),
}

impl fmt::Display for McpAuthError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InsecureBind(refused) => refused.fmt(f),
            Self::Caller(invalid) => invalid.fmt(f),
        }
    }
}

impl Error for McpAuthError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InsecureBind(refused) => Some(refused),
            Self::Caller(invalid) => Some(invalid),
        }
    }
}

impl From<InsecureBindError> for McpAuthError {
    fn from(refused: InsecureBindError) -> Self {
        Self::InsecureBind(refused)
    }
}

impl From<CallerConfigError> for McpAuthError {
    fn from(invalid: CallerConfigError) -> Self {
        Self::Caller(invalid)
    }
}

/// Resolve how `/mcp` authenticates on `host`, and say it at startup.
///
/// The service's shared key is in `key_env`, and its caller tokens are read
/// here with [`CallerTokenConfig::from_env`] under `caller_prefix`, so the
/// variables a refusal names are the ones that were read.
///
/// - Caller tokens configured: a [`CallerAuthHook`] verifies them, keys
///   fetched with `http`, every refusal answered with the metadata's
///   challenge. The shared key, when set, is admitted beside them with no
///   identity. A request carrying no credential is refused on every host,
///   loopback included: configured caller tokens are a gate, so the posture
///   is [`AuthMode::Enforced`] and nothing is served anonymously. A host that
///   wants anonymous loopback development leaves the caller variables unset.
/// - Only the shared key: [`shared_key_gate`]'s [`ApiKeyAuthHook`] with a
///   `Bearer realm="<realm>"` challenge. No request carries a verified caller.
/// - Neither: no hook, which only a loopback bind may serve.
///
/// A shared key or an anonymous loopback request never carries an identity,
/// so a tool that requires one is still refused by the registry whatever
/// this resolves.
///
/// # Errors
///
/// [`McpAuthError::Caller`] when the caller variables under `caller_prefix`
/// are partly set or name a resource identifier RFC 9728 does not allow;
/// [`McpAuthError::InsecureBind`] when neither gate is armed and `host` is
/// not loopback, its `gate` naming `key_env` and the `caller_prefix`
/// variables.
pub fn resolve_mcp_auth<S: Send + Sync + ?Sized>(
    host: &str,
    key_env: &str,
    realm: &str,
    caller_prefix: &str,
    http: Client,
) -> Result<McpAuth<S>, McpAuthError> {
    CallerTokenConfig::from_env(caller_prefix)?.map_or_else(
        || key_auth(host, key_env, realm, caller_prefix),
        |config| {
            let shared_key =
                api_key_configured(key_env).then(|| ApiKeyAuthHook::new(key_env, realm));
            caller_auth(&config, shared_key, http)
        },
    )
}

/// `/mcp` gated by the shared key alone, or by nothing on loopback: no
/// request ever carries a verified caller.
fn key_auth<S: Send + Sync + ?Sized>(
    host: &str,
    key_env: &str,
    realm: &str,
    caller_prefix: &str,
) -> Result<McpAuth<S>, McpAuthError> {
    let shared_key =
        shared_key_gate(key_env, realm, host).map_err(|refused| InsecureBindError {
            gate: Some(format!(
                "{key_env} or the {caller_prefix}_CALLER_* variables"
            )),
            ..refused
        })?;
    let mode = if shared_key.is_some() {
        info!(
            gate = key_env,
            "no caller tokens configured — no request carries a verified caller, so tools that \
             need one do not run"
        );
        AuthMode::Enforced
    } else {
        AuthMode::LoopbackDev
    };
    let hook = shared_key.map(|hook| -> Arc<dyn AuthHook<S>> { Arc::new(hook) });
    Ok(McpAuth {
        mode,
        hook,
        metadata: None,
    })
}

/// `/mcp` gated by verified caller tokens, with `shared_key` admitted beside
/// them when set.
fn caller_auth<S: Send + Sync + ?Sized>(
    config: &CallerTokenConfig,
    shared_key: Option<ApiKeyAuthHook>,
    http: Client,
) -> Result<McpAuth<S>, McpAuthError> {
    let metadata = config
        .resource_metadata()
        .map_err(CallerConfigError::InvalidResource)?;
    let keyed = shared_key.is_some();
    let verifying =
        CallerAuthHook::new(Arc::new(config.verifier(http)), metadata.www_authenticate());
    let hook = match shared_key {
        Some(shared_key) => verifying.with_shared_key(shared_key),
        None => verifying,
    };
    info!(
        resource = %config.resource,
        shared_key = keyed,
        "/mcp verifies caller tokens; the shared key, when set, is admitted with no identity"
    );
    // Caller tokens are a gate on every host, loopback included.
    Ok(McpAuth {
        mode: AuthMode::Enforced,
        hook: Some(Arc::new(hook)),
        metadata: Some(metadata),
    })
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

    // ---- resolve_mcp_auth: the gate commere and barrage resolved by hand ----

    const KEY_ENV: &str = "TRONC_MCP_AUTH_UNIT_KEY";
    const REALM: &str = "dravr-unit";
    const RESOURCE: &str = "https://svc.example.com/mcp";

    fn caller_config() -> CallerTokenConfig {
        CallerTokenConfig {
            resource: RESOURCE.to_owned(),
            issuer: "pierre-mcp-server".to_owned(),
            jwks_url: "https://api.example.com/.well-known/jwks.json".to_owned(),
            authorization_server: "https://api.example.com".to_owned(),
        }
    }

    /// Resolve with `caller` set as the `PREFIX` variables, or none of them.
    fn resolve(
        host: &str,
        caller: Option<&CallerTokenConfig>,
    ) -> Result<McpAuth<()>, McpAuthError> {
        clear();
        if let Some(config) = caller {
            let values = [
                &config.resource,
                &config.issuer,
                &config.jwks_url,
                &config.authorization_server,
            ];
            for (name, value) in CallerTokenConfig::variables(PREFIX).iter().zip(values) {
                env::set_var(name, value);
            }
        }
        let resolved = resolve_mcp_auth(host, KEY_ENV, REALM, PREFIX, Client::new());
        clear();
        resolved
    }

    async fn call(auth: &McpAuth<()>, token: Option<&str>) -> Result<ToolContext, AuthError> {
        let mut request: JsonRpcRequest =
            serde_json::from_str(r#"{"jsonrpc":"2.0","id":5,"method":"tools/list"}"#).unwrap();
        request.auth_token = token.map(str::to_owned);
        let hook = auth.hook.as_ref().expect("the resolved auth has a hook");
        hook.authenticate(&request, &Arc::new(())).await
    }

    fn challenge_of(refused: Result<ToolContext, AuthError>) -> String {
        match refused {
            Err(AuthError::Unauthorized { www_authenticate }) => www_authenticate,
            other => panic!("expected a 401 refusal, got {other:?}"),
        }
    }

    #[test]
    #[serial]
    fn neither_gate_on_a_reachable_bind_is_refused_naming_both() {
        env::remove_var(KEY_ENV);

        let refused = resolve("0.0.0.0", None).expect_err("nothing gates a reachable bind");

        assert_eq!(
            refused,
            McpAuthError::InsecureBind(InsecureBindError {
                host: "0.0.0.0".to_owned(),
                gate: Some(format!("{KEY_ENV} or the {PREFIX}_CALLER_* variables")),
            })
        );
        assert!(
            refused
                .to_string()
                .contains(&format!("Set {KEY_ENV} or the {PREFIX}_CALLER_*")),
            "the refusal says what arms the gate: {refused}"
        );
    }

    #[test]
    #[serial]
    fn neither_gate_on_loopback_serves_without_a_hook() {
        env::remove_var(KEY_ENV);

        let auth = resolve("127.0.0.1", None).unwrap();

        assert_eq!(auth.mode, AuthMode::LoopbackDev);
        assert!(auth.hook.is_none(), "loopback development installs no hook");
        assert!(
            !auth.verifies_callers(),
            "no caller tokens, no verified callers"
        );
    }

    #[tokio::test]
    #[serial]
    async fn the_shared_key_alone_is_an_api_key_gate_with_the_realm_challenge() {
        env::set_var(KEY_ENV, "k-shared");

        let auth = resolve("0.0.0.0", None).unwrap();
        let admitted = call(&auth, Some("k-shared")).await;
        let wrong = call(&auth, Some("k-other")).await;
        let missing = call(&auth, None).await;
        env::remove_var(KEY_ENV);

        assert_eq!(auth.mode, AuthMode::Enforced);
        assert!(!auth.verifies_callers(), "a key names no caller");
        assert!(auth.metadata.is_none(), "no metadata without caller tokens");
        let ctx = admitted.unwrap();
        assert_eq!(ctx.auth_method.as_deref(), Some("api_key"));
        assert_eq!(ctx.user_id, None, "the key carries no identity");
        assert_eq!(challenge_of(wrong), format!("Bearer realm=\"{REALM}\""));
        assert_eq!(challenge_of(missing), format!("Bearer realm=\"{REALM}\""));
    }

    #[tokio::test]
    #[serial]
    async fn caller_tokens_with_the_key_admit_the_key_and_refuse_no_credential() {
        env::set_var(KEY_ENV, "k-shared");
        let config = caller_config();

        let auth = resolve("0.0.0.0", Some(&config)).unwrap();
        let admitted = call(&auth, Some("k-shared")).await;
        let missing = call(&auth, None).await;
        env::remove_var(KEY_ENV);

        let metadata = config.resource_metadata().unwrap();
        assert_eq!(auth.mode, AuthMode::Enforced);
        assert!(auth.verifies_callers(), "caller tokens configured");
        assert_eq!(auth.metadata.as_ref(), Some(&metadata));
        let ctx = admitted.unwrap();
        assert_eq!(ctx.auth_method.as_deref(), Some("api_key"));
        assert_eq!(ctx.user_id, None, "the key stands in for no caller");
        assert_eq!(
            challenge_of(missing),
            metadata.www_authenticate(),
            "a request with no credential is sent to the metadata"
        );
    }

    /// commere and barrage, which this lifts, resolved the posture with caller
    /// tokens counted as a gate, so it was always `Enforced` and their
    /// anonymous-loopback branch never ran: no credential was refused even on
    /// loopback. That fail-closed behaviour is what is kept.
    #[tokio::test]
    #[serial]
    async fn caller_tokens_without_a_key_refuse_no_credential_even_on_loopback() {
        env::remove_var(KEY_ENV);
        let config = caller_config();

        let auth = resolve("127.0.0.1", Some(&config)).unwrap();
        let missing = call(&auth, None).await;

        assert_eq!(auth.mode, AuthMode::Enforced, "caller tokens are a gate");
        assert!(auth.verifies_callers());
        assert_eq!(
            challenge_of(missing),
            config.resource_metadata().unwrap().www_authenticate(),
            "nothing is served anonymously once caller tokens are configured"
        );
    }

    #[tokio::test]
    #[serial]
    async fn caller_tokens_without_a_key_off_loopback_admit_no_anonymous_request() {
        env::remove_var(KEY_ENV);
        let config = caller_config();

        let auth = resolve("0.0.0.0", Some(&config)).unwrap();
        let missing = call(&auth, None).await;
        let shared = call(&auth, Some("")).await;

        assert_eq!(auth.mode, AuthMode::Enforced);
        let expected = config.resource_metadata().unwrap().www_authenticate();
        assert_eq!(
            challenge_of(missing),
            expected,
            "anonymous is loopback-only"
        );
        assert_eq!(
            challenge_of(shared),
            expected,
            "an empty token is no credential"
        );
    }

    #[test]
    #[serial]
    fn an_invalid_resource_is_a_caller_configuration_error() {
        env::remove_var(KEY_ENV);
        let config = CallerTokenConfig {
            resource: "http://svc.example.com/mcp".to_owned(),
            ..caller_config()
        };

        let refused = resolve("0.0.0.0", Some(&config)).expect_err("plain http off loopback");

        assert!(
            matches!(
                refused,
                McpAuthError::Caller(CallerConfigError::InvalidResource(_))
            ),
            "got {refused:?}"
        );
    }

    #[test]
    #[serial]
    fn partly_set_caller_variables_are_refused_naming_the_missing_ones() {
        env::remove_var(KEY_ENV);
        clear();
        env::set_var(format!("{PREFIX}_RESOURCE_URL"), RESOURCE);

        let refused = resolve_mcp_auth::<()>("127.0.0.1", KEY_ENV, REALM, PREFIX, Client::new())
            .expect_err("a partial caller configuration never starts");
        clear();

        assert_eq!(
            refused,
            McpAuthError::Caller(CallerConfigError::Incomplete {
                missing: vec![
                    format!("{PREFIX}_CALLER_ISSUER"),
                    format!("{PREFIX}_CALLER_JWKS_URL"),
                    format!("{PREFIX}_AUTHORIZATION_SERVER"),
                ]
            })
        );
    }
}
