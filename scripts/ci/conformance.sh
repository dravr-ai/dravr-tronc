#!/bin/bash
# ABOUTME: Runs the official MCP conformance suite against examples/conformance_server.rs
# ABOUTME: Both eras — suite 0.1.16 vs 2025-11-25, 0.2.0-alpha.11 vs 2026-07-28 — each against its baseline
#
# SPDX-License-Identifier: MIT OR Apache-2.0
# Copyright (c) 2026 dravr.ai
#
# Each run passes its committed expected-failure baseline, conformance/baseline-<spec>.yml.
# The suite exits non-zero on a failure the baseline does not list (a regression) and on a
# listed scenario that now passes (a stale entry), so a fix lands together with the line it
# removes from the baseline. Both suites run even when the first fails, so one CI run shows
# both. Results are written under target/conformance/<spec>/.
#
# Requires cargo and npx. CONFORMANCE_PORT overrides the port (default 3001).

set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
PORT="${CONFORMANCE_PORT:-3001}"
URL="http://127.0.0.1:${PORT}/mcp"
OUT="${ROOT}/target/conformance"
PACKAGE="@modelcontextprotocol/conformance"

cd "$ROOT" || exit 1
cargo build --example conformance_server --quiet || exit 1
SERVER="${CARGO_TARGET_DIR:-${ROOT}/target}/debug/examples/conformance_server"

"$SERVER" --port "$PORT" &
SERVER_PID=$!
trap 'kill "$SERVER_PID" 2>/dev/null' EXIT

ready=0
for _ in $(seq 1 50); do
    if curl -sf -X POST "$URL" -H 'content-type: application/json' \
        -d '{"jsonrpc":"2.0","id":0,"method":"ping"}' >/dev/null; then
        ready=1
        break
    fi
    sleep 0.2
done
if [ "$ready" -ne 1 ]; then
    echo "conformance_server did not answer on ${URL}"
    exit 1
fi

# run <suite version> <spec version>
run() {
    echo "=== ${PACKAGE}@$1 against spec $2"
    npx --yes "${PACKAGE}@$1" server \
        --url "$URL" \
        --spec-version "$2" \
        --expected-failures "${ROOT}/conformance/baseline-$2.yml" \
        --output-dir "${OUT}/$2"
}

status=0
run 0.1.16 2025-11-25 || status=1
run 0.2.0-alpha.11 2026-07-28 || status=1
exit "$status"
