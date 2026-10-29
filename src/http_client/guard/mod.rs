// ABOUTME: SSRF-guarded outbound fetch: one client per policy, IP pinned by a checking resolver
// ABOUTME: Every hop is re-checked, the socket connects only to vetted addresses, bodies are capped
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

//! A fetch of a URL that someone other than the operator chose.
//!
//! [`GuardedClient`] is a `reqwest` client built for one [`FetchPolicy`]:
//!
//! - **The address is checked where it is used.** The client resolves names
//!   through its own resolver, which looks the name up once, refuses the
//!   whole answer if any address lies in the deny list, and hands the
//!   connector exactly the addresses it checked. There is no separate "check,
//!   then connect" lookup a DNS rebinding could slip between, and the URL
//!   keeps its host name, so TLS verification and the `Host` header are those
//!   of the name.
//! - **Every redirect hop is held to the policy** — scheme, credentials,
//!   port, host allowlist and literal-address rules — and its name goes
//!   through the same resolver, up to [`MAX_REDIRECTS`] hops. The `webhook`
//!   policy follows none.
//! - **No proxy is used**, since a proxy would resolve the name itself.
//! - **The body is capped** in bytes received, in idle time between bytes,
//!   and in total time ([`FetchLimits`]).
//! - **Errors carry no part of the URL** ([`GuardError`]); log
//!   [`url_digest`] to correlate one.
//!
//! ```ignore
//! let client = GuardedClient::new(FetchPolicy::archive_fetch(
//!     HostAllowlist::new(["export.example.com"]),
//! ))?;
//! let mut response = client.get(&pasted_url)?.send().await?;
//! while let Some(chunk) = response.chunk().await? {
//!     spool.write_all(&chunk).await?;
//! }
//! ```

mod deny;
mod error;
mod policy;

use std::future::Future;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use futures::future::BoxFuture;
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use reqwest::redirect::{Attempt, Policy};
use reqwest::{Body, Method, StatusCode, Url};
use ring::digest::{digest, SHA256};
use serde::Serialize;
use tokio::net::lookup_host;
use tokio::time::{timeout_at, Instant};
use url::Host;

pub use deny::is_denied_ip;
pub use error::GuardError;
pub use policy::{FetchLimits, FetchPolicy, HostAllowlist, InvalidAllowlistEntry, MAX_REDIRECTS};

/// The SHA-256 of a URL, in lowercase hex: what a log line or a span carries
/// in place of a URL that may hold a signed token.
#[must_use]
pub fn url_digest(url: &str) -> String {
    hex::encode(digest(&SHA256, url.as_bytes()).as_ref())
}

/// Looks a host name up. The guard checks every address it returns.
///
/// [`SystemResolver`] is the production one; a test supplies its own to
/// answer with chosen addresses.
pub trait HostResolver: Send + Sync + 'static {
    /// Every address `host` currently resolves to.
    fn lookup(&self, host: &str) -> BoxFuture<'static, io::Result<Vec<IpAddr>>>;
}

/// The operating system's resolver, through `tokio::net::lookup_host`.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemResolver;

impl HostResolver for SystemResolver {
    fn lookup(&self, host: &str) -> BoxFuture<'static, io::Result<Vec<IpAddr>>> {
        let host = host.to_owned();
        Box::pin(async move {
            Ok(lookup_host((host.as_str(), 0))
                .await?
                .map(|addr| addr.ip())
                .collect())
        })
    }
}

/// Which resolved addresses a client refuses.
#[derive(Debug, Clone, Copy)]
struct AddressRule {
    deny: bool,
    permit_loopback: bool,
}

impl AddressRule {
    fn refuses(self, ip: IpAddr) -> bool {
        if !self.deny {
            return false;
        }
        if self.permit_loopback && is_loopback(ip) {
            return false;
        }
        is_denied_ip(ip)
    }
}

fn is_loopback(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_loopback(),
        IpAddr::V6(v6) => {
            v6.is_loopback() || v6.to_ipv4_mapped().is_some_and(|v4| v4.is_loopback())
        }
    }
}

/// The resolver the client connects through: one lookup, every address
/// checked, and only checked addresses returned to the connector.
struct PinningResolver {
    inner: Arc<dyn HostResolver>,
    rule: AddressRule,
}

