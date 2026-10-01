// ABOUTME: Structured errors of ServiceClient: what a call to another service failed with, and under which id
// ABOUTME: Every sent attempt carries its Exchange; no variant holds a reqwest::Error, whose Display leaks the URL
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use std::error::Error as StdError;
use std::fmt;

use reqwest::StatusCode;

use super::response::Exchange;
use super::transport::TransportFailure;
use crate::iam::IamError;
use crate::server::request_guard::REQUEST_ID_HEADER;

/// Why a request that started was not finished.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unfinished {
    /// A `504` whose `error.type` is the guard's
    /// [`REQUEST_TIMEOUT`](crate::server::request_guard::REQUEST_TIMEOUT): the
    /// service dropped the handler at its own deadline.
    ServiceDeadline,
    /// Any other `504`: a gateway in front of the service gave up first.
    GatewayDeadline,
    /// A `502`: a gateway got nothing usable from the service.
    BadGateway,
    /// A `500` whose `error.type` is the guard's
    /// [`HANDLER_PANIC`](crate::server::request_guard::HANDLER_PANIC).
    HandlerPanic,
}

impl Unfinished {
    /// What happened, as an error message says it.
    #[must_use]
    pub const fn describe(self) -> &'static str {
        match self {
            Self::ServiceDeadline => "outlived the service's request deadline",
            Self::GatewayDeadline => "outlived the gateway's deadline",
            Self::BadGateway => "got no usable answer through the gateway",
            Self::HandlerPanic => "failed in a service handler that panicked",
        }
    }
}

/// What a call through [`ServiceClient`](super::ServiceClient) failed with.
///
/// A response the service's own handler produced is never one of these,
/// whatever its status: see the [module docs](super).
///
/// The [`Exchange`] is boxed so that a `Result` carrying this error stays
/// small on the path where nothing failed.
#[derive(Debug)]
pub enum ServiceError {
    /// No identity token could be minted. Nothing was sent, so there is no
    /// request id.
    Identity {
        /// The service the token was for.
        service: String,
        /// The operation the request was for.
        operation: String,
        /// Why no token was obtained.
        source: IamError,
    },
    /// The send produced no response.
    Transport {
        /// The attempt that failed.
        exchange: Box<Exchange>,
        /// What the transport saw.
        failure: TransportFailure,
        /// The transport's own cause chain, without the request URL.
        cause: String,
    },
    /// The service, or a gateway in front of it, declined to start the request.
    Shed {
        /// The attempt that was shed.
        exchange: Box<Exchange>,
        /// The status the shed came back under.
        status: StatusCode,
        /// How long to wait before sending the request again.
        retry_after_secs: u64,
        /// Why, in the service's words, when it said.
        reason: Option<String>,
    },
    /// The request started and was not finished.
    Unfinished {
        /// The attempt that was not finished.
        exchange: Box<Exchange>,
        /// The status the answer came back under.
        status: StatusCode,
        /// Who stopped it, and how.
        kind: Unfinished,
        /// The guard's own message, when the answer was the guard's.
        detail: Option<String>,
    },
    /// Status and headers arrived; the body did not.
    Body {
        /// The attempt whose body was cut short.
        exchange: Box<Exchange>,
        /// The status that did arrive.
        status: StatusCode,
        /// The transport's own cause chain, without the request URL.
        cause: String,
    },
    /// [`ServiceResponse::json`](super::ServiceResponse::json) could not decode
    /// the body as the asked type.
    Decode {
        /// The attempt whose response was read.
        exchange: Box<Exchange>,
        /// The status of that response.
        status: StatusCode,
        /// What kind of mismatch and where in the body. Never the body's
        /// content, which can be a secret.
        detail: String,
    },
}

impl ServiceError {
    /// The attempt this error describes; `None` only for
    /// [`Identity`](Self::Identity), where nothing was sent.
    #[must_use]
    pub fn exchange(&self) -> Option<&Exchange> {
        match self {
            Self::Identity { .. } => None,
            Self::Transport { exchange, .. }
            | Self::Shed { exchange, .. }
            | Self::Unfinished { exchange, .. }
            | Self::Body { exchange, .. }
            | Self::Decode { exchange, .. } => Some(exchange),
        }
    }
}

