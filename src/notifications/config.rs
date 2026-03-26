// ABOUTME: Configuration types for notification channels (Slack, email)
// ABOUTME: Reads from environment variables with sensible defaults for batching and rate limiting
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use std::env;
use std::time::Duration;

/// Top-level notification configuration
///
/// Controls which notification channels are active and global settings
/// for batching and rate limiting.
#[derive(Debug, Clone)]
pub struct NotificationConfig {
    /// Slack channel configuration (None = disabled)
    pub slack: Option<SlackConfig>,
    /// Email channel configuration (None = disabled)
    pub email: Option<EmailConfig>,
    /// How long to batch errors before sending a digest (default: 5 seconds)
    pub batch_window: Duration,
    /// Maximum messages per minute across all channels (default: 10)
    pub max_messages_per_minute: u32,
    /// Minimum interval between alerts for the same error key (default: 30 seconds)
    pub dedup_window: Duration,
    /// Service name included in notifications for identification
    pub service_name: String,
    /// Environment label (development, staging, production)
    pub environment: String,
}

/// Slack notification configuration
#[derive(Debug, Clone)]
pub struct SlackConfig {
    /// Bot token for Slack API authentication (xoxb-...)
    pub bot_token: String,
    /// Channel ID or name for error alerts
    pub error_channel: String,
    /// Signing secret for verifying incoming Slack requests
    pub signing_secret: Option<String>,
}

/// Email notification configuration via the Resend API
#[derive(Debug, Clone)]
pub struct EmailConfig {
    /// Resend API key for authentication
    pub resend_api_key: String,
    /// Sender email address (e.g., "Pierre Alerts <alerts@dravr.ai>")
    pub from_address: String,
    /// Recipient email addresses for error alerts
    pub to_addresses: Vec<String>,
}

impl NotificationConfig {
    /// Create configuration from environment variables
    ///
    /// ## Slack env vars
    /// - `SLACK_BOT_TOKEN` — Bot token (required for Slack)
    /// - `SLACK_ERROR_CHANNEL` — Channel for error alerts (required for Slack)
    /// - `SLACK_SIGNING_SECRET` — Signing secret for request verification
    ///
    /// ## Email env vars (via Resend API)
    /// - `RESEND_API_KEY` — Resend API key (required for email)
    /// - `NOTIFY_EMAIL_FROM` — Sender address (required for email)
    /// - `NOTIFY_EMAIL_TO` — Comma-separated recipient addresses (required for email)
    ///
    /// ## General env vars
    /// - `NOTIFY_BATCH_WINDOW_SECS` — Batch window in seconds (default: 5)
    /// - `NOTIFY_MAX_MESSAGES_PER_MIN` — Rate limit (default: 10)
    /// - `NOTIFY_DEDUP_WINDOW_SECS` — Dedup window in seconds (default: 30)
    /// - `SERVICE_NAME` — Service identifier (default: "dravr-service")
    /// - `ENVIRONMENT` — Environment label (default: "development")
    pub fn from_env() -> Self {
        let slack = SlackConfig::from_env();
        let email = EmailConfig::from_env();

        let batch_window_secs: u64 = env::var("NOTIFY_BATCH_WINDOW_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(5);

        let max_messages_per_minute: u32 = env::var("NOTIFY_MAX_MESSAGES_PER_MIN")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(10);

        let dedup_window_secs: u64 = env::var("NOTIFY_DEDUP_WINDOW_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(30);

        let service_name = env::var("SERVICE_NAME").unwrap_or_else(|_| "dravr-service".into());
        let environment = env::var("ENVIRONMENT").unwrap_or_else(|_| "development".into());

        Self {
            slack,
            email,
            batch_window: Duration::from_secs(batch_window_secs),
            max_messages_per_minute,
            dedup_window: Duration::from_secs(dedup_window_secs),
            service_name,
            environment,
        }
    }
}

impl SlackConfig {
    /// Build from environment variables. Returns `None` if required vars are missing.
    fn from_env() -> Option<Self> {
        let bot_token = env::var("SLACK_BOT_TOKEN").ok().filter(|s| !s.is_empty())?;
        let error_channel = env::var("SLACK_ERROR_CHANNEL")
            .ok()
            .filter(|s| !s.is_empty())?;
        let signing_secret = env::var("SLACK_SIGNING_SECRET")
            .ok()
            .filter(|s| !s.is_empty());

        Some(Self {
            bot_token,
            error_channel,
            signing_secret,
        })
    }
}

impl EmailConfig {
    /// Build from environment variables. Returns `None` if required vars are missing.
    fn from_env() -> Option<Self> {
        let resend_api_key = env::var("RESEND_API_KEY").ok().filter(|s| !s.is_empty())?;
        let from_address = env::var("NOTIFY_EMAIL_FROM")
            .ok()
            .filter(|s| !s.is_empty())?;
        let to_addresses_str = env::var("NOTIFY_EMAIL_TO").ok().filter(|s| !s.is_empty())?;

        let to_addresses: Vec<String> = to_addresses_str
            .split(',')
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
            .collect();

        if to_addresses.is_empty() {
            return None;
        }

        Some(Self {
            resend_api_key,
            from_address,
            to_addresses,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_batch_window_is_5_seconds() {
        // Clear env vars that might be set
        env::remove_var("NOTIFY_BATCH_WINDOW_SECS");
        env::remove_var("SLACK_BOT_TOKEN");
        env::remove_var("RESEND_API_KEY");

        let config = NotificationConfig::from_env();
        assert_eq!(config.batch_window, Duration::from_secs(5));
        assert_eq!(config.max_messages_per_minute, 10);
        assert_eq!(config.dedup_window, Duration::from_secs(30));
    }

    #[test]
    fn slack_config_requires_both_token_and_channel() {
        env::remove_var("SLACK_BOT_TOKEN");
        env::remove_var("SLACK_ERROR_CHANNEL");
        assert!(SlackConfig::from_env().is_none());
    }

    #[test]
    fn email_config_requires_all_fields() {
        env::remove_var("RESEND_API_KEY");
        assert!(EmailConfig::from_env().is_none());
    }
}
