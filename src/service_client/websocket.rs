// ABOUTME: ServiceClient::connect_websocket: a WebSocket to one dravr service under its Google ID token
// ABOUTME: Same origin, token and request-id rules as an HTTP request; a refused upgrade reads as a ServiceResponse
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use reqwest::header::{HeaderValue, AUTHORIZATION};
use reqwest::Url;
use rustls::crypto::ring::default_provider;
use rustls::{ClientConfig, RootCertStore};
use tokio::net::TcpStream;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::{self, handshake::client::Request};
use tokio_tungstenite::{Connector, MaybeTlsStream, WebSocketStream};

use super::client::ServiceClient;
use super::error::ServiceError;
use super::response::{classify, ServiceResponse};
use super::transport::TransportFailure;
use crate::server::request_guard::{RequestId, REQUEST_ID_HEADER};
#[cfg(feature = "otel")]
use crate::server::trace_context::inject_current_context;

pub use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
pub use tokio_tungstenite::tungstenite::protocol::CloseFrame;
pub use tokio_tungstenite::tungstenite::Message as WebSocketMessage;

/// An open WebSocket to a dravr service: a `Stream` of
/// `Result<WebSocketMessage, _>` and a `Sink<WebSocketMessage>`.
pub type ServiceWebSocket = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// How long the TCP connect, TLS and upgrade handshake may take together.
/// The socket itself has no deadline: it lives as long as both ends keep it.
pub const WEBSOCKET_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);

/// What a WebSocket upgrade request came back with.
///
/// Mirrors [`ServiceRequest::send`](super::ServiceRequest::send): `Ok` means
/// the service's handler answered. A handler that refused the upgrade — a
/// `401`, a `404` for a resource it does not hold — is `Refused`, with the
/// status and body for the consumer to read; a shed or an unfinished request
/// is a [`ServiceError`] as it is for any request.
pub enum WebSocketUpgrade {
    /// The service accepted the upgrade.
    Open(Box<ServiceWebSocket>),
    /// The service answered with something other than `101 Switching Protocols`.
    Refused(Box<ServiceResponse>),
}

impl ServiceClient {
    /// Open a WebSocket to `path`, which starts with `/` and has its segments
    /// already encoded; `operation` names the call in errors and logs.
    ///
    /// The upgrade request carries what every request from this client does:
    /// the service's ID token as `Authorization` (none on loopback), a fresh
    /// request id, and with `otel` the current trace context. The scheme
    /// follows the base URL: `wss` for `https`, `ws` for a loopback `http`.
    /// TLS uses rustls with the webpki roots on the `ring` provider, chosen
    /// here, so a process with no default crypto provider, or two, still
    /// connects.
    ///
    /// Never re-sent. The handshake has [`WEBSOCKET_HANDSHAKE_TIMEOUT`].
    ///
    /// # Errors
    ///
    /// `Transport` when the path has no leading `/` or leaves the service's
    /// origin (nothing sent), when no connection opens, or when the handshake
    /// fails or times out; `Identity` when no token could be minted; `Shed`
    /// and `Unfinished` for those answers to the upgrade.
    pub async fn connect_websocket(
        &self,
        operation: &str,
        path: &str,
    ) -> Result<WebSocketUpgrade, ServiceError> {
        let request_id = RequestId::mint();
        let fail = |request_id: RequestId, elapsed: Duration, failure, cause: String| {
            ServiceError::Transport {
                exchange: Box::new(self.exchange(operation, request_id, elapsed, false)),
                failure,
                cause,
            }
        };
        let unsendable = |cause: &str| {
            fail(
                request_id.clone(),
                Duration::ZERO,
                TransportFailure::Other,
                cause.to_owned(),
            )
        };

        // The path is not echoed: it can carry an identifier.
        if !path.starts_with('/') {
            return Err(unsendable(
                "its path does not start with '/'; nothing was sent",
            ));
        }
        let url = self.websocket_url(path).ok_or_else(|| {
            unsendable("its URL is outside the service's base URL; nothing was sent")
        })?;
        let mut request: Request = url.as_str().into_client_request().map_err(|_| {
            unsendable("its URL is not a valid WebSocket request; nothing was sent")
        })?;

        match self.authorization(operation).await? {
            Some(authorization) => {
                request.headers_mut().insert(AUTHORIZATION, authorization);
            }
            None => {
                request.headers_mut().remove(AUTHORIZATION);
            }
        }
        let id_value = HeaderValue::from_str(request_id.as_str())
            .map_err(|error| unsendable(&error.to_string()))?;
        request.headers_mut().insert(REQUEST_ID_HEADER, id_value);
        #[cfg(feature = "otel")]
        inject_current_context(request.headers_mut());

        let started = Instant::now();
        let tls = tls_config().map_err(|error| {
            fail(
                request_id.clone(),
                Duration::ZERO,
                TransportFailure::Other,
                format!("no TLS config could be built: {error}"),
            )
        })?;
        let connector = Connector::Rustls(tls);
        let handshake =
            tokio_tungstenite::connect_async_tls_with_config(request, None, false, Some(connector));
        match timeout(WEBSOCKET_HANDSHAKE_TIMEOUT, handshake).await {
            Err(_) => Err(fail(
                request_id,
                started.elapsed(),
                TransportFailure::TimedOut,
                format!(
                    "the WebSocket handshake did not finish within {}s",
                    WEBSOCKET_HANDSHAKE_TIMEOUT.as_secs()
                ),
            )),
            Ok(Ok((socket, _response))) => Ok(WebSocketUpgrade::Open(Box::new(socket))),
            Ok(Err(tungstenite::Error::Http(response))) => {
                let exchange = self.exchange(operation, request_id, started.elapsed(), false);
                let (parts, body) = response.into_parts();
                classify(
                    exchange,
                    parts.status,
                    parts.headers,
                    body.unwrap_or_default(),
                )
                .map(|answer| WebSocketUpgrade::Refused(Box::new(answer)))
            }
            Ok(Err(error)) => {
                let failure = match &error {
                    tungstenite::Error::Io(_) => TransportFailure::Unreachable,
                    tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed => {
                        TransportFailure::ClosedBeforeResponse
                    }
                    _ => TransportFailure::Other,
                };
                Err(fail(
                    request_id,
                    started.elapsed(),
                    failure,
                    error.to_string(),
                ))
            }
        }
    }