/// `service operation` and the request id, as every message names an attempt.
struct Named<'a>(&'a Exchange, &'static str);

impl fmt::Display for Named<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self(exchange, noun) = self;
        write!(
            f,
            "{} {} {noun} ({REQUEST_ID_HEADER} {}",
            exchange.service, exchange.operation, exchange.request_id
        )
    }
}

impl fmt::Display for ServiceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Identity {
                service, source, ..
            } => write!(
                f,
                "could not mint an identity token for {service}: {source}"
            ),
            Self::Transport {
                exchange,
                failure,
                cause,
            } => write!(
                f,
                "{}) {} after {} ms: {cause}",
                Named(exchange, "request"),
                failure.describe(),
                exchange.elapsed.as_millis()
            ),
            Self::Shed {
                exchange,
                status,
                retry_after_secs,
                reason,
            } => {
                write!(
                    f,
                    "{} shed the {} request ({REQUEST_ID_HEADER} {}, {status}); retry after \
                     {retry_after_secs}s",
                    exchange.service, exchange.operation, exchange.request_id
                )?;
                reason
                    .as_ref()
                    .map_or(Ok(()), |reason| write!(f, ": {reason}"))
            }
            Self::Unfinished {
                exchange,
                status,
                kind,
                detail,
            } => write!(
                f,
                "{}) {} ({status}): {}",
                Named(exchange, "request"),
                kind.describe(),
                detail.as_deref().unwrap_or("no detail")
            ),
            Self::Body {
                exchange,
                status,
                cause,
            } => write!(
                f,
                "{}) was answered {status} but its body could not be read after {} ms: {cause}",
                Named(exchange, "request"),
                exchange.elapsed.as_millis()
            ),
            Self::Decode {
                exchange,
                status,
                detail,
            } => write!(
                f,
                "{}, {status}) is not the expected shape: {detail}",
                Named(exchange, "response")
            ),
        }
    }
}

impl StdError for ServiceError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            Self::Identity { source, .. } => Some(source),
            Self::Transport { .. }
            | Self::Shed { .. }
            | Self::Unfinished { .. }
            | Self::Body { .. }
            | Self::Decode { .. } => None,
        }
    }
}

/// Why a [`ServiceClient`](super::ServiceClient) could not be built.
#[derive(Debug)]
pub enum ConfigError {
    /// The base URL does not parse, is not `http` or `https`, names no host,
    /// carries credentials, a query or a fragment, or is `http` for a service
    /// that is not on loopback.
    InvalidBaseUrl {
        /// The service the URL was for.
        service: String,
        /// What is wrong with it. Never the URL itself, which can carry a
        /// credential.
        detail: String,
    },
    /// `ServiceClient::new`: a URL that is not on loopback, with no audience.
    AudienceRequired {
        /// The service the client was for.
        service: String,
    },
    /// `ServiceClient::from_env`: `url_var` names a service that is not on
    /// loopback and `audience_var` is unset or empty.
    AudienceNotSet {
        /// The service the client was for.
        service: String,
        /// The variable holding the base URL.
        url_var: String,
        /// The variable that should hold the audience.
        audience_var: String,
    },
    /// The HTTP client itself could not be built.
    HttpClient {
        /// The service the client was for.
        service: String,
        /// Why the build failed.
        detail: String,
    },
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidBaseUrl { service, detail } => {
                write!(f, "{service} base URL is not usable: {detail}")
            }
            Self::AudienceRequired { service } => write!(
                f,
                "{service} is not on loopback, so an ID-token audience is required"
            ),
            Self::AudienceNotSet {
                service,
                url_var,
                audience_var,
            } => write!(
                f,
                "{url_var} names a {service} service that is not on loopback, so {audience_var} \
                 must be set"
            ),
            Self::HttpClient { service, detail } => {
                write!(f, "could not build the HTTP client for {service}: {detail}")
            }
        }
    }
}

impl StdError for ConfigError {}
