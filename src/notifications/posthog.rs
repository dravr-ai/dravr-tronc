// ABOUTME: PostHog capture-API client — fire-and-forget product/system event analytics sink
// ABOUTME: Each event carries its own time and UUIDv7; a 5xx or transport error is resent, never propagated
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use std::time::Duration;

use chrono::{SecondsFormat, Utc};
use reqwest::Client;
use serde_json::{json, Value};
use tokio::time;
use tracing::{debug, warn};
use uuid::Uuid;

use crate::http_client::describe_request_error;

/// Default `PostHog` capture-API host (US cloud).
const DEFAULT_POSTHOG_HOST: &str = "https://us.i.posthog.com";

/// Sends per event, the first included. A capture runs in its own task: three
/// ride out a brief ingest blip, and bound that task to about half a minute
/// even when every send runs to [`REQUEST_TIMEOUT`].
const MAX_ATTEMPTS: u32 = 3;

/// Wait before the first resend; each later resend waits twice as long.
const FIRST_BACKOFF: Duration = Duration::from_millis(500);

/// Ceiling on one send. reqwest's default client never times out, and a
/// connection that hangs would otherwise hold its task for good.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// `PostHog` capture-API client.
///
/// Posts events to the `/capture/` endpoint with a project API key. Like
/// [`SlackClient`](super::SlackClient), [`capture`](Self::capture) is
/// fire-and-forget: a failure is logged at WARN and never propagated, so
/// analytics can never break a request path.
///
/// Each event is stamped at the call with its own time and a v7 UUID, so
/// `PostHog` records when it happened rather than when it arrived, and a
/// resend is the same event — `PostHog` deduplicates on the uuid — rather
/// than a second one. A 5xx or a transport error is resent with backoff, up
/// to [`MAX_ATTEMPTS`] sends; any other status is final, because resending
/// the same body cannot change the answer.
///
/// The client carries no identity, consent, or tier policy — callers (the
/// `NotifyLayer` analytics fan-out via an `AnalyticsProvider`) decide the
/// `distinct_id` and which properties are safe to send. This keeps the
/// transport reusable and free of platform-specific knowledge.
#[derive(Clone)]
pub struct PostHogClient {
    http: Client,
    api_key: String,
    host: String,
    first_backoff: Duration,
}

/// How one event's delivery ended.
#[derive(Debug, PartialEq, Eq)]
enum Delivery {
    /// `PostHog` answered 2xx.
    Accepted,
    /// `PostHog` answered a status a resend cannot change: dropped.
    Refused,
    /// Every send hit a 5xx or a transport error: dropped.
    GaveUp,
}

impl PostHogClient {
    /// Create a client for the given project API key, using the default US host.
    pub fn new(api_key: impl Into<String>) -> Self {
        Self::with_host(api_key, DEFAULT_POSTHOG_HOST.to_owned())
    }

    /// Create a client pointed at a specific host (EU cloud or self-hosted).
    pub fn with_host(api_key: impl Into<String>, host: impl Into<String>) -> Self {
        Self {
            http: Client::new(),
            api_key: api_key.into(),
            host: host.into(),
            first_backoff: FIRST_BACKOFF,
        }
    }

    /// Capture one event for `distinct_id` with the given `properties`.
    ///
    /// Fire-and-forget: stamps the event, then spawns a background task that
    /// sends it. A failure is logged at WARN, never propagated.
    pub fn capture(&self, distinct_id: &str, event: &str, properties: Value) {
        let body = self.event_body(distinct_id, event, properties);
        let client = self.clone();
        let event = event.to_owned();
        tokio::spawn(async move {
            client.send(&event, &body).await;
        });
    }

    /// The wire body of one event, stamped now — at the call, before any
    /// send — with its time and a fresh v7 UUID, so every resend carries the
    /// same pair.
    fn event_body(&self, distinct_id: &str, event: &str, properties: Value) -> Value {
        let mut body = json!({
            "api_key": self.api_key,
            "event": event,
            "distinct_id": distinct_id,
            "timestamp": Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
            "uuid": Uuid::now_v7().to_string(),
        });
        // Moved in rather than through `json!`, which would deep-copy it.
        body["properties"] = properties;
        body
    }