    /// `path` on this service with the WebSocket scheme, or `None` when the
    /// result would leave the service's origin.
    fn websocket_url(&self, path: &str) -> Option<Url> {
        let mut url = Url::parse(&format!("{}{path}", self.base_url())).ok()?;
        if url.origin() != self.base().origin() {
            return None;
        }
        let scheme = match url.scheme() {
            "https" => "wss",
            "http" => "ws",
            _ => return None,
        };
        url.set_scheme(scheme).ok()?;
        Some(url)
    }
}

/// One rustls config for every socket: webpki roots on the `ring` provider.
///
/// A `Result` because rustls types the version check as fallible; the `ring`
/// provider supports the default versions, so it holds the config in practice.
fn tls_config() -> Result<Arc<ClientConfig>, rustls::Error> {
    static CONFIG: OnceLock<Result<Arc<ClientConfig>, rustls::Error>> = OnceLock::new();
    CONFIG
        .get_or_init(|| {
            let roots = RootCertStore {
                roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
            };
            let provider = Arc::new(default_provider());
            let config = ClientConfig::builder_with_provider(provider)
                .with_safe_default_protocol_versions()?
                .with_root_certificates(roots)
                .with_no_client_auth();
            Ok(Arc::new(config))
        })
        .clone()
}

#[cfg(test)]
mod tests {
    use std::env;
    use std::net::SocketAddr;

    use axum::body::Body;
    use axum::extract::ws::{Message as AxumMessage, WebSocketUpgrade as AxumUpgrade};
    use axum::http::{HeaderMap, StatusCode};
    use axum::response::{IntoResponse, Response};
    use axum::routing::get;
    use axum::Router;
    use futures::{SinkExt, StreamExt};
    use reqwest::Client;
    use serial_test::serial;
    use tokio::net::TcpListener;
    use tokio::sync::mpsc;

    use super::*;
    use crate::iam::METADATA_HOST_ENV;

