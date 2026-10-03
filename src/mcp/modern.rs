// ABOUTME: Modern (2026-07-28) MCP protocol per-request metadata + era detection.
// ABOUTME: The stateless `_meta` model that coexists with legacy 2025-11-25 (initialize/sessions).
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

//! Modern MCP protocol revision (`2026-07-28`) support.
//!
//! Unlike the legacy `initialize`-handshake model, the modern revision is
//! stateless: every request carries its protocol version, client identity, and
//! client capabilities in `params._meta`. This module extracts and validates
//! that metadata and distinguishes a modern request from a legacy one (era
//! detection), so a single `/mcp` endpoint can serve both eras concurrently.

use serde::{Deserialize, Serialize};
use serde_json::{from_value, Map, Value};

use crate::mcp::schema::{ServerCapabilities, ServerInfo};

/// The modern (stateless, per-request-metadata) MCP protocol revision string.
pub const PROTOCOL_VERSION_2026_07_28: &str = "2026-07-28";

/// The `MCP-Protocol-Version` HTTP header, lower-cased as HTTP/2 carries it.
///
/// The Streamable HTTP transport stores its value under this key in
/// [`JsonRpcRequest::metadata`](crate::mcp::protocol::JsonRpcRequest::metadata),
/// where era detection reads it back.
pub const PROTOCOL_VERSION_HEADER: &str = "mcp-protocol-version";

/// Methods revision 2026-07-28 removed with the `initialize` handshake.
///
/// A modern request naming one is answered method-not-found by the
/// engine itself, never offered to a host handler that may still serve it
/// for the legacy era.
pub const REMOVED_METHODS: [&str; 5] = [
    "initialize",
    "ping",
    "logging/setLevel",
    "resources/subscribe",
    "resources/unsubscribe",
];

/// Whether `version` names a per-request-metadata revision (2026-07-28 or
/// later) rather than an `initialize`-era one.
///
/// Revisions are `YYYY-MM-DD` dates, so they order as text; anything not
/// shaped like one is no modern revision.
#[must_use]
pub fn is_modern_revision(version: &str) -> bool {
    let shaped = version.len() == PROTOCOL_VERSION_2026_07_28.len()
        && version.bytes().enumerate().all(|(i, b)| {
            if i == 4 || i == 7 {
                b == b'-'
            } else {
                b.is_ascii_digit()
            }
        });
    shaped && version >= PROTOCOL_VERSION_2026_07_28
}

/// Reserved `_meta` keys carrying per-request protocol metadata (revision 2026-07-28).
///
/// All keys use the reserved `io.modelcontextprotocol/` prefix.
pub mod meta_keys {
    /// Protocol version for this request, e.g. `"2026-07-28"`. Required.
    pub const PROTOCOL_VERSION: &str = "io.modelcontextprotocol/protocolVersion";
    /// Client name and version (`Implementation`). Optional.
    pub const CLIENT_INFO: &str = "io.modelcontextprotocol/clientInfo";
    /// Client capabilities relevant to this request. Required.
    pub const CLIENT_CAPABILITIES: &str = "io.modelcontextprotocol/clientCapabilities";
    /// Minimum log level the server should emit for this request. Optional.
    pub const LOG_LEVEL: &str = "io.modelcontextprotocol/logLevel";
}

/// Methods whose result extends the revision's `CacheableResult`, on which
/// `ttlMs` and `cacheScope` are REQUIRED, not optional.
///
/// `server/discover` is cacheable too but builds its own values in
/// [`DiscoverResult::new`], so it is not listed here.
///
/// A conforming client validates the result against the schema and discards one
/// that fails. Claude Code 2.1.278 does exactly that: it retried a `tools/list`
/// answer carrying `resultType` alone four times, then reported the server
/// `connected` with zero tools — so the model had nothing to call and said so
/// to the athlete. Nothing errored on either side.
pub(crate) const CACHEABLE_RESULT_METHODS: [&str; 5] = [
    "tools/list",
    "resources/list",
    "resources/templates/list",
    "prompts/list",
    "resources/read",
];

/// `ttlMs` a cacheable result carries when its producer states none: the
/// response is immediately stale and the client re-fetches when it needs it.
pub(crate) const UNCACHED_TTL_MS: u64 = 0;

/// `cacheScope` a cacheable result carries when its producer states none.
///
/// What a caller may list depends on who is asking — the registry withholds
/// `ADMIN_ONLY` tools from a non-admin, and a host dispatcher scopes by tenant
/// — so a shared intermediary must never serve one caller's answer to another.
/// `"private"` is the only value that is safe without knowing the producer.
pub(crate) const UNSHARED_CACHE_SCOPE: &str = "private";

