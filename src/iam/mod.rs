// ABOUTME: Google Cloud service-to-service identity-token auth, both ends of the handshake
// ABOUTME: IdTokenSource for the caller, GoogleIdTokenVerifier + middleware for the callee
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

//! # Service-to-service auth without a shared secret
//!
//! Every `dravr-*` service that calls another has needed this and none had it,
//! so each pair was protected by a bearer key someone had to mint, distribute,
//! rotate, and remember to set. The failure mode is not theoretical: the gate
//! guarding those keys passes every request through when its environment
//! variable is unset, so a config slip turns a private service into a public
//! one, quietly.
//!
//! Google already signs a statement of who a workload is. This module uses it
//! at both ends.
//!
//! - [`IdTokenSource`] — the caller asks the metadata server for a token
//!   addressed to one audience, and caches it. No key material is ever held.
//! - [`GoogleIdTokenVerifier`] and [`require_google_id_token`] — the callee
//!   checks the signature against Google's published keys, pins the audience so
//!   a token minted for another service is refused, and optionally names the
//!   service accounts it expects.
//!
//! ## What this is, relative to Cloud Run's own check
//!
//! With `allow_unauthenticated = false`, Cloud Run verifies the token and
//! enforces `run.invoker` before the request reaches the process. The verifier
//! here is therefore a second, independent check rather than the only one. It
//! is worth having because it survives the service being flipped to
//! unauthenticated, it is the entire gate anywhere that is not Cloud Run, and
//! the caller allowlist says something IAM does not: that this service expects
//! exactly these callers.
//!
//! The caller half has no such redundancy. Without it the callee cannot be
//! configured to require authentication at all, which is why it is the piece
//! that actually unblocks turning the gate on.
//!
//! ## Audience must match exactly
//!
//! The audience is baked into the token. For Cloud Run it is the service URL
//! with no trailing slash and no path. A mismatch is refused, which is the
//! intent: a token captured on one hop cannot be replayed at another.

mod error;
mod token_source;
mod verify;

pub use error::IamError;
pub use token_source::IdTokenSource;
pub use verify::{require_google_id_token, GoogleIdTokenVerifier, IdTokenClaims};
