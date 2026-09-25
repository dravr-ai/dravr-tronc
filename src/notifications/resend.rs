// ABOUTME: Resend email client — the one send path, retrying a 429 within Resend's advertised reset
// ABOUTME: Shared by the alert mailer (EmailClient) and a host's transactional mail on one API key
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

//! The Resend client every dravr service sends email through.
//!
//! Resend's rate limit is per API key, and a service's alert mail and its
//! transactional mail share one `RESEND_API_KEY`. Two clients on that key, one
//! of which gives up on a `429`, meant an error alert fired during a burst of
//! password-reset mail was dropped rather than delayed. One client, one retry
//! policy: a `429` is retried after the reset Resend advertises, within a bound.

use std::error::Error;
use std::fmt;
use std::future::Future;
use std::time::Duration;

use reqwest::header::HeaderMap;
use reqwest::{Client, Response, StatusCode};
use serde::Serialize;
use tokio::time::sleep;
use tracing::warn;

use crate::http_client::describe_request_error;

/// Resend's send-email endpoint.
const RESEND_API_URL: &str = "https://api.resend.com/emails";

/// Maximum number of automatic retries after a `429 Too Many Requests`.
const MAX_RETRIES: u32 = 3;

/// Upper bound on how long a send waits for a rate-limit reset.
///
/// Resend's per-second limit resets within a second; a reset window longer
/// than this is a daily or monthly cap, where holding the caller (an inline
/// password-reset handler, the alert dispatcher) is worse than failing the send.
const MAX_RETRY_DELAY: Duration = Duration::from_secs(5);

/// Backoff used when a `429` arrives without a usable reset header.
const DEFAULT_RETRY_DELAY: Duration = Duration::from_secs(1);

/// Floor applied to every retry so a `0`-second reset cannot become a tight loop.
const RETRY_DELAY_FLOOR: Duration = Duration::from_millis(200);

/// The body of an email.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResendBody {
    /// A plain-text body.
    Text(String),
    /// An HTML body.
    Html(String),
}

/// One email to send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResendEmail {
    /// Sender, e.g. `"Dravr <no-reply@dravr.ai>"`.
    pub from: String,
    /// Recipients.
    pub to: Vec<String>,
    /// Subject line.
    pub subject: String,
    /// Body.
    pub body: ResendBody,
}

/// The JSON Resend's send-email endpoint takes.
#[derive(Serialize)]
struct Payload<'a> {
    from: &'a str,
    to: &'a [String],
    subject: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    html: Option<&'a str>,
}

impl<'a> Payload<'a> {
    fn of(email: &'a ResendEmail) -> Self {
        let (text, html) = match &email.body {
            ResendBody::Text(text) => (Some(text.as_str()), None),
            ResendBody::Html(html) => (None, Some(html.as_str())),
        };
        Self {
            from: &email.from,
            to: &email.to,
            subject: &email.subject,
            text,
            html,
        }
    }
}

/// Why a send failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResendError {
    /// The client was given an empty API key, which Resend can only refuse.
    MissingApiKey,
    /// The request never went out, or its response never arrived. The text is
    /// built by [`describe_request_error`], so it carries no URL.
    Transport(String),
    /// Resend answered with a non-success status — for a `429`, once the retry
    /// budget or the wait bound was exhausted.
    Api {
        /// The HTTP status Resend answered with.
        status: u16,
        /// Resend's response body, which names the reason.
        body: String,
    },
}

impl fmt::Display for ResendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingApiKey => f.write_str("Resend API key is empty"),
            Self::Transport(reason) => write!(f, "Resend request failed: {reason}"),
            Self::Api { status, body } => write!(f, "Resend API error (HTTP {status}): {body}"),
        }
    }
}

impl Error for ResendError {}

/// A Resend API client.
///
/// Cheap to clone: the HTTP client is a pooled handle.
#[derive(Clone)]
pub struct ResendClient {
    http: Client,
    api_key: String,
}

impl ResendClient {
    /// A client sending with `api_key` over `http`.
    ///
    /// Takes the HTTP client so a host can pass the one it already configured
    /// (timeouts, pooling) rather than growing a second pool.
    ///
    /// # Errors
    ///
    /// [`ResendError::MissingApiKey`] when `api_key` is empty.
    pub fn new(http: Client, api_key: impl Into<String>) -> Result<Self, ResendError> {
        let api_key = api_key.into();
        if api_key.is_empty() {
            return Err(ResendError::MissingApiKey);
        }
        Ok(Self { http, api_key })
    }

