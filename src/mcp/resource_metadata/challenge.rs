// ABOUTME: The Bearer WWW-Authenticate challenge of a refused MCP request (RFC 9728 §5.1, RFC 6750 §3)
// ABOUTME: Points at the protected-resource metadata and may name an RFC 6750 error code and the scope a call needs
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use std::fmt;

/// An RFC 6750 §3.1 error code a `Bearer` challenge carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum BearerError {
    /// The request is malformed: a missing or repeated parameter (400).
    InvalidRequest,
    /// The token is expired, revoked, malformed or otherwise refused (401).
    /// The client may obtain a new one and retry.
    InvalidToken,
    /// The token is valid but its grant does not cover the request (403).
    /// The challenge's `scope` names the grant to request.
    InsufficientScope,
}

impl BearerError {
    /// The error code as the challenge spells it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidRequest => "invalid_request",
            Self::InvalidToken => "invalid_token",
            Self::InsufficientScope => "insufficient_scope",
        }
    }
}

impl fmt::Display for BearerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The `Bearer` challenge of a refused request.
///
/// Rendered (with [`fmt::Display`]) as the `WWW-Authenticate` value
/// `Bearer resource_metadata="<url>"`, followed by `, error="<code>"` when an
/// error is set and `, scope="<scope>"` when a scope is: the `resource_metadata`
/// parameter of RFC 9728 §5.1 tells the client which authorization server to
/// ask, the RFC 6750 `error` why it was refused, and `scope` which grant would
/// let it through. A `"` or `\` in a value is escaped, as an HTTP quoted-string
/// asks.
///
/// Built from [`ProtectedResourceMetadata::challenge`](super::ProtectedResourceMetadata::challenge),
/// or [`BearerChallenge::new`] from the metadata URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BearerChallenge {
    resource_metadata: String,
    error: Option<BearerError>,
    scope: Option<String>,
}

impl BearerChallenge {
    /// A challenge pointing at the metadata document at `resource_metadata`.
    #[must_use]
    pub fn new(resource_metadata: impl Into<String>) -> Self {
        Self {
            resource_metadata: resource_metadata.into(),
            error: None,
            scope: None,
        }
    }

    /// Name the RFC 6750 error code of the refusal.
    #[must_use]
    pub const fn with_error(mut self, error: BearerError) -> Self {
        self.error = Some(error);
        self
    }

    /// Name the scope the request needs: a space-separated list of scope
    /// tokens, as RFC 6750 §3 writes it.
    #[must_use]
    pub fn with_scope(mut self, scope: impl Into<String>) -> Self {
        self.scope = Some(scope.into());
        self
    }

    /// The `invalid_token` challenge of a refused credential.
    #[must_use]
    pub const fn invalid_token(self) -> Self {
        self.with_error(BearerError::InvalidToken)
    }

    /// The `insufficient_scope` challenge of a grant that does not cover the
    /// request, naming the scope that would.
    #[must_use]
    pub fn insufficient_scope(self, scope: impl Into<String>) -> Self {
        self.with_error(BearerError::InsufficientScope)
            .with_scope(scope)
    }

    /// The URL of the metadata document the challenge points at.
    #[must_use]
    pub fn resource_metadata(&self) -> &str {
        &self.resource_metadata
    }

    /// The error code, when one is set.
    #[must_use]
    pub const fn error(&self) -> Option<BearerError> {
        self.error
    }

    /// The scope, when one is set.
    #[must_use]
    pub fn scope(&self) -> Option<&str> {
        self.scope.as_deref()
    }
}

impl fmt::Display for BearerChallenge {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Bearer resource_metadata=")?;
        write_quoted(f, &self.resource_metadata)?;
        if let Some(error) = self.error {
            f.write_str(", error=")?;
            write_quoted(f, error.as_str())?;
        }
        if let Some(scope) = &self.scope {
            f.write_str(", scope=")?;
            write_quoted(f, scope)?;
        }
        Ok(())
    }
}

impl From<BearerChallenge> for String {
    fn from(challenge: BearerChallenge) -> Self {
        challenge.to_string()
    }
}

/// Write `value` as an HTTP quoted-string (RFC 9110 §5.6.4).
fn write_quoted(f: &mut fmt::Formatter<'_>, value: &str) -> fmt::Result {
    f.write_str("\"")?;
    for c in value.chars() {
        if matches!(c, '"' | '\\') {
            f.write_str("\\")?;
        }
        write!(f, "{c}")?;
    }
    f.write_str("\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    const METADATA: &str = "https://mcp.example.com/.well-known/oauth-protected-resource";

    #[test]
    fn a_bare_challenge_names_only_the_metadata() {
        assert_eq!(
            BearerChallenge::new(METADATA).to_string(),
            "Bearer resource_metadata=\"https://mcp.example.com/.well-known/oauth-protected-resource\""
        );
    }

    #[test]
    fn an_error_and_a_scope_follow_in_rfc_6750_order() {
        assert_eq!(
            BearerChallenge::new(METADATA).invalid_token().to_string(),
            "Bearer resource_metadata=\"https://mcp.example.com/.well-known/oauth-protected-resource\", error=\"invalid_token\""
        );
        assert_eq!(
            BearerChallenge::new(METADATA)
                .insufficient_scope("fitness:read")
                .to_string(),
            "Bearer resource_metadata=\"https://mcp.example.com/.well-known/oauth-protected-resource\", error=\"insufficient_scope\", scope=\"fitness:read\""
        );
        assert_eq!(
            BearerChallenge::new(METADATA)
                .with_scope("a b")
                .with_error(BearerError::InvalidRequest)
                .to_string(),
            "Bearer resource_metadata=\"https://mcp.example.com/.well-known/oauth-protected-resource\", error=\"invalid_request\", scope=\"a b\"",
            "the order is fixed whatever order the builder ran in"
        );
    }

    #[test]
    fn a_quote_in_a_value_cannot_end_the_parameter() {
        assert_eq!(
            BearerChallenge::new(METADATA)
                .with_scope("x\", error=\"forged\\")
                .to_string(),
            "Bearer resource_metadata=\"https://mcp.example.com/.well-known/oauth-protected-resource\", scope=\"x\\\", error=\\\"forged\\\\\""
        );
    }

    #[test]
    fn the_parts_read_back() {
        let challenge = BearerChallenge::new(METADATA).insufficient_scope("s");
        assert_eq!(challenge.resource_metadata(), METADATA);
        assert_eq!(challenge.error(), Some(BearerError::InsufficientScope));
        assert_eq!(challenge.scope(), Some("s"));
        assert_eq!(BearerError::InvalidToken.to_string(), "invalid_token");
        let header: String = challenge.into();
        assert!(header.starts_with("Bearer "));
    }
}
