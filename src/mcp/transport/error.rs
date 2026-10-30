// ABOUTME: TransportError — why serve, serve_with_shutdown or the stdio run loop stopped
// ABOUTME: A refused bind, a resolution, bind or serve failure, or a stdio read/write failure, typed
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use std::error::Error;
use std::fmt;
use std::io;

use crate::server::auth::InsecureBindError;

/// Why a transport entry point returned an error.
///
/// The entry points are [`http::serve`](crate::mcp::transport::http::serve),
/// [`http::serve_with_shutdown`](crate::mcp::transport::http::serve_with_shutdown)
/// and [`stdio::run`](crate::mcp::transport::stdio::run).
///
/// Each variant keeps the underlying error, so a binary can act on the cause:
/// retry another port on a [`Self::Bind`] whose source is
/// [`io::ErrorKind::AddrInUse`], or tell an operator which variable arms the
/// gate a [`Self::InsecureBind`] refused. A `main` that only reports it still
/// `?`s it into `Box<dyn Error>`.
#[derive(Debug)]
#[non_exhaustive]
pub enum TransportError {
    /// The bind was refused before any socket opened: nothing authenticates
    /// requests and the address is reachable from another host.
    InsecureBind(InsecureBindError),
    /// `addr` did not resolve to any socket address.
    Resolve {
        /// The `host:port` that was looked up.
        addr: String,
        /// The resolver's error.
        source: io::Error,
    },
    /// The listener could not bind `addr`; [`io::ErrorKind::AddrInUse`] when
    /// another process holds the port.
    Bind {
        /// The `host:port` that was bound.
        addr: String,
        /// The bind error.
        source: io::Error,
    },
    /// The HTTP server stopped on an I/O error while serving.
    Serve(io::Error),
    /// Reading stdin or writing stdout failed, or the stdout writer stopped
    /// without finishing.
    Stdio(io::Error),
    /// A JSON-RPC message could not be serialized for stdout.
    Encode(serde_json::Error),
}

impl fmt::Display for TransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InsecureBind(refused) => refused.fmt(f),
            Self::Resolve { addr, source } => write!(f, "failed to resolve {addr}: {source}"),
            Self::Bind { addr, source } => write!(f, "failed to bind {addr}: {source}"),
            Self::Serve(source) => write!(f, "HTTP server error: {source}"),
            Self::Stdio(source) => write!(f, "stdio transport I/O failed: {source}"),
            Self::Encode(source) => write!(f, "JSON-RPC message serialization failed: {source}"),
        }
    }
}

impl Error for TransportError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InsecureBind(refused) => Some(refused),
            Self::Resolve { source, .. }
            | Self::Bind { source, .. }
            | Self::Serve(source)
            | Self::Stdio(source) => Some(source),
            Self::Encode(source) => Some(source),
        }
    }
}

impl From<InsecureBindError> for TransportError {
    fn from(refused: InsecureBindError) -> Self {
        Self::InsecureBind(refused)
    }
}