    /// Send one stamped event body, resending it after a 5xx or a transport
    /// error until [`MAX_ATTEMPTS`] sends have been made.
    async fn send(&self, event: &str, body: &Value) -> Delivery {
        let url = format!("{}/capture/", self.host);
        let mut backoff = self.first_backoff;
        let mut attempt = 1;
        loop {
            let sent = self
                .http
                .post(&url)
                .timeout(REQUEST_TIMEOUT)
                .json(body)
                .send()
                .await;
            let failure = match sent {
                Ok(resp) if resp.status().is_success() => return Delivery::Accepted,
                Ok(resp) if resp.status().is_server_error() => format!("status {}", resp.status()),
                Ok(resp) => {
                    warn!(event = %event, status = %resp.status(), "PostHog refused a capture; dropping it");
                    return Delivery::Refused;
                }
                Err(e) => describe_request_error(e),
            };
            if attempt >= MAX_ATTEMPTS {
                warn!(event = %event, attempts = attempt, error = %failure, "PostHog capture failed; dropping it");
                return Delivery::GaveUp;
            }
            debug!(event = %event, attempt, error = %failure, "PostHog capture failed; resending");
            time::sleep(backoff).await;
            backoff = backoff.saturating_mul(2);
            attempt += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use axum::http::StatusCode;
    use axum::routing::post;
    use axum::{Json, Router};
    use tokio::net::TcpListener;

    use super::*;

    /// Received capture bodies, in arrival order.
    type Seen = Arc<Mutex<Vec<Value>>>;

    /// A local capture endpoint answering each request with the next status
    /// of `statuses` (200 once they run out), recording every body it gets.
    async fn capture_endpoint(statuses: &[u16]) -> (String, Seen) {
        let seen: Seen = Arc::new(Mutex::new(Vec::new()));
        let script = Arc::new(Mutex::new(
            statuses.iter().copied().collect::<VecDeque<_>>(),
        ));
        let recorder = Arc::clone(&seen);
        let app = Router::new().route(
            "/capture/",
            post(move |Json(body): Json<Value>| {
                let recorder = Arc::clone(&recorder);
                let script = Arc::clone(&script);
                async move {
                    recorder.lock().expect("lock").push(body); // Safe: test assertion
                    let code = script.lock().expect("lock").pop_front().unwrap_or(200); // Safe: test assertion
                    StatusCode::from_u16(code).expect("valid status") // Safe: test assertion
                }
            }),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind"); // Safe: test assertion
        let addr = listener.local_addr().expect("local addr"); // Safe: test assertion
        tokio::spawn(async move { axum::serve(listener, app).await });
        (format!("http://{addr}"), seen)
    }

    /// A client for `host` that resends without waiting.
    fn quick_client(host: &str) -> PostHogClient {
        PostHogClient {
            first_backoff: Duration::ZERO,
            ..PostHogClient::with_host("phc_test", host)
        }
    }

    fn sample_body(client: &PostHogClient) -> Value {
        client.event_body("u_abc", "user.login", json!({ "channel": "web" }))
    }

    #[test]
    fn new_uses_default_us_host() {
        let client = PostHogClient::new("phc_test");
        assert_eq!(client.host, DEFAULT_POSTHOG_HOST);
        assert_eq!(client.api_key, "phc_test");
    }

    #[test]
    fn with_host_overrides_host() {
        let client = PostHogClient::with_host("phc_test", "https://eu.i.posthog.com");
        assert_eq!(client.host, "https://eu.i.posthog.com");
    }

    #[test]
    fn every_event_carries_its_own_time_and_v7_uuid() {
        let client = PostHogClient::new("phc_test");
        let first = sample_body(&client);
        let second = sample_body(&client);

        let stamp = first["timestamp"].as_str().expect("timestamp is a string"); // Safe: test assertion
        chrono::DateTime::parse_from_rfc3339(stamp).expect("timestamp is RFC 3339"); // Safe: test assertion
        let id = first["uuid"].as_str().expect("uuid is a string"); // Safe: test assertion
        let parsed = Uuid::parse_str(id).expect("uuid parses"); // Safe: test assertion
        assert_eq!(parsed.get_version_num(), 7, "the event id is a v7 uuid");
        assert_ne!(
            first["uuid"], second["uuid"],
            "each event gets its own uuid"
        );

        assert_eq!(first["api_key"], "phc_test", "the body carries the key");
        assert_eq!(first["event"], "user.login", "the body names the event");
        assert_eq!(first["distinct_id"], "u_abc", "the body names the person");
        assert_eq!(
            first["properties"],
            json!({ "channel": "web" }),
            "the properties pass through unchanged"
        );
    }

    #[tokio::test]
    async fn a_5xx_is_resent_as_the_same_event() {
        let (host, seen) = capture_endpoint(&[503]).await;
        let client = quick_client(&host);
        let body = sample_body(&client);

        assert_eq!(client.send("user.login", &body).await, Delivery::Accepted);

        let seen = seen.lock().expect("lock"); // Safe: test assertion
        assert_eq!(seen.len(), 2, "one 503, then one accepted resend");
        assert_eq!(
            seen[0], seen[1],
            "the resend carries the same uuid and timestamp"
        );
        assert_eq!(seen[1], body, "the stamped body is sent as built");
    }

    #[tokio::test]
    async fn a_4xx_is_never_resent() {
        let (host, seen) = capture_endpoint(&[400]).await;
        let client = quick_client(&host);

        let delivery = client.send("user.login", &sample_body(&client)).await;

        assert_eq!(delivery, Delivery::Refused, "a 4xx is final");
        let sends = seen.lock().expect("lock").len(); // Safe: test assertion
        assert_eq!(sends, 1, "a 4xx is sent once, never resent");
    }

    #[tokio::test]
    async fn a_persistent_5xx_stops_after_the_bounded_attempts() {
        let (host, seen) = capture_endpoint(&[500, 502, 503, 504]).await;
        let client = quick_client(&host);

        let delivery = client.send("user.login", &sample_body(&client)).await;

        assert_eq!(delivery, Delivery::GaveUp, "every send hit a 5xx");
        let attempts = seen.lock().expect("lock").len(); // Safe: test assertion
        assert_eq!(
            attempts, MAX_ATTEMPTS as usize,
            "a persistent 5xx is sent the bounded number of times, no more"
        );
    }

    /// A local endpoint that closes every connection without answering, so
    /// each send fails in transport; returns its host and a count of the
    /// connections it took.
    async fn hang_up_endpoint() -> (String, Arc<AtomicUsize>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind"); // Safe: test assertion
        let addr = listener.local_addr().expect("local addr"); // Safe: test assertion
        let connections = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&connections);
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                counter.fetch_add(1, Ordering::SeqCst);
                drop(stream);
            }
        });
        (format!("http://{addr}"), connections)
    }

    #[tokio::test]
    async fn a_transport_error_is_resent_then_dropped() {
        let (host, connections) = hang_up_endpoint().await;
        let client = quick_client(&host);

        let delivery = client.send("user.login", &sample_body(&client)).await;

        assert_eq!(delivery, Delivery::GaveUp, "every send failed in transport");
        assert_eq!(
            connections.load(Ordering::SeqCst),
            MAX_ATTEMPTS as usize,
            "a transport error is resent the bounded number of times, no more"
        );
    }

    #[tokio::test]
    async fn capture_does_not_panic_without_network() {
        // Fire-and-forget: the spawned task may fail to connect, but capture()
        // itself must return cleanly. Points at an unroutable host so the
        // background send errors and exercises the WARN path.
        let client = PostHogClient::with_host("phc_test", "http://127.0.0.1:1");
        client.capture(
            "u_abc",
            "user.login",
            serde_json::json!({ "channel": "web" }),
        );
    }
}
