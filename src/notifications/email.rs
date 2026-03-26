// ABOUTME: Email notification client using the Resend HTTP API for sending error alerts
// ABOUTME: Fire-and-forget or awaitable email delivery to configured recipients
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use reqwest::Client;
use serde::Serialize;
use tracing::warn;

use super::EmailConfig;

/// Resend API endpoint for sending emails
const RESEND_API_URL: &str = "https://api.resend.com/emails";

/// Email client for sending alert notifications via the Resend API
#[derive(Clone)]
pub struct EmailClient {
    http: Client,
    api_key: String,
    from_address: String,
    to_addresses: Vec<String>,
}

/// Resend API request payload
#[derive(Serialize)]
struct ResendPayload {
    from: String,
    to: Vec<String>,
    subject: String,
    text: String,
}

/// Result of an email send operation
#[derive(Debug)]
pub enum EmailResult {
    /// Email sent successfully
    Ok,
    /// Resend API returned an error
    ApiError(String),
    /// HTTP-level failure
    HttpError(String),
}

impl EmailClient {
    /// Create a new email client from configuration
    ///
    /// Uses the Resend HTTP API — no SMTP configuration needed.
    pub fn new(config: &EmailConfig) -> Result<Self, String> {
        if config.resend_api_key.is_empty() {
            return Err("RESEND_API_KEY is empty".to_owned());
        }

        Ok(Self {
            http: Client::new(),
            api_key: config.resend_api_key.clone(),
            from_address: config.from_address.clone(),
            to_addresses: config.to_addresses.clone(),
        })
    }

    /// Send an error alert email
    ///
    /// Fire-and-forget: spawns a background task. Errors are logged, never propagated.
    pub fn send_alert(&self, subject: &str, body: &str) {
        let client = self.http.clone();
        let api_key = self.api_key.clone();
        let from = self.from_address.clone();
        let recipients = self.to_addresses.clone();
        let subject = subject.to_owned();
        let body = body.to_owned();

        tokio::spawn(async move {
            let result =
                send_via_resend(&client, &api_key, &from, &recipients, &subject, &body).await;
            if let EmailResult::ApiError(e) | EmailResult::HttpError(e) = result {
                warn!(error = %e, "Email alert via Resend failed");
            }
        });
    }

    /// Send an error alert email and return the result (awaitable)
    pub async fn send_alert_await(&self, subject: &str, body: &str) -> EmailResult {
        send_via_resend(
            &self.http,
            &self.api_key,
            &self.from_address,
            &self.to_addresses,
            subject,
            body,
        )
        .await
    }
}

/// Send an email to all recipients via the Resend API
async fn send_via_resend(
    client: &Client,
    api_key: &str,
    from: &str,
    to: &[String],
    subject: &str,
    body: &str,
) -> EmailResult {
    let payload = ResendPayload {
        from: from.to_owned(),
        to: to.to_vec(),
        subject: subject.to_owned(),
        text: body.to_owned(),
    };

    let response = match client
        .post(RESEND_API_URL)
        .header("Authorization", format!("Bearer {api_key}"))
        .json(&payload)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return EmailResult::HttpError(e.to_string()),
    };

    if response.status().is_success() {
        EmailResult::Ok
    } else {
        let status = response.status();
        let body = response
            .text()
            .await
            .unwrap_or_else(|_| "no body".to_owned());
        EmailResult::ApiError(format!("HTTP {status}: {body}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn email_client_rejects_empty_api_key() {
        let config = EmailConfig {
            resend_api_key: String::new(),
            from_address: "alerts@dravr.ai".into(),
            to_addresses: vec!["test@dravr.ai".into()],
        };
        assert!(EmailClient::new(&config).is_err());
    }

    #[test]
    fn email_client_accepts_valid_config() {
        let config = EmailConfig {
            resend_api_key: "re_test_key".into(),
            from_address: "alerts@dravr.ai".into(),
            to_addresses: vec!["jf@dravr.ai".into(), "phil@dravr.ai".into()],
        };
        assert!(EmailClient::new(&config).is_ok());
    }
}
