// ABOUTME: PostHog capture-API client — fire-and-forget product/system event analytics sink
// ABOUTME: Shared across dravr-xxx services; POSTs to /capture/, errors logged never propagated
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use reqwest::Client;
use serde_json::Value;
use tracing::warn;

/// Default `PostHog` capture-API host (US cloud).
const DEFAULT_POSTHOG_HOST: &str = "https://us.i.posthog.com";

/// `PostHog` capture-API client.
///
/// Posts events to the `/capture/` endpoint with a project API key. Like
/// [`SlackClient`](super::SlackClient), [`capture`](Self::capture) is
/// fire-and-forget: a non-2xx response or transport error is logged at WARN
/// and never propagated, so analytics can never break a request path.
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
        }
    }

    /// Capture one event for `distinct_id` with the given `properties`.
    ///
    /// Fire-and-forget: spawns a background task. A non-2xx response or
    /// transport error is logged at WARN, never propagated.
    pub fn capture(&self, distinct_id: &str, event: &str, properties: Value) {
        let client = self.http.clone();
        let url = format!("{}/capture/", self.host);
        let api_key = self.api_key.clone();
        let event = event.to_owned();
        let distinct_id = distinct_id.to_owned();

        tokio::spawn(async move {
            let body = serde_json::json!({
                "api_key": api_key,
                "event": event,
                "distinct_id": distinct_id,
                "properties": properties,
            });

            match client
                .post(&url)
                .header("Content-Type", "application/json")
                .json(&body)
                .send()
                .await
            {
                Ok(resp) if !resp.status().is_success() => {
                    warn!(event = %event, status = %resp.status(), "PostHog capture returned non-2xx");
                }
                Err(e) => {
                    warn!(event = %event, error = %e, "PostHog capture failed");
                }
                Ok(_) => {}
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
