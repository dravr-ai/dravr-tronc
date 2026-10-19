// ABOUTME: RFC 9728 OAuth 2.0 Protected Resource Metadata the MCP router can serve for a host
// ABOUTME: Names the resource, its authorization servers and scopes, and the challenge pointing at them
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

//! OAuth 2.0 Protected Resource Metadata (RFC 9728) for an MCP server.
//!
//! An MCP client that is refused with 401 reads the `resource_metadata` URL
//! off the `WWW-Authenticate` challenge, fetches the metadata there, and learns
//! which authorization server to obtain a token from. Every host that
//! authenticates over OAuth has to publish that document; a host that sets one
//! with [`McpServer::with_protected_resource_metadata`](crate::mcp::server::McpServer::with_protected_resource_metadata)
//! gets it served by [`mcp_router`](crate::mcp::transport::http::mcp_router)
//! at the well-known URI the resource identifier derives, and builds the
//! matching challenge with [`ProtectedResourceMetadata::www_authenticate`].

use std::error::Error;
use std::fmt;

use serde::Serialize;

use crate::server::auth::is_loopback_host;

/// The well-known path segment RFC 9728 §3 registers.
pub const WELL_KNOWN_PROTECTED_RESOURCE: &str = "/.well-known/oauth-protected-resource";

/// The resource identifier a [`ProtectedResourceMetadata`] was given is not
/// one RFC 9728 §2 allows: an `https` URL (`http` for a loopback host) with
/// no fragment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidResource {
    /// The identifier refused.
    pub resource: String,
}

impl fmt::Display for InvalidResource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "'{}' is not a protected resource identifier: an https URL (http only for loopback) with no query or fragment",
            self.resource
        )
    }
}

impl Error for InvalidResource {}

/// The RFC 9728 metadata document of an MCP server acting as an OAuth 2.0
/// protected resource.
///
/// Serialized as the document itself; empty optional fields are left out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProtectedResourceMetadata {
    /// The resource identifier: the MCP endpoint's URL, which a token's
    /// audience must name (RFC 8707).
    pub resource: String,
    /// Issuer identifiers of the authorization servers that issue tokens for
    /// this resource.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub authorization_servers: Vec<String>,
    /// Scopes a request to this resource may need.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub scopes_supported: Vec<String>,
    /// How a bearer token may be presented. The transport reads it from the
    /// `Authorization` header only, so this is `["header"]`.
    pub bearer_methods_supported: Vec<String>,
    /// A human-readable name for the resource.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource_name: Option<String>,
    /// A URL of human-readable documentation for the resource.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource_documentation: Option<String>,
}

impl ProtectedResourceMetadata {
    /// Metadata for the MCP endpoint at `resource`, whose tokens are issued by
    /// `authorization_servers`.
    ///
    /// # Errors
    ///
    /// [`InvalidResource`] unless `resource` is an `https` URL (or `http` on a
    /// loopback host, for local development) with no query or fragment.
    pub fn new(
        resource: impl Into<String>,
        authorization_servers: Vec<String>,
    ) -> Result<Self, InvalidResource> {
        let resource = resource.into();
        if split_resource(&resource).is_none() {
            return Err(InvalidResource { resource });
        }
        Ok(Self {
            resource,
            authorization_servers,
            scopes_supported: Vec::new(),
            bearer_methods_supported: vec!["header".to_owned()],
            resource_name: None,
            resource_documentation: None,
        })
    }

    /// Set the scopes a request to this resource may need.
    #[must_use]
    pub fn with_scopes(mut self, scopes: Vec<String>) -> Self {
        self.scopes_supported = scopes;
        self
    }

    /// Set the human-readable name of the resource.
    #[must_use]
    pub fn with_resource_name(mut self, name: impl Into<String>) -> Self {
        self.resource_name = Some(name.into());
        self
    }

    /// Set the URL of the resource's human-readable documentation.
    #[must_use]
    pub fn with_resource_documentation(mut self, url: impl Into<String>) -> Self {
        self.resource_documentation = Some(url.into());
        self
    }

    /// The path the document is served at: the well-known segment inserted
    /// before the resource's own path (RFC 9728 §3.1), so the metadata of
    /// `https://api.example.com/mcp` lives at
    /// `/.well-known/oauth-protected-resource/mcp`.
    #[must_use]
    pub fn metadata_path(&self) -> String {
        let path = split_resource(&self.resource).map_or("", |(_, path)| path);
        let path = path.trim_end_matches('/');
        format!("{WELL_KNOWN_PROTECTED_RESOURCE}{path}")
    }