/// Give a cacheable method's result the cache fields the revision requires.
///
/// Values the producer already set are kept, so a host handler serving a
/// genuinely static `resources/list` can advertise a real TTL and `"public"`.
/// The defaults promise nothing: immediately stale, never shared.
pub(crate) fn frame_cacheable_result(method: &str, result: &mut Map<String, Value>) {
    if !CACHEABLE_RESULT_METHODS.contains(&method) {
        return;
    }
    result
        .entry("ttlMs")
        .or_insert_with(|| Value::from(UNCACHED_TTL_MS));
    result
        .entry("cacheScope")
        .or_insert_with(|| Value::String(UNSHARED_CACHE_SCOPE.to_owned()));
}

/// Client identity (`Implementation`) carried in a modern request's `_meta`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModernClientInfo {
    /// Machine-readable client name.
    pub name: String,
    /// Client version string.
    pub version: String,
}

/// Per-request protocol metadata for the modern (`2026-07-28`) stateless model,
/// extracted from `params._meta`. Every modern request MUST carry these fields.
#[derive(Debug, Clone)]
pub struct ModernRequestMeta {
    /// Declared protocol version for this request.
    pub protocol_version: String,
    /// Client identity, when the client chose to send it. The specification
    /// types this field optional, so a request omitting it is well-formed.
    pub client_info: Option<ModernClientInfo>,
    /// Declared client capabilities (kept as raw JSON so capability checks can
    /// look up arbitrary keys and report `MissingRequiredClientCapabilityError`).
    pub client_capabilities: Value,
    /// Optional minimum log level the server should emit for this request.
    pub log_level: Option<String>,
}

/// Outcome of reading modern `_meta` from a request's `params`.
///
/// Era detection keys off the presence of [`meta_keys::PROTOCOL_VERSION`]: a
/// request without it is legacy (`initialize`-based); a request with it is
/// modern and MUST also carry the other required fields.
#[non_exhaustive]
pub enum ModernMeta {
    /// No modern protocol version in `_meta` — handle as a legacy request.
    Legacy,
    /// A well-formed modern request.
    Modern(Box<ModernRequestMeta>),
    /// A modern request (protocol version present) missing a required field,
    /// or a request whose transport header names a modern revision while its
    /// body carries no `_meta` protocol version. The caller maps this to
    /// JSON-RPC `-32602` (Invalid params).
    Malformed(String),
    /// The transport's `MCP-Protocol-Version` header names another revision
    /// than the body's `_meta`. The caller maps this to `HeaderMismatchError`
    /// (`-32020`), which Streamable HTTP pairs with HTTP 400.
    HeaderMismatch(String),
}

impl ModernRequestMeta {
    /// Detect and extract modern protocol metadata from a request's `params`.
    ///
    /// Returns [`ModernMeta::Legacy`] when the request carries no modern
    /// protocol version (so the caller routes it through the legacy
    /// `initialize`/session path), [`ModernMeta::Modern`] when all required
    /// fields are present, or [`ModernMeta::Malformed`] when the protocol
    /// version is present but a required field is missing or invalid.
    #[must_use]
    pub fn from_params(params: Option<&Value>) -> ModernMeta {
        let Some(meta) = params.and_then(|p| p.get("_meta")) else {
            return ModernMeta::Legacy;
        };

        // Era detection: no protocolVersion key => legacy request.
        let Some(protocol_version) = meta
            .get(meta_keys::PROTOCOL_VERSION)
            .and_then(Value::as_str)
        else {
            return ModernMeta::Legacy;
        };

        let client_info = meta
            .get(meta_keys::CLIENT_INFO)
            .and_then(|v| from_value::<ModernClientInfo>(v.clone()).ok());

        let Some(client_capabilities) = meta.get(meta_keys::CLIENT_CAPABILITIES).cloned() else {
            return ModernMeta::Malformed(format!(
                "missing required _meta field '{}'",
                meta_keys::CLIENT_CAPABILITIES
            ));
        };

        let log_level = meta
            .get(meta_keys::LOG_LEVEL)
            .and_then(Value::as_str)
            .map(str::to_owned);

        ModernMeta::Modern(Box::new(Self {
            protocol_version: protocol_version.to_owned(),
            client_info,
            client_capabilities,
            log_level,
        }))
    }