impl PinningResolver {
    async fn vetted(
        inner: Arc<dyn HostResolver>,
        rule: AddressRule,
        host: String,
    ) -> Result<Vec<IpAddr>, GuardError> {
        let addresses = inner
            .lookup(&host)
            .await
            .map_err(|_| GuardError::Unresolvable)?;
        if addresses.is_empty() {
            return Err(GuardError::Unresolvable);
        }
        // One denied address refuses the whole answer: a name that resolves
        // to both a public and an internal address is an attack, not a
        // choice between them.
        if addresses.iter().any(|&ip| rule.refuses(ip)) {
            return Err(GuardError::DeniedAddress);
        }
        Ok(addresses)
    }
}

impl Resolve for PinningResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let inner = Arc::clone(&self.inner);
        let rule = self.rule;
        let host = name.as_str().to_owned();
        Box::pin(async move {
            let addresses = Self::vetted(inner, rule, host).await?;
            // Port 0: the connector puts the URL's port on each address.
            let addrs: Addrs = Box::new(addresses.into_iter().map(|ip| SocketAddr::new(ip, 0)));
            Ok(addrs)
        })
    }
}

/// Builds a [`GuardedClient`].
pub struct GuardedClientBuilder {
    policy: FetchPolicy,
    limits: FetchLimits,
    resolver: Arc<dyn HostResolver>,
    permit_loopback: bool,
}

impl GuardedClientBuilder {
    /// Replace the policy's default caps.
    #[must_use]
    pub const fn limits(mut self, limits: FetchLimits) -> Self {
        self.limits = limits;
        self
    }

    /// Resolve names through `resolver` instead of the system's.
    #[must_use]
    pub fn resolver(mut self, resolver: Arc<dyn HostResolver>) -> Self {
        self.resolver = resolver;
        self
    }

    /// Let the client reach loopback addresses (`127.0.0.0/8`, `::1`) despite
    /// the deny list, so a test can serve the fetch from a local listener.
    /// Every other denied range stays denied.
    #[cfg(feature = "testkit")]
    #[must_use]
    pub const fn permit_loopback_for_tests(mut self) -> Self {
        self.permit_loopback = true;
        self
    }

    /// Build the client.
    ///
    /// # Errors
    ///
    /// [`GuardError::Transport`] when the TLS backend cannot be initialised.
    pub fn build(self) -> Result<GuardedClient, GuardError> {
        let rule = AddressRule {
            deny: self.policy.applies_deny_list(),
            permit_loopback: self.permit_loopback,
        };
        let redirect = match self.policy.max_redirects() {
            0 => Policy::none(),
            max => {
                let policy = self.policy.clone();
                Policy::custom(move |attempt: Attempt<'_>| {
                    // `previous` holds the original URL and every hop
                    // followed so far, so its length is this hop's number.
                    if attempt.previous().len() > max {
                        return attempt.error(GuardError::TooManyRedirects);
                    }
                    match policy.check(attempt.url(), |ip| rule.refuses(ip)) {
                        Ok(()) => attempt.follow(),
                        Err(refusal) => attempt.error(refusal),
                    }
                })
            }
        };
        let client = reqwest::Client::builder()
            .no_proxy()
            .dns_resolver(Arc::new(PinningResolver {
                inner: Arc::clone(&self.resolver),
                rule,
            }))
            .redirect(redirect)
            .referer(false)
            .https_only(self.policy.https_only())
            .connect_timeout(self.limits.connect_timeout)
            .timeout(self.limits.total_timeout)
            .build()
            .map_err(GuardError::from_reqwest)?;
        Ok(GuardedClient {
            policy: self.policy,
            limits: self.limits,
            rule,
            resolver: self.resolver,
            client,
        })
    }
}

/// A `reqwest` client that fetches only what its [`FetchPolicy`] allows.
///
/// Build one per policy and keep it: it pools connections like any client.
pub struct GuardedClient {
    policy: FetchPolicy,
    limits: FetchLimits,
    rule: AddressRule,
    resolver: Arc<dyn HostResolver>,
    client: reqwest::Client,
}

impl GuardedClient {
    /// A builder for a client under `policy`, starting from the policy's
    /// default caps and the system resolver.
    #[must_use]
    pub fn builder(policy: FetchPolicy) -> GuardedClientBuilder {
        GuardedClientBuilder {
            limits: policy.default_limits(),
            policy,
            resolver: Arc::new(SystemResolver),
            permit_loopback: false,
        }
    }

