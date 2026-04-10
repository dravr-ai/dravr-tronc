// ABOUTME: Tracing Layer that intercepts ERROR events and dispatches to Slack/email
// ABOUTME: Implements batching, deduplication, and rate limiting for production safety
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use std::collections::HashMap;
use std::fmt::{self, Write as _};
use std::time::{Duration, Instant};

use serde_json::json;
use tokio::sync::mpsc;
use tokio::time::{self, Instant as TokioInstant};
use tracing::field::{Field, Visit};
use tracing::{warn, Event, Level, Subscriber};
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::Layer;

use super::{EmailClient, NotificationConfig, SlackClient};

/// A `tracing::Layer` that captures ERROR-level events and dispatches
/// batched notifications to Slack and/or email.
///
/// ## How it works
///
/// 1. `on_event` fires synchronously for every tracing event
/// 2. ERROR events are extracted and sent via an `mpsc` channel (non-blocking)
/// 3. A background tokio task consumes the channel, batches events over a
///    configurable window, deduplicates by error key, and dispatches to
///    configured notification channels
///
/// ## Thread safety
///
/// The layer itself is `Send + Sync`. The background dispatcher runs in a
/// separate tokio task.
pub struct ErrorNotificationLayer {
    sender: mpsc::UnboundedSender<ErrorEvent>,
}

/// Captured error event ready for dispatch
#[derive(Debug, Clone)]
struct ErrorEvent {
    /// Tracing target (e.g. `dravr_canot::dispatch`)
    target: String,
    /// Error message
    message: String,
    /// Structured fields from the tracing event
    fields: HashMap<String, String>,
}

impl ErrorEvent {
    /// Deduplication key: target + first 80 chars of message
    fn dedup_key(&self) -> String {
        let msg_prefix: String = self.message.chars().take(80).collect();
        format!("{}::{}", self.target, msg_prefix)
    }
}

/// Visitor that extracts message and fields from a tracing Event
struct EventVisitor {
    message: String,
    fields: HashMap<String, String>,
}

impl EventVisitor {
    fn new() -> Self {
        Self {
            message: String::new(),
            fields: HashMap::new(),
        }
    }
}

impl Visit for EventVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        let val = format!("{value:?}");
        if field.name() == "message" {
            self.message = val;
        } else {
            self.fields.insert(field.name().to_owned(), val);
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            value.clone_into(&mut self.message);
        } else {
            self.fields
                .insert(field.name().to_owned(), value.to_owned());
        }
    }
}

impl ErrorNotificationLayer {
    /// Create a new error notification layer and start the background dispatcher
    ///
    /// The dispatcher runs as a `tokio::spawn`ed task and lives for the lifetime
    /// of the application. When the layer is dropped, the channel closes and
    /// the dispatcher flushes remaining events before exiting.
    pub fn new(
        config: NotificationConfig,
        slack: Option<SlackClient>,
        email: Option<EmailClient>,
    ) -> Self {
        let (sender, receiver) = mpsc::unbounded_channel();
        let dispatcher = Dispatcher::new(config, slack, email, receiver);
        tokio::spawn(dispatcher.run());
        Self { sender }
    }
}

impl<S> Layer<S> for ErrorNotificationLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        if *event.metadata().level() != Level::ERROR {
            return;
        }

        let mut visitor = EventVisitor::new();
        event.record(&mut visitor);

        let error_event = ErrorEvent {
            target: event.metadata().target().to_owned(),
            message: visitor.message,
            fields: visitor.fields,
        };

        // Non-blocking send — if the channel is full/closed, we just drop the event
        let _ = self.sender.send(error_event);
    }
}

/// Background dispatcher that batches, deduplicates, and rate-limits notifications
struct Dispatcher {
    config: NotificationConfig,
    slack: Option<SlackClient>,
    email: Option<EmailClient>,
    receiver: mpsc::UnboundedReceiver<ErrorEvent>,
    /// Track last notification time per dedup key
    last_notified: HashMap<String, Instant>,
    /// Global rate limiter: timestamps of recent sends
    recent_sends: Vec<Instant>,
}