    /// Detect the era of a request whose transport may carry the protocol
    /// version beside the body: `header` is the `MCP-Protocol-Version` value,
    /// or `None` when the transport carries no such header (stdio, an
    /// in-process call).
    ///
    /// The body stays the source of truth, but the two must agree: a modern
    /// header over a body with no `_meta` protocol version is
    /// [`ModernMeta::Malformed`] — it used to be served as legacy, so a
    /// modern client that left `_meta` out reached the `initialize`-era
    /// dispatch — and a header naming another revision than the body is
    /// [`ModernMeta::HeaderMismatch`]. A legacy header over a legacy body is
    /// legacy.
    #[must_use]
    pub fn detect(header: Option<&str>, params: Option<&Value>) -> ModernMeta {
        let body = Self::from_params(params);
        let Some(header) = header.map(str::trim) else {
            return body;
        };
        match body {
            ModernMeta::Modern(meta) if meta.protocol_version != header => {
                ModernMeta::HeaderMismatch(format!(
                    "Header mismatch: MCP-Protocol-Version header value '{header}' does not \
                     match body _meta '{}' value '{}'",
                    meta_keys::PROTOCOL_VERSION,
                    meta.protocol_version
                ))
            }
            ModernMeta::Legacy if is_modern_revision(header) => ModernMeta::Malformed(format!(
                "missing required _meta field '{}'",
                meta_keys::PROTOCOL_VERSION
            )),
            other => other,
        }
    }
}

/// Result of the modern `server/discover` RPC: the server's supported protocol
/// versions, capabilities, and identity.
///
/// Lets a client learn what the server speaks before sending any other request.
/// Reuses the schema's [`ServerCapabilities`]/[`ServerInfo`] so discovery and
/// the legacy `initialize` response stay in lock-step.
#[derive(Debug, Clone, Serialize)]
pub struct DiscoverResult {
    /// Polymorphic result discriminator; always `"complete"` for discovery.
    #[serde(rename = "resultType")]
    pub result_type: String,
    /// Protocol versions the server supports, in preference order.
    #[serde(rename = "supportedVersions")]
    pub supported_versions: Vec<String>,
    /// Server capabilities (tools, resources, prompts, auth, ...).
    pub capabilities: ServerCapabilities,
    /// Name and version of the server software.
    #[serde(rename = "serverInfo")]
    pub server_info: ServerInfo,
    /// Optional natural-language guidance for LLMs on using this server.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    /// Freshness hint (ms) for caching the discovery payload.
    #[serde(rename = "ttlMs", skip_serializing_if = "Option::is_none")]
    pub ttl_ms: Option<u64>,
    /// Whether shared intermediaries may cache the response (`public`/`private`).
    #[serde(rename = "cacheScope", skip_serializing_if = "Option::is_none")]
    pub cache_scope: Option<String>,
}

