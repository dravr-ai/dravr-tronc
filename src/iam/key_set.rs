// ABOUTME: Google's published signing keys behind one key-set URL, cached, with a refetch cooldown
// ABOUTME: The one cache for every Google-signed token a service checks, whoever minted it
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use std::time::{Duration, Instant};

use reqwest::Client;
use serde::Deserialize;
use tokio::sync::{Mutex, RwLock};
use tracing::debug;

use super::error::IamError;
use crate::http_client::describe_request_error;

/// Google's key set for the identity tokens `accounts.google.com` signs, which
/// is what a service account presents to another service.
///
/// [`super::GoogleIdTokenVerifier::new`] reads it. It is public so a host that
/// builds its verifier from configuration can name the production URL rather
/// than copy it.
pub const GOOGLE_OIDC_JWKS_URL: &str = "https://www.googleapis.com/oauth2/v3/certs";

/// How long a fetched key set is reused before it is fetched again.
///
/// Google rotates on the order of days, so an hour is conservative. A `kid`
/// miss can refetch sooner, which is what covers a rotation; this bound only
/// stops the cache going stale forever on a quiet service.
const KEY_SET_TTL: Duration = Duration::from_hours(1);

/// The least time between two fetches of one key set.
///
/// A token names its signing key by `kid`, and a `kid` the cache does not hold
/// is how a rotation first shows up, so a miss has to be able to refetch. But
/// the token is presented before anything about it is verified: without a
/// floor, anyone can send tokens with made-up `kid`s and turn each request into
/// an outbound fetch from Google.
///
/// Thirty seconds holds that to two fetches a minute per key set, far below
/// anything Google would throttle, which matters because a throttled key-set
/// fetch refuses every honest caller too. It costs a real rotation nothing:
/// Google publishes a key well before signing with it, so the hourly fetch
/// already holds it. The one case it bites is a new key used within thirty
/// seconds of a fetch that did not yet carry it, and those tokens are refused
/// for at most the remainder of the interval.
pub const MIN_REFETCH_INTERVAL: Duration = Duration::from_secs(30);

/// One RSA public key from a Google key set.
///
/// The modulus and exponent are the base64url strings the set publishes, which
/// is what `jsonwebtoken`'s `DecodingKey::from_rsa_components` takes in 9 and
/// 10 alike — so a caller builds its decoding key with the `jsonwebtoken` it
/// depends on, whichever release this crate's own verifier uses.
#[derive(Debug, Clone, Deserialize)]
pub struct GoogleSigningKey {
    kid: String,
    n: String,
    e: String,
}

impl GoogleSigningKey {
    /// The key id tokens signed with this key carry in their header.
    #[must_use]
    pub fn kid(&self) -> &str {
        &self.kid
    }

    /// The RSA modulus, base64url without padding.
    #[must_use]
    pub fn modulus(&self) -> &str {
        &self.n
    }

    /// The RSA public exponent, base64url without padding.
    #[must_use]
    pub fn exponent(&self) -> &str {
        &self.e
    }
}

/// The key set document, as Google publishes it.
#[derive(Deserialize)]
struct KeySetDocument {
    keys: Vec<GoogleSigningKey>,
}

/// A fetched key set and when the fetch that brought it finished.
struct FetchedKeys {
    keys: Vec<GoogleSigningKey>,
    fetched_at: Instant,
}

/// What the cache remembers between lookups.
struct CacheState {
    /// The last key set fetched successfully.
    fetched: Option<FetchedKeys>,
    /// When the last fetch finished, whether or not it succeeded. The interval
    /// runs from here, so an endpoint that is failing is not retried once per
    /// request either. Set when a fetch ends rather than when it starts: a
    /// lookup arriving while the first fetch is in flight must wait for it,
    /// not read "a fetch just happened and left no set" as a failure.
    last_attempt: Option<Instant>,
}

/// Google's signing keys behind one key-set URL.
///
/// Google publishes the public half of the keys that sign its tokens as a JWK
/// set, one URL per signer: [`GOOGLE_OIDC_JWKS_URL`] for identity tokens,
/// `https://www.googleapis.com/service_accounts/v1/jwk/securetoken@system.gserviceaccount.com`
/// for Firebase Authentication ID tokens. This type is the cache over one of
/// them; what a token must claim is the verifier's business, not the cache's.
///
/// # Refetching
///
/// A set is reused for an hour. A `kid` missing from it forces a refetch, so a
/// rotation is picked up without waiting out the hour, but no fetch starts
/// within [`MIN_REFETCH_INTERVAL`] of the end of the previous one. A `kid` still
/// unknown after a refetch is therefore refused from memory until the next
/// refetch is allowed: presenting it again, or any other unknown `kid`, costs
/// nothing outbound. Concurrent lookups that need a fetch share one.
pub struct GoogleKeySet {
    url: String,
    http: Client,
    min_refetch_interval: Duration,
    state: RwLock<CacheState>,
    /// Held for the length of a fetch, so lookups that need one while it is in
    /// flight wait for its result instead of starting their own.
    fetching: Mutex<()>,
}

