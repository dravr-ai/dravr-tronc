// ABOUTME: Tracing subscriber initialization shared across all dravr-xxx server binaries
// ABOUTME: Logs to stderr for stdio transport; optional error notification and OTLP export, flushed by a guard
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

#[cfg(feature = "otel")]
use std::env;
#[cfg(feature = "otel")]
use std::error::Error;
#[cfg(feature = "otel")]
use std::fmt::{Display, Formatter, Result as FmtResult};
use std::io;

#[cfg(feature = "otel")]
use opentelemetry::global;
#[cfg(feature = "otel")]
use opentelemetry::trace::TracerProvider as _;
#[cfg(feature = "otel")]
use opentelemetry::KeyValue;
#[cfg(feature = "otel")]
use opentelemetry_otlp::{ExporterBuildError, MetricExporter, SpanExporter, WithExportConfig};
#[cfg(feature = "otel")]
use opentelemetry_sdk::metrics::SdkMeterProvider;
#[cfg(feature = "otel")]
use opentelemetry_sdk::propagation::TraceContextPropagator;
#[cfg(feature = "otel")]
use opentelemetry_sdk::trace::{SdkTracer, SdkTracerProvider};
#[cfg(feature = "otel")]
use opentelemetry_sdk::Resource;
#[cfg(feature = "otel")]
use tracing::warn;
#[cfg(feature = "otel")]
use tracing_opentelemetry::OpenTelemetryLayer;
use tracing_subscriber::fmt;
use tracing_subscriber::fmt::writer::BoxMakeWriter;
#[cfg(not(feature = "otel"))]
use tracing_subscriber::layer::Identity;
use tracing_subscriber::layer::SubscriberExt;
#[cfg(feature = "otel")]
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

/// `service.name` of a process that does not set `OTEL_SERVICE_NAME`.
#[cfg(feature = "otel")]
const DEFAULT_SERVICE_NAME: &str = "dravr-service";

/// Keeps the process's telemetry export alive; dropping it flushes and stops
/// it.
///
/// [`init`] and `init_with_notifications` return it. Hold it in `main` for
/// the life of the process — `let _telemetry_guard = tracing_init::init(..);`
/// — and let it drop when `main` returns, after the server has stopped: the
/// OTLP span and meter providers then export the last batch of spans and the
/// last metric period before shutting down, so a service stopped by SIGTERM
/// keeps what it recorded while it drained. Dropped at once (`let _ = ..`),
/// export stops before it starts.
///
/// Without the `otel` feature, or without `OTEL_EXPORTER_OTLP_ENDPOINT`,
/// nothing is exported and the guard has nothing to flush.
#[must_use = "dropping the guard stops OTLP export; hold it until the process exits"]
#[derive(Debug)]
pub struct TelemetryGuard {
    #[cfg(feature = "otel")]
    providers: Option<OtelProviders>,
}

/// Initialize the tracing subscriber based on the transport mode
///
/// - `"stdio"` transport: logs go to **stderr** (stdout is reserved for JSON-RPC)
/// - Any other transport: logs go to **stdout**
///
/// Reads `RUST_LOG` env var for filter directives, defaults to `"info"`.
///
/// When the `otel` feature is enabled and `OTEL_EXPORTER_OTLP_ENDPOINT`
/// is set in the environment, also wires an OTLP/gRPC tracing layer so
/// every span (including downstream `#[instrument]` spans across the
/// dravr workspace) is exported to a collector — Tempo, Jaeger,
/// Honeycomb, or the GCP managed `OpenTelemetry` collector that fronts
/// Cloud Trace on Cloud Run. The same endpoint receives metrics — the
/// `mcp.server.operation.duration` histogram an
/// [`OtelObserver`](crate::mcp::observe::OtelObserver) records — through a
/// global meter provider, and the W3C trace-context propagator is installed
/// for [`trace_context`](crate::server::trace_context). The
/// `OTEL_SERVICE_NAME` env var labels the service in the trace UI; falls back
/// to `"dravr-service"` when unset.
///
/// An exporter that fails to build does not stop the process: it is logged at
/// WARN once the subscriber is installed, naming which exporter failed, and
/// the process runs on with logs only (or traces without metrics).
///
/// Hold the returned [`TelemetryGuard`] until the process exits; dropping it
/// flushes what is still buffered.
pub fn init(transport: &str) -> TelemetryGuard {
    init_writing_to(log_writer(transport))
}

/// Where the log lines of `transport` go: stderr for stdio, whose stdout
/// carries the protocol, stdout for everything else.
fn log_writer(transport: &str) -> BoxMakeWriter {
    if transport == "stdio" {
        BoxMakeWriter::new(io::stderr)
    } else {
        BoxMakeWriter::new(io::stdout)
    }
}

