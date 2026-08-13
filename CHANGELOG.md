# Changelog

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


