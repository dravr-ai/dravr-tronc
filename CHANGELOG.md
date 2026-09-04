# Changelog

## [0.11.0] — 2026-09-03

Finishes the determinism fix 0.10.0 only half made.

### Fixed

- fix(mcp): `PropertySchema::properties` — the NESTED property map — is
  `BTreeMap`, not `HashMap`. 0.10.0 moved `JsonSchema::properties` and `$defs`
  and claimed schema serialization was deterministic; it was not. A schema whose
  outer keys were stable still reshuffled `properties.x.properties` per process,
  so anything hashing or diffing a real (nested) tool schema still saw drift.
  The 0.10.0 entry below overstates what it fixed — this is the rest of it.
- test(mcp): `nested_schema_keys_serialize_in_order` pins the property at every
  level. Asserted against the serialized string, not `serde_json::to_value`: a
  `Value` map is a `BTreeMap` unless `preserve_order` is on, so it sorts keys
  itself and reports success over an unordered source — and whether that feature
  is on depends on which consumer's build unified it in. The first draft of this
  test made exactly that mistake and passed against the unfixed code.

## [0.10.0] — 2026-09-03

Protocol revision `2026-07-28`, second pass: the two conformance gaps the
HTTP transport still had, plus the seam a host needs to enforce OAuth scopes.
Breaking: `AuthError` is now `#[non_exhaustive]` and has a third variant,
`ToolContext` gained a field (use `..Default::default()`), and
`JsonSchema::properties`/`defs` changed map type.

### Added

- feat(mcp): `AuthError::InsufficientScope { www_authenticate, reason }` —
  rendered by the HTTP transport as `403` **carrying** the RFC 6750 §3.1
  challenge, so a client that authenticated but lacks the grant reads
  `scope="…"` off the header and knows what to re-request. A bare `Forbidden`
  tells it only that it lost. Kept as its own variant rather than a field on
  `Forbidden` so hosts that *construct* a `Forbidden` are untouched.
- feat(mcp): `AuthError` is `#[non_exhaustive]`, so the next rejection reason
  is additive for every consumer instead of a release like this one.
- feat(mcp): `ToolContext::scopes: Vec<String>` — the grant on the credential
  the host validated. Empty is the pre-existing behaviour: a host that never
  populates it enforces nothing and keeps working. The vocabulary stays the
  host's; tronc does not name scopes.
- feat(mcp): `ToolCapability::PROFILE` — reading or writing the caller's own
  identity, split out from `READS_DATA`. It is the split an OAuth resource
  server scopes on: history without identity, or identity without history.
  Folded together, a grant for one is a grant for both and no consent screen
  can say which was asked for.
- feat(mcp): `McpServer::accepts_protocol_version` and
  `advertised_protocol_versions`, public because the HTTP transport is the only
  place a stateless server can judge the header.

### Fixed

- fix(mcp): the HTTP transport read `MCP-Protocol-Version` into request
  metadata that nothing read back — wired in appearance only. A revision the
  server does not speak is now refused with `-32022` and a
  `{supported, requested}` body, instead of being served as though it were
  negotiated.
- fix(iam): `token_source_reports_metadata_absence_distinctly` did not carry
  `#[serial]` though its pair sets `GCE_METADATA_HOST`, a process-global. A
  guard one side of a pair holds guards nothing — the two interleaved and both
  failed, intermittently and only under parallel test threads.
- fix(mcp): `JsonSchema::properties` and `$defs` are `BTreeMap`, not `HashMap`.
  A tool schema is hashed, diffed and checked into generated SDK types by
  consumers; `HashMap` made its key order vary run to run, so identical schemas
  serialized differently and read as drift.

## [0.9.0] — 2026-09-02

Re-release of the yanked 0.8.1 under a breaking version. `Tool::execution`
(SEP-2663) is a new `pub` field, which breaks every struct literal that builds
a `Tool` — `embacle-tool-host` does — so it could not ship as a patch. Content
is otherwise 0.8.1's: the `subscriptions/listen` stream and per-tool task
support.

## [0.8.0] — 2026-08-31

Protocol revision `2026-07-28` conformance. Breaking: `Tool`,
`ToolSchema`, `ServerCapabilities`, `JsonSchema`, `PropertySchema` and
`ToolContext` all gained fields, so exhaustive struct literals need updating —
add `..Default::default()` (every one of these now derives `Default`). The two
reserved error-code constants changed value.

### Added

- feat(mcp): implement the `io.modelcontextprotocol/tasks` extension (SEP-2663)
  — durable task handles returned in lieu of a `tools/call` result and polled
  via `tasks/get`, plus `tasks/update` and `tasks/cancel`. New `mcp::tasks`
  module: the flat `CreateTaskResult`, the five-state lifecycle
  (`working`/`input_required`/`completed`/`failed`/`cancelled`), a `TaskStore`
  seam with an in-memory default, and `TaskManager`. Enabled per server with
  `McpServer::with_task_manager`; without one the extension is absent
  end-to-end rather than advertised-but-unserved.
- feat(mcp): `ToolDispatcher::call_tool_outcome` — a defaulted trait method
  returning `CallToolOutcome::{Immediate, Task}`, so an existing dispatcher
  keeps its behaviour untouched.
