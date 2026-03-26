// ABOUTME: Email notification client using SMTP for sending error alerts
// ABOUTME: Supports STARTTLS with configurable SMTP server credentials
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use lettre::message::header::ContentType;
use lettre::transport::smtp::authentication::Credentials;
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};
use tracing::warn;

use super::EmailConfig;

/// Email client for sending alert notifications via SMTP
#[derive(Clone)]
pub struct EmailClient {
    transport: AsyncSmtpTransport<Tokio1Executor>,
    from_address: String,
    to_addresses: Vec<String>,
}

/// Result of an email send operation
#[derive(Debug)]
pub enum EmailResult {
    /// Email sent successfully
    Ok,
    /// Failed to build the email message
    BuildError(String),
    /// SMTP transport error
    SendError(String),
}

impl EmailClient {
    /// Create a new email client from configuration
    ///
    /// Establishes an SMTP connection with STARTTLS and credential authentication.
    pub fn new(config: &EmailConfig) -> Result<Self, String> {
        let creds = Credentials::new(config.smtp_username.clone(), config.smtp_password.clone());

        let transport = AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&config.smtp_host)
            .map_err(|e| format!("SMTP relay setup failed: {e}"))?
            .port(config.smtp_port)
            .credentials(creds)
            .build();

        Ok(Self {
            transport,
            from_address: config.from_address.clone(),
            to_addresses: config.to_addresses.clone(),
        })
    }

    /// Send an error alert email
    ///
    /// Fire-and-forget: spawns a background task. Errors are logged, never propagated.
    pub fn send_alert(&self, subject: &str, body: &str) {
        let transport = self.transport.clone();
        let from = self.from_address.clone();
        let recipients = self.to_addresses.clone();
        let subject = subject.to_owned();
        let body = body.to_owned();

        tokio::spawn(async move {
            for to in &recipients {
                let result = send_single_email(&transport, &from, to, &subject, &body).await;
                if let EmailResult::BuildError(e) | EmailResult::SendError(e) = result {
                    warn!(to = %to, error = %e, "Email alert failed");
                }
            }
        });
    }

    /// Send an error alert email and return the result (awaitable)
    pub async fn send_alert_await(&self, subject: &str, body: &str) -> Vec<EmailResult> {
        let mut results = Vec::with_capacity(self.to_addresses.len());
        for to in &self.to_addresses {
            let result =
                send_single_email(&self.transport, &self.from_address, to, subject, body).await;
            results.push(result);
        }
        results
    }
}

/// Send a single email to one recipient
async fn send_single_email(
    transport: &AsyncSmtpTransport<Tokio1Executor>,
    from: &str,
    to: &str,
    subject: &str,
    body: &str,
) -> EmailResult {
    let email = match Message::builder()
        .from(from.parse().unwrap_or_else(|_| {
            "alerts@dravr.ai"
                .parse()
                .expect("valid fallback email address")
        }))
        .to(match to.parse() {
            Ok(addr) => addr,
            Err(e) => return EmailResult::BuildError(format!("invalid recipient {to}: {e}")),
        })
        .subject(subject)
        .header(ContentType::TEXT_PLAIN)
        .body(body.to_owned())
    {
        Ok(m) => m,
        Err(e) => return EmailResult::BuildError(e.to_string()),
    };

    match transport.send(email).await {
        Ok(_) => EmailResult::Ok,
        Err(e) => EmailResult::SendError(e.to_string()),
    }
}