    /// A loopback axum server: `/echo` upgrades, records the request headers
    /// and echoes text; `/refuse` answers `404` with a JSON body.
    async fn spawn_ws_server() -> (SocketAddr, mpsc::UnboundedReceiver<HeaderMap>) {
        let (seen_tx, seen_rx) = mpsc::unbounded_channel();
        let app = Router::new()
            .route(
                "/echo",
                get(move |headers: HeaderMap, upgrade: AxumUpgrade| {
                    let seen_tx = seen_tx.clone();
                    async move {
                        seen_tx.send(headers).ok();
                        upgrade.on_upgrade(|mut socket| async move {
                            while let Some(Ok(AxumMessage::Text(text))) = socket.recv().await {
                                if socket.send(AxumMessage::Text(text)).await.is_err() {
                                    break;
                                }
                            }
                        })
                    }
                }),
            )
            .route(
                "/refuse",
                get(|| async {
                    Response::builder()
                        .status(StatusCode::NOT_FOUND)
                        .header("content-type", "application/json")
                        .body(Body::from(r#"{"error":"no such flow"}"#))
                        .unwrap() // Safe: test assertion
                        .into_response()
                }),
            );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap(); // Safe: test assertion
        let addr = listener.local_addr().unwrap(); // Safe: test assertion
        tokio::spawn(async move { axum::serve(listener, app).await });
        (addr, seen_rx)
    }

    fn loopback_client(addr: SocketAddr) -> ServiceClient {
        ServiceClient::new(
            "svc",
            &format!("http://127.0.0.1:{}", addr.port()),
            None,
            Client::new(),
        )
        .unwrap() // Safe: test assertion
    }

    #[tokio::test]
    async fn a_loopback_socket_opens_and_carries_a_request_id_and_no_token() {
        let (addr, mut seen) = spawn_ws_server().await;
        let client = loopback_client(addr);

        let upgrade = client.connect_websocket("echo", "/echo").await;
        let Ok(WebSocketUpgrade::Open(mut socket)) = upgrade else {
            panic!("the upgrade is accepted"); // Safe: test assertion
        };
        socket.send(WebSocketMessage::text("ping")).await.unwrap(); // Safe: test assertion
        let echoed = socket.next().await.unwrap().unwrap(); // Safe: test assertion
        assert_eq!(echoed, WebSocketMessage::text("ping"));

        let headers = seen.recv().await.unwrap(); // Safe: test assertion
        assert!(headers.get(REQUEST_ID_HEADER).is_some());
        assert!(
            headers.get(AUTHORIZATION).is_none(),
            "a loopback service is called without a token"
        );
    }

    #[tokio::test]
    async fn a_refused_upgrade_reads_as_the_services_answer() {
        let (addr, _seen) = spawn_ws_server().await;
        let client = loopback_client(addr);

        let upgrade = client.connect_websocket("refuse", "/refuse").await;
        let Ok(WebSocketUpgrade::Refused(answer)) = upgrade else {
            panic!("a 404 is the handler's answer, not an error"); // Safe: test assertion
        };
        assert_eq!(answer.status(), StatusCode::NOT_FOUND);
        assert_eq!(answer.json_value()["error"], "no such flow");
    }

    #[tokio::test]
    async fn a_path_without_a_leading_slash_is_never_sent() {
        let (addr, mut seen) = spawn_ws_server().await;
        let client = loopback_client(addr);

        let upgrade = client.connect_websocket("echo", "@evil.test/echo").await;
        assert!(matches!(
            upgrade,
            Err(ServiceError::Transport { failure: TransportFailure::Other, ref cause, .. })
                if cause.contains("'/'")
        ));
        assert!(seen.try_recv().is_err(), "nothing reached the service");
    }

    #[tokio::test]
    #[serial]
    async fn a_remote_service_socket_carries_the_minted_token() {
        const STAND_IN_TOKEN: &str = "eyJhbGciOiJSUzI1NiJ9.eyJhdWQiOiJ0ZXN0In0.c2ln";
        let metadata = Router::new().route(
            "/computeMetadata/v1/instance/service-accounts/default/identity",
            get(|| async { STAND_IN_TOKEN }),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap(); // Safe: test assertion
        let metadata_addr = listener.local_addr().unwrap(); // Safe: test assertion
        tokio::spawn(async move { axum::serve(listener, metadata).await });
        let (addr, mut seen) = spawn_ws_server().await;
        let client = ServiceClient::token_bearing_on_loopback(
            &format!("http://127.0.0.1:{}", addr.port()),
            Client::new(),
        );

        env::set_var(METADATA_HOST_ENV, metadata_addr.to_string());
        let upgrade = client.connect_websocket("echo", "/echo").await;
        env::remove_var(METADATA_HOST_ENV);
        assert!(matches!(upgrade, Ok(WebSocketUpgrade::Open(_))));

        let headers = seen.recv().await.unwrap(); // Safe: test assertion
        let authorization = headers.get_all(AUTHORIZATION).iter().collect::<Vec<_>>();
        assert_eq!(authorization.len(), 1);
        assert_eq!(
            authorization[0],
            format!("Bearer {STAND_IN_TOKEN}").as_str()
        );
    }

    #[test]
    fn the_scheme_follows_the_base_url() {
        let https = ServiceClient::new(
            "svc",
            "https://svc.example.run.app",
            Some("https://svc.example.run.app"),
            Client::new(),
        )
        .unwrap(); // Safe: test assertion
        assert_eq!(
            https.websocket_url("/auth/takeover/abc").map(String::from),
            Some("wss://svc.example.run.app/auth/takeover/abc".to_owned())
        );
        let dotted = https.websocket_url("/../x").map(String::from);
        assert_eq!(dotted, Some("wss://svc.example.run.app/x".to_owned()));
    }

    #[test]
    fn the_tls_config_is_built_once_on_ring() {
        let first = tls_config().unwrap(); // Safe: test assertion
        let second = tls_config().unwrap(); // Safe: test assertion
        assert!(Arc::ptr_eq(&first, &second));
    }
}
