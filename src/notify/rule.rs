// ABOUTME: Routing rule types consumed by NotifyLayer to decide channel/dedup/batch/sample/env
// ABOUTME: Pure data — RoutingProvider implementations build these from YAML, env, or static tables
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use std::time::Duration;

/// How a single `event` should be routed to Slack.
///
/// One rule per event name. The `RoutingProvider` owns the lookup; `NotifyLayer`
/// applies the rule. All fields except `channel` are optional refinements.
#[derive(Debug, Clone)]
pub struct RoutingRule {
    /// Slack channel (e.g. `#pierre-events`) the event posts to.
    pub channel: String,
    /// Master kill-switch. When `false` the event is dropped before sampling.
    pub enabled: bool,
    /// Deduplicate identical events within a sliding window.
    pub dedup: Option<DedupRule>,
    /// Coalesce events into one Slack message flushed at a fixed cadence.
    pub batch: Option<BatchRule>,
    /// Probability in `0.0..=1.0` that any given event survives sampling.
    /// `1.0` means "never sample" (deliver every event).
    pub sample_rate: f32,
    /// If `Some`, the event only fires when the current environment matches
    /// one of the listed values (e.g. `["dev"]`). If `None`, fires in any env.
    pub enabled_envs: Option<Vec<String>>,
}

impl RoutingRule {
    /// Build a minimal always-on rule pointed at `channel`.
    pub fn to_channel(channel: impl Into<String>) -> Self {
        Self {
            channel: channel.into(),
            enabled: true,
            dedup: None,
            batch: None,
            sample_rate: 1.0,
            enabled_envs: None,
        }
    }

    /// Returns whether this rule is active for the given environment label.
    pub fn allows_env(&self, env: &str) -> bool {
        self.enabled_envs
            .as_ref()
            .is_none_or(|list| list.iter().any(|allowed| allowed == env))
    }
}

/// Deduplication rule. The layer keeps a per-(event, key tuple) timestamp
/// and drops repeats inside the window.
#[derive(Debug, Clone)]
pub struct DedupRule {
    /// Field names used to build the dedup key. Missing fields render as the
    /// empty string in the key tuple so they still collapse identically.
    pub keys: Vec<String>,
    /// Sliding window during which a repeated event is dropped.
    pub window: Duration,
}

/// Batching rule. Events accumulate in a buffer flushed every `interval`
/// as a single Slack message.
#[derive(Debug, Clone)]
pub struct BatchRule {
    /// Flush interval. Practical lower bound is around one second.
    pub interval: Duration,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_env_when_unset_means_any() {
        let rule = RoutingRule::to_channel("#anywhere");
        assert!(rule.allows_env("dev"));
        assert!(rule.allows_env("production"));
    }

    #[test]
    fn allows_env_filters_to_listed_values() {
        let mut rule = RoutingRule::to_channel("#dev-only");
        rule.enabled_envs = Some(vec!["dev".into()]);
        assert!(rule.allows_env("dev"));
        assert!(!rule.allows_env("production"));
    }

    #[test]
    fn to_channel_defaults_to_enabled_unsampled() {
        let rule = RoutingRule::to_channel("#x");
        assert!(rule.enabled);
        assert!((rule.sample_rate - 1.0).abs() < f32::EPSILON);
        assert!(rule.dedup.is_none());
        assert!(rule.batch.is_none());
    }
}
