// ABOUTME: Slack API client for posting messages, updating messages, and verifying signatures
// ABOUTME: Shared across all dravr-xxx services for consistent Slack integration
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use std::fmt;
use std::str;

use reqwest::Client;
use ring::hmac;
use serde_json::Value;
use subtle::ConstantTimeEq;
use tracing::{error, warn};

use super::SlackConfig;

/// Slack API endpoint for posting messages
const SLACK_POST_MESSAGE_URL: &str = "https://slack.com/api/chat.postMessage";

/// Slack API endpoint for updating messages
const SLACK_CHAT_UPDATE_URL: &str = "https://slack.com/api/chat.update";

/// Maximum age of a Slack request timestamp before rejection (5 minutes)
const MAX_TIMESTAMP_AGE_SECS: u64 = 300;

/// Slack API error codes that mean the delivery target is misconfigured
///
/// These never clear on their own — a renamed channel, an uninvited bot, or a
/// revoked token stays broken until an operator changes configuration. Posting
/// is fire-and-forget, so a `warn!` here is invisible: the caller believes it
/// notified and nothing ever arrives. Escalating to `error!` routes the failure
/// through `ErrorNotificationLayer` (which fires only on `Level::ERROR`) and out
/// to the independent error channel, so a dead route announces itself instead of
/// going quiet.
///
/// Transient codes (`ratelimited`, `service_unavailable`, `request_timeout`) are
/// deliberately absent — they retry into success and would only add noise.
const SLACK_MISCONFIGURED_TARGET_ERRORS: &[&str] = &[
    "channel_not_found",
    "is_archived",
    "not_in_channel",
    "invalid_auth",
    "not_authed",
    "account_inactive",
    "token_revoked",
    "missing_scope",
    "restricted_action",
];

/// Whether a Slack API error code indicates a misconfigured delivery target
///
/// See [`SLACK_MISCONFIGURED_TARGET_ERRORS`] for why these are treated apart
/// from transient failures.
fn is_misconfigured_target(api_error: &str) -> bool {
    SLACK_MISCONFIGURED_TARGET_ERRORS.contains(&api_error)
}

/// Log a failed fire-and-forget Slack call at the severity its cause deserves
///
/// A misconfigured target is an operator-actionable outage of the notification
/// path itself, so it logs at ERROR with the channel that failed. Everything
/// else stays at WARN.
fn log_delivery_failure(result: SlackResult, channel: &str, operation: &str) {
    match result {
        SlackResult::Ok => {}
        SlackResult::ApiError(e) if is_misconfigured_target(&e) => {
            error!(
                error = %e,
                channel = %channel,
                operation = operation,
                "Slack delivery target is misconfigured: notifications to this channel are being dropped"
            );
        }
        SlackResult::ApiError(e) | SlackResult::HttpError(e) => {
            warn!(error = %e, channel = %channel, operation = operation, "Slack {operation} failed");
        }
    }
}

/// Slack API client with bot token authentication
///
/// Provides methods for posting Block Kit messages, updating existing messages,
/// and verifying HMAC-SHA256 signatures on incoming Slack requests.
#[derive(Clone)]
pub struct SlackClient {
    http: Client,
    bot_token: String,
    signing_secret: Option<String>,
}

/// Result of a Slack API call
#[derive(Debug)]
pub enum SlackResult {
    /// Message sent successfully
    Ok,
    /// Slack API returned ok=false with an error string
    ApiError(String),
    /// HTTP-level failure
    HttpError(String),
}

/// Error from Slack signature verification
#[derive(Debug)]
pub enum SignatureError {
    /// Missing required header
    MissingHeader(&'static str),
    /// Timestamp too old (replay attack protection)
    TimestampExpired(u64),
    /// HMAC signature mismatch
    InvalidSignature,
    /// No signing secret configured
    NotConfigured,
}

impl fmt::Display for SignatureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingHeader(h) => write!(f, "missing header: {h}"),
            Self::TimestampExpired(age) => write!(f, "timestamp too old ({age}s)"),
            Self::InvalidSignature => write!(f, "invalid HMAC signature"),
            Self::NotConfigured => write!(f, "signing secret not configured"),
        }
    }
}

impl SlackClient {
    /// Create a new Slack client from configuration
    pub fn new(config: &SlackConfig) -> Self {
        Self {
            http: Client::new(),
            bot_token: config.bot_token.clone(),
            signing_secret: config.signing_secret.clone(),
        }
    }

