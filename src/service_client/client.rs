// ABOUTME: ServiceClient: an HTTP client for one dravr service, authenticated by Google ID token
// ABOUTME: Sends each attempt under a fresh request id and re-sends a GET once when its connection closed unanswered
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use std::env;
use std::sync::Arc;
use std::time::{Duration, Instant};

use reqwest::header::{HeaderValue, AUTHORIZATION};
use reqwest::{Client, Method, RequestBuilder, Url};
use serde::Serialize;
use tracing::{debug, warn};

use super::error::{ConfigError, ServiceError};
use super::response::{classify, Exchange, ServiceResponse};
use super::transport::TransportFailure;
use crate::http_client::describe_request_error;
use crate::iam::{IamError, IdTokenSource};
use crate::server::auth::is_loopback_host;
use crate::server::request_guard::{RequestId, REQUEST_ID_HEADER};
#[cfg(feature = "otel")]
use crate::server::trace_context::inject_current_context;

/// Wait reported for a shed that names none (a gateway's bare 503).
pub const DEFAULT_SHED_RETRY_AFTER_SECS: u64 = 30;

/// An HTTP client for one dravr service.
///
/// Cheap to clone: clones share the connection pool and the token cache. See
/// the [module docs](super) for what a request carries and what comes back.
#[derive(Clone)]
pub struct ServiceClient {
    /// The service's name, as errors and logs say it.
    service: Arc<str>,
    http: Client,
    /// Scheme, host and port, with no trailing slash.
    base_url: String,
    /// `base_url` parsed: the origin the loopback and audience decisions were
    /// made on, and the only one a request may be sent to.
    base: Url,
    /// `None` only for a loopback service. `Arc` so clones share one token cache.
    tokens: Option<Arc<IdTokenSource>>,
}

impl ServiceClient {
    /// A client for the service at `base_url`, sending through `http`.
    ///
    /// `service` is the name errors and logs use. A loopback `base_url`
    /// (`localhost`, `127.0.0.0/8`, `::1`) is called with no token and its
    /// `audience` is ignored: a service bound to loopback serves an ungated
    /// router, and there is no metadata server to mint from on a developer's
    /// machine. Any other URL needs the `audience` its tokens are addressed
    /// to — for Cloud Run, the service URL — and must be `https`: an identity
    /// token is a bearer credential and is never sent in clear.
    ///
    /// `http` carries the default timeout; the token source mints through it
    /// too.
    ///
    /// # Errors
    ///
    /// [`ConfigError::InvalidBaseUrl`] when `base_url` does not parse, is not
    /// `http` or `https`, or names no host — `localhost:8080`, with no scheme,
    /// is refused; when it carries credentials, a query or a fragment, which
    /// every request path would be appended to; and when it is `http` and not
    /// on loopback. [`ConfigError::AudienceRequired`] when the URL is not on
    /// loopback and `audience` is `None` or empty: there is no way to build a
    /// client that calls a reachable service unauthenticated.
    pub fn new(
        service: &str,
        base_url: &str,
        audience: Option<&str>,
        http: Client,
    ) -> Result<Self, ConfigError> {
        let base_url = base_url.trim_end_matches('/');
        let invalid = |detail: String| ConfigError::InvalidBaseUrl {
            service: service.to_owned(),
            detail,
        };
        let url = Url::parse(base_url).map_err(|error| invalid(error.to_string()))?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err(invalid(format!(
                "its scheme is {:?}, not http or https",
                url.scheme()
            )));
        }
        let host = url
            .host_str()
            .ok_or_else(|| invalid("it names no host".to_owned()))?;
        if !url.username().is_empty() || url.password().is_some() {
            return Err(invalid("it carries credentials".to_owned()));
        }
        // A request URL is this string with the path appended, so anything
        // after the base path would swallow every path sent.
        if url.query().is_some() || url.fragment().is_some() {
            return Err(invalid("it carries a query or a fragment".to_owned()));
        }