/// `RUST_LOG`'s directives, `info` when unset or unreadable.
fn log_filter() -> EnvFilter {
    EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"))
}

/// [`init`] with the log lines written to `writer`.
fn init_writing_to(writer: BoxMakeWriter) -> TelemetryGuard {
    let (export, pending) = export_layer();
    tracing_subscriber::registry()
        .with(log_filter())
        .with(fmt::layer().with_writer(writer))
        .with(export)
        .init();
    installed(pending)
}

/// The export layer of a build without the `otel` feature: nothing to add.
#[cfg(not(feature = "otel"))]
fn export_layer() -> (Identity, TelemetryGuard) {
    (Identity::new(), TelemetryGuard {})
}

/// The guard of a build without the `otel` feature, which has nothing to
/// report.
#[cfg(not(feature = "otel"))]
const fn installed(guard: TelemetryGuard) -> TelemetryGuard {
    guard
}

/// The OTLP layer `OTEL_EXPORTER_OTLP_ENDPOINT` configures, with the
/// propagator and the global providers installed beside it, and what came of
/// setting it up.
///
/// No layer when the variable is unset — local development stays log-only
/// without a collector running — or when the span exporter fails to build.
/// Nothing is logged here: no subscriber exists yet, so a `warn!` would reach
/// nobody. [`installed`] logs the failures once one does.
#[cfg(feature = "otel")]
fn export_layer<S>() -> (Option<OpenTelemetryLayer<S, SdkTracer>>, OtelSetup)
where
    S: tracing::Subscriber + for<'span> LookupSpan<'span>,
{
    let Some(config) = OtelConfig::from_env() else {
        return (None, OtelSetup::default());
    };
    global::set_text_map_propagator(TraceContextPropagator::new());
    match OtelPipeline::build(&config) {
        Ok(pipeline) => {
            global::set_tracer_provider(pipeline.providers.tracer.clone());
            if let Some(meter) = &pipeline.providers.meter {
                global::set_meter_provider(meter.clone());
            }
            let layer = tracing_opentelemetry::layer().with_tracer(pipeline.tracer);
            let setup = OtelSetup {
                providers: Some(pipeline.providers),
                failures: pipeline.metric_failure.into_iter().collect(),
            };
            (Some(layer), setup)
        }
        Err(failure) => (
            None,
            OtelSetup {
                providers: None,
                failures: vec![failure],
            },
        ),
    }
}

/// Log what failed while export was set up — now that a subscriber exists to
/// receive it — and hand the providers to the guard that flushes them.
#[cfg(feature = "otel")]
fn installed(setup: OtelSetup) -> TelemetryGuard {
    for failure in &setup.failures {
        warn!(error = %failure, "{}", failure.consequence());
    }
    TelemetryGuard {
        providers: setup.providers,
    }
}

/// Where and as what the process exports, read from the environment.
#[cfg(feature = "otel")]
#[derive(Debug, Clone, PartialEq, Eq)]
struct OtelConfig {
    /// `OTEL_EXPORTER_OTLP_ENDPOINT`.
    endpoint: String,
    /// `OTEL_SERVICE_NAME`, or [`DEFAULT_SERVICE_NAME`].
    service_name: String,
}

#[cfg(feature = "otel")]
impl OtelConfig {
    /// The configuration the environment names, or `None` when
    /// `OTEL_EXPORTER_OTLP_ENDPOINT` is unset.
    fn from_env() -> Option<Self> {
        let endpoint = env::var("OTEL_EXPORTER_OTLP_ENDPOINT").ok()?;
        let service_name =
            env::var("OTEL_SERVICE_NAME").unwrap_or_else(|_| DEFAULT_SERVICE_NAME.to_owned());
        Some(Self {
            endpoint,
            service_name,
        })
    }
}

/// An exporter that could not be built.
#[cfg(feature = "otel")]
#[derive(Debug)]
enum OtelInitError {
    /// The span exporter: nothing is exported over OTLP.
    SpanExporter(ExporterBuildError),
    /// The metric exporter: spans export, metrics do not.
    MetricExporter(ExporterBuildError),
}

#[cfg(feature = "otel")]
impl OtelInitError {
    /// What the process does without the exporter that failed.
    const fn consequence(&self) -> &'static str {
        match self {
            Self::SpanExporter(_) => "OTLP export disabled; falling back to logs only",
            Self::MetricExporter(_) => "OTLP metric export disabled; exporting traces only",
        }
    }
}

#[cfg(feature = "otel")]
impl Display for OtelInitError {
    fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult {
        match self {
            Self::SpanExporter(source) => write!(f, "OTLP span exporter init failed: {source}"),
            Self::MetricExporter(source) => {
                write!(f, "OTLP metric exporter init failed: {source}")
            }
        }
    }
}

