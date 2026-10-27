// ABOUTME: GuardError, why a guarded fetch refused or failed — built without any part of the URL
// ABOUTME: Recovers a guard refusal raised inside reqwest (resolver, redirect policy) from its error chain
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use std::error::Error;
use std::fmt;
use std::iter;

use crate::http_client::describe_request_error;

/// Why a guarded fetch was refused or failed.
///
/// No variant carries the URL, its host or its query: a URL a user pasted can
/// carry a signed token, and this error ends up in responses and logs. A
/// caller that needs to correlate a failure logs
/// [`url_digest`](super::url_digest) beside it.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum GuardError {
    /// The text is not an absolute URL.
    InvalidUrl,
    /// The scheme is not one the policy accepts (`http` under `archive_fetch`, `ftp`, …).
    SchemeNotAllowed,
    /// The URL embeds a username or password.
    CredentialsInUrl,
    /// The URL names a port the policy does not accept.
    PortNotAllowed,
    /// The URL has no host.
    MissingHost,
    /// The host is not on the policy's allowlist.
    HostNotAllowed,
    /// The host is a local-only name (`localhost`, `*.local`, `*.internal`).
    LocalName,
    /// The host is, or resolves to, an address in a denied range.
    DeniedAddress,
    /// The host did not resolve to any address.
    Unresolvable,
    /// The redirect chain was longer than the policy allows.
    TooManyRedirects,
    /// The response body grew past the byte cap.
    BodyTooLarge {
        /// The cap, in bytes.
        limit: u64,
    },
    /// No byte arrived within the idle cap.
    IdleTimeout,
    /// The whole fetch outlived its total-time cap.
    TotalTimeout,
    /// The request failed in transport; the text is the cause, without the URL.
    Transport(String),
}

impl fmt::Display for GuardError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidUrl => f.write_str("the URL is not a valid absolute URL"),
            Self::SchemeNotAllowed => f.write_str("the URL scheme is not allowed"),
            Self::CredentialsInUrl => f.write_str("the URL must not embed credentials"),
            Self::PortNotAllowed => f.write_str("the URL port is not allowed"),
            Self::MissingHost => f.write_str("the URL has no host"),
            Self::HostNotAllowed => f.write_str("the URL host is not on the allowlist"),
            Self::LocalName => f.write_str("the URL host is a local-only name"),
            Self::DeniedAddress => {
                f.write_str("the URL host resolves to a forbidden address range")
            }
            Self::Unresolvable => f.write_str("the URL host does not resolve"),
            Self::TooManyRedirects => f.write_str("too many redirects"),
            Self::BodyTooLarge { limit } => {
                write!(f, "the response body exceeds the {limit}-byte cap")
            }
            Self::IdleTimeout => f.write_str("the response stalled past the idle cap"),
            Self::TotalTimeout => f.write_str("the fetch exceeded its total-time cap"),
            Self::Transport(cause) => write!(f, "request failed: {cause}"),
        }
    }
}

impl Error for GuardError {}

impl GuardError {
    /// The error a failed `reqwest` call becomes.
    ///
    /// A refusal the guard raised inside `reqwest` — from the resolver at
    /// connect time, or from the redirect policy on a hop — is found in the
    /// error's cause chain and returned as itself. Anything else becomes
    /// [`GuardError::Transport`], described without the URL.
    pub(crate) fn from_reqwest(error: reqwest::Error) -> Self {
        let refusal = iter::successors(error.source(), |&cause| cause.source())
            .find_map(|cause| cause.downcast_ref::<Self>().cloned());
        if let Some(refusal) = refusal {
            return refusal;
        }
        if error.is_timeout() {
            return Self::TotalTimeout;
        }
        Self::Transport(describe_request_error(error))
    }
}