    /// Send `email`, retrying a `429` after the reset Resend advertises.
    ///
    /// Up to three retries, each after the `Retry-After` or `RateLimit-Reset`
    /// the `429` carried (one second when it carried neither, never less than
    /// 200ms). A reset longer than five seconds is a daily or monthly cap, and
    /// the send fails at once instead of waiting it out.
    ///
    /// # Errors
    ///
    /// [`ResendError::Transport`] when the request could not be made, and
    /// [`ResendError::Api`] for a non-success answer that retrying did not cure.
    pub async fn send(&self, email: &ResendEmail) -> Result<(), ResendError> {
        let payload = Payload::of(email);
        send_with_retry(|| {
            self.http
                .post(RESEND_API_URL)
                .bearer_auth(&self.api_key)
                .json(&payload)
                .send()
        })
        .await
    }
}

/// POST through `post` until it succeeds, a non-`429` failure arrives, or the
/// `429` retry plan gives up.
async fn send_with_retry<F, Fut>(mut post: F) -> Result<(), ResendError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<Response, reqwest::Error>>,
{
    let mut attempt: u32 = 0;
    loop {
        let response = post()
            .await
            .map_err(|e| ResendError::Transport(describe_request_error(e)))?;
        let status = response.status();
        if status.is_success() {
            return Ok(());
        }

        let retry = if status == StatusCode::TOO_MANY_REQUESTS {
            plan_retry(attempt, retry_delay_from_headers(response.headers()))
        } else {
            None
        };
        let Some(delay) = retry else {
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "no body".to_owned());
            return Err(ResendError::Api {
                status: status.as_u16(),
                body,
            });
        };

        attempt += 1;
        warn!(
            attempt,
            delay_ms = u64::try_from(delay.as_millis()).unwrap_or(u64::MAX),
            "Resend rate limit (HTTP 429) — waiting for the reset, then retrying"
        );
        sleep(delay).await;
    }
}

/// Resend's reset hint as a backoff: the standard `Retry-After` (delta
/// seconds), else Resend's `RateLimit-Reset` (seconds until the window
/// resets). `None` when neither is a non-negative integer.
fn parse_retry_delay(retry_after: Option<&str>, ratelimit_reset: Option<&str>) -> Option<Duration> {
    let secs = |v: Option<&str>| v.and_then(|s| s.trim().parse::<u64>().ok());
    secs(retry_after)
        .or_else(|| secs(ratelimit_reset))
        .map(Duration::from_secs)
}

/// Whether to retry a `429`, and how long to wait first: `None` once `attempt`
/// reaches [`MAX_RETRIES`] or the reset exceeds [`MAX_RETRY_DELAY`].
fn plan_retry(attempt: u32, hint: Option<Duration>) -> Option<Duration> {
    if attempt >= MAX_RETRIES {
        return None;
    }
    let delay = hint.unwrap_or(DEFAULT_RETRY_DELAY);
    (delay <= MAX_RETRY_DELAY).then(|| delay.max(RETRY_DELAY_FLOOR))
}

