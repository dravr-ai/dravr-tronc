// ABOUTME: Assertions on tool results and JSON-RPC responses, and a tools/list snapshot
// ABOUTME: Each fails the test with the response it was looking at; the snapshot pins a whole catalog
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

//! Assertions for MCP responses.
//!
//! Each one fails the calling test, at the caller's line, with the response
//! it was given in the message — so a failure reads as what the server said,
//! not as a bare `left != right`.

use std::env;
use std::fs;
use std::path::Path;

use serde_json::Value;

use crate::mcp::protocol::JsonRpcResponse;
use crate::mcp::schema::{Tool, ToolResponse};

/// Environment variable that makes [`assert_tools_snapshot`] write the
/// snapshot instead of comparing against it.
pub const UPDATE_SNAPSHOTS_ENV: &str = "TRONC_UPDATE_SNAPSHOTS";

/// The text of every text block of `response`, joined by newlines.
#[must_use]
pub fn tool_text(response: &ToolResponse) -> String {
    response
        .content
        .iter()
        .filter_map(|block| block.as_text())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Assert `response` is a success, and return its text.
///
/// # Panics
///
/// When it is a tool error — failing the test.
#[track_caller]
pub fn assert_tool_success(response: &ToolResponse) -> String {
    let text = tool_text(response);
    assert!(
        !response.is_error,
        "expected a successful tool result, got an error: {text}"
    );
    text
}

/// Assert `response` is a tool error (`isError: true`) whose text contains
/// `expected`.
///
/// # Panics
///
/// When it is a success, or its text lacks `expected` — failing the test.
#[track_caller]
pub fn assert_tool_error(response: &ToolResponse, expected: &str) {
    let text = tool_text(response);
    assert!(
        response.is_error,
        "expected a tool error containing {expected:?}, got a success: {text}"
    );
    assert!(
        text.contains(expected),
        "expected a tool error containing {expected:?}, got: {text}"
    );
}

/// Assert `response` is a success with `structuredContent` equal to
/// `expected`.
///
/// # Panics
///
/// When it is a tool error or its structured content differs — failing the
/// test.
#[track_caller]
pub fn assert_structured_content(response: &ToolResponse, expected: &Value) {
    assert_tool_success(response);
    assert_eq!(
        response.structured_content.as_ref(),
        Some(expected),
        "structuredContent differs; text was: {}",
        tool_text(response)
    );
}

/// Assert `response` is a JSON-RPC success, and return its result.
///
/// # Panics
///
/// When it is a JSON-RPC error — failing the test.
#[track_caller]
pub fn assert_rpc_success(response: &JsonRpcResponse) -> Value {
    assert!(
        response.error.is_none(),
        "expected a JSON-RPC result, got error {:?}",
        response.error
    );
    response.result.clone().unwrap_or(Value::Null)
}

/// Assert `response` is a JSON-RPC error with `code`, and return its message.
///
/// # Panics
///
/// When it is a success or another error — failing the test.
#[track_caller]
pub fn assert_rpc_error(response: &JsonRpcResponse, code: i32) -> String {
    assert_eq!(
        response.error.as_ref().map(|e| e.code),
        Some(code),
        "expected JSON-RPC error {code}, got {response:?}"
    );
    response
        .error
        .as_ref()
        .map(|e| e.message.clone())
        .unwrap_or_default()
}

/// Assert a server's `tools/list` matches the snapshot committed at `path`.
///
/// The snapshot is the tool definitions sorted by name, as pretty JSON, so a
/// diff of the file is a review of what changed in the catalog a model
/// reads — a description reworded, a field made required, a tool gone. Run
/// the test with [`UPDATE_SNAPSHOTS_ENV`] set to write it, then commit it; a
/// missing snapshot fails the test rather than passing silently.
///
/// Pass an absolute path, typically
/// `concat!(env!("CARGO_MANIFEST_DIR"), "/tests/snapshots/tools.json")`.
///
/// # Panics
///
/// When the catalog differs from the snapshot, the snapshot is missing, or
/// it cannot be written — failing the test.
#[track_caller]
pub fn assert_tools_snapshot(tools: &[Tool], path: impl AsRef<Path>) {
    let path = path.as_ref();
    let actual = render_tools(tools);

    if env::var_os(UPDATE_SNAPSHOTS_ENV).is_some() {
        let written = path
            .parent()
            .map_or(Ok(()), fs::create_dir_all)
            .and_then(|()| fs::write(path, &actual));
        assert!(
            written.is_ok(),
            "could not write the tools snapshot {}: {written:?}",
            path.display()
        );
        return;
    }

    let expected = fs::read_to_string(path);
    assert!(
        expected.is_ok(),
        "no tools snapshot at {} ({expected:?}); run with {UPDATE_SNAPSHOTS_ENV}=1 to write it, \
         then commit it",
        path.display()
    );
    assert_eq!(
        expected.unwrap_or_default(),
        actual,
        "tools/list differs from {}; if the change is intended, rerun with \
         {UPDATE_SNAPSHOTS_ENV}=1 and commit the new snapshot",
        path.display()
    );
}

/// The snapshot text of `tools`: sorted by name, pretty JSON, a final newline.
fn render_tools(tools: &[Tool]) -> String {
    let mut sorted: Vec<&Tool> = tools.iter().collect();
    sorted.sort_by(|a, b| a.name.cmp(&b.name));
    let rendered = serde_json::to_string_pretty(&sorted)
        .unwrap_or_else(|e| format!("tools/list could not be rendered: {e}"));
    format!("{rendered}\n")
}
