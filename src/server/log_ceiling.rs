// ABOUTME: Per-target level ceilings: a tracing filter no RUST_LOG directive can raise
// ABOUTME: Keeps secret-bearing TRACE output of a dependency (chromiumoxide's CDP traffic) out of every sink
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use tracing::{Metadata, Subscriber};
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::layer::{Context, Filter, Layer};

/// The most verbose level a target and its child modules may log at.
///
/// `target` names a crate or module the way a tracing target does
/// (`"chromiumoxide"`, `"chromiumoxide::conn"`). It covers that target and
/// every module under it — `"chromiumoxide"` covers `"chromiumoxide::conn"` —
/// but not a target that merely starts with the same letters:
/// `"chromiumoxide_cdp"` is another crate, listed separately when it needs a
/// ceiling too.
///
/// A ceiling exists for a dependency whose verbose output carries secrets.
/// chromiumoxide logs every CDP command and message at TRACE, typed passwords,
/// one-time codes and session cookies included; a service driving it declares
/// `TargetCeiling::new("chromiumoxide", LevelFilter::DEBUG)` so no `RUST_LOG`
/// can bring those lines back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TargetCeiling {
    /// The target capped, with every module under it.
    pub target: &'static str,
    /// The most verbose level that target may log at; `LevelFilter::OFF`
    /// silences it.
    pub max_level: LevelFilter,
}

impl TargetCeiling {
    /// Cap `target`, and every module under it, at `max_level`.
    #[must_use]
    pub const fn new(target: &'static str, max_level: LevelFilter) -> Self {
        Self { target, max_level }
    }

    /// Whether this ceiling covers `target`: the same target, or a module
    /// under it.
    fn covers(&self, target: &str) -> bool {
        target
            .strip_prefix(self.target)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with("::"))
    }

    /// Whether `metadata` is within this ceiling, or outside its target.
    fn admits(&self, metadata: &Metadata<'_>) -> bool {
        !self.covers(metadata.target()) || *metadata.level() <= self.max_level
    }
}

/// A filter that drops spans and events above their target's ceiling, whatever
/// any other filter enabled.
///
/// It is a filter of its own, AND-ed with the subscriber's `EnvFilter` rather
/// than merged into its directives, so a `RUST_LOG` of `trace`, or of
/// `chromiumoxide=trace`, cannot raise a capped target: a span or event passes
/// only when the `EnvFilter` and every ceiling covering its target admit it.
/// When several ceilings cover one target (`"chromiumoxide"` and
/// `"chromiumoxide::conn"`), every one of them holds, so the strictest wins. A
/// target no ceiling covers is left to the other filters.
///
/// Added to a subscriber as a [`Layer`] it is a global filter, in front of
/// every sink of that subscriber — the fmt output, an OTLP export, an error
/// notifier:
///
/// ```rust
/// use dravr_tronc::server::log_ceiling::{LevelCeilings, TargetCeiling};
/// use tracing_subscriber::filter::LevelFilter;
/// use tracing_subscriber::layer::SubscriberExt;
/// use tracing_subscriber::{fmt, EnvFilter};
///
/// const CEILINGS: &[TargetCeiling] = &[TargetCeiling::new("chromiumoxide", LevelFilter::DEBUG)];
///
/// let subscriber = tracing_subscriber::registry()
///     .with(EnvFilter::new("trace"))
///     .with(LevelCeilings::new(CEILINGS))
///     .with(fmt::layer());
/// # drop(subscriber);
/// ```
///
/// It is also a per-layer [`Filter`], for a subscriber whose sinks filter one
/// by one: each sink then needs it.
///
/// [`init_with_ceilings`](crate::server::tracing_init::init_with_ceilings)
/// installs it in the subscriber `tracing_init` builds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LevelCeilings {
    ceilings: &'static [TargetCeiling],
}

impl LevelCeilings {
    /// A filter holding every one of `ceilings`. With none it admits
    /// everything.
    #[must_use]
    pub const fn new(ceilings: &'static [TargetCeiling]) -> Self {
        Self { ceilings }
    }

    /// The ceilings this filter holds.
    #[must_use]
    pub const fn ceilings(&self) -> &'static [TargetCeiling] {
        self.ceilings
    }

    /// Whether `metadata` is within every ceiling covering its target.
    ///
    /// Reads only the target and the level, so the answer is fixed per
    /// callsite: a capped callsite is registered as never enabled, and costs
    /// nothing afterwards.
    #[must_use]
    pub fn admits(&self, metadata: &Metadata<'_>) -> bool {
        self.ceilings.iter().all(|ceiling| ceiling.admits(metadata))
    }
}

impl<S: Subscriber> Layer<S> for LevelCeilings {
    fn enabled(&self, metadata: &Metadata<'_>, _ctx: Context<'_, S>) -> bool {
        self.admits(metadata)
    }

    /// No global bound: a target no ceiling covers may still log at TRACE, and
    /// `None` leaves the hint to the subscriber's other filters. A `Some`
    /// here would raise the hint they computed.
    fn max_level_hint(&self) -> Option<LevelFilter> {
        None
    }
}

impl<S> Filter<S> for LevelCeilings {
    fn enabled(&self, metadata: &Metadata<'_>, _ctx: &Context<'_, S>) -> bool {
        self.admits(metadata)
    }

    fn max_level_hint(&self) -> Option<LevelFilter> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_ceiling_covers_its_target_and_the_modules_under_it() {
        let ceiling = TargetCeiling::new("chromiumoxide", LevelFilter::DEBUG);

        assert!(ceiling.covers("chromiumoxide"));
        assert!(ceiling.covers("chromiumoxide::conn"));
        assert!(ceiling.covers("chromiumoxide::handler::page"));
    }

    #[test]
    fn a_ceiling_does_not_cover_a_target_sharing_its_prefix() {
        let ceiling = TargetCeiling::new("chromiumoxide", LevelFilter::DEBUG);

        assert!(!ceiling.covers("chromiumoxide_cdp"));
        assert!(!ceiling.covers("chromiumoxide_cdp::cdp"));
        assert!(!ceiling.covers("chromium"));
        assert!(!ceiling.covers("other::chromiumoxide"));
        assert!(!ceiling.covers("chromiumoxide:"));
    }
}
