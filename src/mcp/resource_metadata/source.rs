// ABOUTME: Where the MCP router gets the protected-resource metadata it answers a request with
// ABOUTME: One fixed document, one per dialed host, or a host's own choice made per request
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use std::fmt;
use std::iter;
use std::sync::Arc;

use axum::http::HeaderMap;

use super::{
    dialed_host, ProtectedResourceMetadata, ProtectedResourceMetadataSet,
    WELL_KNOWN_PROTECTED_RESOURCE,
};

/// A request for the protected-resource metadata, as a
/// [`ResourceMetadataSource`] reads it.
#[derive(Clone, Copy)]
pub struct MetadataRequest<'a> {
    path: &'a str,
    headers: &'a HeaderMap,
}

impl<'a> MetadataRequest<'a> {
    /// A request for the document at `path`, carrying `headers`.
    #[must_use]
    pub const fn new(path: &'a str, headers: &'a HeaderMap) -> Self {
        Self { path, headers }
    }

    /// The path the document was requested at, one of the source's
    /// [`metadata_paths`](ResourceMetadataSource::metadata_paths).
    #[must_use]
    pub const fn path(&self) -> &'a str {
        self.path
    }

    /// The request's headers.
    #[must_use]
    pub const fn headers(&self) -> &'a HeaderMap {
        self.headers
    }

    /// The authority the client dialed: see [`dialed_host`].
    #[must_use]
    pub fn dialed_host(&self) -> Option<&'a str> {
        dialed_host(self.headers)
    }
}

impl fmt::Debug for MetadataRequest<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The headers stay out: they may carry a credential.
        f.debug_struct("MetadataRequest")
            .field("path", &self.path)
            .field("dialed_host", &self.dialed_host())
            .finish_non_exhaustive()
    }
}

/// Where the router gets the protected-resource metadata it answers each
/// request with.
///
/// A [`ProtectedResourceMetadata`] is one: the same document to every request.
/// A [`ProtectedResourceMetadataSet`] is another: the document of the host
/// the request dialed. A host with a rule of its own implements it, or passes
/// a closure `Fn(&MetadataRequest) -> Option<Arc<ProtectedResourceMetadata>>`.
pub trait ResourceMetadataSource: Send + Sync {
    /// The document to answer `request` with, or `None` for a 404.
    fn metadata_for(&self, request: &MetadataRequest<'_>)
        -> Option<Arc<ProtectedResourceMetadata>>;

    /// The paths the document is served at. The bare
    /// [`WELL_KNOWN_PROTECTED_RESOURCE`] unless the source says otherwise,
    /// which is where the document of a resource that has no path of its own
    /// lives, and where an MCP client falls back to.
    fn metadata_paths(&self) -> Vec<String> {
        vec![WELL_KNOWN_PROTECTED_RESOURCE.to_owned()]
    }
}

/// The paths `documents` are served at: each one's own well-known path and
/// the bare fallback, without repeats.
fn paths_of<'a>(documents: impl Iterator<Item = &'a ProtectedResourceMetadata>) -> Vec<String> {
    let mut paths = vec![WELL_KNOWN_PROTECTED_RESOURCE.to_owned()];
    for document in documents {
        let path = document.metadata_path();
        if !paths.contains(&path) {
            paths.push(path);
        }
    }
    paths
}

impl ResourceMetadataSource for Arc<ProtectedResourceMetadata> {
    fn metadata_for(&self, _: &MetadataRequest<'_>) -> Option<Arc<ProtectedResourceMetadata>> {
        Some(Self::clone(self))
    }

    fn metadata_paths(&self) -> Vec<String> {
        paths_of(iter::once(self.as_ref()))
    }
}

impl ResourceMetadataSource for ProtectedResourceMetadataSet {
    fn metadata_for(
        &self,
        request: &MetadataRequest<'_>,
    ) -> Option<Arc<ProtectedResourceMetadata>> {
        Some(Arc::clone(self.for_host(request.dialed_host())))
    }

    fn metadata_paths(&self) -> Vec<String> {
        paths_of(self.documents().map(AsRef::as_ref))
    }
}

impl<F> ResourceMetadataSource for F
where
    F: Fn(&MetadataRequest<'_>) -> Option<Arc<ProtectedResourceMetadata>> + Send + Sync,
{
    fn metadata_for(
        &self,
        request: &MetadataRequest<'_>,
    ) -> Option<Arc<ProtectedResourceMetadata>> {
        self(request)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metadata(resource: &str) -> ProtectedResourceMetadata {
        ProtectedResourceMetadata::new(resource, vec!["https://auth.example.com".to_owned()])
            .expect("a valid resource") // Safe: test fixture
    }

    fn headers(host: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert("host", host.parse().expect("header")); // Safe: test fixture
        headers
    }

    #[test]
    fn a_fixed_document_answers_every_request_at_its_paths() {
        let source = Arc::new(metadata("https://api.example.com/mcp"));
        let any = headers("whatever.example");
        let answer = source
            .metadata_for(&MetadataRequest::new(WELL_KNOWN_PROTECTED_RESOURCE, &any))
            .expect("a document"); // Safe: test assertion
        assert_eq!(answer.resource, "https://api.example.com/mcp");
        assert_eq!(
            source.metadata_paths(),
            [
                "/.well-known/oauth-protected-resource",
                "/.well-known/oauth-protected-resource/mcp"
            ]
        );
        assert_eq!(
            Arc::new(metadata("https://api.example.com")).metadata_paths(),
            ["/.well-known/oauth-protected-resource"]
        );
    }

    #[test]
    fn a_set_answers_by_the_dialed_host() {
        let set = ProtectedResourceMetadataSet::new(metadata("https://mcp.example.com"))
            .with_alias(metadata("https://app.example.com"));
        let dialed = headers("app.example.com");
        let request = MetadataRequest::new(WELL_KNOWN_PROTECTED_RESOURCE, &dialed);
        assert_eq!(request.dialed_host(), Some("app.example.com"));
        assert_eq!(
            set.metadata_for(&request).map(|doc| doc.resource.clone()),
            Some("https://app.example.com".to_owned())
        );
        assert_eq!(
            set.metadata_paths(),
            ["/.well-known/oauth-protected-resource"]
        );
    }

    #[test]
    fn a_closure_is_a_source() {
        let known = Arc::new(metadata("https://mcp.example.com"));
        let source = move |request: &MetadataRequest<'_>| {
            (request.dialed_host() == Some("mcp.example.com")).then(|| Arc::clone(&known))
        };
        let dialed = headers("mcp.example.com");
        let other = headers("other.example.com");
        assert!(source
            .metadata_for(&MetadataRequest::new(
                WELL_KNOWN_PROTECTED_RESOURCE,
                &dialed
            ))
            .is_some());
        assert!(source
            .metadata_for(&MetadataRequest::new(WELL_KNOWN_PROTECTED_RESOURCE, &other))
            .is_none());
        assert_eq!(source.metadata_paths(), [WELL_KNOWN_PROTECTED_RESOURCE]);
        let debug = format!("{:?}", MetadataRequest::new("/p", &dialed));
        assert!(debug.contains("mcp.example.com"), "{debug}");
    }
}