#[cfg(feature = "otel")]
impl Error for OtelInitError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::SpanExporter(source) | Self::MetricExporter(source) => Some(source),
        }
    }
}

/// What [`export_layer`] set up: the providers for the guard, and the
/// failures to log once a subscriber is installed.
#[cfg(feature = "otel")]
#[derive(Debug, Default)]
struct OtelSetup {
    providers: Option<OtelProviders>,
    failures: Vec<OtelInitError>,
}

/// The providers export runs through, kept so the guard can flush them.
#[cfg(feature = "otel")]
#[derive(Debug)]
struct OtelProviders {
    tracer: SdkTracerProvider,
    meter: Option<SdkMeterProvider>,
}

#[cfg(feature = "otel")]
impl OtelProviders {
    /// Export what is still buffered, then stop. A failure is logged: the
    /// process is exiting, and there is nothing else to do with it.
    fn shutdown(self) {
        if let Err(e) = self.tracer.force_flush() {
            warn!(error = %e, "OTLP span flush failed at shutdown");
        }
        if let Err(e) = self.tracer.shutdown() {
            warn!(error = %e, "OTLP span provider shutdown failed");
        }
        if let Some(meter) = self.meter {
            if let Err(e) = meter.force_flush() {
                warn!(error = %e, "OTLP metric flush failed at shutdown");
            }
            if let Err(e) = meter.shutdown() {
                warn!(error = %e, "OTLP meter provider shutdown failed");
            }
        }
    }
}

#[cfg(feature = "otel")]
impl Drop for TelemetryGuard {
    fn drop(&mut self) {
        if let Some(providers) = self.providers.take() {
            providers.shutdown();
        }
    }
}

/// The exporters and providers an [`OtelConfig`] describes, not yet
/// installed anywhere.
#[cfg(feature = "otel")]
struct OtelPipeline {
    tracer: SdkTracer,
    providers: OtelProviders,
    /// The metric exporter's failure, when spans export without metrics.
    metric_failure: Option<OtelInitError>,
}

#[cfg(feature = "otel")]
impl OtelPipeline {
    /// Build the span exporter and, beside it, the metric exporter on the
    /// SDK's periodic schedule, both over OTLP/gRPC to `config.endpoint`.
    ///
    /// A metric exporter that fails to build leaves the meters no-ops and is
    /// reported in [`Self::metric_failure`]; spans still export.
    fn build(config: &OtelConfig) -> Result<Self, OtelInitError> {
        let exporter = SpanExporter::builder()
            .with_tonic()
            .with_endpoint(config.endpoint.clone())
            .build()
            .map_err(OtelInitError::SpanExporter)?;
        let resource = Resource::builder()
            .with_attribute(KeyValue::new("service.name", config.service_name.clone()))
            .build();
        let tracer_provider = SdkTracerProvider::builder()
            .with_batch_exporter(exporter)
            .with_resource(resource.clone())
            .build();
        let tracer = tracer_provider.tracer(config.service_name.clone());

        let (meter, metric_failure) = match MetricExporter::builder()
            .with_tonic()
            .with_endpoint(config.endpoint.clone())
            .build()
        {
            Ok(exporter) => {
                let meter = SdkMeterProvider::builder()
                    .with_periodic_exporter(exporter)
                    .with_resource(resource)
                    .build();
                (Some(meter), None)
            }
            Err(e) => (None, Some(OtelInitError::MetricExporter(e))),
        };

        Ok(Self {
            tracer,
            providers: OtelProviders {
                tracer: tracer_provider,
                meter,
            },
            metric_failure,
        })
    }
}

/// Initialize tracing with the error notification layer enabled
///
/// Same as [`init`] — including the OTLP layer when the `otel` feature is on
/// and `OTEL_EXPORTER_OTLP_ENDPOINT` is set, and the [`TelemetryGuard`] to
/// hold — but adds an [`ErrorNotificationLayer`] that captures ERROR-level
/// events and dispatches them to Slack and/or email based on environment
/// configuration.
///
/// Call this instead of [`init`] when you want automatic error alerting.
///
/// [`ErrorNotificationLayer`]: crate::notifications::ErrorNotificationLayer
#[cfg(feature = "notifications")]
pub fn init_with_notifications(transport: &str) -> TelemetryGuard {
    use crate::notifications::{
        EmailClient, ErrorNotificationLayer, NotificationConfig, SlackClient,
    };

    let config = NotificationConfig::from_env();

    let slack = config.slack.as_ref().map(SlackClient::new);

    let email = config.email.as_ref().and_then(|c| EmailClient::new(c).ok());

    let has_channels = slack.is_some() || email.is_some();

    let error_layer = has_channels.then(|| ErrorNotificationLayer::new(config, slack, email));

    let (export, pending) = export_layer();
    tracing_subscriber::registry()
        .with(log_filter())
        .with(fmt::layer().with_writer(log_writer(transport)))
        .with(export)
        .with(error_layer)
        .init();
    installed(pending)
}

