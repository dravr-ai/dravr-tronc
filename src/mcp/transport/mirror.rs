// ABOUTME: SEP-2243 request headers: Mcp-Method, Mcp-Name and Mcp-Param-* mirrored from the body
// ABOUTME: Decodes the Base64 sentinel and holds every mirror to the JSON-RPC body it summarises
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

//! The request headers Streamable HTTP mirrors out of the JSON-RPC body
//! (revision 2026-07-28, SEP-2243).
//!
//! A gateway routes and rate-limits on `Mcp-Method`, `Mcp-Name` and the
//! `Mcp-Param-{Name}` headers a tool's input schema asks for through
//! `x-mcp-header`, without parsing the body; the server executes the body. If
//! the two could disagree, what the gateway judged would not be what ran, so
//! every mirror that is present must match its body value, and a modern
//! request must carry the mirrors the revision requires. Every failure is a
//! `HeaderMismatchError` (`-32020`), which the transport answers with 400.

use std::collections::HashMap;
use std::hash::BuildHasher;

use axum::http::{HeaderMap, HeaderName};
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use serde_json::Value;

use crate::mcp::protocol::JsonRpcRequest;

/// `Mcp-Method`: mirrors the JSON-RPC `method`.
pub const MCP_METHOD_HEADER: &str = "mcp-method";

/// `Mcp-Name`: mirrors `params.name` (or `params.uri`) of an addressed method.
pub const MCP_NAME_HEADER: &str = "mcp-name";

/// Prefix of the headers mirroring `x-mcp-header`-annotated tool arguments.
pub const MCP_PARAM_PREFIX: &str = "mcp-param-";

/// The input-schema keyword naming the `Mcp-Param-{Name}` header a property
/// mirrors into.
pub const X_MCP_HEADER: &str = "x-mcp-header";

/// Opening marker of a Base64-wrapped header value (case-sensitive).
const BASE64_PREFIX: &str = "=?base64?";

/// Closing marker of a Base64-wrapped header value (case-sensitive).
const BASE64_SUFFIX: &str = "?=";

/// 2^53 - 1: past it an integer cannot cross a JavaScript intermediary intact,
/// so no header could have mirrored it faithfully.
const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

/// The `params` field `Mcp-Name` mirrors for `method`, or `None` for a method
/// that addresses no named target.
#[must_use]
pub fn mirrored_name_field(method: &str) -> Option<&'static str> {
    match method {
        "resources/read" => Some("uri"),
        "tools/call" | "prompts/get" => Some("name"),
        _ => None,
    }
}

/// Whether `name` is an `Mcp-Param-*` header (header names are matched
/// case-insensitively; `HeaderName` is already lower-case).
fn is_param_header(name: &str) -> bool {
    name.get(..MCP_PARAM_PREFIX.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(MCP_PARAM_PREFIX))
}

/// Decode a mirror's wire value: trimmed, and unwrapped from the
/// `=?base64?…?=` sentinel when it carries one.
///
/// # Errors
///
/// The sentinel wraps a payload that is not canonical, padded Base64 of UTF-8
/// text. The revision requires rejecting bad padding, not only a bad
/// alphabet, and the standard engine refuses both.
pub fn decode_header_value(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    let Some(payload) = trimmed
        .strip_prefix(BASE64_PREFIX)
        .and_then(|rest| rest.strip_suffix(BASE64_SUFFIX))
    else {
        return Ok(trimmed.to_owned());
    };
    let bytes = STANDARD
        .decode(payload)
        .map_err(|_| "invalid Base64 encoding".to_owned())?;
    String::from_utf8(bytes).map_err(|_| "Base64 payload is not UTF-8".to_owned())
}

/// Encode a mirror's value for the wire, the counterpart of [`decode_header_value`].
///
/// The value goes as it is when every byte is permitted in a field value, and
/// wrapped in the `=?base64?…?=` sentinel otherwise.
#[must_use]
pub fn encode_header_value(value: &str) -> String {
    if is_permitted_field_value(value.as_bytes()) && value.trim() == value {
        value.to_owned()
    } else {
        format!("{BASE64_PREFIX}{}{BASE64_SUFFIX}", STANDARD.encode(value))
    }
}

