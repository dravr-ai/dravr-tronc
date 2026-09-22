// ABOUTME: Pins NOTIFY_TARGET's value and its reachability from the crate root
// ABOUTME: The value is a wire contract between every emitter and the notify layer

// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

//! `NOTIFY_TARGET` is shared between crates that never compile together: an
//! emitter writes `info!(target: NOTIFY_TARGET, …)`, and `NotifyLayer` keeps
//! only events whose target equals it. A change to the string silently
//! reclassifies every emitter's events as ordinary logs — nothing fails, the
//! notifications simply stop — so the value is pinned here rather than left to
//! whoever edits the layer next.
//!
//! Reachability from the crate root is the other half. The constant sat behind
//! the `notifications` feature, which made a crate that only *emits* pull
//! reqwest, ring and hex for a `&str`; dravr-stripe declared its own copy
//! instead (v0.1.15), and two declarations that merely agree are the drift this
//! constant exists to prevent. `cargo check --no-default-features` in CI is
//! what actually enforces the ungating — this asserts the path a consumer uses.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![allow(missing_docs)]

#[cfg(feature = "notifications")]
use dravr_tronc::notify::NOTIFY_TARGET as LAYER_TARGET;
use dravr_tronc::NOTIFY_TARGET;

#[test]
fn notify_target_value_is_the_wire_contract() {
    assert_eq!(
        NOTIFY_TARGET, "notify",
        "every dravr emitter and NotifyLayer agree on this literal; changing it \
         silently stops routing fleet-wide"
    );
}

#[test]
fn notify_target_is_reachable_from_the_crate_root() {
    // The import above is the assertion: an emitter reaches the constant as
    // `dravr_tronc::NOTIFY_TARGET`, with no feature and no module path into the
    // gated sender machinery.
    assert!(!NOTIFY_TARGET.is_empty());
}

#[cfg(feature = "notifications")]
#[test]
fn the_layer_module_re_exports_the_same_constant() {
    assert_eq!(
        LAYER_TARGET, NOTIFY_TARGET,
        "the notify:: path must re-export the root definition, never redeclare it"
    );
}
