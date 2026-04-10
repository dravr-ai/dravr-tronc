// ABOUTME: Tracing subscriber initialization shared across all dravr-xxx server binaries
// ABOUTME: Routes logs to stderr for stdio transport, optionally adds error notification layer
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use std::io;

use tracing_subscriber::fmt;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

/// Initialize the tracing subscriber based on the transport mode
///
/// - `"stdio"` transport: logs go to **stderr** (stdout is reserved for JSON-RPC)
/// - Any other transport: logs go to **stdout**
///
/// Reads `RUST_LOG` env var for filter directives, defaults to `"info"`.
pub fn init(transport: &str) {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    if transport == "stdio" {
        tracing_subscriber::registry()
            .with(filter)
            .with(fmt::layer().with_writer(io::stderr))
            .init();
    } else {
        tracing_subscriber::registry()
            .with(filter)
            .with(fmt::layer())
            .init();
    }
}

/// Initialize tracing with the error notification layer enabled
///
/// Same as [`init`] but adds an [`ErrorNotificationLayer`] that captures
/// ERROR-level events and dispatches them to Slack and/or email based on
/// environment configuration.
///
/// Call this instead of [`init`] when you want automatic error alerting.
///
/// [`ErrorNotificationLayer`]: crate::notifications::ErrorNotificationLayer
#[cfg(feature = "notifications")]
pub fn init_with_notifications(transport: &str) {
    use crate::notifications::{
        EmailClient, ErrorNotificationLayer, NotificationConfig, SlackClient,
    };

    let config = NotificationConfig::from_env();

    let slack = config.slack.as_ref().map(SlackClient::new);

    let email = config.email.as_ref().and_then(|c| EmailClient::new(c).ok());

    let has_channels = slack.is_some() || email.is_some();

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    if transport == "stdio" {
        let registry = tracing_subscriber::registry()
            .with(filter)
            .with(fmt::layer().with_writer(io::stderr));

        if has_channels {
            let error_layer = ErrorNotificationLayer::new(config, slack, email);
            registry.with(error_layer).init();
        } else {
            registry.init();
        }
    } else {
        let registry = tracing_subscriber::registry()
            .with(filter)
            .with(fmt::layer());

        if has_channels {
            let error_layer = ErrorNotificationLayer::new(config, slack, email);
            registry.with(error_layer).init();
        } else {
            registry.init();
        }
    }
}
