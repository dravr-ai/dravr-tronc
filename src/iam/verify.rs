// ABOUTME: Callee half of service-to-service auth — verifies Google-signed identity tokens
// ABOUTME: Defence in depth behind Cloud Run's own IAM check, and the whole gate anywhere else
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::Request;
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use jsonwebtoken::{decode, decode_header, Algorithm, DecodingKey, Validation};
use reqwest::Client;
use serde::Deserialize;
use tokio::sync::RwLock;
use tracing::warn;

use super::error::IamError;
use crate::error::ErrorResponse;

/// Google's published signing keys for identity tokens.
const GOOGLE_JWKS_URL: &str = "https://www.googleapis.com/oauth2/v3/certs";

/// Accepted issuers. Google mints with both spellings.
const GOOGLE_ISSUERS: [&str; 2] = ["https://accounts.google.com", "accounts.google.com"];

/// How long signing keys are reused.
///
/// Google rotates on the order of days, so an hour is conservative. A `kid`
/// miss also forces a refetch, which is what actually covers rotation — this
/// bound only stops the cache going stale forever on a quiet service.
const JWKS_TTL: Duration = Duration::from_hours(1);

/// One RSA key from the JWK set.
#[derive(Debug, Clone, Deserialize)]
struct Jwk {
    kid: String,
    n: String,
    e: String,
}

/// The JWK set as Google publishes it.
#[derive(Debug, Clone, Deserialize)]
struct Jwks {
    keys: Vec<Jwk>,
}

/// A fetched key set and when it arrived.
struct CachedJwks {
    jwks: Jwks,
    fetched_at: Instant,
}

/// The claims this verifier cares about.
#[derive(Debug, Deserialize)]
pub struct IdTokenClaims {
    /// Subject — the service account's numeric id.
    pub sub: String,
    /// Audience the token was minted for.
    pub aud: String,
    /// Service account email, present on `format=full` tokens.
    #[serde(default)]
    pub email: Option<String>,
}

/// Verifies Google identity tokens for one audience.
///
/// On Cloud Run with `allow_unauthenticated = false` the platform has already
/// checked the token and `run.invoker` before the request reaches the process,
/// so this is a second, independent check rather than the only one. It earns
/// its place in three ways: it survives someone flipping the service to
/// unauthenticated, it is the *entire* gate anywhere that is not Cloud Run
/// (local, Compute Engine, another cloud), and the caller allowlist expresses
/// something IAM alone does not — that this service expects exactly these
/// callers, not merely some principal holding `run.invoker`.
pub struct GoogleIdTokenVerifier {
    audience: String,
    allowed_emails: HashSet<String>,
    http: Client,
    jwks: RwLock<Option<CachedJwks>>,
}

impl GoogleIdTokenVerifier {
    /// Verify tokens minted for `audience`, from any Google caller.
    ///
    /// The audience must match what the caller asked for exactly — for Cloud
    /// Run, the service URL with no trailing slash.
    #[must_use]
    pub fn new(audience: impl Into<String>, http: Client) -> Self {
        Self {
            audience: audience.into(),
            allowed_emails: HashSet::new(),
            http,
            jwks: RwLock::new(None),
        }
    }

    /// Restrict to specific service-account emails.
    ///
    /// An empty allowlist accepts any caller whose token carries the right
    /// audience, which on Cloud Run means anyone IAM granted `run.invoker`.
    /// Naming the callers narrows that to the ones actually expected.
    #[must_use]
    pub fn allowing(mut self, emails: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.allowed_emails = emails.into_iter().map(Into::into).collect();
        self
    }