/// The retry backoff a Resend response's rate-limit headers advertise.
fn retry_delay_from_headers(headers: &HeaderMap) -> Option<Duration> {
    let get = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
    parse_retry_delay(get("retry-after"), get("ratelimit-reset"))
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::future::{ready, Ready};
    use std::sync::atomic::{AtomicU32, Ordering};

    use serde_json::{json, Value};

    use super::*;

    fn answer(status: u16, headers: &[(&str, &str)], body: &'static str) -> Response {
        let mut builder = http::Response::builder().status(status);
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        Response::from(builder.body(body).expect("response")) // Safe: test assertion
    }

    /// Replays `answers` in order and counts the posts made.
    async fn replay(answers: Vec<Response>) -> (Result<(), ResendError>, u32) {
        let mut answers = VecDeque::from(answers);
        let posts = AtomicU32::new(0);
        let result = send_with_retry(|| -> Ready<Result<Response, reqwest::Error>> {
            posts.fetch_add(1, Ordering::SeqCst);
            ready(Ok(answers
                .pop_front()
                .expect("a post past the scripted answers"))) // Safe: test assertion
        })
        .await;
        (result, posts.load(Ordering::SeqCst))
    }

    #[tokio::test]
    async fn a_429_is_retried_after_its_reset_and_then_succeeds() {
        let (result, posts) = replay(vec![
            answer(429, &[("retry-after", "0")], "slow down"),
            answer(200, &[], r#"{"id":"e-1"}"#),
        ])
        .await;
        assert_eq!(result, Ok(()));
        assert_eq!(posts, 2);
    }

    #[tokio::test]
    async fn a_persistent_429_gives_up_after_three_retries() {
        let (result, posts) = replay(
            (0..4)
                .map(|_| answer(429, &[("ratelimit-reset", "0")], "still limited"))
                .collect(),
        )
        .await;
        assert_eq!(
            result,
            Err(ResendError::Api {
                status: 429,
                body: "still limited".to_owned()
            })
        );
        assert_eq!(posts, 4, "one post plus MAX_RETRIES retries");
    }

    #[tokio::test]
    async fn a_daily_cap_is_not_waited_out() {
        let (result, posts) = replay(vec![answer(
            429,
            &[("ratelimit-reset", "3600")],
            "daily quota",
        )])
        .await;
        assert!(matches!(result, Err(ResendError::Api { status: 429, .. })));
        assert_eq!(posts, 1);
    }

    #[tokio::test]
    async fn any_other_failure_is_not_retried_and_keeps_resends_reason() {
        let (result, posts) = replay(vec![answer(
            422,
            &[("retry-after", "0")],
            r#"{"name":"validation_error"}"#,
        )])
        .await;
        assert_eq!(
            result,
            Err(ResendError::Api {
                status: 422,
                body: r#"{"name":"validation_error"}"#.to_owned()
            })
        );
        assert_eq!(posts, 1);
    }

    #[test]
    fn retry_after_wins_over_ratelimit_reset() {
        assert_eq!(
            parse_retry_delay(Some("2"), Some("9")),
            Some(Duration::from_secs(2))
        );
        assert_eq!(
            parse_retry_delay(Some("soon"), Some("3")),
            Some(Duration::from_secs(3))
        );
        assert_eq!(parse_retry_delay(Some(""), Some(" ")), None);
    }

    #[test]
    fn the_retry_plan_floors_defaults_and_bounds_the_wait() {
        assert_eq!(plan_retry(0, Some(Duration::ZERO)), Some(RETRY_DELAY_FLOOR));
        assert_eq!(plan_retry(0, None), Some(DEFAULT_RETRY_DELAY));
        assert_eq!(
            plan_retry(2, Some(Duration::from_secs(5))),
            Some(Duration::from_secs(5))
        );
        assert_eq!(plan_retry(0, Some(Duration::from_secs(6))), None);
        assert_eq!(plan_retry(MAX_RETRIES, Some(Duration::ZERO)), None);
    }

    #[test]
    fn the_payload_carries_exactly_one_body_kind() {
        let mut email = ResendEmail {
            from: "Alerts <alerts@dravr.ai>".to_owned(),
            to: vec!["a@dravr.ai".to_owned(), "b@dravr.ai".to_owned()],
            subject: "ERROR in svc".to_owned(),
            body: ResendBody::Text("stack".to_owned()),
        };
        let text: Value = serde_json::to_value(Payload::of(&email)).expect("json"); // Safe: test assertion
        assert_eq!(
            text,
            json!({
                "from": "Alerts <alerts@dravr.ai>",
                "to": ["a@dravr.ai", "b@dravr.ai"],
                "subject": "ERROR in svc",
                "text": "stack",
            })
        );

        email.body = ResendBody::Html("<p>code</p>".to_owned());
        let html: Value = serde_json::to_value(Payload::of(&email)).expect("json"); // Safe: test assertion
        assert_eq!(html["html"], "<p>code</p>");
        assert!(html.get("text").is_none());
    }

    #[test]
    fn an_empty_key_is_refused_at_construction() {
        assert!(matches!(
            ResendClient::new(Client::new(), ""),
            Err(ResendError::MissingApiKey)
        ));
        assert!(ResendClient::new(Client::new(), "re_key").is_ok());
    }
}
