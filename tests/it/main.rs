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
mod ssrf_guard_test;
mod task_signal_test;
mod tasks_extension_test;
mod testkit_test;
// The date-type suites share helpers that unwrap outside a #[test] function
// (building a value from a literal). cfg(test) is what tells clippy those
// helpers are test code; it is always true in this binary.
#[cfg(test)]
#[cfg(feature = "zones")]
mod civil_clock_clock_date_test;
#[cfg(test)]
#[cfg(feature = "zones")]
mod civil_clock_relative_day_test;
#[cfg(test)]
mod dates_arith_test;
#[cfg(test)]
mod dates_format_test;
#[cfg(test)]
mod dates_offset_test;
#[cfg(test)]
mod dates_parse_test;
#[cfg(test)]
mod dates_pattern_test;
#[cfg(test)]
#[cfg(feature = "schemars1")]
mod dates_schema_test;
#[cfg(test)]
#[cfg(feature = "sqlx08-postgres")]
mod dates_sqlx_postgres_test;
#[cfg(test)]
#[cfg(feature = "sqlx08-sqlite")]
mod dates_sqlx_sqlite_test;
#[cfg(test)]
mod dates_support;
#[cfg(test)]
#[cfg(feature = "zones")]
mod dates_zone_test;
mod tool_response_test;
#[cfg(feature = "otel")]
mod trace_context_test;
#[cfg(feature = "otel-http-client")]
mod trace_propagation_middleware_test;