impl Dispatcher {
    fn new(
        config: NotificationConfig,
        slack: Option<SlackClient>,
        email: Option<EmailClient>,
        receiver: mpsc::UnboundedReceiver<ErrorEvent>,
    ) -> Self {
        Self {
            config,
            slack,
            email,
            receiver,
            last_notified: HashMap::new(),
            recent_sends: Vec::new(),
        }
    }

    /// Main dispatch loop
    ///
    /// Collects events over the batch window, then dispatches a single
    /// notification summarizing all errors in the window.
    async fn run(mut self) {
        loop {
            let mut batch: Vec<ErrorEvent> = Vec::new();

            // Wait for the first event (blocks until one arrives or channel closes)
            let first = self.receiver.recv().await;
            let Some(first_event) = first else {
                // Channel closed — flush and exit
                break;
            };
            batch.push(first_event);

            // Collect more events within the batch window
            let deadline = TokioInstant::now() + self.config.batch_window;
            loop {
                let remaining = deadline.saturating_duration_since(TokioInstant::now());
                if remaining.is_zero() {
                    break;
                }
                match time::timeout(remaining, self.receiver.recv()).await {
                    Ok(Some(event)) => batch.push(event),
                    Ok(None) => return, // Channel closed
                    Err(_) => break,    // Timeout — batch window elapsed
                }
            }

            self.dispatch_batch(batch).await;
        }
    }

    /// Process a batch of error events
    async fn dispatch_batch(&mut self, batch: Vec<ErrorEvent>) {
        // Deduplicate by error key
        let mut grouped: HashMap<String, (ErrorEvent, u64)> = HashMap::new();
        for event in batch {
            let key = event.dedup_key();
            grouped
                .entry(key)
                .and_modify(|(_, count)| *count += 1)
                .or_insert((event, 1));
        }

        // Filter out recently-notified errors
        let now = Instant::now();
        let entries: Vec<(String, ErrorEvent, u64)> = grouped
            .into_iter()
            .filter(|(key, _)| {
                self.last_notified
                    .get(key)
                    .is_none_or(|last| now.duration_since(*last) >= self.config.dedup_window)
            })
            .map(|(key, (event, count))| (key, event, count))
            .collect();

        if entries.is_empty() {
            return;
        }

        // Global rate limit check
        self.prune_recent_sends(now);
        if !self.under_rate_limit() {
            warn!(
                queued_errors = entries.len(),
                "Error notification rate limit reached, dropping batch"
            );
            return;
        }

        // Build and send notifications
        self.send_slack_digest(&entries).await;
        self.send_email_digest(&entries);

        // Update bookkeeping
        for (key, _, _) in &entries {
            self.last_notified.insert(key.clone(), now);
        }
        self.recent_sends.push(now);
    }