- feat(mcp): `ToolContext::client_capabilities` plus `supports_tasks` /
  `declares_extension`, carrying the per-request capability declaration the
  extension's opt-in requires.
- feat(mcp): `ServerCapabilities::extensions` — the specified reverse-DNS
  extension map, distinct from the unspecified `experimental`.
- feat(mcp): `outputSchema` on `Tool` and `ToolSchema`.
- feat(mcp): JSON Schema 2020-12 vocabulary on `JsonSchema`/`PropertySchema` —
  `$schema`, `$defs`, `$ref`, `oneOf`/`anyOf`/`allOf`, `enum`, `const`,
  `default`, `format`, `pattern`, numeric and length and item bounds,
  `additionalProperties`. An empty `type` is omitted, which is what a
  `$ref`-only or composition-only subschema needs.

### Fixed

- fix(mcp): the reserved error codes sat inside the implementation-defined
  band, where no client could recognise them. `MISSING_REQUIRED_CLIENT_CAPABILITY`
  moves `-32003` → `-32021` and `UNSUPPORTED_PROTOCOL_VERSION` `-32004` →
  `-32022`; `HEADER_MISMATCH` (`-32020`) is added. The specification reserves
  `-32020`..`-32099` for itself and `-32000`..`-32019` for implementations.
- fix(mcp): `clientInfo` was rejected as a required `_meta` field though the
  specification types it optional, making every conformant client that omits it
  unreachable with `-32602` before any handler ran.

### Changed

- chore: `chrono` is no longer optional — task timestamps need it
  unconditionally. It is dropped from the `notifications` feature list.

## [0.7.1] — 2026-08-18



## [0.7.0] — 2026-08-17

### Added

- feat(iam): service-to-service auth without a shared secret

### Fixed

- fix: repair the SessionStart bootstrap guard for an empty .build



## [0.6.2] — 2026-08-13



## [0.6.1] — 2026-08-13

### Fixed

- fix(slack): a misconfigured channel pages instead of going quiet post_message/update_message are fire-and-forget, so a renamed channel dropped every notification at WARN with the caller believing it had notified (the dev- rename left notify-routing.yaml on #dravr-{events,pulse,signal,trace} and chat.postMessage answered channel_not_found until a human noticed the silence); operator-actionable codes (channel_not_found, is_archived, not_in_channel, auth/scope family) now log at ERROR with the failing channel so ErrorNotificationLayer routes them out the independent error channel, transient codes stay WARN, and the error layer's own digest failure stays WARN on post_message_await as the recursion break.
- fix(security): constant-time Slack HMAC compare + char-safe token Debug redaction Slack sig uses subtle::ct_eq (was ==, timing leak) and JsonRpcRequest Debug slices the auth token by chars not bytes (was panic on multibyte); both covered by new regression tests.

### Other

- chore(register): ledger + weekly phase review
- chore(register): point at dravr-carnet, the dravr-family register



## [0.6.0] — 2026-07-02

### Added

- feat(notifications): host identity in error digests (NotificationConfig.host) Alerts from processes sharing a service_name/environment (dev laptop vs deployed Cloud Run dev) were indistinguishable — an anonymous 'Environment: development' Slack digest cost a morning of sender archaeology. NotificationConfig gains host (DRAVR_NOTIFY_HOST -> K_REVISION -> HOSTNAME -> HOST env chain, None when unset; hosts that resolve better values set it explicitly), the Slack digest context line renders 'Host:' when present via the new pure context_line helper, and the email digest subject body carries the same identity. Tests cover the env chain and both context-line renderings.



## [0.5.3] — 2026-06-18

### Added

- feat(notify): NotifyEnricher seam (NotifyLayerBuilder::with_enricher) that mutates an event's merged fields once, before both the Slack and PostHog sinks — the host injects derived fields (e.g. a cache-resolved user_email, a display emoji) without each call site repeating them.

### Changed

- Immediate Slack posts render identity-led Block Kit: user_email leads, event signal follows, and *_id identifiers move to a muted context block (kept in full for correlation). `emoji` is lifted into the headline. Batched digests keep the compact single-line format. Additive and backward-compatible (enricher defaults to None).



## [0.3.1] — 2026-05-13

### Other

- style(notify): rustfmt formatter cleanup Formatting-only follow-up to the 8b06475 commit; behaviour unchanged.



## [0.3.0] — 2026-05-12



## [0.2.4] — 2026-05-01

### Added

- feat: add otel feature wiring tracing-opentelemetry OTLP exporter Opt-in feature; OTLP/gRPC layer activates when OTEL_EXPORTER_OTLP_ENDPOINT is set, no-op otherwise; service name from OTEL_SERVICE_NAME (default 'dravr-service').

### Fixed

- fix(notifications): use Duration::from_mins(1) for clippy::duration_suboptimal_units (Rust 1.95 pedantic lint)



## [0.2.3] — 2026-04-10

### Other

- build: prune tokio features and remove unused transitive deps



## [0.2.2] — 2026-03-31

### Fixed

- fix: resolve error handling violations found by dravr-build-config validation



## [0.2.1] — 2026-03-26



## [0.2.0] — 2026-03-26

### Added

- feat: add notifications module with Slack, email, and error tracing layer