    /// A client under `policy` with its default caps.
    ///
    /// # Errors
    ///
    /// [`GuardError::Transport`] when the TLS backend cannot be initialised.
    pub fn new(policy: FetchPolicy) -> Result<Self, GuardError> {
        Self::builder(policy).build()
    }

    /// The caps this client applies.
    #[must_use]
    pub const fn limits(&self) -> FetchLimits {
        self.limits
    }

    /// Check `url` against every rule that needs no DNS: scheme, credentials,
    /// port, host allowlist, local names and a literal address.
    ///
    /// # Errors
    ///
    /// The [`GuardError`] naming the first rule the URL breaks.
    pub fn check_url(&self, url: &str) -> Result<Url, GuardError> {
        let parsed = Url::parse(url).map_err(|_| GuardError::InvalidUrl)?;
        self.policy.check(&parsed, |ip| self.rule.refuses(ip))?;
        Ok(parsed)
    }

    /// Check `url` as [`check_url`](Self::check_url) does, then resolve its
    /// host and check every address — for a URL stored now and fetched later.
    ///
    /// A fetch re-checks at connect time whatever this said, since the name
    /// may resolve elsewhere by then.
    ///
    /// # Errors
    ///
    /// The [`GuardError`] naming the first rule the URL breaks.
    pub async fn validate(&self, url: &str) -> Result<(), GuardError> {
        let parsed = self.check_url(url)?;
        if let Some(Host::Domain(domain)) = parsed.host() {
            PinningResolver::vetted(Arc::clone(&self.resolver), self.rule, domain.to_owned())
                .await?;
        }
        Ok(())
    }

    /// A request of `method` to `url`, once `url` passes
    /// [`check_url`](Self::check_url).
    ///
    /// # Errors
    ///
    /// The [`GuardError`] naming the first rule the URL breaks.
    pub fn request(&self, method: Method, url: &str) -> Result<GuardedRequest, GuardError> {
        let parsed = self.check_url(url)?;
        Ok(GuardedRequest {
            builder: self.client.request(method, parsed),
            limits: self.limits,
        })
    }

    /// A `GET` of `url`.
    ///
    /// # Errors
    ///
    /// The [`GuardError`] naming the first rule the URL breaks.
    pub fn get(&self, url: &str) -> Result<GuardedRequest, GuardError> {
        self.request(Method::GET, url)
    }

    /// A `POST` to `url`.
    ///
    /// # Errors
    ///
    /// The [`GuardError`] naming the first rule the URL breaks.
    pub fn post(&self, url: &str) -> Result<GuardedRequest, GuardError> {
        self.request(Method::POST, url)
    }
}

/// A request a [`GuardedClient`] has accepted, not yet sent.
pub struct GuardedRequest {
    builder: reqwest::RequestBuilder,
    limits: FetchLimits,
}

impl GuardedRequest {
    /// Add a header.
    #[must_use]
    pub fn header(mut self, name: HeaderName, value: HeaderValue) -> Self {
        self.builder = self.builder.header(name, value);
        self
    }

    /// Send `value` as a JSON body.
    #[must_use]
    pub fn json<T: Serialize + ?Sized>(mut self, value: &T) -> Self {
        self.builder = self.builder.json(value);
        self
    }

    /// Send `body` as the request body.
    #[must_use]
    pub fn body(mut self, body: impl Into<Body>) -> Self {
        self.builder = self.builder.body(body);
        self
    }

    /// Send the request and wait for the response head.
    ///
    /// # Errors
    ///
    /// A guard refusal raised on a redirect hop or at connect time, a cap that
    /// ran out, or [`GuardError::Transport`].
    pub async fn send(self) -> Result<GuardedResponse, GuardError> {
        let deadline = cap_after(Instant::now(), self.limits.total_timeout);
        let response = within(deadline, self.limits.idle_timeout, self.builder.send())
            .await?
            .map_err(GuardError::from_reqwest)?;
        Ok(GuardedResponse {
            inner: response,
            deadline,
            idle_timeout: self.limits.idle_timeout,
            max_body_bytes: self.limits.max_body_bytes,
            received: 0,
        })
    }
}

/// A response whose body is read under the client's caps.
pub struct GuardedResponse {
    inner: reqwest::Response,
    deadline: Instant,
    idle_timeout: Duration,
    max_body_bytes: u64,
    received: u64,
}

