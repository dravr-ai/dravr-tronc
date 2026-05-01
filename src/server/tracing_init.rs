// ABOUTME: Tracing subscriber initialization shared across all dravr-xxx server binaries
// ABOUTME: Routes logs to stderr for stdio transport, optionally adds error notification + OTLP layers
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

#[cfg(feature = "otel")]
use std::env;
use std::io;

#[cfg(feature = "otel")]
use opentelemetry::global;
#[cfg(feature = "otel")]
use opentelemetry::trace::TracerProvider as _;
#[cfg(feature = "otel")]
use opentelemetry::KeyValue;
#[cfg(feature = "otel")]
use opentelemetry_otlp::{SpanExporter, WithExportConfig};
#[cfg(feature = "otel")]
use opentelemetry_sdk::propagation::TraceContextPropagator;
#[cfg(feature = "otel")]
use opentelemetry_sdk::trace::{SdkTracer, SdkTracerProvider};
#[cfg(feature = "otel")]
use opentelemetry_sdk::Resource;
#[cfg(feature = "otel")]
use tracing_opentelemetry::OpenTelemetryLayer;
use tracing_subscriber::fmt;
use tracing_subscriber::layer::SubscriberExt;
#[cfg(feature = "otel")]
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

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
/// Cloud Trace on Cloud Run. The `OTEL_SERVICE_NAME` env var labels the
/// service in the trace UI; falls back to `"dravr-service"` when unset.
pub fn init(transport: &str) {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    if transport == "stdio" {
        let registry = tracing_subscriber::registry()
            .with(filter)
            .with(fmt::layer().with_writer(io::stderr));
        #[cfg(feature = "otel")]
        let registry = registry.with(build_otel_layer());
        registry.init();
    } else {
        let registry = tracing_subscriber::registry()
            .with(filter)
            .with(fmt::layer());
        #[cfg(feature = "otel")]
        let registry = registry.with(build_otel_layer());
        registry.init();
    }
}

/// Build the OpenTelemetry tracing layer when `OTEL_EXPORTER_OTLP_ENDPOINT`
/// is configured.
///
/// Returns `None` when the env var is unset (so the local dev experience
/// stays log-only without needing a collector running) or when exporter
/// initialization fails (logged as a warning so a misconfigured prod
/// deploy is visible without crashing the process).
#[cfg(feature = "otel")]
fn build_otel_layer<S>() -> Option<OpenTelemetryLayer<S, SdkTracer>>
where
    S: tracing::Subscriber + for<'span> LookupSpan<'span>,
{
    let endpoint = env::var("OTEL_EXPORTER_OTLP_ENDPOINT").ok()?;

    let service_name = env::var("OTEL_SERVICE_NAME").unwrap_or_else(|_| "dravr-service".to_owned());

    global::set_text_map_propagator(TraceContextPropagator::new());

    let exporter = match SpanExporter::builder()
        .with_tonic()
        .with_endpoint(endpoint)
        .build()
    {
        Ok(exp) => exp,
        Err(e) => {
            tracing::warn!(error = %e, "OTLP span exporter init failed; falling back to logs only");
            return None;
        }
    };

    let resource = Resource::builder()
        .with_attribute(KeyValue::new("service.name", service_name.clone()))
        .build();

    let provider = SdkTracerProvider::builder()
        .with_batch_exporter(exporter)
        .with_resource(resource)
        .build();

    let tracer = provider.tracer(service_name);
    global::set_tracer_provider(provider);

    Some(tracing_opentelemetry::layer().with_tracer(tracer))
}

/// Initialize tracing with the error notification layer enabled
///
/// Same as [`init`] but adds an [`ErrorNotificationLayer`] that captures
/// ERROR-level events and dispatches them to Slack and/or email based on
/// environment configuration.
///
/// Call this instead of [`init`] when you want automatic error alerting.
///
/// [`ErrorNotificationLayer`]: crate::notifications::ErrorNotificationLayer
#[cfg(feature = "notifications")]
pub fn init_with_notifications(transport: &str) {
    use crate::notifications::{
        EmailClient, ErrorNotificationLayer, NotificationConfig, SlackClient,
    };

    let config = NotificationConfig::from_env();

    let slack = config.slack.as_ref().map(SlackClient::new);

    let email = config.email.as_ref().and_then(|c| EmailClient::new(c).ok());

    let has_channels = slack.is_some() || email.is_some();

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    if transport == "stdio" {
        let registry = tracing_subscriber::registry()
            .with(filter)
            .with(fmt::layer().with_writer(io::stderr));

        if has_channels {
            let error_layer = ErrorNotificationLayer::new(config, slack, email);
            registry.with(error_layer).init();
        } else {
            registry.init();
        }
    } else {
        let registry = tracing_subscriber::registry()
            .with(filter)
            .with(fmt::layer());

        if has_channels {
            let error_layer = ErrorNotificationLayer::new(config, slack, email);
            registry.with(error_layer).init();
        } else {
            registry.init();
        }
    }
}