    /// Check one bearer token.
    ///
    /// # Errors
    ///
    /// [`IamError::Rejected`] when the token is malformed, signed by a key that
    /// is not Google's, minted for another audience, expired, or from a caller
    /// outside the allowlist. [`IamError::JwksUnavailable`] when the signing
    /// keys could not be fetched — kept distinct so an outage fetching keys is
    /// not recorded as somebody presenting a bad token.
    pub async fn verify(&self, token: &str) -> Result<IdTokenClaims, IamError> {
        let header = decode_header(token).map_err(|e| IamError::Rejected(e.to_string()))?;
        let kid = header
            .kid
            .ok_or_else(|| IamError::Rejected("token header carries no kid".to_owned()))?;

        let key = match self.key_for(&kid, false).await? {
            Some(key) => key,
            // Unknown `kid` means rotation, so refetch once before rejecting.
            // Without this a rotation locks every caller out until the TTL
            // lapses, which is an outage caused by the defence rather than by
            // anything an attacker did.
            None => self.key_for(&kid, true).await?.ok_or_else(|| {
                IamError::Rejected(format!("no Google signing key for kid {kid}"))
            })?,
        };

        let decoding_key = DecodingKey::from_rsa_components(&key.n, &key.e)
            .map_err(|e| IamError::Rejected(e.to_string()))?;

        // Validation enforces exp itself; audience and issuer are set here so a
        // token minted for a different service, or by a different issuer, is
        // refused by the library rather than by hand afterwards.
        let mut validation = Validation::new(Algorithm::RS256);
        validation.set_audience(&[self.audience.as_str()]);
        validation.set_issuer(&GOOGLE_ISSUERS);

        let data = decode::<IdTokenClaims>(token, &decoding_key, &validation)
            .map_err(|e| IamError::Rejected(e.to_string()))?;

        if !self.allowed_emails.is_empty() {
            let email = data.claims.email.as_deref().unwrap_or_default();
            if !self.allowed_emails.contains(email) {
                return Err(IamError::Rejected(format!(
                    "caller {email} is not in this service's allowlist"
                )));
            }
        }

        Ok(data.claims)
    }

    /// Look up a signing key, optionally forcing a refetch first.
    async fn key_for(&self, kid: &str, force_refresh: bool) -> Result<Option<Jwk>, IamError> {
        if !force_refresh {
            if let Some(cached) = self.jwks.read().await.as_ref() {
                if cached.fetched_at.elapsed() < JWKS_TTL {
                    return Ok(cached.jwks.keys.iter().find(|k| k.kid == kid).cloned());
                }
            }
        }

        let jwks: Jwks = self
            .http
            .get(GOOGLE_JWKS_URL)
            .send()
            .await
            .map_err(|e| IamError::JwksUnavailable(e.to_string()))?
            .json()
            .await
            .map_err(|e| IamError::JwksUnavailable(e.to_string()))?;

        let found = jwks.keys.iter().find(|k| k.kid == kid).cloned();
        *self.jwks.write().await = Some(CachedJwks {
            jwks,
            fetched_at: Instant::now(),
        });
        Ok(found)
    }
}

/// Axum middleware requiring a valid Google identity token.
///
/// Fails closed: no header, wrong scheme, or a token that does not verify all
/// end the request at 401. This is deliberately unlike
/// [`crate::server::auth::require_auth`], which passes everything through when
/// its key is unset — a default that is survivable for a service reachable only
/// from inside a cluster, and not for one on a public URL.
///
/// # Usage
///
/// ```rust,ignore
/// use std::sync::Arc;
/// use axum::middleware;
/// use dravr_tronc::iam::{require_google_id_token, GoogleIdTokenVerifier};
///
/// let verifier = Arc::new(GoogleIdTokenVerifier::new(service_url, http));
/// let app = Router::new()
///     .route("/render", post(handler))
///     .layer(middleware::from_fn(move |req, next| {
///         let verifier = Arc::clone(&verifier);
///         async move { require_google_id_token(verifier, req, next).await }
///     }));
/// ```
pub async fn require_google_id_token(
    verifier: Arc<GoogleIdTokenVerifier>,
    request: Request,
    next: Next,
) -> Response {
    let presented = request
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "));

    let Some(token) = presented else {
        return unauthorized("Missing bearer identity token");
    };

    match verifier.verify(token).await {
        Ok(_) => next.run(request).await,
        Err(why) => {
            // Logged with the reason, answered without it. The caller learns
            // that it failed, not which claim to adjust.
            warn!(error = %why, "rejected an identity token");
            unauthorized("Invalid identity token")
        }
    }
}

/// The one shape of refusal this middleware returns.
fn unauthorized(message: &str) -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(ErrorResponse::new("unauthorized", message)),
    )
        .into_response()
}