impl GuardedResponse {
    /// The response status.
    #[must_use]
    pub fn status(&self) -> StatusCode {
        self.inner.status()
    }

    /// The response headers.
    #[must_use]
    pub fn headers(&self) -> &HeaderMap {
        self.inner.headers()
    }

    /// The next piece of the body, or `None` once it is complete.
    ///
    /// # Errors
    ///
    /// [`GuardError::BodyTooLarge`] once the bytes received pass the cap,
    /// [`GuardError::IdleTimeout`] or [`GuardError::TotalTimeout`] when a cap
    /// runs out, or [`GuardError::Transport`].
    pub async fn chunk(&mut self) -> Result<Option<Bytes>, GuardError> {
        let next = within(self.deadline, self.idle_timeout, self.inner.chunk())
            .await?
            .map_err(GuardError::from_reqwest)?;
        if let Some(chunk) = &next {
            self.received = self.received.saturating_add(chunk.len() as u64);
            if self.received > self.max_body_bytes {
                return Err(GuardError::BodyTooLarge {
                    limit: self.max_body_bytes,
                });
            }
        }
        Ok(next)
    }

    /// The whole body.
    ///
    /// # Errors
    ///
    /// As [`chunk`](Self::chunk).
    pub async fn bytes(mut self) -> Result<Vec<u8>, GuardError> {
        let mut body = Vec::new();
        while let Some(chunk) = self.chunk().await? {
            body.extend_from_slice(&chunk);
        }
        Ok(body)
    }
}

/// How far a cap can reach: a duration past it (`Duration::MAX` for "no cap")
/// is clamped here rather than overflowing `Instant`, which would panic.
const FURTHEST_CAP: Duration = Duration::from_hours(30 * 365 * 24);

/// The instant `cap` after `start`, clamped to [`FURTHEST_CAP`].
fn cap_after(start: Instant, cap: Duration) -> Instant {
    start.checked_add(cap.min(FURTHEST_CAP)).unwrap_or(start)
}

/// Run `future` until the total deadline or the idle cap, whichever is first.
async fn within<T>(
    deadline: Instant,
    idle_timeout: Duration,
    future: impl Future<Output = T>,
) -> Result<T, GuardError> {
    let idle_deadline = cap_after(Instant::now(), idle_timeout);
    let (cap, refusal) = if idle_deadline < deadline {
        (idle_deadline, GuardError::IdleTimeout)
    } else {
        (deadline, GuardError::TotalTimeout)
    };
    timeout_at(cap, future).await.map_err(|_| refusal)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_digest_is_stable_lowercase_hex_and_hides_the_url() {
        let digest = url_digest("https://export.example.com/a.zip?token=SECRET");
        assert_eq!(digest.len(), 64);
        assert!(digest
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        assert!(!digest.contains("SECRET"));
        assert_eq!(
            digest,
            url_digest("https://export.example.com/a.zip?token=SECRET")
        );
    }

    #[test]
    fn the_loopback_permission_lifts_only_loopback() {
        let rule = AddressRule {
            deny: true,
            permit_loopback: true,
        };
        assert!(!rule.refuses("127.0.0.1".parse().expect("ip"))); // Safe: test assertion
        assert!(!rule.refuses("::1".parse().expect("ip"))); // Safe: test assertion
        assert!(rule.refuses("169.254.169.254".parse().expect("ip"))); // Safe: test assertion
        assert!(rule.refuses("10.0.0.1".parse().expect("ip"))); // Safe: test assertion
    }

    #[tokio::test]
    async fn a_mixed_answer_is_refused_whole() {
        struct Mixed;
        impl HostResolver for Mixed {
            fn lookup(&self, _host: &str) -> BoxFuture<'static, io::Result<Vec<IpAddr>>> {
                Box::pin(async {
                    Ok(vec![
                        "93.184.216.34".parse().expect("ip"),   // Safe: test assertion
                        "169.254.169.254".parse().expect("ip"), // Safe: test assertion
                    ])
                })
            }
        }
        let rule = AddressRule {
            deny: true,
            permit_loopback: false,
        };
        let refusal = PinningResolver::vetted(Arc::new(Mixed), rule, "a.example".into())
            .await
            .expect_err("a denied address in the answer refuses it"); // Safe: test assertion
        assert_eq!(refusal, GuardError::DeniedAddress);
    }
}