impl GoogleKeySet {
    /// A cache over the key set served at `url`, fetched with `http`.
    #[must_use]
    pub fn new(url: impl Into<String>, http: Client) -> Self {
        Self {
            url: url.into(),
            http,
            min_refetch_interval: MIN_REFETCH_INTERVAL,
            state: RwLock::new(CacheState {
                fetched: None,
                last_attempt: None,
            }),
            fetching: Mutex::new(()),
        }
    }

    /// Change the least time between two fetches from [`MIN_REFETCH_INTERVAL`].
    ///
    /// Longer bounds outbound fetches more tightly; shorter picks up a key
    /// published after the last fetch sooner. An interval longer than the
    /// hour a set is reused for is cut to that hour: past it the set is due
    /// for a refresh anyway, and refusing to fetch a set that has expired
    /// would refuse every token.
    #[must_use]
    pub fn with_min_refetch_interval(mut self, interval: Duration) -> Self {
        self.min_refetch_interval = interval.min(KEY_SET_TTL);
        self
    }

    /// The signing key `kid` names.
    ///
    /// # Errors
    ///
    /// [`IamError::Rejected`] when the current set has no key under `kid` and
    /// the set cannot be refetched yet, or was refetched and still has none.
    /// [`IamError::JwksUnavailable`] when the set could not be fetched, and,
    /// until the interval has passed, when the attempt that failed left no
    /// current set to answer from.
    pub async fn key(&self, kid: &str) -> Result<GoogleSigningKey, IamError> {
        if let Some(answer) = self.answer_from_memory(kid).await {
            return answer;
        }

        let turn = self.fetching.lock().await;
        // Whoever held the lock before this task may have fetched the key it
        // needs, or started the interval that forbids another fetch.
        let answer = match self.answer_from_memory(kid).await {
            Some(answer) => answer,
            None => self.fetch(kid).await,
        };
        drop(turn);
        answer
    }

    /// The answer the cache gives without fetching, or `None` when a fetch is
    /// both needed and allowed.
    async fn answer_from_memory(&self, kid: &str) -> Option<Result<GoogleSigningKey, IamError>> {
        let state = self.state.read().await;
        let now = Instant::now();
        let cooling_down = state
            .last_attempt
            .is_some_and(|at| now.duration_since(at) < self.min_refetch_interval);

        match &state.fetched {
            Some(set) if now.duration_since(set.fetched_at) < KEY_SET_TTL => {
                if let Some(key) = set.keys.iter().find(|k| k.kid == kid) {
                    return Some(Ok(key.clone()));
                }
                cooling_down.then(|| Err(unknown_kid(kid)))
            }
            // An attempt that succeeded inside the interval left a current set,
            // since the interval never outlasts a set, so having none here
            // means the last attempt failed.
            _ => cooling_down.then(|| {
                Err(IamError::JwksUnavailable(format!(
                    "the last fetch failed less than {:?} ago",
                    self.min_refetch_interval
                )))
            }),
        }
    }

    /// Fetch the set, keep it, and look `kid` up in it.
    async fn fetch(&self, kid: &str) -> Result<GoogleSigningKey, IamError> {
        let downloaded = self.download().await;
        let finished = Instant::now();

        let mut state = self.state.write().await;
        state.last_attempt = Some(finished);
        let keys = downloaded?;
        let found = keys.iter().find(|k| k.kid == kid).cloned();
        state.fetched = Some(FetchedKeys {
            keys,
            fetched_at: finished,
        });
        drop(state);

        found.ok_or_else(|| unknown_kid(kid))
    }

    /// One request for the key set.
    async fn download(&self) -> Result<Vec<GoogleSigningKey>, IamError> {
        let document: KeySetDocument = self
            .http
            .get(&self.url)
            .send()
            .await
            .map_err(|e| IamError::JwksUnavailable(describe_request_error(e)))?
            .json()
            .await
            .map_err(|e| IamError::JwksUnavailable(describe_request_error(e)))?;
        debug!(url = %self.url, keys = document.keys.len(), "fetched Google signing keys");
        Ok(document.keys)
    }
}

/// The refusal for a `kid` the current set does not carry.
fn unknown_kid(kid: &str) -> IamError {
    IamError::Rejected(format!("no Google signing key for kid {kid}"))
}
