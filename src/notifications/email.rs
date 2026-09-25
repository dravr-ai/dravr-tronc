// ABOUTME: Email alert client: error alerts to the configured recipients, through ResendClient
// ABOUTME: Fire-and-forget delivery on the same Resend send path and 429 retry policy as host mail
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use reqwest::Client;
use tracing::warn;

use super::resend::{ResendBody, ResendClient, ResendEmail, ResendError};
use super::EmailConfig;

/// Email client for sending alert notifications via the Resend API
///
/// Sends through [`ResendClient`], the same client and `429` retry policy a
/// host's transactional mail uses, so alerts sharing a service's
/// `RESEND_API_KEY` wait out a rate limit instead of being dropped by it.
#[derive(Clone)]
pub struct EmailClient {
    resend: ResendClient,
    from_address: String,
    to_addresses: Vec<String>,
}

impl EmailClient {
    /// Create a new email client from configuration
    ///
    /// Uses the Resend HTTP API — no SMTP configuration needed.
    ///
    /// # Errors
    ///
    /// [`ResendError::MissingApiKey`] when the configured key is empty.
    pub fn new(config: &EmailConfig) -> Result<Self, ResendError> {
        Ok(Self {
            resend: ResendClient::new(Client::new(), config.resend_api_key.clone())?,
            from_address: config.from_address.clone(),
            to_addresses: config.to_addresses.clone(),
        })
    }

    /// Send an error alert email
    ///
    /// Fire-and-forget: spawns a background task. Errors are logged, never propagated.
    pub fn send_alert(&self, subject: &str, body: &str) {
        let resend = self.resend.clone();
        let email = self.alert(subject, body);

        tokio::spawn(async move {
            if let Err(e) = resend.send(&email).await {
                warn!(error = %e, "Email alert via Resend failed");
            }
        });
    }

    /// The alert email for `subject` and `body`, addressed from the configured
    /// sender to every configured recipient, as plain text.
    fn alert(&self, subject: &str, body: &str) -> ResendEmail {
        ResendEmail {
            from: self.from_address.clone(),
            to: self.to_addresses.clone(),
            subject: subject.to_owned(),
            body: ResendBody::Text(body.to_owned()),
        }
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
        assert!(matches!(
            EmailClient::new(&config),
            Err(ResendError::MissingApiKey)
        ));
    }

    #[test]
    fn an_alert_is_plain_text_from_the_sender_to_every_recipient() {
        let config = EmailConfig {
            resend_api_key: "re_test_key".into(),
            from_address: "alerts@dravr.ai".into(),
            to_addresses: vec!["jf@dravr.ai".into(), "phil@dravr.ai".into()],
        };
        let client = EmailClient::new(&config).expect("valid config"); // Safe: test assertion

        assert_eq!(
            client.alert("ERROR in svc", "trace"),
            ResendEmail {
                from: "alerts@dravr.ai".into(),
                to: vec!["jf@dravr.ai".into(), "phil@dravr.ai".into()],
                subject: "ERROR in svc".into(),
                body: ResendBody::Text("trace".into()),
            }
        );
    }
}