    /// Send a Slack Block Kit digest for the batch
    async fn send_slack_digest(&self, entries: &[(String, ErrorEvent, u64)]) {
        let Some(slack) = &self.slack else { return };

        let mut blocks = vec![json!({
            "type": "header",
            "text": {
                "type": "plain_text",
                "text": format!("Error Alert — {}", self.config.service_name),
            }
        })];

        // Environment context
        blocks.push(json!({
            "type": "context",
            "elements": [{
                "type": "mrkdwn",
                "text": format!(
                    "*Environment:* {} | *Errors:* {}",
                    self.config.environment,
                    entries.len()
                )
            }]
        }));

        blocks.push(json!({ "type": "divider" }));

        // Each unique error as a section (cap at 8 to stay within Slack limits)
        let display_limit = entries.len().min(8);
        for (_, event, count) in entries.iter().take(display_limit) {
            let count_suffix = if *count > 1 {
                format!(" (x{count})")
            } else {
                String::new()
            };

            let fields_str = if event.fields.is_empty() {
                String::new()
            } else {
                let pairs: Vec<String> = event
                    .fields
                    .iter()
                    .take(4)
                    .map(|(k, v)| format!("`{k}`: {v}"))
                    .collect();
                format!("\n{}", pairs.join(" | "))
            };

            blocks.push(json!({
                "type": "section",
                "text": {
                    "type": "mrkdwn",
                    "text": format!(
                        "*{}*{}\n```{}```{}",
                        event.target,
                        count_suffix,
                        truncate_message(&event.message, 500),
                        fields_str,
                    )
                }
            }));
        }

        if entries.len() > display_limit {
            blocks.push(json!({
                "type": "context",
                "elements": [{
                    "type": "mrkdwn",
                    "text": format!("...and {} more errors", entries.len() - display_limit)
                }]
            }));
        }

        let channel = self
            .config
            .slack
            .as_ref()
            .map_or("#errors", |s| s.error_channel.as_str());

        let blocks_value = serde_json::Value::Array(blocks);
        let result = slack.post_message_await(channel, &blocks_value).await;
        if let super::slack::SlackResult::ApiError(e) | super::slack::SlackResult::HttpError(e) =
            result
        {
            warn!(error = %e, "Failed to send error digest to Slack");
        }
    }

    /// Send an email digest for the batch
    fn send_email_digest(&self, entries: &[(String, ErrorEvent, u64)]) {
        let Some(email) = &self.email else { return };

        let subject = format!(
            "[{}] {} error(s) in {}",
            self.config.environment,
            entries.len(),
            self.config.service_name,
        );

        let mut body = format!(
            "Error digest for {} ({})\n\n",
            self.config.service_name, self.config.environment
        );

        for (_, event, count) in entries {
            let count_suffix = if *count > 1 {
                format!(" (x{count})")
            } else {
                String::new()
            };

            let _ = write!(
                body,
                "--- {} ---{}\n{}\n",
                event.target, count_suffix, event.message
            );

            for (k, v) in &event.fields {
                let _ = writeln!(body, "  {k}: {v}");
            }
            body.push('\n');
        }

        email.send_alert(&subject, &body);
    }

    /// Remove send timestamps older than 1 minute
    fn prune_recent_sends(&mut self, now: Instant) {
        let one_minute = Duration::from_secs(60);
        self.recent_sends
            .retain(|ts| now.duration_since(*ts) < one_minute);
    }

    /// Check whether we're under the global rate limit
    fn under_rate_limit(&self) -> bool {
        (self.recent_sends.len() as u32) < self.config.max_messages_per_minute
    }
}

/// Truncate a message to a maximum length, adding "..." if truncated
fn truncate_message(msg: &str, max_len: usize) -> String {
    if msg.len() <= max_len {
        msg.to_owned()
    } else {
        let truncated: String = msg.chars().take(max_len.saturating_sub(3)).collect();
        format!("{truncated}...")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_event_dedup_key_uses_target_and_prefix() {
        let event = ErrorEvent {
            target: "dravr_canot::dispatch".into(),
            message: "connection refused to webhook endpoint https://example.com/hook".into(),
            fields: HashMap::new(),
        };

        let key = event.dedup_key();
        assert!(key.starts_with("dravr_canot::dispatch::"));
        assert!(key.contains("connection refused"));
    }

    #[test]
    fn truncate_message_short_passthrough() {
        assert_eq!(truncate_message("short", 100), "short");
    }

    #[test]
    fn truncate_message_long_truncates() {
        let long = "a".repeat(600);
        let result = truncate_message(&long, 500);
        assert!(result.len() <= 500);
        assert!(result.ends_with("..."));
    }

    #[test]
    fn event_visitor_starts_empty() {
        let visitor = EventVisitor::new();
        assert!(visitor.message.is_empty());
        assert!(visitor.fields.is_empty());
    }
}