    /// Post a Block Kit message to a channel
    ///
    /// Fire-and-forget: spawns a background task. Errors are logged, never propagated.
    pub fn post_message(&self, channel: &str, blocks: &Value) {
        let token = self.bot_token.clone();
        let client = self.http.clone();
        let channel = channel.to_owned();
        let payload = serde_json::json!({
            "channel": channel,
            "blocks": blocks,
        });

        tokio::spawn(async move {
            let result =
                send_slack_request(&client, SLACK_POST_MESSAGE_URL, &token, &payload).await;
            log_delivery_failure(result, &channel, "post_message");
        });
    }

    /// Post a Block Kit message and return the result (awaitable)
    ///
    /// Use this when you need to know whether the message was sent.
    pub async fn post_message_await(&self, channel: &str, blocks: &Value) -> SlackResult {
        let payload = serde_json::json!({
            "channel": channel,
            "blocks": blocks,
        });
        send_slack_request(
            &self.http,
            SLACK_POST_MESSAGE_URL,
            &self.bot_token,
            &payload,
        )
        .await
    }

    /// Update an existing Slack message (replace blocks)
    ///
    /// Fire-and-forget: spawns a background task.
    pub fn update_message(&self, channel: &str, message_ts: &str, blocks: &Value) {
        let token = self.bot_token.clone();
        let client = self.http.clone();
        let channel = channel.to_owned();
        let payload = serde_json::json!({
            "channel": channel,
            "ts": message_ts,
            "blocks": blocks,
        });

        tokio::spawn(async move {
            let result = send_slack_request(&client, SLACK_CHAT_UPDATE_URL, &token, &payload).await;
            log_delivery_failure(result, &channel, "update_message");
        });
    }

    /// Verify a Slack request signature (HMAC-SHA256 v0 scheme)
    ///
    /// Validates:
    /// - `x-slack-request-timestamp` is present and within 5 minutes
    /// - `x-slack-signature` matches HMAC-SHA256 of `v0:{timestamp}:{body}`
    pub fn verify_signature(
        &self,
        timestamp: &str,
        signature: &str,
        body: &[u8],
    ) -> Result<(), SignatureError> {
        let signing_secret = self
            .signing_secret
            .as_deref()
            .ok_or(SignatureError::NotConfigured)?;

        // Replay protection
        let ts: u64 = timestamp
            .parse()
            .map_err(|_| SignatureError::MissingHeader("x-slack-request-timestamp"))?;
        let now = u64::try_from(chrono::Utc::now().timestamp()).unwrap_or(0);
        let age = now.saturating_sub(ts);
        if age > MAX_TIMESTAMP_AGE_SECS {
            return Err(SignatureError::TimestampExpired(age));
        }

        // Compute HMAC-SHA256 using Slack v0 scheme
        let body_str = str::from_utf8(body).unwrap_or("");
        let basestring = format!("v0:{timestamp}:{body_str}");
        let key = hmac::Key::new(hmac::HMAC_SHA256, signing_secret.as_bytes());
        let tag = hmac::sign(&key, basestring.as_bytes());
        let expected = format!("v0={}", hex::encode(tag.as_ref()));

        // Constant-time comparison to avoid leaking the expected HMAC via timing
        // (mirrors the bearer-token check in `crate::server::auth`). Length is not
        // secret, so the length-mismatch short-circuit in `ct_eq` is acceptable.
        if signature.as_bytes().ct_eq(expected.as_bytes()).into() {
            Ok(())
        } else {
            Err(SignatureError::InvalidSignature)
        }
    }
}

