// ABOUTME: The tracing target that marks an event as a notification, usable without the sender
// ABOUTME: Ungated so any dravr crate can emit notify events without the notifications feature

// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

//! The one string the notification path hinges on.
//!
//! Two roles share it and they live in different crates. An **emitter** writes
//! `info!(target: NOTIFY_TARGET, event = "…", …)`; the **consumer** is
//! [`NotifyLayer`], which keeps events whose `metadata().target()` equals this
//! and drops the rest. If the two ever name different strings the emitter's
//! events stop being notifications, and nothing fails — they are simply
//! filtered out as ordinary logs.
//!
//! It lives here, outside the `notifications` feature, because emitting is not
//! sending. A crate that only reports business events — dravr-stripe emits
//! four and consumes nothing else — needed `features = ["notifications"]` to
//! reach this constant, which pulled reqwest, ring and hex for a `&str`. That
//! weight is what pushed it to declare its own copy instead (dravr-stripe
//! v0.1.15), and a second declaration that merely agrees is exactly the drift
//! this constant exists to prevent. Ungating it removes the reason to copy.
//!
//! [`NotifyLayer`]: crate::notify::NotifyLayer

/// Tracing target marking an event as a notification rather than a log.
///
/// Events whose target differs are ignored by the notify layer — they are
/// regular application logs, not notify-channel pings.
pub const NOTIFY_TARGET: &str = "notify";
