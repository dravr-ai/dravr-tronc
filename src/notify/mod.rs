// ABOUTME: Tracing-based business-event notification layer with per-event routing rules
// ABOUTME: Wires `info!(target: "notify", event = "...", ...)` emissions to Slack via NotifyLayer
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

//! # notify
//!
//! Tracing-based business-event routing to Slack.
//!
//! See [ADR-014](https://github.com/dravr-ai/dravr-vault/blob/main/Architecture/ADRs/ADR-014%20NotifyLayer%20tracing-based%20Slack%20event%20notifications.md)
//! for the full design rationale. In short:
//!
//! Call sites emit a single `info!` with `target: "notify"`. The
//! [`NotifyLayer`] reads the enclosing `#[tracing::instrument]` span for
//! `tenant_id` / `user_id` context, looks up a [`RoutingRule`] from the
//! [`RoutingProvider`], applies dedup / sampling / env filtering / batching,
//! and posts a Slack message via [`SlackClient`](crate::notifications::SlackClient).
//!
//! ## Example
//!
//! ```rust,ignore
//! use std::sync::Arc;
//! use std::time::Duration;
//! use dravr_tronc::notifications::SlackClient;
//! use dravr_tronc::notify::{
//!     DedupRule, NotifyLayer, RoutingRule, StaticRoutingProvider,
//! };
//! use tracing::{info, instrument};
//! use tracing_subscriber::layer::SubscriberExt as _;
//! use tracing_subscriber::util::SubscriberInitExt as _;
//!
//! let slack = Arc::new(slack_client);
//! let mut login_rule = RoutingRule::to_channel("#pierre-pulse");
//! login_rule.dedup = Some(DedupRule {
//!     keys: vec!["user_id".to_owned()],
//!     window: Duration::from_mins(60),
//! });
//! let provider = StaticRoutingProvider::new().with_rule("user.login", login_rule);
//! let notify = NotifyLayer::new(slack, Arc::new(provider), "production".to_owned());
//!
//! tracing_subscriber::registry()
//!     .with(notify)
//!     .with(tracing_subscriber::fmt::layer())
//!     .init();
//!
//! #[instrument(skip_all, fields(tenant_id = %tenant_id, user_id = %user_id))]
//! async fn login_handler(tenant_id: &str, user_id: &str) {
//!     // ... auth logic ...
//!     info!(target: "notify", event = "user.login", "user authenticated");
//! }
//! ```

mod layer;
mod provider;
mod rule;
mod state;
mod visit;

pub use layer::{NotifyLayer, NotifyLayerBuilder, NOTIFY_TARGET};
pub use provider::{RoutingProvider, StaticRoutingProvider};
pub use rule::{BatchRule, DedupRule, RoutingRule};