        let tokens = if is_loopback_host(host) {
            debug!(
                service,
                host, "service is on loopback; calling it without an identity token"
            );
            None
        } else {
            if url.scheme() != "https" {
                return Err(invalid(
                    "a service that is not on loopback must be https; an identity token is \
                     never sent in clear"
                        .to_owned(),
                ));
            }
            let audience = audience
                .filter(|audience| !audience.is_empty())
                .ok_or_else(|| ConfigError::AudienceRequired {
                    service: service.to_owned(),
                })?;
            Some(Arc::new(IdTokenSource::new(audience, http.clone())))
        };

        Ok(Self {
            service: Arc::from(service),
            http,
            base_url: base_url.to_owned(),
            base: url,
            tokens,
        })
    }

    /// A client configured by two environment variables, or `None` when the
    /// service is not configured.
    ///
    /// `Ok(None)` means exactly one thing: `url_var` is unset, empty, or only
    /// slashes. The audience is read from `audience_var`, where empty counts
    /// as unset. `timeout` is the default budget of every request, connection
    /// to last body byte; [`ServiceRequest::timeout`] overrides it per request.
    ///
    /// # Errors
    ///
    /// [`ConfigError::AudienceNotSet`] when the URL is not on loopback and no
    /// audience is set — a misconfiguration the caller decides to fail on or
    /// to log, not one this returns `None` for. [`ConfigError::InvalidBaseUrl`]
    /// as [`new`](Self::new), and [`ConfigError::HttpClient`] when the HTTP
    /// client cannot be built.
    pub fn from_env(
        service: &str,
        url_var: &str,
        audience_var: &str,
        timeout: Duration,
    ) -> Result<Option<Self>, ConfigError> {
        let Ok(base_url) = env::var(url_var) else {
            return Ok(None);
        };
        if base_url.trim_end_matches('/').is_empty() {
            return Ok(None);
        }
        let audience = env::var(audience_var)
            .ok()
            .filter(|audience| !audience.is_empty());
        let http = Client::builder()
            .timeout(timeout)
            .build()
            .map_err(|error| ConfigError::HttpClient {
                service: service.to_owned(),
                detail: describe_request_error(error),
            })?;

        match Self::new(service, &base_url, audience.as_deref(), http) {
            Ok(client) => Ok(Some(client)),
            Err(ConfigError::AudienceRequired { service }) => Err(ConfigError::AudienceNotSet {
                service,
                url_var: url_var.to_owned(),
                audience_var: audience_var.to_owned(),
            }),
            Err(other) => Err(other),
        }
    }

    /// The service's name, as errors and logs say it.
    #[must_use]
    pub fn service(&self) -> &str {
        &self.service
    }

    /// The service's base URL, with no trailing slash.
    #[must_use]
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// A GET of `path`, which starts with `/` and has its segments already
    /// encoded. `operation` names the call in errors and logs.
    ///
    /// A `path` with no leading `/` fails at [`send`](ServiceRequest::send)
    /// with nothing sent: appended to the base URL it could name another host,
    /// and the token is minted for this one.
    ///
    /// The only request that is re-sent: once, when its connection closed
    /// before any response.
    pub fn get(&self, operation: &str, path: &str) -> ServiceRequest {
        self.request(operation, Method::GET, path, true)
    }

    /// A POST of `body` as JSON to `path`. Never re-sent.
    pub fn post_json<B: Serialize + ?Sized>(
        &self,
        operation: &str,
        path: &str,
        body: &B,
    ) -> ServiceRequest {
        let mut request = self.request(operation, Method::POST, path, false);
        request.builder = request.builder.json(body);
        request
    }

    /// A DELETE of `path`. Never re-sent.
    pub fn delete(&self, operation: &str, path: &str) -> ServiceRequest {
        self.request(operation, Method::DELETE, path, false)
    }

    fn request(
        &self,
        operation: &str,
        method: Method,
        path: &str,
        resend_after_close: bool,
    ) -> ServiceRequest {
        // The path is not echoed: it can carry an identifier.
        let refused = (!path.starts_with('/'))
            .then(|| "its path does not start with '/'; nothing was sent".to_owned());
        ServiceRequest {
            client: self.clone(),
            operation: operation.to_owned(),
            builder: self
                .http
                .request(method, format!("{}{path}", self.base_url)),
            resend_after_close,
            refused,
        }
    }

    /// A request that never reached the network. The id minted here still
    /// names the attempt in the error.
    fn unsendable(&self, operation: &str, resent: bool, cause: String) -> ServiceError {
        ServiceError::Transport {
            exchange: Box::new(self.exchange(operation, RequestId::mint(), Duration::ZERO, resent)),
            failure: TransportFailure::Other,
            cause,
        }
    }

    fn exchange(
        &self,
        operation: &str,
        request_id: RequestId,
        elapsed: Duration,
        resent: bool,
    ) -> Exchange {
        Exchange {
            service: self.service.as_ref().to_owned(),
            operation: operation.to_owned(),
            request_id,
            elapsed,
            resent,
        }
    }

    fn identity_error(&self, operation: &str, source: IamError) -> ServiceError {
        ServiceError::Identity {
            service: self.service.as_ref().to_owned(),
            operation: operation.to_owned(),
            source,
        }
    }

    /// `Authorization` for this service: a bearer token, or nothing on loopback.
    async fn authorization(&self, operation: &str) -> Result<Option<HeaderValue>, ServiceError> {
        let Some(tokens) = &self.tokens else {
            return Ok(None);
        };
        let token = tokens
            .token()
            .await
            .map_err(|source| self.identity_error(operation, source))?;
        let mut value = HeaderValue::from_str(&format!("Bearer {token}")).map_err(|_| {
            self.identity_error(
                operation,
                IamError::MalformedToken(
                    "the token holds a byte an HTTP header cannot carry".to_owned(),
                ),
            )
        })?;
        value.set_sensitive(true);
        Ok(Some(value))
    }

    /// Send one attempt and sort what came back.
    async fn attempt(
        &self,
        operation: &str,
        builder: RequestBuilder,
        resent: bool,
    ) -> Result<ServiceResponse, ServiceError> {
        // Nothing reaches the network when the request cannot be built or sent
        // under an id.
        let unsendable = |cause: String| self.unsendable(operation, resent, cause);
        let mut request = builder
            .build()
            .map_err(|error| unsendable(describe_request_error(error)))?;

        // The token is addressed to the base URL's service and the loopback
        // decision was made on its host, so a request for any other origin is
        // refused before a token is minted for it.
        if request.url().origin() != self.base.origin() {
            return Err(unsendable(
                "its URL is outside the service's base URL; nothing was sent".to_owned(),
            ));
        }

        // `Authorization` is the client's: its own token, or none on loopback,
        // whatever the caller put in its own headers.
        match self.authorization(operation).await? {
            Some(authorization) => {
                request.headers_mut().insert(AUTHORIZATION, authorization);
            }
            None => {
                request.headers_mut().remove(AUTHORIZATION);
            }
        }

        // Inserted, not appended: exactly one id goes out whatever the caller
        // put in its own headers, and it is the one the error will name.
        let request_id = RequestId::mint();
        let id_value = HeaderValue::from_str(request_id.as_str())
            .map_err(|error| unsendable(error.to_string()))?;
        request.headers_mut().insert(REQUEST_ID_HEADER, id_value);

        // The attempt joins the caller's trace, so the service's spans land in
        // it rather than starting a trace of their own.
        #[cfg(feature = "otel")]
        inject_current_context(request.headers_mut());

        let started = Instant::now();
        let response = match self.http.execute(request).await {
            Ok(response) => response,
            Err(error) => {
                // Classified on the reference: describing the error consumes it.
                let failure = TransportFailure::of(&error);
                return Err(ServiceError::Transport {
                    exchange: Box::new(self.exchange(
                        operation,
                        request_id,
                        started.elapsed(),
                        resent,
                    )),
                    failure,
                    cause: describe_request_error(error),
                });
            }
        };

        let status = response.status();
        let headers = response.headers().clone();
        let body = response.bytes().await;
        let exchange = self.exchange(operation, request_id, started.elapsed(), resent);
        match body {
            Ok(body) => classify(exchange, status, headers, body.to_vec()),
            Err(error) => {
                // A shed or an unfinished request is told by its status, so it
                // keeps its name when the body was cut: a gateway's 503 whose
                // page did not arrive is still a shed. Only an answer that
                // needed its body to be read is a body error.
                classify(exchange.clone(), status, headers, Vec::new())?;
                Err(ServiceError::Body {
                    exchange: Box::new(exchange),
                    status,
                    cause: describe_request_error(error),
                })
            }
        }
    }
}