/// The single value of header `name`, as text.
///
/// # Errors
///
/// The header is repeated, or is not visible ASCII.
fn single_header<'h>(headers: &'h HeaderMap, name: &str) -> Result<Option<&'h str>, String> {
    let mut values = headers.get_all(name).iter();
    let Some(value) = values.next() else {
        return Ok(None);
    };
    if values.next().is_some() {
        return Err(format!("Header mismatch: {name} is repeated"));
    }
    value.to_str().map(Some).map_err(|_| {
        format!("Header mismatch: {name} contains characters not permitted in an HTTP field value")
    })
}

/// Whether a field value holds only what the revision permits on the wire:
/// horizontal tab, space and visible ASCII. Anything else must travel
/// Base64-wrapped.
fn is_permitted_field_value(value: &[u8]) -> bool {
    value
        .iter()
        .all(|&b| b == b'\t' || (0x20..=0x7E).contains(&b))
}

/// Check the mirrors a request's HTTP headers carry against its body:
/// `Mcp-Method`, `Mcp-Name`, and the wire form of every `Mcp-Param-*`.
///
/// A mirror that is present must match in every era. A `modern` request
/// (one whose `MCP-Protocol-Version` names 2026-07-28 or later) must also
/// carry `Mcp-Method`, and `Mcp-Name` on `tools/call`, `resources/read` and
/// `prompts/get`. A notification carries no header requirement of its own
/// (the revision defines none), but a mirror it does carry must still match.
/// Mirrors are singletons: a repeated one names no single value to route on.
///
/// The `Mcp-Param-*` values are only checked for their characters here; whether
/// each matches its argument depends on the tool's input schema, which
/// [`check_param_headers`] reads at dispatch.
///
/// # Errors
///
/// The `HeaderMismatchError` message for the first rule the request breaks.
pub fn check_standard_headers(
    headers: &HeaderMap,
    request: &JsonRpcRequest,
    modern: bool,
) -> Result<(), String> {
    let required = modern && request.id.is_some();

    for name in headers.keys().map(HeaderName::as_str) {
        if !is_param_header(name) {
            continue;
        }
        if headers.get_all(name).iter().count() > 1 {
            return Err(format!("Header mismatch: {name} is repeated"));
        }
        if headers
            .get_all(name)
            .iter()
            .any(|value| !is_permitted_field_value(value.as_bytes()))
        {
            return Err(format!(
                "Header mismatch: {name} contains characters not permitted in an HTTP field value"
            ));
        }
    }

    match single_header(headers, MCP_METHOD_HEADER)? {
        Some(method) if method.trim() != request.method => {
            return Err(format!(
                "Header mismatch: Mcp-Method header value '{}' does not match body method '{}'",
                method.trim(),
                request.method
            ));
        }
        None if required => {
            return Err(
                "Header mismatch: Mcp-Method is required and must mirror body method".to_owned(),
            );
        }
        _ => {}
    }

    let name_header = single_header(headers, MCP_NAME_HEADER)?;
    let Some(field) = mirrored_name_field(&request.method) else {
        // A method addressing no target has nothing for the header to name.
        return Ok(());
    };
    let body_name = request
        .params
        .as_ref()
        .and_then(|p| p.get(field))
        .and_then(Value::as_str);
    match name_header {
        Some(raw) => {
            // A payload that does not decode carries no value that could
            // match, so it is reported as the mismatch it is.
            let decoded = decode_header_value(raw).ok();
            if decoded.as_deref() != body_name || body_name.is_none() {
                return Err(format!(
                    "Header mismatch: Mcp-Name header value '{}' does not match body value '{}'",
                    decoded.as_deref().unwrap_or_else(|| raw.trim()),
                    body_name.unwrap_or_default()
                ));
            }
        }
        None if required => {
            return Err(format!(
                "Header mismatch: Mcp-Name is required for method '{}'",
                request.method
            ));
        }
        None => {}
    }
    Ok(())
}

