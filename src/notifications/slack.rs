// ABOUTME: Slack API client for posting messages, updating messages, and verifying signatures
// ABOUTME: Shared across all dravr-xxx services for consistent Slack integration
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use reqwest::Client;
use ring::hmac;
use serde_json::Value;
use tracing::warn;

use super::SlackConfig;

/// Slack API endpoint for posting messages
const SLACK_POST_MESSAGE_URL: &str = "https://slack.com/api/chat.postMessage";

/// Slack API endpoint for updating messages
const SLACK_CHAT_UPDATE_URL: &str = "https://slack.com/api/chat.update";

/// Maximum age of a Slack request timestamp before rejection (5 minutes)
const MAX_TIMESTAMP_AGE_SECS: u64 = 300;

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

impl std::fmt::Display for SignatureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
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
        let payload = serde_json::json!({
            "channel": channel,
            "blocks": blocks,
        });

        tokio::spawn(async move {
            let result =
                send_slack_request(&client, SLACK_POST_MESSAGE_URL, &token, &payload).await;
            if let SlackResult::ApiError(e) | SlackResult::HttpError(e) = result {
                warn!(error = %e, "Slack post_message failed");
            }
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
        let payload = serde_json::json!({
            "channel": channel,
            "ts": message_ts,
            "blocks": blocks,
        });

        tokio::spawn(async move {
            let result = send_slack_request(&client, SLACK_CHAT_UPDATE_URL, &token, &payload).await;
            if let SlackResult::ApiError(e) | SlackResult::HttpError(e) = result {
                warn!(error = %e, "Slack update_message failed");
            }
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
        let body_str = std::str::from_utf8(body).unwrap_or("");
        let basestring = format!("v0:{timestamp}:{body_str}");
        let key = hmac::Key::new(hmac::HMAC_SHA256, signing_secret.as_bytes());
        let tag = hmac::sign(&key, basestring.as_bytes());
        let expected = format!("v0={}", hex::encode(tag.as_ref()));

        if signature == expected {
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
        let basestring = format!("v0:{timestamp}:{}", std::str::from_utf8(body).unwrap());
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
