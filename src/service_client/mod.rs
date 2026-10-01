// ABOUTME: The client half of server::request_guard: one dravr service calling another over HTTP
// ABOUTME: ServiceClient adds ID-token auth and a request id per attempt, and types the answers the guard owns
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

//! Service-to-service HTTP, the caller's side.
//!
//! [`server::request_guard`](crate::server::request_guard) makes a dravr
//! server answer every request it started: a request id on every response, a
//! JSON `500` for a handler that panicked, a JSON `504` for one that outlived
//! its deadline. [`ServiceClient`] is what reads those answers. It is built for
//! one service, and every request it sends:
//!
//! - carries a Google ID token addressed to the service's audience, minted by
//!   [`iam::IdTokenSource`](crate::iam::IdTokenSource) — or no token at all
//!   when the base URL is on loopback, the mirror of the server's ungated
//!   loopback bind. A service that is not on loopback must be `https`, and a
//!   request whose path would leave the service's origin is never sent: the
//!   token goes to the service it was minted for, encrypted, or nowhere;
//! - goes out under a fresh [`REQUEST_ID_HEADER`](crate::server::request_guard::REQUEST_ID_HEADER),
//!   which the guard logs and echoes, so one id finds the request in both
//!   services' logs;
//! - comes back as a [`ServiceResponse`] when the service's own handler
//!   answered, with any status, and as a [`ServiceError`] otherwise.
//!
//! ```rust,ignore
//! use std::time::Duration;
//!
//! use dravr_tronc::service_client::{ServiceClient, ServiceError};
//!
//! let client = ServiceClient::from_env(
//!     "widgets", "WIDGETS_URL", "WIDGETS_AUDIENCE", Duration::from_secs(330),
//! )?
//! .ok_or(MyError::NotConfigured)?;
//!
//! let answer = client
//!     .get("list", "/api/widgets")
//!     .query(&[("limit", "5")])
//!     .header("x-session-id", session)
//!     .send()
//!     .await?;
//! let page: WidgetPage = answer.json()?;
//! ```
//!
//! # What `Ok` means
//!
//! `send` returns `Ok` when the service's handler produced the response. That
//! includes its refusals — a `401` for a session it does not hold, a `404`, a
//! `500` it reported itself — which only the consumer can read, through
//! [`ServiceResponse::status`] and [`ServiceResponse::json_value`]. This crate
//! names only the outcomes it owns the wire shape of:
//!
//! | [`ServiceError`] | When |
//! |---|---|
//! | `Identity` | no token could be minted; nothing was sent |
//! | `Transport` | no response came back: see [`TransportFailure`] |
//! | `Shed` | a `503`, or a non-2xx naming `retry_after_secs` — see [`server::shed`](crate::server::shed) |
//! | `Unfinished` | the guard's `request_timeout` 504 or `handler_panic` 500, or a gateway's 504 / 502 |
//! | `Body` | the status arrived and the body did not, on a response its status alone does not name a shed or unfinished |
//! | `Decode` | [`ServiceResponse::json`] on a body of another shape; says where, never quotes the body |
//!
//! # One re-send
//!
//! A [`ServiceClient::get`] whose connection closed before any response is
//! sent once more, immediately, under a new request id; [`Exchange::resent`]
//! says so. A pooled connection the service closed while idle fails exactly
//! this way, and a read is safe to repeat. Nothing else is ever re-sent: not a
//! `post_json` or a `delete`, whose first copy may have run; not a timeout,
//! which would double the wait; and not a request that was answered.
//!
//! # Sizing the timeout
//!
//! Set the client's timeout above the service's
//! [`enforce_deadline`](crate::server::request_guard::enforce_deadline), so an
//! overrun arrives as the guard's `504` — which names the request id the
//! service logged it under — and not as the client's own timeout.

mod client;
mod error;
mod response;
mod transport;

pub use client::{ServiceClient, ServiceRequest, DEFAULT_SHED_RETRY_AFTER_SECS};
pub use error::{ConfigError, ServiceError, Unfinished};
pub use reqwest::header::HeaderMap;
pub use reqwest::StatusCode;
pub use response::{Exchange, ServiceResponse};
pub use transport::TransportFailure;