/// One argument a tool's input schema asks a caller to mirror into an
/// `Mcp-Param-*` header.
struct MirroredArgument<'s> {
    /// The lower-cased header name, `mcp-param-{name}`.
    header: String,
    /// The chain of `properties` keys leading to the argument.
    path: Vec<&'s str>,
}

/// Every `x-mcp-header` annotation statically reachable from the schema root
/// through `properties` keys alone — the only places the revision lets one
/// stand.
fn mirrored_arguments(schema: &Value) -> Vec<MirroredArgument<'_>> {
    fn walk<'s>(schema: &'s Value, path: &[&'s str], found: &mut Vec<MirroredArgument<'s>>) {
        let Some(properties) = schema.get("properties").and_then(Value::as_object) else {
            return;
        };
        for (key, property) in properties {
            let mut here = path.to_vec();
            here.push(key.as_str());
            if let Some(name) = property.get(X_MCP_HEADER).and_then(Value::as_str) {
                found.push(MirroredArgument {
                    header: format!("{MCP_PARAM_PREFIX}{}", name.to_ascii_lowercase()),
                    path: here.clone(),
                });
            }
            walk(property, &here, found);
        }
    }
    let mut found = Vec::new();
    walk(schema, &[], &mut found);
    found
}

/// What a body value must appear as in its header: a string as-is, a boolean
/// in lower case, a number in decimal. `None` for a value no header could
/// carry — absent, `null`, an object or an array — which the header must
/// then be absent for too.
fn mirrorable(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(s) => Some(s.clone()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// Whether a header carrying `header` mirrors body value `body`. Numbers
/// compare numerically, so `42` and `42.0` agree — which also stops a
/// spoof, since JSON Schema accepts `42.0` as an `integer`.
fn mirrors(body: &Value, body_text: &str, header: &str) -> bool {
    body.as_f64().map_or_else(
        || header == body_text,
        |number| {
            header
                .parse::<f64>()
                .is_ok_and(|parsed| parsed.total_cmp(&number).is_eq())
        },
    )
}

/// Whether any `Mcp-Param-*` header was forwarded — the cheap skip for the
/// clients that send none.
#[must_use]
pub fn carries_param_header<H: BuildHasher>(headers: &HashMap<String, Value, H>) -> bool {
    headers.keys().any(|name| is_param_header(name))
}

/// Hold the `Mcp-Param-*` headers of a `tools/call` to the arguments the
/// tool's `input_schema` annotates with `x-mcp-header`.
///
/// `headers` are the request's forwarded headers (lower-case names). A
/// header present for an argument the call did not pass, or that does not
/// match it after decoding, is refused in every era; a `modern` call that
/// passes an annotated argument without its header is refused too, since a
/// gateway would then see less than the server executes. An integer beyond
/// the JavaScript safe range could not have been mirrored faithfully and is
/// refused when mirrored.
///
/// # Errors
///
/// The `HeaderMismatchError` message for the first argument that fails.
pub fn check_param_headers<H: BuildHasher>(
    input_schema: &Value,
    arguments: Option<&Value>,
    headers: &HashMap<String, Value, H>,
    modern: bool,
) -> Result<(), String> {
    for argument in mirrored_arguments(input_schema) {
        let body = arguments.and_then(|arguments| {
            argument
                .path
                .iter()
                .try_fold(arguments, |value, key| value.get(*key))
        });
        let header = headers.get(&argument.header).and_then(Value::as_str);
        let name = &argument.header;
        let property = argument.path.join(".");
        match (header, body, mirrorable(body)) {
            (None, _, Some(_)) if modern => {
                return Err(format!(
                    "Header mismatch: {name} is required because body arguments contains '{property}'"
                ));
            }
            (Some(_), _, None) => {
                return Err(format!(
                    "Header mismatch: {name} is present but body has no matching value"
                ));
            }
            (Some(raw), Some(body), Some(body_text)) => {
                if body
                    .as_f64()
                    .is_some_and(|n| !n.is_finite() || n.abs() > MAX_SAFE_INTEGER)
                {
                    return Err(format!(
                        "Header mismatch: {name} value '{body_text}' is outside the safe integer range and cannot be mirrored"
                    ));
                }
                let decoded = decode_header_value(raw)
                    .map_err(|reason| format!("Header mismatch: {name} has {reason}"))?;
                if !mirrors(body, &body_text, &decoded) {
                    return Err(format!(
                        "Header mismatch: {name} header value '{decoded}' does not match body value '{body_text}'"
                    ));
                }
            }
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn decode_reads_literals_and_the_base64_sentinel() {
        assert_eq!(
            decode_header_value("  us-west1 ").as_deref(),
            Ok("us-west1")
        );
        assert_eq!(
            decode_header_value("=?base64?SGVsbG8sIOS4lueVjA==?=").as_deref(),
            Ok("Hello, 世界")
        );
        assert_eq!(
            decode_header_value("=?base64?PT9iYXNlNjQ/bGl0ZXJhbD89?=").as_deref(),
            Ok("=?base64?literal?=")
        );
        // The markers are case-sensitive: anything else is a literal.
        assert_eq!(
            decode_header_value("=?BASE64?SGk=?=").as_deref(),
            Ok("=?BASE64?SGk=?=")
        );
        // Missing padding and a bad alphabet are both refused.
        assert!(decode_header_value("=?base64?SGVsbG8?=").is_err());
        assert!(decode_header_value("=?base64?SGV*bG8=?=").is_err());
    }

    #[test]
    fn mirrored_name_fields_follow_the_addressed_methods() {
        assert_eq!(mirrored_name_field("tools/call"), Some("name"));
        assert_eq!(mirrored_name_field("prompts/get"), Some("name"));
        assert_eq!(mirrored_name_field("resources/read"), Some("uri"));
        assert_eq!(mirrored_name_field("tools/list"), None);
    }

    #[test]
    fn annotations_are_found_only_through_properties_chains() {
        let schema = json!({
            "type": "object",
            "properties": {
                "a": { "type": "string", "x-mcp-header": "A" },
                "nested": { "type": "object", "properties": {
                    "b": { "type": "boolean", "x-mcp-header": "B" }
                }},
                "list": { "type": "array", "items": {
                    "type": "string", "x-mcp-header": "Hidden"
                }}
            }
        });
        let mut found: Vec<(String, String)> = mirrored_arguments(&schema)
            .into_iter()
            .map(|m| (m.header, m.path.join(".")))
            .collect();
        found.sort();
        assert_eq!(
            found,
            vec![
                ("mcp-param-a".to_owned(), "a".to_owned()),
                ("mcp-param-b".to_owned(), "nested.b".to_owned()),
            ]
        );
    }

    #[test]
    fn param_headers_compare_booleans_and_numbers_by_value() {
        let schema = json!({ "properties": {
            "flag": { "type": "boolean", "x-mcp-header": "Flag" },
            "n": { "type": "integer", "x-mcp-header": "N" }
        }});
        let headers: HashMap<String, Value> = [
            ("mcp-param-flag".to_owned(), json!("true")),
            ("mcp-param-n".to_owned(), json!("42")),
        ]
        .into_iter()
        .collect();
        let arguments = json!({ "flag": true, "n": 42.0 });
        assert!(check_param_headers(&schema, Some(&arguments), &headers, true).is_ok());
        let arguments = json!({ "flag": false, "n": 42 });
        assert!(check_param_headers(&schema, Some(&arguments), &headers, true).is_err());
    }

    #[test]
    fn a_null_or_absent_argument_expects_no_header() {
        let schema = json!({ "properties": { "r": { "type": "string", "x-mcp-header": "R" } } });
        let none = HashMap::new();
        assert!(check_param_headers(&schema, Some(&json!({ "r": null })), &none, true).is_ok());
        assert!(check_param_headers(&schema, None, &none, true).is_ok());
        assert!(check_param_headers(&schema, Some(&json!({ "r": "x" })), &none, false).is_ok());
        assert!(check_param_headers(&schema, Some(&json!({ "r": "x" })), &none, true).is_err());
    }
}