impl DiscoverResult {
    /// Build a discovery result, marking it `complete` and advertising a 1-hour
    /// public cache window (the discovery payload is effectively static).
    #[must_use]
    pub fn new(
        supported_versions: Vec<String>,
        capabilities: ServerCapabilities,
        server_info: ServerInfo,
        instructions: Option<String>,
    ) -> Self {
        Self {
            result_type: "complete".to_owned(),
            supported_versions,
            capabilities,
            server_info,
            instructions,
            ttl_ms: Some(3_600_000),
            cache_scope: Some("public".to_owned()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::protocol::PROTOCOL_VERSION;
    use serde_json::json;

    #[test]
    fn no_meta_is_legacy() {
        let params = json!({ "name": "get_activities" });
        assert!(matches!(
            ModernRequestMeta::from_params(Some(&params)),
            ModernMeta::Legacy
        ));
        assert!(matches!(
            ModernRequestMeta::from_params(None),
            ModernMeta::Legacy
        ));
    }

    #[test]
    fn meta_without_protocol_version_is_legacy() {
        let params = json!({ "_meta": { "traceparent": "00-abc-def-01" } });
        assert!(matches!(
            ModernRequestMeta::from_params(Some(&params)),
            ModernMeta::Legacy
        ));
    }

    #[test]
    fn well_formed_modern_request_extracts_meta() {
        let params = json!({
            "_meta": {
                "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                "io.modelcontextprotocol/clientInfo": { "name": "ExampleClient", "version": "1.0.0" },
                "io.modelcontextprotocol/clientCapabilities": { "tools": {} },
                "io.modelcontextprotocol/logLevel": "info"
            }
        });

        let outcome = ModernRequestMeta::from_params(Some(&params));
        assert!(matches!(outcome, ModernMeta::Modern(_)));
        if let ModernMeta::Modern(meta) = outcome {
            assert_eq!(meta.protocol_version, "2026-07-28");
            let client_info = meta.client_info.expect("clientInfo was supplied"); // Safe: test assertion
            assert_eq!(client_info.name, "ExampleClient");
            assert_eq!(client_info.version, "1.0.0");
            assert_eq!(meta.log_level.as_deref(), Some("info"));
            assert!(meta.client_capabilities.get("tools").is_some());
        }
    }

    #[test]
    fn modern_request_without_client_info_is_well_formed() {
        // The specification types `clientInfo` optional. Rejecting a request
        // that omits it made every conformant client that leaves it out
        // unreachable, extensions included.
        let params = json!({
            "_meta": {
                "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                "io.modelcontextprotocol/clientCapabilities": {}
            }
        });
        let outcome = ModernRequestMeta::from_params(Some(&params));
        assert!(matches!(outcome, ModernMeta::Modern(_)));
        if let ModernMeta::Modern(meta) = outcome {
            assert!(meta.client_info.is_none());
        }
    }

    #[test]
    fn modern_request_missing_client_capabilities_is_malformed() {
        // `clientCapabilities` genuinely is required — capabilities are declared
        // per request and a server may not infer them from an earlier one.
        let params = json!({
            "_meta": {
                "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                "io.modelcontextprotocol/clientInfo": { "name": "C", "version": "1" }
            }
        });
        assert!(matches!(
            ModernRequestMeta::from_params(Some(&params)),
            ModernMeta::Malformed(_)
        ));
    }

    #[test]
    fn modern_request_missing_capabilities_is_malformed() {
        let params = json!({
            "_meta": {
                "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                "io.modelcontextprotocol/clientInfo": { "name": "C", "version": "1" }
            }
        });
        assert!(matches!(
            ModernRequestMeta::from_params(Some(&params)),
            ModernMeta::Malformed(_)
        ));
    }

    #[test]
    fn modern_revisions_are_dated_on_or_after_2026_07_28() {
        assert!(is_modern_revision("2026-07-28"));
        assert!(is_modern_revision("2027-01-01"));
        assert!(!is_modern_revision("2025-11-25"));
        assert!(!is_modern_revision("2024-11-05"));
        assert!(!is_modern_revision("9999"));
        assert!(!is_modern_revision("2026-07-28x"));
        assert!(!is_modern_revision("2026/07/28"));
    }

    fn modern_body(version: &str) -> Value {
        json!({ "_meta": {
            "io.modelcontextprotocol/protocolVersion": version,
            "io.modelcontextprotocol/clientCapabilities": {}
        }})
    }

    #[test]
    fn detect_without_a_header_reads_the_body_alone() {
        assert!(matches!(
            ModernRequestMeta::detect(None, Some(&modern_body("2026-07-28"))),
            ModernMeta::Modern(_)
        ));
        assert!(matches!(
            ModernRequestMeta::detect(None, None),
            ModernMeta::Legacy
        ));
    }

    #[test]
    fn detect_holds_the_header_to_the_body() {
        assert!(matches!(
            ModernRequestMeta::detect(Some(" 2026-07-28 "), Some(&modern_body("2026-07-28"))),
            ModernMeta::Modern(_)
        ));
        assert!(matches!(
            ModernRequestMeta::detect(Some("2025-11-25"), Some(&modern_body("2026-07-28"))),
            ModernMeta::HeaderMismatch(_)
        ));
        assert!(matches!(
            ModernRequestMeta::detect(Some("2026-07-28"), Some(&json!({ "name": "x" }))),
            ModernMeta::Malformed(_)
        ));
        assert!(matches!(
            ModernRequestMeta::detect(Some("2025-11-25"), None),
            ModernMeta::Legacy
        ));
    }

    #[test]
    fn meta_key_constants_match_spec_strings() {
        assert_eq!(
            meta_keys::PROTOCOL_VERSION,
            "io.modelcontextprotocol/protocolVersion"
        );
        assert_eq!(meta_keys::CLIENT_INFO, "io.modelcontextprotocol/clientInfo");
        assert_eq!(
            meta_keys::CLIENT_CAPABILITIES,
            "io.modelcontextprotocol/clientCapabilities"
        );
        assert_eq!(meta_keys::LOG_LEVEL, "io.modelcontextprotocol/logLevel");
        assert_eq!(PROTOCOL_VERSION_2026_07_28, "2026-07-28");
    }

    #[test]
    fn discover_result_serializes_complete_with_cache_hints() {
        let result = DiscoverResult::new(
            vec![
                PROTOCOL_VERSION_2026_07_28.to_owned(),
                PROTOCOL_VERSION.to_owned(),
            ],
            ServerCapabilities::tools_only(),
            ServerInfo::new("test-server", "0.1.0"),
            None,
        );
        let json = serde_json::to_value(&result).expect("serialize"); // Safe: test assertion
        assert_eq!(json["resultType"], "complete");
        assert_eq!(json["supportedVersions"][0], "2026-07-28");
        assert_eq!(json["supportedVersions"][1], "2025-11-25");
        assert!(json["capabilities"]["tools"].is_object());
        assert_eq!(json["serverInfo"]["name"], "test-server");
        assert_eq!(json["cacheScope"], "public");
        assert!(json["ttlMs"].is_number());
    }
}
