// ABOUTME: Notification infrastructure for error alerts and operational events
// ABOUTME: Provides Slack client, email client, and a tracing Layer for automatic error dispatch
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

//! # Notifications
//!
//! Shared notification infrastructure for all dravr-xxx services.
//!
//! - [`SlackClient`]: Post and update Slack messages, verify request signatures
//! - [`EmailClient`]: Send email alerts via SMTP
//! - [`ErrorNotificationLayer`]: A `tracing::Layer` that intercepts ERROR-level
//!   events and dispatches them to configured channels with batching,
//!   deduplication, and rate limiting.

mod config;
mod email;
mod error_layer;
mod slack;

pub use config::{EmailConfig, NotificationConfig, SlackConfig};
pub use email::EmailClient;
pub use error_layer::ErrorNotificationLayer;
pub use slack::SlackClient;
