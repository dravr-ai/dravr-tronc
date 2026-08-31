# Changelog

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


