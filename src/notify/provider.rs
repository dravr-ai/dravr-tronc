// ABOUTME: RoutingProvider trait and StaticRoutingProvider implementation
// ABOUTME: Trait keeps NotifyLayer free of YAML/contremaitre knowledge; static impl serves tests and small services
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use std::collections::HashMap;

use super::rule::RoutingRule;

/// Resolves an event name to a `RoutingRule`.
///
/// Implementations are expected to be cheap to call (one lookup per emitted
/// event) and thread-safe — `NotifyLayer` calls `route_for` from the tracing
/// thread synchronously.
///
/// Concrete back-ends:
///
/// - [`StaticRoutingProvider`] — in-memory `HashMap`, suitable for tests
///   and services without hot-reload.
/// - `ContremaitreRoutingProvider` (in `pierre-server`) — ArcSwap-backed
///   YAML reloaded from the dravr-contremaitre submodule.
pub trait RoutingProvider: Send + Sync + 'static {
    /// Look up the rule for `event`. `None` means "no rule registered" —
    /// `NotifyLayer` will fall back to its configured default rule if any.
    fn route_for(&self, event: &str) -> Option<RoutingRule>;
}

/// In-memory `RoutingProvider`. Holds a frozen map; mutate via the builder
/// methods before handing it to `NotifyLayer`.
#[derive(Debug, Default, Clone)]
pub struct StaticRoutingProvider {
    rules: HashMap<String, RoutingRule>,
}

impl StaticRoutingProvider {
    /// Create an empty provider.
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert a rule keyed by event name. Replaces any prior rule for that key.
    pub fn with_rule(mut self, event: impl Into<String>, rule: RoutingRule) -> Self {
        self.rules.insert(event.into(), rule);
        self
    }

    /// Number of registered rules. Useful for tests.
    pub fn len(&self) -> usize {
        self.rules.len()
    }

    /// Whether no rules are registered.
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }
}

impl RoutingProvider for StaticRoutingProvider {
    fn route_for(&self, event: &str) -> Option<RoutingRule> {
        self.rules.get(event).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_for_hits_registered_event() {
        let provider =
            StaticRoutingProvider::new().with_rule("user.login", RoutingRule::to_channel("#pulse"));
        let rule = provider.route_for("user.login").expect("rule registered"); // Safe: test assertion
        assert_eq!(rule.channel, "#pulse");
    }

    #[test]
    fn route_for_misses_unknown_event() {
        let provider = StaticRoutingProvider::new();
        assert!(provider.route_for("never.registered").is_none());
    }

    #[test]
    fn with_rule_replaces_prior_entry() {
        let provider = StaticRoutingProvider::new()
            .with_rule("user.login", RoutingRule::to_channel("#first"))
            .with_rule("user.login", RoutingRule::to_channel("#second"));
        let rule = provider.route_for("user.login").expect("rule registered"); // Safe: test assertion
        assert_eq!(rule.channel, "#second");
        assert_eq!(provider.len(), 1);
    }
}
