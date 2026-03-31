# dravr-tronc — Agent Instructions

## Mandatory Session Setup (ALL AI Agents)

**Run these commands at the START OF EVERY SESSION:**

```bash
# 1. Initialize shared build config (required for validation)
git submodule update --init --recursive

# 2. Set git hooks
git config core.hooksPath .build/hooks
```

## Mandatory Pre-Push Validation

**Before EVERY push, run:**

```bash
# 1. Format
cargo fmt --all

# 2. Clippy with warnings as errors
cargo clippy --workspace --all-targets -- -D warnings

# 3. Architectural validation (MUST exit 0)
.build/validation/validate.sh
```

**DO NOT push if `.build/validation/validate.sh` fails.** Fix all reported issues first.

The validation checks: placeholder code, forbidden anyhow usage, problematic unwraps/expects/panics,
underscore-prefixed names, unauthorized clippy allows, dead code annotations, test integrity, and more.

## About

dravr-tronc is the error notification layer for dravr services. It provides Slack and email alerts on ERROR events.
