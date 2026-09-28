#!/bin/bash
# ABOUTME: Pre-push gate for dravr-tronc: the shared satellite gate from dravr-build-config
# ABOUTME: Sets tronc's cargo features and execs .build/validation/satellite-pre-push-validate.sh
#
# SPDX-License-Identifier: MIT OR Apache-2.0
# Copyright (c) 2026 dravr.ai

cd "$(dirname "${BASH_SOURCE[0]}")/../.." || exit 1
GATE=.build/validation/satellite-pre-push-validate.sh
if [ ! -f "$GATE" ]; then
    echo "BLOCKED: $GATE is missing. Run: git submodule update --init --recursive .build"
    exit 1
fi
# --all-features, as CI's clippy and test jobs pass: google-iam, notifications and
# otel all sit behind features, and the default set would leave their tests unbuilt.
SATELLITE_FEATURES="--all-features" exec bash "$GATE" "$@"
