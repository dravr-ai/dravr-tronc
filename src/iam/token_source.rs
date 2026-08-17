// ABOUTME: Caller half of service-to-service auth — mints Google identity tokens for a target audience
// ABOUTME: Reads the GCE/Cloud Run metadata server, caches, and refreshes before expiry
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use std::time::{Duration, Instant};

use reqwest::Client;
use tokio::sync::RwLock;
use tracing::debug;

use super::error::IamError;

/// Where a workload running on Google infrastructure asks for its own identity.
///
/// Only resolves inside Google infrastructure, which is the point: the token is
/// minted by the platform for the attached service account, so nothing has to
/// hold a signing key.
const METADATA_IDENTITY_URL: &str =
    "http://metadata.google.internal/computeMetadata/v1/instance/service-accounts/default/identity";

/// How long a minted token is reused before another is fetched.
///
/// Google issues these with an hour of life. Refreshing at 45 minutes leaves a
/// quarter-hour of margin, which covers clock skew between us and the verifier
/// and a slow refresh under load. Tracked from the fetch rather than parsed out
/// of the `exp` claim: the token is opaque to its holder by design, and a
/// caller that starts decoding its own credentials to decide when to renew is
/// one refactor away from trusting what it decoded.
const REFRESH_AFTER: Duration = Duration::from_mins(45);

/// A cached token and when it was obtained.
struct Cached {
    token: String,
    fetched_at: Instant,
}

/// Mints and caches identity tokens for one audience.
///
/// One per callee: the audience is baked into the token, so a source pointed at
/// service A produces tokens service B will refuse. That is the intended
/// behaviour — it stops a token leaked from one hop being replayed at another.
pub struct IdTokenSource {
    audience: String,
    http: Client,
    cached: RwLock<Option<Cached>>,
}

impl IdTokenSource {
    /// Build a source for one target audience.
    ///
    /// The audience is the callee's base URL, exactly as the callee expects it
    /// — for Cloud Run, the service URL with no trailing slash and no path.
    #[must_use]
    pub fn new(audience: impl Into<String>, http: Client) -> Self {
        Self {
            audience: audience.into(),
            http,
            cached: RwLock::new(None),
        }
    }

    /// The audience this source mints for.
    #[must_use]
    pub fn audience(&self) -> &str {
        &self.audience
    }

    /// A valid identity token, from cache when one is fresh enough.
    ///
    /// # Errors
    ///
    /// [`IamError::MetadataUnavailable`] when the metadata server cannot be
    /// reached or refuses — which is the normal answer off Google
    /// infrastructure, so a local developer sees a clear cause rather than a
    /// confusing 401 from the callee.
    pub async fn token(&self) -> Result<String, IamError> {
        if let Some(cached) = self.cached.read().await.as_ref() {
            if cached.fetched_at.elapsed() < REFRESH_AFTER {
                return Ok(cached.token.clone());
            }
        }

        let token = self.fetch().await?;

        // Re-check under the write lock rather than assuming this task is the
        // only refresher. Several requests can find the cache stale at once;
        // without this they each overwrite the entry in an arbitrary order and
        // the newest token can lose to a slower one that started earlier.
        let mut guard = self.cached.write().await;
        match guard.as_ref() {
            Some(existing) if existing.fetched_at.elapsed() < REFRESH_AFTER => {
                Ok(existing.token.clone())
            }
            _ => {
                *guard = Some(Cached {
                    token: token.clone(),
                    fetched_at: Instant::now(),
                });
                Ok(token)
            }
        }
    }

    /// Ask the metadata server for a fresh token.
    async fn fetch(&self) -> Result<String, IamError> {
        let response = self
            .http
            .get(METADATA_IDENTITY_URL)
            .query(&[("audience", self.audience.as_str()), ("format", "full")])
            .header("Metadata-Flavor", "Google")
            .send()
            .await
            .map_err(|e| IamError::MetadataUnavailable(e.to_string()))?;

        let status = response.status();
        if !status.is_success() {
            return Err(IamError::MetadataUnavailable(format!(
                "metadata server answered {status}"
            )));
        }

        let token = response
            .text()
            .await
            .map_err(|e| IamError::MalformedToken(e.to_string()))?;

        // A JWT is three dot-separated segments. Checking the shape here turns a
        // proxy's HTML error page — which arrives with a 200 — into a named
        // failure instead of a token the callee rejects for reasons we cannot
        // see from this side.
        if token.split('.').count() != 3 {
            return Err(IamError::MalformedToken(
                "response was not a three-segment JWT".to_owned(),
            ));
        }

        debug!(audience = %self.audience, "minted a fresh identity token");
        Ok(token)
    }
}
