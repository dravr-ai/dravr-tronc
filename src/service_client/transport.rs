// ABOUTME: How a request to another service failed when no HTTP response came back
// ABOUTME: Tells a timeout, an unreachable service and a connection closed before any response apart
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

//! Transport failures: a send that produced no response.
//!
//! Such a request is described by what the transport saw — the client's own
//! timeout, no connection, or a connection that closed after the request went
//! out — because that is what tells a slow service from one that dropped the
//! request. [`ServiceClient`](super::ServiceClient) re-sends a GET on
//! [`TransportFailure::ClosedBeforeResponse`] and on nothing else.

use std::error::Error as StdError;
use std::io;

/// How a request failed when no HTTP response came back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum TransportFailure {
    /// The client's own timeout elapsed first.
    TimedOut,
    /// No connection to the service could be opened.
    Unreachable,
    /// The connection closed after the request went out and before any
    /// response: the peer went away mid-request.
    ClosedBeforeResponse,
    /// The request could not be built or sent for another reason.
    Other,
}

impl TransportFailure {
    /// Classify the error a send failed with.
    ///
    /// Takes a reference because describing a `reqwest::Error` without its URL
    /// consumes it: classify first, describe second.
    ///
    /// Not public: a consumer is never handed a `reqwest::Error`, and naming
    /// that type here would make reqwest's version part of this module's API.
    pub(super) fn of(error: &reqwest::Error) -> Self {
        if error.is_timeout() {
            Self::TimedOut
        } else if error.is_connect() {
            Self::Unreachable
        } else if closed_before_response(error) {
            Self::ClosedBeforeResponse
        } else {
            Self::Other
        }
    }

    /// What happened, as an error message says it.
    #[must_use]
    pub const fn describe(self) -> &'static str {
        match self {
            Self::TimedOut => "timed out waiting for a response",
            Self::Unreachable => "could not connect to the service",
            Self::ClosedBeforeResponse => "saw the connection close before any response",
            Self::Other => "could not be sent",
        }
    }
}

/// Whether a send failed because the connection closed before any response.
///
/// hyper reports that as an incomplete message (the peer closed mid-exchange)
/// or a canceled request (the pooled connection closed before it could be
/// written); a reset or broken connection surfaces as the I/O error beneath.
fn closed_before_response(error: &reqwest::Error) -> bool {
    let mut cause: Option<&(dyn StdError + 'static)> = error.source();
    while let Some(current) = cause {
        if let Some(hyper_error) = current.downcast_ref::<hyper::Error>() {
            if hyper_error.is_incomplete_message() || hyper_error.is_canceled() {
                return true;
            }
        }
        if let Some(io_error) = current.downcast_ref::<io::Error>() {
            if matches!(
                io_error.kind(),
                io::ErrorKind::ConnectionReset
                    | io::ErrorKind::ConnectionAborted
                    | io::ErrorKind::BrokenPipe
                    | io::ErrorKind::UnexpectedEof
            ) {
                return true;
            }
        }
        cause = current.source();
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describe_pins_each_phrase() {
        assert_eq!(
            TransportFailure::TimedOut.describe(),
            "timed out waiting for a response"
        );
        assert_eq!(
            TransportFailure::Unreachable.describe(),
            "could not connect to the service"
        );
        assert_eq!(
            TransportFailure::ClosedBeforeResponse.describe(),
            "saw the connection close before any response"
        );
        assert_eq!(TransportFailure::Other.describe(), "could not be sent");
    }
}
