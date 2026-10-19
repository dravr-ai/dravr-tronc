// ABOUTME: RFC 9728 OAuth 2.0 Protected Resource Metadata the MCP router can serve for a host
// ABOUTME: Names the resource, its authorization servers and scopes, one document or one per dialed host
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
//! matching challenge with [`ProtectedResourceMetadata::challenge`].
//!
//! A server known by several origins publishes one document per origin, since
//! RFC 9728 §3.3 has a client refuse a document whose `resource` is not the
//! URL it dialed: a [`ProtectedResourceMetadataSet`] picks the document of the
//! host a request dialed, and any [`ResourceMetadataSource`] can choose one per
//! request.

mod challenge;
mod source;

use std::error::Error;
use std::fmt;
use std::iter;
use std::sync::Arc;

use axum::http::HeaderMap;
use serde::Serialize;

use crate::server::auth::is_loopback_host;

pub use challenge::{BearerChallenge, BearerError};
pub use source::{MetadataRequest, ResourceMetadataSource};

/// The well-known path segment RFC 9728 §3 registers.
pub const WELL_KNOWN_PROTECTED_RESOURCE: &str = "/.well-known/oauth-protected-resource";

/// The request headers that carry the authority a client dialed, most
/// specific first: the `X-Forwarded-Host` a fronting proxy writes (it rewrites
/// `Host` on the way in), then `Host`.
pub const DIALED_HOST_HEADERS: [&str; 2] = ["x-forwarded-host", "host"];

/// The dialed-host header value of an HTTP request: the first of
/// [`DIALED_HOST_HEADERS`] it carries, as sent.
///
/// A proxy may write a comma-separated `X-Forwarded-Host` list;
/// [`ProtectedResourceMetadataSet::for_host`] reads its first entry.
#[must_use]
pub fn dialed_host(headers: &HeaderMap) -> Option<&str> {
    DIALED_HOST_HEADERS
        .iter()
        .find_map(|name| headers.get(*name).and_then(|value| value.to_str().ok()))
}

/// Which schemes a resource identifier may use.
///
/// RFC 9728 §2 asks for `https`. The default also takes `http` on a loopback
/// host, for local development. A deployment that serves plain `http` beyond
/// loopback — a development stack behind a private network — opts into
/// [`SchemePolicy::AllowHttp`] explicitly, and one that must never see a
/// plain-HTTP hop, loopback included, into [`SchemePolicy::RequireHttps`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum SchemePolicy {
    /// `https`, or `http` on a loopback host.
    #[default]
    HttpsOrLoopbackHttp,
    /// `https` only, loopback included.
    RequireHttps,
    /// `https` or `http` on any host.
    AllowHttp,
}

impl SchemePolicy {
    /// Whether an identifier with `scheme` on `host` passes this policy.
    fn admits(self, scheme: Scheme, host: &str) -> bool {
        match (self, scheme) {
            (_, Scheme::Https) | (Self::AllowHttp, Scheme::Http) => true,
            (Self::HttpsOrLoopbackHttp, Scheme::Http) => is_loopback_host(host),
            (Self::RequireHttps, Scheme::Http) => false,
        }
    }
}

/// A resource identifier RFC 9728 §2 does not allow under its [`SchemePolicy`].
///
/// One is an `http`/`https` URL with no query, fragment or credentials, whose
/// scheme the policy admits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidResource {
    /// The identifier refused.
    pub resource: String,
}

impl fmt::Display for InvalidResource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "'{}' is not a protected resource identifier: an https URL (http only where the scheme policy allows it) with no query, fragment or credentials",
            self.resource
        )
    }
}

impl Error for InvalidResource {}

