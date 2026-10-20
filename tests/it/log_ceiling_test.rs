// ABOUTME: Tests LevelCeilings: a capped target's TRACE spans and events are dropped whatever EnvFilter enabled
// ABOUTME: Records what reaches a capture layer, under the global-filter and the per-layer-filter wiring alike
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use std::sync::{Arc, Mutex, PoisonError};

use dravr_tronc::server::log_ceiling::{LevelCeilings, TargetCeiling};
use tracing::span::{Attributes, Id};
use tracing::subscriber::with_default;
use tracing::{debug, info, trace, trace_span, Event, Level, Subscriber};
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::layer::{Context, Layer, SubscriberExt};
use tracing_subscriber::EnvFilter;

const CEILINGS: &[TargetCeiling] = &[TargetCeiling::new("chromiumoxide", LevelFilter::DEBUG)];

/// One span or event that reached the capture layer.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Seen {
    kind: &'static str,
    target: String,
    level: Level,
}

/// A sink that records the target and level of every span and event it is
/// handed.
#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<Seen>>>);

impl Capture {
    fn record(&self, kind: &'static str, target: &str, level: Level) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(Seen {
                kind,
                target: target.to_owned(),
                level,
            });
    }

    fn seen(&self) -> Vec<Seen> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn saw(&self, kind: &str, target: &str, level: Level) -> bool {
        self.seen()
            .iter()
            .any(|seen| seen.kind == kind && seen.target == target && seen.level == level)
    }
}

impl<S: Subscriber> Layer<S> for Capture {
    fn on_new_span(&self, attrs: &Attributes<'_>, _id: &Id, _ctx: Context<'_, S>) {
        let metadata = attrs.metadata();
        self.record("span", metadata.target(), *metadata.level());
    }

    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let metadata = event.metadata();
        self.record("event", metadata.target(), *metadata.level());
    }
}

/// A span and an event at TRACE and DEBUG under chromiumoxide, a TRACE event
/// from the sibling `chromiumoxide_cdp` crate, and an INFO event elsewhere.
fn emit() {
    let span = trace_span!(target: "chromiumoxide::conn", "cdp_command");
    span.in_scope(|| {});
    trace!(target: "chromiumoxide::conn", "Sending");
    debug!(target: "chromiumoxide::conn", "connected");
    trace!(target: "chromiumoxide", "root");
    trace!(target: "chromiumoxide_cdp::cdp", "decoded");
    info!(target: "dravr_service::session", "logged in");
}

fn assert_capped(capture: &Capture, filter: &str) {
    let seen = capture.seen();
    assert!(
        !capture.saw("event", "chromiumoxide::conn", Level::TRACE),
        "{filter}: a TRACE event under a DEBUG ceiling reached the sink: {seen:?}"
    );
    assert!(
        !capture.saw("event", "chromiumoxide", Level::TRACE),
        "{filter}: the ceiling covers its own target: {seen:?}"
    );
    assert!(
        !capture.saw("span", "chromiumoxide::conn", Level::TRACE),
        "{filter}: a TRACE span under a DEBUG ceiling reached the sink: {seen:?}"
    );
    assert!(
        capture.saw("event", "chromiumoxide::conn", Level::DEBUG),
        "{filter}: DEBUG is within the ceiling: {seen:?}"
    );
    assert!(
        capture.saw("event", "chromiumoxide_cdp::cdp", Level::TRACE),
        "{filter}: chromiumoxide_cdp is not under the chromiumoxide ceiling: {seen:?}"
    );
    assert!(
        capture.saw("event", "dravr_service::session", Level::INFO),
        "{filter}: an uncapped target is left to EnvFilter: {seen:?}"
    );
}

#[test]
fn a_global_ceiling_holds_whatever_rust_log_raises() {
    for filter in ["chromiumoxide=trace,chromiumoxide_cdp=trace,info", "trace"] {
        let capture = Capture::default();
        let subscriber = tracing_subscriber::registry()
            .with(EnvFilter::new(filter))
            .with(LevelCeilings::new(CEILINGS))
            .with(capture.clone());

        with_default(subscriber, emit);

        assert_capped(&capture, filter);
    }
}

/// The global filter's position in the stack does not matter: a sink added
/// before it is behind it all the same.
#[test]
fn a_global_ceiling_also_covers_a_sink_added_before_it() {
    let capture = Capture::default();
    let subscriber = tracing_subscriber::registry()
        .with(capture.clone())
        .with(EnvFilter::new("trace"))
        .with(LevelCeilings::new(CEILINGS));

    with_default(subscriber, emit);

    assert_capped(&capture, "trace");
}

#[test]
fn a_per_layer_ceiling_holds_for_the_sink_it_filters() {
    let capture = Capture::default();
    let subscriber = tracing_subscriber::registry()
        .with(EnvFilter::new("trace"))
        .with(capture.clone().with_filter(LevelCeilings::new(CEILINGS)));

    with_default(subscriber, emit);

    assert_capped(&capture, "trace");
}

#[test]
fn the_strictest_of_several_covering_ceilings_wins() {
    const NESTED: &[TargetCeiling] = &[
        TargetCeiling::new("chromiumoxide", LevelFilter::TRACE),
        TargetCeiling::new("chromiumoxide::conn", LevelFilter::OFF),
    ];
    let capture = Capture::default();
    let subscriber = tracing_subscriber::registry()
        .with(EnvFilter::new("trace"))
        .with(LevelCeilings::new(NESTED))
        .with(capture.clone());

    with_default(subscriber, emit);

    let seen = capture.seen();
    assert!(
        !seen.iter().any(|seen| seen.target == "chromiumoxide::conn"),
        "OFF silences the module whatever its parent allows: {seen:?}"
    );
    assert!(
        capture.saw("event", "chromiumoxide", Level::TRACE),
        "the parent's own ceiling admits TRACE: {seen:?}"
    );
}

#[test]
fn no_ceilings_admit_whatever_the_env_filter_admits() {
    let capture = Capture::default();
    let subscriber = tracing_subscriber::registry()
        .with(EnvFilter::new("trace"))
        .with(LevelCeilings::new(&[]))
        .with(capture.clone());

    with_default(subscriber, emit);

    assert!(capture.saw("event", "chromiumoxide::conn", Level::TRACE));
    assert!(capture.saw("span", "chromiumoxide::conn", Level::TRACE));
    assert_eq!(capture.seen().len(), 6, "{:?}", capture.seen());
}