/// Send a request to a Slack API endpoint and parse the response
async fn send_slack_request(
    client: &Client,
    url: &str,
    token: &str,
    payload: &Value,
) -> SlackResult {
    let response = match client
        .post(url)
        .header("Authorization", format!("Bearer {token}"))
        .json(payload)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return SlackResult::HttpError(e.to_string()),
    };

    if !response.status().is_success() {
        return SlackResult::HttpError(format!("HTTP {}", response.status()));
    }

    match response.json::<Value>().await {
        Ok(body) => {
            let ok = body.get("ok").and_then(Value::as_bool).unwrap_or(false);
            if ok {
                SlackResult::Ok
            } else {
                let error = body
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                SlackResult::ApiError(error.to_owned())
            }
        }
        Err(e) => SlackResult::HttpError(format!("response parse: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renamed_channel_is_a_misconfigured_target() {
        // The exact outage this classifier exists for: renaming a Slack channel
        // makes every post to the old #name return ok=false/channel_not_found,
        // and fire-and-forget posting swallowed it at WARN.
        assert!(is_misconfigured_target("channel_not_found"));
    }

    #[test]
    fn operator_actionable_slack_errors_are_misconfigured_targets() {
        for code in [
            "channel_not_found",
            "is_archived",
            "not_in_channel",
            "invalid_auth",
            "not_authed",
            "account_inactive",
            "token_revoked",
            "missing_scope",
            "restricted_action",
        ] {
            assert!(
                is_misconfigured_target(code),
                "{code} must escalate to ERROR"
            );
        }
        assert_eq!(SLACK_MISCONFIGURED_TARGET_ERRORS.len(), 9);
    }

    #[test]
    fn transient_slack_errors_are_not_misconfigured_targets() {
        // These clear on retry; escalating them would page on ordinary noise.
        for code in [
            "ratelimited",
            "service_unavailable",
            "request_timeout",
            "fatal_error",
            "internal_error",
            "",
        ] {
            assert!(!is_misconfigured_target(code), "{code} must stay at WARN");
        }
    }

    #[test]
    fn signature_error_display() {
        assert_eq!(
            SignatureError::MissingHeader("x-slack-request-timestamp").to_string(),
            "missing header: x-slack-request-timestamp"
        );
        assert_eq!(
            SignatureError::TimestampExpired(600).to_string(),
            "timestamp too old (600s)"
        );
        assert_eq!(
            SignatureError::InvalidSignature.to_string(),
            "invalid HMAC signature"
        );
        assert_eq!(
            SignatureError::NotConfigured.to_string(),
            "signing secret not configured"
        );
    }

    #[test]
    fn verify_signature_rejects_without_secret() {
        let config = SlackConfig {
            bot_token: "xoxb-test".into(),
            error_channel: "#errors".into(),
            signing_secret: None,
        };
        let client = SlackClient::new(&config);
        let result = client.verify_signature("1234567890", "v0=abc", b"body");
        assert!(matches!(result, Err(SignatureError::NotConfigured)));
    }

    #[test]
    fn verify_signature_validates_correctly() {
        let secret = "test_signing_secret";
        let timestamp = "1234567890";
        let body = b"test_body";

        // Compute expected signature
        let basestring = format!(
            "v0:{timestamp}:{}",
            str::from_utf8(body).expect("test body is valid UTF-8") // Safe: test assertion
        );
        let key = hmac::Key::new(hmac::HMAC_SHA256, secret.as_bytes());
        let tag = hmac::sign(&key, basestring.as_bytes());
        let signature = format!("v0={}", hex::encode(tag.as_ref()));

        let config = SlackConfig {
            bot_token: "xoxb-test".into(),
            error_channel: "#errors".into(),
            signing_secret: Some(secret.into()),
        };
        let client = SlackClient::new(&config);

        // Valid signature (timestamp will be "expired" but we test the HMAC path)
        // We expect TimestampExpired since the timestamp is from 2009
        let result = client.verify_signature(timestamp, &signature, body);
        assert!(matches!(result, Err(SignatureError::TimestampExpired(_))));
    }

    #[test]
    fn verify_signature_accepts_valid_with_fresh_timestamp() {
        // Regression: the valid-signature path must accept a correctly computed
        // HMAC when compared in constant time (ct_eq), not only reject bad ones.
        let secret = "test_signing_secret";
        let now = chrono::Utc::now().timestamp().to_string();
        let body = b"payload=hello";

        let basestring = format!(
            "v0:{now}:{}",
            str::from_utf8(body).expect("test body is valid UTF-8") // Safe: test assertion
        );
        let key = hmac::Key::new(hmac::HMAC_SHA256, secret.as_bytes());
        let tag = hmac::sign(&key, basestring.as_bytes());
        let signature = format!("v0={}", hex::encode(tag.as_ref()));

        let config = SlackConfig {
            bot_token: "xoxb-test".into(),
            error_channel: "#errors".into(),
            signing_secret: Some(secret.into()),
        };
        let client = SlackClient::new(&config);

        let result = client.verify_signature(&now, &signature, body);
        assert!(result.is_ok());
    }

    #[test]
    fn verify_signature_rejects_bad_signature() {
        let config = SlackConfig {
            bot_token: "xoxb-test".into(),
            error_channel: "#errors".into(),
            signing_secret: Some("secret".into()),
        };
        let client = SlackClient::new(&config);

        // Use a recent timestamp so we don't hit the expiry check
        let now = chrono::Utc::now().timestamp().to_string();
        let result = client.verify_signature(&now, "v0=badhash", b"body");
        assert!(matches!(result, Err(SignatureError::InvalidSignature)));
    }
}
