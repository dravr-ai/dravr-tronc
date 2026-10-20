// ABOUTME: The crate's one integration-test binary: each former tests/*.rs file is a module here
// ABOUTME: Files that write the process environment stay separate binaries beside this one
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

//! Integration tests for dravr-tronc, linked as one test binary.
//!
//! Run one module with `cargo test --test it -- <module>::`.

mod client_channel_test;
#[cfg(feature = "computation")]
mod computation_test;
#[cfg(feature = "google-iam")]
mod google_key_set_test;
mod graceful_shutdown_test;
mod input_required_test;
mod integration_test;
mod log_ceiling_test;
mod notify_target_test;
mod observe_test;
#[cfg(feature = "otel")]
mod otel_observer_test;
mod request_cancellation_test;
mod request_guard_test;
mod request_meta_test;
mod schema_2020_12_test;
#[cfg(feature = "schema-validation")]
mod schema_validation_test;
mod shed_test;
mod task_signal_test;
mod tasks_extension_test;
mod testkit_test;
mod tool_response_test;
#[cfg(feature = "otel")]
mod trace_context_test;
#[cfg(feature = "otel-http-client")]
mod trace_propagation_middleware_test;
