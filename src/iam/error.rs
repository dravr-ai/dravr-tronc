// ABOUTME: Structured errors for Google Cloud service-to-service identity-token auth
// ABOUTME: Separate from ErrorResponse so callers can branch on cause rather than parse strings
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use std::error::Error as StdError;
use std::fmt;

/// What can go wrong minting or checking an identity token.
///
/// Deliberately distinguishes "could not obtain a token" from "obtained one and
/// it was refused": the first is a caller-side misconfiguration (not running on
/// Google infrastructure, no service account attached), the second is a real
/// authorization decision. Collapsing them produces the failure mode where a
/// developer spends an afternoon on IAM bindings because their laptop has no
/// metadata server.
#[derive(Debug)]
pub enum IamError {
    /// The metadata server was unreachable or answered non-200.
    ///
    /// On Cloud Run this should not happen; off it, it always does, because
    /// `metadata.google.internal` only resolves inside Google infrastructure.
    MetadataUnavailable(String),
    /// The metadata server answered, but not with a usable token.
    MalformedToken(String),
    /// Google's signing keys could not be fetched.
    JwksUnavailable(String),
    /// The presented token failed verification.
    ///
    /// Carries why for the server's own logs. It is never returned to the
    /// caller, who gets an opaque 401 — a verifier that explains which claim
    /// failed is a claim-guessing oracle.
    Rejected(String),
}

impl fmt::Display for IamError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MetadataUnavailable(why) => {
                write!(
                    f,
                    "identity token unavailable from the metadata server: {why}"
                )
            }
            Self::MalformedToken(why) => {
                write!(f, "metadata server returned an unusable token: {why}")
            }
            Self::JwksUnavailable(why) => write!(f, "could not fetch Google signing keys: {why}"),
            Self::Rejected(why) => write!(f, "identity token rejected: {why}"),
        }
    }
}

impl StdError for IamError {}