/// The RFC 9728 metadata document of an MCP server acting as an OAuth 2.0
/// protected resource.
///
/// Serialized as the document itself; empty optional fields are left out.
/// Built with [`ProtectedResourceMetadata::new`] and the `with_*` setters,
/// which is what keeps a new field from breaking a consumer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct ProtectedResourceMetadata {
    /// The resource identifier: the MCP endpoint's URL, which a token's
    /// audience must name (RFC 8707).
    pub resource: String,
    /// Issuer identifiers of the authorization servers that issue tokens for
    /// this resource.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub authorization_servers: Vec<String>,
    /// The URL of a JSON Web Key Set document (RFC 9728 §2 `jwks_uri`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jwks_uri: Option<String>,
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
    /// `authorization_servers`, under the default [`SchemePolicy`].
    ///
    /// # Errors
    ///
    /// [`InvalidResource`] unless `resource` is an `https` URL (or `http` on a
    /// loopback host, for local development) with no query or fragment.
    pub fn new(
        resource: impl Into<String>,
        authorization_servers: Vec<String>,
    ) -> Result<Self, InvalidResource> {
        Self::with_scheme_policy(resource, authorization_servers, SchemePolicy::default())
    }

    /// Metadata for the MCP endpoint at `resource`, its scheme checked against
    /// `policy` rather than the default.
    ///
    /// # Errors
    ///
    /// [`InvalidResource`] unless `resource` is an `http`/`https` URL with no
    /// query, fragment or credentials whose scheme `policy` admits.
    pub fn with_scheme_policy(
        resource: impl Into<String>,
        authorization_servers: Vec<String>,
        policy: SchemePolicy,
    ) -> Result<Self, InvalidResource> {
        let resource = resource.into();
        let admitted = ParsedResource::parse(&resource)
            .is_some_and(|parsed| policy.admits(parsed.scheme, parsed.host()));
        if !admitted {
            return Err(InvalidResource { resource });
        }
        Ok(Self {
            resource,
            authorization_servers,
            jwks_uri: None,
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

    /// Set the URL of the JSON Web Key Set document the metadata names.
    #[must_use]
    pub fn with_jwks_uri(mut self, url: impl Into<String>) -> Self {
        self.jwks_uri = Some(url.into());
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
        let path = ParsedResource::parse(&self.resource).map_or("", |parsed| parsed.path);
        let path = path.trim_end_matches('/');
        format!("{WELL_KNOWN_PROTECTED_RESOURCE}{path}")
    }

    /// The absolute URL of the document, as a challenge names it.
    #[must_use]
    pub fn metadata_url(&self) -> String {
        let origin = ParsedResource::parse(&self.resource).map_or("", |parsed| parsed.origin);
        format!("{origin}{}", self.metadata_path())
    }

    /// The `Bearer` challenge of a refusal that points the client at this
    /// document (RFC 9728 §5.1), to which an RFC 6750 error code and scope can
    /// be added.
    #[must_use]
    pub fn challenge(&self) -> BearerChallenge {
        BearerChallenge::new(self.metadata_url())
    }

    /// The `WWW-Authenticate` value of a 401 that points the client at this
    /// document and says nothing else, for an
    /// [`AuthError::Unauthorized`](crate::mcp::auth::AuthError::Unauthorized)
    /// answering a request that carried no credential.
    #[must_use]
    pub fn www_authenticate(&self) -> String {
        self.challenge().to_string()
    }

    /// Whether a client that dialed `host` (`host[:port]`, as a `Host` header
    /// carries it) knows the server by this resource identifier.
    ///
    /// The host compares without case, and a default port (443 for `https`,
    /// 80 for `http`) written in the identifier is not part of its authority.
    #[must_use]
    pub fn is_dialed_as(&self, host: &str) -> bool {
        ParsedResource::parse(&self.resource).is_some_and(|parsed| {
            parsed
                .authority_without_default_port()
                .eq_ignore_ascii_case(host)
        })
    }
}

/// The metadata documents of one MCP server known by several origins, the
/// first of them its canonical one.
///
/// RFC 9728 §3.3 has a client refuse a document whose `resource` differs from
/// the URL it fetched it from, so a server that moved its published host, or
/// answers to a second, publishes each origin's own document: this set serves
/// a request the document of the host it dialed, and the canonical document to
/// one that dialed none of them. A forged `Host` can at most pick another of
/// the server's own documents.
///
/// It is a [`ResourceMetadataSource`], and the host's
/// [`AuthHook`](crate::mcp::auth::AuthHook) reads [`Self::for_host`] to build
/// the challenge of a refusal from the same document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtectedResourceMetadataSet {
    canonical: Arc<ProtectedResourceMetadata>,
    aliases: Vec<Arc<ProtectedResourceMetadata>>,
}

impl ProtectedResourceMetadataSet {
    /// A set whose canonical document is `canonical`.
    #[must_use]
    pub fn new(canonical: ProtectedResourceMetadata) -> Self {
        Self {
            canonical: Arc::new(canonical),
            aliases: Vec::new(),
        }
    }

    /// Add the document of another origin the server answers to.
    #[must_use]
    pub fn with_alias(mut self, alias: ProtectedResourceMetadata) -> Self {
        self.aliases.push(Arc::new(alias));
        self
    }

    /// The canonical document.
    #[must_use]
    pub fn canonical(&self) -> &Arc<ProtectedResourceMetadata> {
        &self.canonical
    }

    /// Every document, the canonical one first.
    pub fn documents(&self) -> impl Iterator<Item = &Arc<ProtectedResourceMetadata>> {
        iter::once(&self.canonical).chain(&self.aliases)
    }

    /// The document of the origin a client that dialed `host` knows the
    /// server by, else the canonical one.
    ///
    /// `host` is a [`dialed_host`] value: a `Host` header, or an
    /// `X-Forwarded-Host` list, of which the first entry is read.
    #[must_use]
    pub fn for_host(&self, host: Option<&str>) -> &Arc<ProtectedResourceMetadata> {
        let Some(host) = host
            .and_then(|host| host.split(',').next())
            .map(str::trim)
            .filter(|host| !host.is_empty())
        else {
            return &self.canonical;
        };
        self.documents()
            .find(|document| document.is_dialed_as(host))
            .unwrap_or(&self.canonical)
    }
}

/// The scheme of a resource identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scheme {
    Http,
    Https,
}

