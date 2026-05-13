# Changelog

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


