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
use serde_json::{from_value, Value};

use crate::mcp::schema::{ServerCapabilities, ServerInfo};

/// The modern (stateless, per-request-metadata) MCP protocol revision string.
pub const PROTOCOL_VERSION_2026_07_28: &str = "2026-07-28";

/// Reserved `_meta` keys carrying per-request protocol metadata (revision 2026-07-28).
///
/// All keys use the reserved `io.modelcontextprotocol/` prefix.
pub mod meta_keys {
    /// Protocol version for this request, e.g. `"2026-07-28"`. Required.
    pub const PROTOCOL_VERSION: &str = "io.modelcontextprotocol/protocolVersion";
    /// Client name and version (`Implementation`). Required.
    pub const CLIENT_INFO: &str = "io.modelcontextprotocol/clientInfo";
    /// Client capabilities relevant to this request. Required.
    pub const CLIENT_CAPABILITIES: &str = "io.modelcontextprotocol/clientCapabilities";
    /// Minimum log level the server should emit for this request. Optional.
    pub const LOG_LEVEL: &str = "io.modelcontextprotocol/logLevel";
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
    /// Client identity.
    pub client_info: ModernClientInfo,
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
pub enum ModernMeta {
    /// No modern protocol version in `_meta` — handle as a legacy request.
    Legacy,
    /// A well-formed modern request.
    Modern(Box<ModernRequestMeta>),
    /// A modern request (protocol version present) missing a required field.
    /// The caller maps this to JSON-RPC `-32602` (Invalid params) / HTTP 400.
    Malformed(String),
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

        let Some(client_info) = meta
            .get(meta_keys::CLIENT_INFO)
            .and_then(|v| from_value::<ModernClientInfo>(v.clone()).ok())
        else {
            return ModernMeta::Malformed(format!(
                "missing or invalid required _meta field '{}'",
                meta_keys::CLIENT_INFO
            ));
        };

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

        match ModernRequestMeta::from_params(Some(&params)) {
            ModernMeta::Modern(meta) => {
                assert_eq!(meta.protocol_version, "2026-07-28");
                assert_eq!(meta.client_info.name, "ExampleClient");
                assert_eq!(meta.client_info.version, "1.0.0");
                assert_eq!(meta.log_level.as_deref(), Some("info"));
                assert!(meta.client_capabilities.get("tools").is_some());
            }
            _ => panic!("expected a well-formed modern request"), // Safe: test assertion
        }
    }

    #[test]
    fn modern_request_missing_client_info_is_malformed() {
        let params = json!({
            "_meta": {
                "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                "io.modelcontextprotocol/clientCapabilities": {}
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