/// A resource identifier split into its parts: shape only, whatever the
/// [`SchemePolicy`] would make of its scheme.
struct ParsedResource<'a> {
    scheme: Scheme,
    /// `host[:port]`, as written.
    authority: &'a str,
    /// `scheme://authority`.
    origin: &'a str,
    /// Everything after the origin, possibly empty.
    path: &'a str,
}

impl<'a> ParsedResource<'a> {
    /// The parts of `resource`, or `None` unless it is an `http`/`https` URL
    /// with an authority and no query, fragment, whitespace or credentials.
    fn parse(resource: &'a str) -> Option<Self> {
        if resource.contains(['?', '#']) || resource.contains(char::is_whitespace) {
            return None;
        }
        let (scheme_text, rest) = resource.split_once("://")?;
        let scheme = if scheme_text.eq_ignore_ascii_case("https") {
            Scheme::Https
        } else if scheme_text.eq_ignore_ascii_case("http") {
            Scheme::Http
        } else {
            return None;
        };
        let authority_end = rest.find('/').unwrap_or(rest.len());
        let authority = &rest[..authority_end];
        if authority.is_empty() || authority.contains('@') {
            return None;
        }
        let origin_len = scheme_text.len() + "://".len() + authority_end;
        Some(Self {
            scheme,
            authority,
            origin: &resource[..origin_len],
            path: &resource[origin_len..],
        })
    }

    /// The authority split into its host and its port, when it names one.
    fn host_and_port(&self) -> (&'a str, Option<&'a str>) {
        self.authority
            .rsplit_once(':')
            .filter(|(host, port)| {
                !port.is_empty()
                    && port.bytes().all(|b| b.is_ascii_digit())
                    && (!host.starts_with('[') || host.ends_with(']'))
            })
            .map_or((self.authority, None), |(host, port)| (host, Some(port)))
    }