#[cfg(test)]
#[cfg(feature = "otel")]
mod tests {
    use super::*;
    use std::future::{ready, Future};
    use std::io::Write;
    use std::sync::{Arc, Mutex, PoisonError};

    use opentelemetry::trace::Tracer as _;
    use opentelemetry_sdk::error::OTelSdkResult;
    use opentelemetry_sdk::trace::{SpanData, SpanExporter as SdkSpanExporter};
    use tracing_subscriber::fmt::MakeWriter;

    /// Bytes a writer collected, shared with the test that reads them.
    #[derive(Clone, Default)]
    struct Buffer(Arc<Mutex<Vec<u8>>>);

    impl Buffer {
        fn text(&self) -> String {
            let bytes = self
                .0
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone();
            String::from_utf8_lossy(&bytes).into_owned()
        }
    }

    impl Write for Buffer {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl<'a> MakeWriter<'a> for Buffer {
        type Writer = Self;

        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    /// The names of the spans exported to it. Unlike the SDK's in-memory
    /// exporter it keeps them through shutdown, which is the point here.
    #[derive(Debug, Clone, Default)]
    struct Recorded(Arc<Mutex<Vec<String>>>);

    impl Recorded {
        fn names(&self) -> Vec<String> {
            self.0
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone()
        }
    }

    impl SdkSpanExporter for Recorded {
        fn export(&self, batch: Vec<SpanData>) -> impl Future<Output = OTelSdkResult> + Send {
            self.0
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .extend(batch.into_iter().map(|span| span.name.into_owned()));
            ready(Ok(()))
        }
    }

    fn config(endpoint: &str) -> OtelConfig {
        OtelConfig {
            endpoint: endpoint.to_owned(),
            service_name: "tracing-init-test".to_owned(),
        }
    }

    #[test]
    fn an_endpoint_that_is_no_uri_fails_the_span_exporter_by_name() {
        let failure = OtelPipeline::build(&config("not a uri"))
            .err()
            .expect("a malformed endpoint cannot build an exporter"); // Safe: test assertion

        assert!(
            matches!(failure, OtelInitError::SpanExporter(_)),
            "{failure:?}"
        );
        let text = failure.to_string();
        assert!(
            text.starts_with("OTLP span exporter init failed: "),
            "the error names the exporter that failed: {text}"
        );
        assert!(text.contains("not a uri"), "{text}");
        assert_eq!(
            failure.consequence(),
            "OTLP export disabled; falling back to logs only"
        );
    }

    /// The failure used to be logged while the layer was built, before any
    /// subscriber existed, so it reached nobody. It is logged once the
    /// subscriber the process logs through is installed.
    ///
    /// The only test in this crate's unit tests that installs the global
    /// subscriber, which a process does once.
    #[test]
    fn an_exporter_failure_is_logged_through_the_installed_subscriber() {
        const ENDPOINT: &str = "OTEL_EXPORTER_OTLP_ENDPOINT";
        let previous = env::var(ENDPOINT).ok();
        env::set_var(ENDPOINT, "not a uri");
        let logs = Buffer::default();

        let guard = init_writing_to(BoxMakeWriter::new(logs.clone()));
        match previous {
            Some(value) => env::set_var(ENDPOINT, value),
            None => env::remove_var(ENDPOINT),
        }

        let text = logs.text();
        let warning = text
            .lines()
            .find(|line| line.contains("OTLP export disabled"))
            .unwrap_or_else(|| panic!("the failure reaches the log: {text:?}"));
        // The production writer keeps the fmt layer's colours, so the level
        // sits between escape codes rather than spaces.
        assert!(warning.contains("WARN"), "{warning}");
        assert!(
            warning.contains("OTLP span exporter init failed"),
            "the line names the exporter that failed: {warning}"
        );
        assert!(
            guard.providers.is_none(),
            "a failed exporter leaves nothing to flush"
        );
    }

    /// A span recorded before the guard drops is exported by the drop, not
    /// lost with the batch still waiting for its schedule.
    #[test]
    fn dropping_the_guard_exports_the_last_batch() {
        let recorded = Recorded::default();
        let provider = SdkTracerProvider::builder()
            .with_batch_exporter(recorded.clone())
            .build();
        provider.tracer("guard-test").in_span("before-exit", |_| {});
        assert!(
            recorded.names().is_empty(),
            "the batch processor holds the span until its schedule or a flush"
        );

        drop(TelemetryGuard {
            providers: Some(OtelProviders {
                tracer: provider,
                meter: None,
            }),
        });

        assert_eq!(recorded.names(), ["before-exit"]);
    }
}