/// One request to the service, not yet sent.
#[must_use = "a request does nothing until sent"]
pub struct ServiceRequest {
    client: ServiceClient,
    operation: String,
    builder: RequestBuilder,
    /// True only for a GET.
    resend_after_close: bool,
    /// Why this request must not be sent, when its path cannot be trusted to
    /// stay on the service.
    refused: Option<String>,
}

impl ServiceRequest {
    /// Add query parameters; the client encodes them.
    pub fn query<Q: Serialize + ?Sized>(mut self, query: &Q) -> Self {
        self.builder = self.builder.query(query);
        self
    }

    /// Add a header. A name or value a header cannot carry fails the request
    /// at [`send`](Self::send), before anything reaches the network.
    ///
    /// The request id and `Authorization` are the client's. A request id
    /// given here is replaced by the minted one; an `Authorization` given here
    /// is replaced by the service's token, and removed when the service is on
    /// loopback and is called with none.
    pub fn header(mut self, name: &str, value: &str) -> Self {
        self.builder = self.builder.header(name, value);
        self
    }

    /// Overrides the client's default budget for this request.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.builder = self.builder.timeout(timeout);
        self
    }

    /// Send the request and sort what came back.
    ///
    /// `Ok` is an answer the service's own handler produced, with any status.
    ///
    /// # Errors
    ///
    /// A [`ServiceError`]: no token, no response, a shed, a request the
    /// service or a gateway did not finish, or a body that did not arrive.
    /// A response whose body did not arrive is still a shed or an unfinished
    /// request when its status alone says so.
    /// See the [module docs](super).
    pub async fn send(self) -> Result<ServiceResponse, ServiceError> {
        let Self {
            client,
            operation,
            builder,
            resend_after_close,
            refused,
        } = self;

        if let Some(cause) = refused {
            return Err(client.unsendable(&operation, false, cause));
        }

        // A builder that cannot be cloned holds a streaming body or an error
        // of its own; either way there is no second copy to send.
        let spare = if resend_after_close {
            builder.try_clone()
        } else {
            None
        };

        let first = client.attempt(&operation, builder, false).await;
        let (Some(spare), Err(error)) = (spare, &first) else {
            return first;
        };
        let ServiceError::Transport {
            exchange,
            failure: TransportFailure::ClosedBeforeResponse,
            ..
        } = error
        else {
            return first;
        };

        // The service never answered and a read is safe to repeat. Typically a
        // pooled connection the service closed while it sat idle.
        warn!(
            service = client.service(),
            operation = %operation,
            request_id = %exchange.request_id,
            error = %error,
            "connection closed before any response; re-sending the GET once"
        );
        client.attempt(&operation, spare, true).await
    }
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;
    use std::sync::Mutex;

    use serial_test::serial;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use super::*;
    use crate::iam::METADATA_HOST_ENV;

    /// The token the metadata stand-in hands out.
    const STAND_IN_TOKEN: &str = "eyJhbGciOiJSUzI1NiJ9.eyJhdWQiOiJ0ZXN0In0.c2ln";

    /// A loopback server answering every GET `200` with `body`, recording each
    /// request head it read.
    async fn spawn_answering(body: &'static str) -> (SocketAddr, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap(); // Safe: test assertion
        let addr = listener.local_addr().unwrap(); // Safe: test assertion
        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        let log = Arc::clone(&seen);
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let mut bytes = Vec::new();
                let mut chunk = [0_u8; 4096];
                while !bytes.windows(4).any(|window| window == b"\r\n\r\n") {
                    let n = socket.read(&mut chunk).await.unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    bytes.extend_from_slice(&chunk[..n]);
                }
                let head = String::from_utf8_lossy(&bytes).into_owned();
                log.lock().unwrap().push(head); // Safe: test assertion
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                socket.write_all(response.as_bytes()).await.unwrap(); // Safe: test assertion
            }
        });
        (addr, seen)
    }

    /// A token-bearing client for `http://service.test`, resolved to `service`.
    ///
    /// Built field by field because `new` refuses this URL: a service that is
    /// not on loopback must be https, and the stand-in speaks plain HTTP. The
    /// bearer path under test is the same for either scheme.
    fn token_bearing_client(service: SocketAddr) -> ServiceClient {
        let builder = Client::builder()
            .no_proxy()
            .resolve("service.test", service);
        let http = builder.build().unwrap(); // Safe: test assertion
        let base_url = format!("http://service.test:{}", service.port());
        ServiceClient {
            service: Arc::from("svc"),
            base: Url::parse(&base_url).unwrap(), // Safe: test assertion
            base_url,
            tokens: Some(Arc::new(IdTokenSource::new("test-audience", http.clone()))),
            http,
        }
    }

    fn authorizations(raw: &str) -> Vec<String> {
        raw.lines()
            .filter_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("authorization")
                    .then(|| value.trim().to_owned())
            })
            .collect()
    }

    #[tokio::test]
    #[serial]
    async fn a_remote_service_gets_a_bearer_token() {
        let (metadata, asked) = spawn_answering(STAND_IN_TOKEN).await;
        let (service, seen) = spawn_answering("{\"ok\":true}").await;
        let client = token_bearing_client(service);

        env::set_var(METADATA_HOST_ENV, metadata.to_string());
        // The caller's own Authorization must not travel beside the token.
        let first = client
            .get("list", "/api/items")
            .header("authorization", "Bearer mine")
            .send()
            .await;
        let second = client.clone().get("list", "/api/items").send().await;
        env::remove_var(METADATA_HOST_ENV);
        first.expect("the first call is answered"); // Safe: test assertion
        second.expect("the second call is answered"); // Safe: test assertion

        let seen = seen.lock().unwrap().clone(); // Safe: test assertion
        assert_eq!(seen.len(), 2);
        for raw in &seen {
            assert_eq!(
                authorizations(raw),
                vec![format!("Bearer {STAND_IN_TOKEN}")],
                "each request carries the minted token as its one Authorization header"
            );
        }

        let asked = asked.lock().unwrap().clone(); // Safe: test assertion
        assert_eq!(asked.len(), 1, "clones share one token cache");
        assert!(asked[0].contains("audience=test-audience"), "{}", asked[0]);
    }

    #[tokio::test]
    async fn a_request_for_another_origin_is_refused_before_a_token_is_minted() {
        let (service, seen) = spawn_answering("{}").await;
        let client = token_bearing_client(service);

        // Same port, another host: what a path with no leading slash builds.
        let elsewhere = client.http.get(format!(
            "http://service.test.evil.test:{}/x",
            service.port()
        ));
        let outcome = client.attempt("list", elsewhere, false).await;

        // Minting would have failed as `Identity`: it was never reached.
        assert!(
            matches!(
                &outcome,
                Err(ServiceError::Transport { failure: TransportFailure::Other, cause, .. })
                    if cause.contains("outside the service's base URL")
            ),
            "expected a refused send, got: {outcome:?}"
        );
        assert!(seen.lock().unwrap().is_empty()); // Safe: test assertion
    }
}