    /// The host, without its port.
    fn host(&self) -> &'a str {
        self.host_and_port().0
    }

    /// The authority with a port its scheme defaults to left out.
    fn authority_without_default_port(&self) -> &'a str {
        let default_port = match self.scheme {
            Scheme::Https => "443",
            Scheme::Http => "80",
        };
        match self.host_and_port() {
            (host, Some(port)) if port == default_port => host,
            _ => self.authority,
        }
    }
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
        assert!(ProtectedResourceMetadata::new("http://[::1]:8081", vec![]).is_ok());
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
    fn the_scheme_policy_decides_plain_http() {
        let allow = |resource| {
            ProtectedResourceMetadata::with_scheme_policy(resource, vec![], SchemePolicy::AllowHttp)
        };
        let strict = |resource| {
            ProtectedResourceMetadata::with_scheme_policy(
                resource,
                vec![],
                SchemePolicy::RequireHttps,
            )
        };

        let dev = allow("http://api.dev:8081").expect("AllowHttp takes plain http"); // Safe: test assertion
        assert_eq!(
            dev.www_authenticate(),
            "Bearer resource_metadata=\"http://api.dev:8081/.well-known/oauth-protected-resource\"",
            "a plain-http resource still names its own origin"
        );
        assert!(allow("https://api.example.com").is_ok());
        assert!(allow("ftp://api.example.com").is_err());
        assert!(allow("http://api.dev:8081/mcp?x=1").is_err());

        assert!(strict("https://api.example.com").is_ok());
        assert!(strict("http://localhost:8081").is_err());
        assert!(strict("http://api.dev:8081").is_err());
    }

    /// The three challenges an origin resource's host writes — no credential,
    /// a refused one, a grant short of a scope — byte for byte.
    #[test]
    fn an_origin_resource_challenges_at_the_bare_well_known_path() {
        let meta = metadata("https://mcp.example.com");
        let base = "Bearer resource_metadata=\"https://mcp.example.com/.well-known/oauth-protected-resource\"";
        assert_eq!(meta.www_authenticate(), base);
        assert_eq!(
            meta.challenge().invalid_token().to_string(),
            format!("{base}, error=\"invalid_token\"")
        );
        assert_eq!(
            meta.challenge()
                .insufficient_scope("fitness:read")
                .to_string(),
            format!("{base}, error=\"insufficient_scope\", scope=\"fitness:read\"")
        );
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

    #[test]
    fn the_jwks_uri_is_serialized_only_when_set() {
        let meta = metadata("https://api.example.com")
            .with_jwks_uri("https://auth.example.com/.well-known/jwks.json");
        assert_eq!(
            serde_json::to_string(&meta).expect("serialize"), // Safe: test assertion
            r#"{"resource":"https://api.example.com","authorization_servers":["https://auth.example.com"],"jwks_uri":"https://auth.example.com/.well-known/jwks.json","bearer_methods_supported":["header"]}"#
        );
        let bare = serde_json::to_value(metadata("https://api.example.com")).expect("serialize"); // Safe: test assertion
        assert!(bare.get("jwks_uri").is_none(), "{bare}");
    }

    #[test]
    fn a_set_answers_each_dialed_host_its_own_document() {
        let set = ProtectedResourceMetadataSet::new(metadata("https://mcp.example.com"))
            .with_alias(metadata("https://api.example.com:8443"))
            .with_alias(metadata("https://www.example.com:443"));
        let resource = |host| set.for_host(host).resource.clone();

        assert_eq!(resource(None), "https://mcp.example.com");
        assert_eq!(resource(Some("")), "https://mcp.example.com");
        assert_eq!(resource(Some("MCP.example.com")), "https://mcp.example.com");
        assert_eq!(
            resource(Some("api.example.com:8443")),
            "https://api.example.com:8443"
        );
        assert_eq!(
            resource(Some("api.example.com")),
            "https://mcp.example.com",
            "another port is another origin"
        );
        assert_eq!(
            resource(Some(" api.example.com:8443 , proxy.internal")),
            "https://api.example.com:8443",
            "the first X-Forwarded-Host entry is the dialed one"
        );
        assert_eq!(
            resource(Some("www.example.com")),
            "https://www.example.com:443",
            "a default port is no part of the authority"
        );
        assert_eq!(
            resource(Some("evil.example")),
            "https://mcp.example.com",
            "a forged host only ever picks the canonical document"
        );
        assert_eq!(set.documents().count(), 3);
    }

    #[test]
    fn the_dialed_host_is_the_forwarded_one_first() {
        let mut headers = HeaderMap::new();
        assert_eq!(dialed_host(&headers), None);
        headers.insert("host", "backend.internal".parse().expect("header")); // Safe: test fixture
        assert_eq!(dialed_host(&headers), Some("backend.internal"));
        headers.insert(
            "x-forwarded-host",
            "mcp.example.com".parse().expect("header"), // Safe: test fixture
        );
        assert_eq!(dialed_host(&headers), Some("mcp.example.com"));
    }
}