    /// The absolute URL of the document, as a challenge names it.
    #[must_use]
    pub fn metadata_url(&self) -> String {
        let origin = split_resource(&self.resource).map_or("", |(origin, _)| origin);
        format!("{origin}{}", self.metadata_path())
    }

    /// The `WWW-Authenticate` value of a 401 that points the client at this
    /// document (RFC 9728 §5.1), for an
    /// [`AuthError::Unauthorized`](crate::mcp::auth::AuthError::Unauthorized).
    #[must_use]
    pub fn www_authenticate(&self) -> String {
        format!("Bearer resource_metadata=\"{}\"", self.metadata_url())
    }
}

/// Split an RFC 9728 resource identifier into its origin (`scheme://host`)
/// and its path, or `None` when it is no such identifier.
fn split_resource(resource: &str) -> Option<(&str, &str)> {
    if resource.contains(['?', '#']) || resource.contains(char::is_whitespace) {
        return None;
    }
    let (scheme, rest) = resource.split_once("://")?;
    let authority_end = rest.find('/').unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    if authority.is_empty() || authority.contains('@') {
        return None;
    }
    let loopback = authority
        .rsplit_once(':')
        .filter(|(_, port)| port.bytes().all(|b| b.is_ascii_digit()))
        .map_or(authority, |(host, _)| host);
    let secure = scheme.eq_ignore_ascii_case("https")
        || (scheme.eq_ignore_ascii_case("http") && is_loopback_host(loopback));
    if !secure {
        return None;
    }
    let origin_len = scheme.len() + "://".len() + authority_end;
    Some((&resource[..origin_len], &resource[origin_len..]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn metadata(resource: &str) -> ProtectedResourceMetadata {
        ProtectedResourceMetadata::new(resource, vec!["https://auth.example.com".to_owned()])
            .expect("a valid resource") // Safe: test fixture
    }

    #[test]
    fn the_well_known_segment_goes_before_the_resource_path() {
        let meta = metadata("https://api.example.com/mcp");
        assert_eq!(
            meta.metadata_path(),
            "/.well-known/oauth-protected-resource/mcp"
        );
        assert_eq!(
            meta.metadata_url(),
            "https://api.example.com/.well-known/oauth-protected-resource/mcp"
        );
        assert_eq!(
            meta.www_authenticate(),
            "Bearer resource_metadata=\"https://api.example.com/.well-known/oauth-protected-resource/mcp\""
        );

        let root = metadata("https://api.example.com");
        assert_eq!(
            root.metadata_path(),
            "/.well-known/oauth-protected-resource"
        );
        let slash = metadata("https://api.example.com:8443/");
        assert_eq!(
            slash.metadata_url(),
            "https://api.example.com:8443/.well-known/oauth-protected-resource"
        );
    }

    #[test]
    fn only_https_or_loopback_http_without_query_or_fragment_is_a_resource() {
        assert!(ProtectedResourceMetadata::new("http://localhost:3000/mcp", vec![]).is_ok());
        assert!(ProtectedResourceMetadata::new("http://127.0.0.1/mcp", vec![]).is_ok());
        for refused in [
            "http://api.example.com/mcp",
            "https://api.example.com/mcp?x=1",
            "https://api.example.com/mcp#frag",
            "ftp://api.example.com/mcp",
            "api.example.com/mcp",
            "https:///mcp",
            "https://user@api.example.com/mcp",
        ] {
            assert!(
                ProtectedResourceMetadata::new(refused, vec![]).is_err(),
                "{refused}"
            );
        }
    }

    #[test]
    fn serializes_as_the_rfc_9728_document() {
        let meta = metadata("https://api.example.com/mcp")
            .with_scopes(vec!["mcp:tools".to_owned()])
            .with_resource_name("Example MCP");
        assert_eq!(
            serde_json::to_value(&meta).expect("serialize"), // Safe: test assertion
            json!({
                "resource": "https://api.example.com/mcp",
                "authorization_servers": ["https://auth.example.com"],
                "scopes_supported": ["mcp:tools"],
                "bearer_methods_supported": ["header"],
                "resource_name": "Example MCP"
            })
        );
    }
}
