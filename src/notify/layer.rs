// ABOUTME: NotifyLayer — a tracing::Layer that routes `target: "notify"` events to Slack per RoutingProvider rules
// ABOUTME: Handles dedup, sampling, env filtering, and time-windowed batching; stashes span fields for automatic context
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use std::collections::HashMap;
use std::mem;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::time::{interval, MissedTickBehavior};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Subscriber};
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::Layer;

use super::provider::{AnalyticsProvider, NotifyEnricher, RoutingProvider};
use super::rule::RoutingRule;
use super::state::{
    build_dedup_key, BatchBuffer, BatchBuffers, BatchedLine, DedupMap, SharedBatchBuffers,
    SpanFields,
};
use super::visit::NotifyVisitor;
use crate::notifications::{PostHogClient, SlackClient};

/// Tracing target the layer filters on. Events whose target differs are
/// ignored — they're regular application logs, not notify-channel pings.
pub const NOTIFY_TARGET: &str = "notify";

/// Default cadence at which the batch flusher wakes to drain ripe buffers.
/// Individual rules can still configure longer intervals; this just bounds
/// the wake-up granularity.
const DEFAULT_FLUSH_TICK: Duration = Duration::from_secs(5);

/// A `tracing_subscriber::Layer` that turns `info!(target: "notify",
/// event = "...", ...)` events into Slack messages per the rules served by
/// a [`RoutingProvider`].
///
/// `NotifyLayer` is `Clone` so it composes with other layers in a
/// `tracing_subscriber::Registry` stack — all interior state lives behind
/// `Arc` and the trait impls take `&self`.
///
/// ## Call-site contract (from ADR-014)
///
/// ```rust,ignore
/// use tracing::{info, instrument};
///
/// #[instrument(skip_all, fields(tenant_id = %tenant_id, user_id = %user_id))]
/// async fn login_handler(tenant_id: &str, user_id: &str) {
///     // ... auth logic ...
///     info!(target: "notify", event = "user.login", "user authenticated");
/// }
/// ```
///
/// The span fields `tenant_id` and `user_id` are folded into the emitted
/// event automatically — call sites never repeat them. Event-specific
/// fields (e.g. `model`, `latency_ms`) are added as additional event
/// fields on the `info!` call itself.
///
/// ## Construction
///
/// ```rust,ignore
/// use std::sync::Arc;
/// use dravr_tronc::notifications::SlackClient;
/// use dravr_tronc::notify::{NotifyLayer, RoutingRule, StaticRoutingProvider};
///
/// let provider = StaticRoutingProvider::new()
///     .with_rule("user.login", RoutingRule::to_channel("#pierre-pulse"));
/// let layer = NotifyLayer::new(
///     Arc::new(slack_client),
///     Arc::new(provider),
///     "production".to_owned(),
/// );
/// // Compose with `tracing_subscriber::registry()` and other layers as usual.
/// ```
pub struct NotifyLayer<R: RoutingProvider> {
    inner: Arc<LayerInner<R>>,
}

struct LayerInner<R: RoutingProvider> {
    slack: Arc<SlackClient>,
    /// Per-event `PostHog` capture policy. `None` = Slack-only layer.
    analytics_provider: Option<Arc<dyn AnalyticsProvider>>,
    /// `PostHog` transport. Paired with `analytics_provider`; both must be set.
    posthog: Option<Arc<PostHogClient>>,
    /// Per-event field enrichment applied before both sinks. `None` = no
    /// enrichment; events render with exactly the fields the call sites carry.
    enricher: Option<Arc<dyn NotifyEnricher>>,
    provider: Arc<R>,
    environment: String,
    default_rule: Option<RoutingRule>,
    dedup: DedupMap,
    batches: SharedBatchBuffers,
    sample_counter: AtomicU64,
}

impl<R: RoutingProvider> Clone for NotifyLayer<R> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<R: RoutingProvider> NotifyLayer<R> {
    /// Build a layer with no default rule (unknown events are dropped) and
    /// the default `5s` flush tick.
    pub fn new(slack: Arc<SlackClient>, provider: Arc<R>, environment: String) -> Self {
        Self::builder(slack, provider, environment).build()
    }

    /// Start a builder for finer-grained construction.
    pub fn builder(
        slack: Arc<SlackClient>,
        provider: Arc<R>,
        environment: String,
    ) -> NotifyLayerBuilder<R> {
        NotifyLayerBuilder {
            slack,
            provider,
            environment,
            default_rule: None,
            flush_tick: DEFAULT_FLUSH_TICK,
            analytics_provider: None,
            posthog: None,
            enricher: None,
        }
    }

    /// Apply rule + side effects to one `(event_name, merged_fields)` pair.
    /// Splits the synchronous path from `on_event` so it's exercised by tests.
    fn dispatch(&self, event_name: &str, fields: &HashMap<String, String>) {
        let Some(rule) = self
            .inner
            .provider
            .route_for(event_name)
            .or_else(|| self.inner.default_rule.clone())
        else {
            return;
        };

        if !rule.enabled || !rule.allows_env(&self.inner.environment) {
            return;
        }

        if !self.sample_passes(rule.sample_rate) {
            return;
        }

        if let Some(dedup) = &rule.dedup {
            if !self.dedup_passes(event_name, &dedup.keys, dedup.window, fields) {
                return;
            }
        }

        if let Some(batch) = &rule.batch {
            let line = format_line(event_name, fields);
            self.enqueue_batch(event_name, &rule.channel, batch.interval, line);
        } else {
            self.post_immediate(&rule.channel, event_name, fields);
        }
    }

    fn sample_passes(&self, sample_rate: f32) -> bool {
        if sample_rate >= 1.0 {
            return true;
        }
        if sample_rate <= 0.0 {
            return false;
        }
        // Deterministic-rotating quasi-random: lift a u32 fraction from a
        // counter mixed with the high bits of the wall clock so multiple
        // emit threads don't all land on the same modulo bucket.
        let raw = self.inner.sample_counter.fetch_add(1, Ordering::Relaxed);
        let mixed = raw.wrapping_mul(0x9E37_79B9_7F4A_7C15);
        let bucket = (mixed >> 32) as f32 / (u32::MAX as f32);
        bucket < sample_rate
    }

    fn dedup_passes(
        &self,
        event_name: &str,
        keys: &[String],
        window: Duration,
        fields: &HashMap<String, String>,
    ) -> bool {
        let key = build_dedup_key(event_name, keys, fields);
        let now = Instant::now();
        let Ok(mut guard) = self.inner.dedup.lock() else {
            // Poisoned mutex: don't fall over, just let the event through.
            return true;
        };
        match guard.get(&key).copied() {
            Some(last) if now.duration_since(last) < window => false,
            _ => {
                guard.insert(key, now);
                true
            }
        }
    }

    fn enqueue_batch(&self, event_name: &str, channel: &str, interval: Duration, line: String) {
        let Ok(mut guard) = self.inner.batches.lock() else {
            return;
        };
        let buffer = guard
            .by_event
            .entry(event_name.to_owned())
            .or_insert_with(|| BatchBuffer::new(channel.to_owned(), interval));
        buffer.lines.push(BatchedLine {
            queued_at: Instant::now(),
            text: line,
        });
    }

    fn post_immediate(&self, channel: &str, event_name: &str, fields: &HashMap<String, String>) {
        let blocks = event_blocks(event_name, fields);
        self.inner.slack.post_message(channel, &blocks);
    }

    /// Fan one notify event out to the `PostHog` analytics sink when both an
    /// [`AnalyticsProvider`] and a [`PostHogClient`] are configured.
    ///
    /// Independent of the Slack routing rules: analytics captures every
    /// catalogued event, so the sampling / dedup / env filtering that keeps
    /// Slack quiet never suppresses a capture. The provider returns `None` to
    /// skip an event — unknown, or consent withheld for a product event.
    fn capture_analytics(&self, event_name: &str, fields: &HashMap<String, String>) {
        let (Some(provider), Some(posthog)) = (&self.inner.analytics_provider, &self.inner.posthog)
        else {
            return;
        };
        if let Some(capture) = provider.capture_for(event_name, fields) {
            posthog.capture(&capture.distinct_id, event_name, capture.properties);
        }
    }
}

/// Builder for [`NotifyLayer`].
pub struct NotifyLayerBuilder<R: RoutingProvider> {
    slack: Arc<SlackClient>,
    provider: Arc<R>,
    environment: String,
    default_rule: Option<RoutingRule>,
    flush_tick: Duration,
    analytics_provider: Option<Arc<dyn AnalyticsProvider>>,
    posthog: Option<Arc<PostHogClient>>,
    enricher: Option<Arc<dyn NotifyEnricher>>,
}

impl<R: RoutingProvider> NotifyLayerBuilder<R> {
    /// Set the fallback rule applied when `RoutingProvider::route_for`
    /// returns `None`. Without this, unknown events are dropped silently.
    pub fn default_rule(mut self, rule: RoutingRule) -> Self {
        self.default_rule = Some(rule);
        self
    }

    /// Override the batch-flush wake-up cadence (default 5s). Individual
    /// rules' batch intervals are still honored — this only bounds tick
    /// granularity for the background flusher.
    pub fn flush_tick(mut self, tick: Duration) -> Self {
        self.flush_tick = tick;
        self
    }

    /// Attach a `PostHog` analytics sink. The `provider` resolves per-event
    /// capture decisions (tier / consent / `distinct_id`) the layer itself
    /// can't see; the `posthog` client is the transport. Without this, the
    /// layer routes to Slack only.
    pub fn with_analytics(
        mut self,
        provider: Arc<dyn AnalyticsProvider>,
        posthog: Arc<PostHogClient>,
    ) -> Self {
        self.analytics_provider = Some(provider);
        self.posthog = Some(posthog);
        self
    }

    /// Attach a [`NotifyEnricher`] that mutates every event's merged field map
    /// before it reaches Slack and `PostHog`. The host uses this to inject
    /// fields the call sites don't carry — e.g. a `user_email` resolved from an
    /// in-process cache, or a display `emoji`. Without this, events render with
    /// exactly the fields their call sites and enclosing spans provide.
    pub fn with_enricher(mut self, enricher: Arc<dyn NotifyEnricher>) -> Self {
        self.enricher = Some(enricher);
        self
    }

    /// Finalise the layer and spawn the background batch flusher.
    pub fn build(self) -> NotifyLayer<R> {
        let inner = Arc::new(LayerInner {
            slack: self.slack,
            analytics_provider: self.analytics_provider,
            posthog: self.posthog,
            enricher: self.enricher,
            provider: self.provider,
            environment: self.environment,
            default_rule: self.default_rule,
            dedup: Arc::new(Mutex::new(HashMap::new())),
            batches: Arc::new(Mutex::new(BatchBuffers::default())),
            sample_counter: AtomicU64::new(0),
        });
        spawn_batch_flusher(Arc::clone(&inner), self.flush_tick);
        NotifyLayer { inner }
    }
}

impl<S, R> Layer<S> for NotifyLayer<R>
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    R: RoutingProvider,
{
    /// Capture span fields declared by `#[instrument(fields(...))]` into the
    /// span's extensions so `on_event` can read them later.
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else { return };
        let mut visitor = NotifyVisitor::new();
        attrs.record(&mut visitor);

        let mut stash = SpanFields::new();
        stash.fields = visitor.fields;
        if let Some(name) = visitor.event_name {
            // Edge case: a span literally named the field `event` — preserve
            // it as a regular field so it doesn't get lost.
            stash.fields.insert("event".to_owned(), name);
        }
        span.extensions_mut().insert(stash);
    }

    /// Pick up late-bound span fields recorded via `Span::record`.
    fn on_record(&self, id: &Id, values: &Record<'_>, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else { return };
        let mut visitor = NotifyVisitor::new();
        values.record(&mut visitor);

        let mut extensions = span.extensions_mut();
        if let Some(existing) = extensions.get_mut::<SpanFields>() {
            existing.fields.extend(visitor.fields);
            if let Some(name) = visitor.event_name {
                existing.fields.insert("event".to_owned(), name);
            }
        } else {
            let mut fresh = SpanFields::new();
            fresh.fields = visitor.fields;
            if let Some(name) = visitor.event_name {
                fresh.fields.insert("event".to_owned(), name);
            }
            extensions.insert(fresh);
        }
    }

    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        if event.metadata().target() != NOTIFY_TARGET {
            return;
        }

        let mut visitor = NotifyVisitor::new();
        event.record(&mut visitor);
        let Some(event_name) = visitor.event_name else {
            // No `event = "..."` literal — silently ignore; the catalogue
            // typo test in dravr-platform polices call sites at CI time.
            return;
        };

        // Merge span fields (outer-most first) underneath event fields so
        // an event field with the same key wins. Span chain order: the
        // event's immediate parent ascends to the root, so we reverse to
        // apply outer-most first.
        let mut merged: HashMap<String, String> = HashMap::new();
        let scope: Vec<_> = ctx
            .event_scope(event)
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
        for span in scope.iter().rev() {
            let extensions = span.extensions();
            if let Some(stash) = extensions.get::<SpanFields>() {
                for (k, v) in &stash.fields {
                    merged.insert(k.clone(), v.clone());
                }
            }
        }
        for (k, v) in visitor.fields {
            merged.insert(k, v);
        }

        // Enrich once, before both sinks, so Slack and PostHog see the same
        // host-derived fields (e.g. a `user_email` resolved from cache).
        if let Some(enricher) = &self.inner.enricher {
            enricher.enrich(&event_name, &mut merged);
        }

        self.capture_analytics(&event_name, &merged);
        self.dispatch(&event_name, &merged);
    }
}

/// Field names that the visitor inherits from enclosing request spans
/// but operators don't want to see in Slack. axum/tower instruments the
/// HTTP request span with `method`/`uri`/`version` and the `OAuth2`
/// password grant adds `grant_type`/`username`; the `id` and `route`
/// keys come from route-handler `Path<_>` extractors and the project's
/// instrument convention. They're useful in Cloud Logging but pure
/// noise on Slack.
const FIELD_DENYLIST: &[&str] = &[
    "uri",
    "method",
    "version",
    "host",
    "grant_type",
    "username",
    "route",
    "id",
    "x-request-id",
];

/// Field keys consumed into the headline rather than rendered as `key=value`
/// pairs: the tracing `message` body and an optional leading `emoji` (set by a
/// [`NotifyEnricher`](super::provider::NotifyEnricher)).
const LIFTED_KEYS: &[&str] = &["message", "emoji"];

fn is_lifted(key: &str) -> bool {
    LIFTED_KEYS.contains(&key)
}

/// Whether a field is an opaque identifier (`*_id`) — `tenant_id`, `user_id`,
/// `conversation_id`, `turn_id`, `session_id`, … . The immediate-post renderer
/// demotes these to a muted Slack `context` block so the human-readable signal
/// (`user_email`, `provider`, `persona`, latency, …) leads the message.
fn is_id_field(key: &str) -> bool {
    key.ends_with("_id")
}

/// Field-key priority for the rendered line: lower number renders first.
/// `user_email` leads (identity is the most-scanned field), then event-specific
/// signal, then the `*_id` identifiers trail. Within the immediate-post layout
/// the identifiers move to a context block entirely; this ordering still
/// governs the batched-digest line and the order inside the context block.
fn field_priority(key: &str) -> u8 {
    if key == "user_email" {
        10
    } else if is_id_field(key) {
        90
    } else {
        50
    }
}

/// Render the headline: an optional leading `emoji`, the bold event name, and
/// the tracing `message` body when present.
fn headline(event_name: &str, fields: &HashMap<String, String>) -> String {
    let emoji = fields
        .get("emoji")
        .map(String::as_str)
        .filter(|e| !e.is_empty());
    let message = fields
        .get("message")
        .map(String::as_str)
        .filter(|m| !m.is_empty());
    match (emoji, message) {
        (Some(e), Some(m)) => format!("{e} *{event_name}* — {m}"),
        (Some(e), None) => format!("{e} *{event_name}*"),
        (None, Some(m)) => format!("*{event_name}* — {m}"),
        (None, None) => format!("*{event_name}*"),
    }
}

/// Pretty-print a `*_ms` field value as a humanised duration. Falls back
/// to the raw value when the field doesn't parse as a `u64` (e.g. a
/// negative i64 or a Debug-rendered struct).
fn format_value(key: &str, value: &str) -> String {
    if key.ends_with("_ms") {
        if let Ok(ms) = value.parse::<u64>() {
            return format_duration_ms(ms);
        }
    }
    value.to_owned()
}

fn format_duration_ms(ms: u64) -> String {
    if ms < 1000 {
        return format!("{ms}ms");
    }
    let secs = ms / 1000;
    let rem_ms = ms % 1000;
    if secs < 60 {
        if rem_ms == 0 {
            format!("{secs}s")
        } else {
            format!("{secs}.{rem_ms:03}s")
        }
    } else if secs < 3600 {
        let m = secs / 60;
        let s = secs % 60;
        if s == 0 {
            format!("{m}m")
        } else {
            format!("{m}m {s}s")
        }
    } else {
        let h = secs / 3600;
        let m = (secs % 3600) / 60;
        if m == 0 {
            format!("{h}h")
        } else {
            format!("{h}h {m}m")
        }
    }
}

/// Format one event as a single compact line for the batched digest:
///
/// ```text
/// <emoji> *event.name* — <tracing message body>
/// user_email=…, key1=value1, … tenant_id=…, user_id=…
/// ```
///
/// The `message`/`emoji` keys are lifted into the headline. Remaining fields
/// are denylist-filtered to drop HTTP / OAuth plumbing inherited from the
/// enclosing request span, then ordered by [`field_priority`] so `user_email`
/// leads, event-specific data follows, and `*_id` identifiers trail. `*_ms`
/// values are humanised via [`format_duration_ms`]. The immediate (non-batched)
/// path uses the richer two-block layout in [`event_blocks`] instead; this
/// stays a single line so a digest of N events reads compactly.
fn format_line(event_name: &str, fields: &HashMap<String, String>) -> String {
    let head = headline(event_name, fields);

    let mut filtered: Vec<(&String, &String)> = fields
        .iter()
        .filter(|(k, _)| !is_lifted(k) && !FIELD_DENYLIST.contains(&k.as_str()))
        .collect();

    filtered.sort_by_key(|(k, _)| (field_priority(k), k.as_str()));

    let pairs: Vec<String> = filtered
        .into_iter()
        .map(|(k, v)| format!("{k}={}", format_value(k, v)))
        .collect();

    if pairs.is_empty() {
        head
    } else {
        format!("{head}\n{}", pairs.join(", "))
    }
}

/// Render one event as Block Kit for an immediate (non-batched) Slack post:
///
/// - a `section` block — the headline plus the human-readable signal fields
///   (`user_email` first, then event-specific data), and
/// - a muted `context` block — the `*_id` identifiers (`tenant_id`, `user_id`,
///   `conversation_id`, …), kept in full for log correlation but visually
///   demoted out of the way.
///
/// The context block is omitted when the event carries no identifier fields.
fn event_blocks(event_name: &str, fields: &HashMap<String, String>) -> Value {
    let renderable = |k: &str| !is_lifted(k) && !FIELD_DENYLIST.contains(&k);

    let mut signal: Vec<(&String, &String)> = fields
        .iter()
        .filter(|(k, _)| renderable(k.as_str()) && !is_id_field(k.as_str()))
        .collect();
    signal.sort_by_key(|(k, _)| (field_priority(k), k.as_str()));
    let signal_pairs: Vec<String> = signal
        .into_iter()
        .map(|(k, v)| format!("{k}={}", format_value(k, v)))
        .collect();

    let section_text = if signal_pairs.is_empty() {
        headline(event_name, fields)
    } else {
        format!(
            "{}\n{}",
            headline(event_name, fields),
            signal_pairs.join(", ")
        )
    };

    let mut blocks = vec![serde_json::json!({
        "type": "section",
        "text": { "type": "mrkdwn", "text": section_text },
        "block_id": format!("notify-{event_name}"),
    })];

    let mut ids: Vec<(&String, &String)> = fields
        .iter()
        .filter(|(k, _)| renderable(k.as_str()) && is_id_field(k.as_str()))
        .collect();
    ids.sort_by_key(|(k, _)| (field_priority(k), k.as_str()));
    if !ids.is_empty() {
        let id_text = ids
            .into_iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("  ·  ");
        blocks.push(serde_json::json!({
            "type": "context",
            "elements": [ { "type": "mrkdwn", "text": id_text } ],
        }));
    }

    Value::Array(blocks)
}

/// Wrap a single rendered line into Block Kit shape understood by
/// `SlackClient::post_message`. Used by the batched-digest flush path; the
/// immediate path uses the richer [`event_blocks`] layout.
fn single_line_blocks(event_name: &str, line: &str) -> Value {
    serde_json::json!([{
        "type": "section",
        "text": { "type": "mrkdwn", "text": line },
        "block_id": format!("notify-{event_name}"),
    }])
}

/// Spawn the background flusher that drains batch buffers older than their
/// configured interval into a single Slack message per (event, channel).
fn spawn_batch_flusher<R: RoutingProvider>(inner: Arc<LayerInner<R>>, tick: Duration) {
    tokio::spawn(async move {
        let mut ticker = interval(tick);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            flush_ripe_batches(&inner);
        }
    });
}

fn flush_ripe_batches<R: RoutingProvider>(inner: &Arc<LayerInner<R>>) {
    let now = Instant::now();
    let mut to_post: Vec<(String, String, Vec<BatchedLine>)> = Vec::new();
    {
        let Ok(mut guard) = inner.batches.lock() else {
            return;
        };
        for (event_name, buffer) in &mut guard.by_event {
            if buffer.lines.is_empty() {
                continue;
            }
            let oldest = buffer.lines[0].queued_at;
            if now.duration_since(oldest) < buffer.interval {
                continue;
            }
            let drained = mem::take(&mut buffer.lines);
            to_post.push((event_name.clone(), buffer.channel.clone(), drained));
        }
    }

    for (event_name, channel, lines) in to_post {
        let combined = format_batch(&event_name, &lines);
        let blocks = single_line_blocks(&event_name, &combined);
        inner.slack.post_message(&channel, &blocks);
    }
}

fn format_batch(event_name: &str, lines: &[BatchedLine]) -> String {
    let count = lines.len();
    let head = format!("*{event_name}* — batched x{count}");
    let body: Vec<String> = lines
        .iter()
        .map(|line| format!("• {}", line.text))
        .collect();
    format!("{head}\n{}", body.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notifications::SlackConfig;
    use crate::notify::provider::{AnalyticsCapture, NotifyEnricher, StaticRoutingProvider};
    use tokio::time::sleep;
    use tracing::subscriber::with_default;
    use tracing_subscriber::layer::SubscriberExt as _;

    fn dummy_slack() -> Arc<SlackClient> {
        Arc::new(SlackClient::new(&SlackConfig {
            bot_token: "xoxb-test".into(),
            error_channel: "#errors".into(),
            signing_secret: None,
        }))
    }

    fn build_layer(
        provider: StaticRoutingProvider,
        env: &str,
    ) -> NotifyLayer<StaticRoutingProvider> {
        NotifyLayer::new(dummy_slack(), Arc::new(provider), env.to_owned())
    }

    /// Test `AnalyticsProvider` that counts `capture_for` calls and optionally
    /// returns a capture, so the layer's analytics fan-out can be asserted
    /// without a live `PostHog` endpoint.
    struct CountingAnalytics {
        calls: Arc<AtomicU64>,
        capture: bool,
    }

    impl AnalyticsProvider for CountingAnalytics {
        fn capture_for(
            &self,
            _event: &str,
            _fields: &HashMap<String, String>,
        ) -> Option<AnalyticsCapture> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            self.capture.then(|| AnalyticsCapture {
                distinct_id: "u_test".to_owned(),
                properties: serde_json::json!({ "channel": "web" }),
            })
        }
    }

    fn layer_with_analytics(
        provider: Arc<CountingAnalytics>,
    ) -> NotifyLayer<StaticRoutingProvider> {
        // Unroutable host: the fire-and-forget capture send fails harmlessly.
        let posthog = Arc::new(PostHogClient::with_host("phc_test", "http://127.0.0.1:1"));
        NotifyLayer::builder(
            dummy_slack(),
            Arc::new(StaticRoutingProvider::new()),
            "dev".into(),
        )
        .with_analytics(provider, posthog)
        .build()
    }

    #[tokio::test]
    async fn capture_analytics_is_noop_without_provider() {
        // Slack-only layer: no analytics configured → no-op, no panic.
        let layer = build_layer(StaticRoutingProvider::new(), "dev");
        layer.capture_analytics("user.login", &HashMap::new());
    }

    #[tokio::test]
    async fn capture_analytics_invokes_provider_when_configured() {
        let calls = Arc::new(AtomicU64::new(0));
        let layer = layer_with_analytics(Arc::new(CountingAnalytics {
            calls: Arc::clone(&calls),
            capture: true,
        }));
        layer.capture_analytics("user.login", &HashMap::new());
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn capture_analytics_skips_when_provider_returns_none() {
        // Provider declines (e.g. consent withheld): still consulted, but no
        // capture is forwarded — the path must not panic.
        let calls = Arc::new(AtomicU64::new(0));
        let layer = layer_with_analytics(Arc::new(CountingAnalytics {
            calls: Arc::clone(&calls),
            capture: false,
        }));
        layer.capture_analytics("user.login", &HashMap::new());
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn dispatch_drops_unknown_event_without_default() {
        let layer = build_layer(StaticRoutingProvider::new(), "dev");
        // Should be a no-op (no panic, no crash). We can't observe Slack
        // here — the point is the unknown path doesn't unwind.
        layer.dispatch("unknown.event", &HashMap::new());
    }

    #[tokio::test]
    async fn dispatch_honors_default_rule_for_unknown_event() {
        let layer = NotifyLayer::builder(
            dummy_slack(),
            Arc::new(StaticRoutingProvider::new()),
            "dev".into(),
        )
        .default_rule(RoutingRule::to_channel("#fallback"))
        .build();
        // Path is exercised; Slack client is fire-and-forget so we just
        // assert no panic. Behavioural integration is covered by the
        // sample/dedup/env unit tests below.
        layer.dispatch("never.registered", &HashMap::new());
    }

    #[tokio::test]
    async fn env_filter_blocks_disallowed_env() {
        let mut rule = RoutingRule::to_channel("#dev-only");
        rule.enabled_envs = Some(vec!["dev".into()]);
        let layer = build_layer(
            StaticRoutingProvider::new().with_rule("x", rule),
            "production",
        );
        layer.dispatch("x", &HashMap::new());
        // Allowed in dev:
        let mut rule = RoutingRule::to_channel("#dev-only");
        rule.enabled_envs = Some(vec!["dev".into()]);
        let layer = build_layer(StaticRoutingProvider::new().with_rule("x", rule), "dev");
        layer.dispatch("x", &HashMap::new());
    }

    #[tokio::test]
    async fn sample_zero_drops_every_event() {
        let layer = build_layer(StaticRoutingProvider::new(), "dev");
        // sample_passes is internal; test through dispatch by routing to a
        // sampled rule and asserting we don't loop forever.
        let mut rule = RoutingRule::to_channel("#x");
        rule.sample_rate = 0.0;
        let layer_with_rule = build_layer(StaticRoutingProvider::new().with_rule("e", rule), "dev");
        for _ in 0..100 {
            layer_with_rule.dispatch("e", &HashMap::new());
        }
        // Counter wasn't incremented because sample_passes short-circuits
        // on 0.0 before touching the counter.
        assert_eq!(
            layer_with_rule.inner.sample_counter.load(Ordering::Relaxed),
            0
        );
        drop(layer);
    }

    #[tokio::test]
    async fn sample_one_lets_every_event_through() {
        let layer = build_layer(
            StaticRoutingProvider::new().with_rule("e", RoutingRule::to_channel("#x")),
            "dev",
        );
        for _ in 0..10 {
            assert!(layer.sample_passes(1.0));
        }
    }

    #[tokio::test]
    async fn sample_half_passes_roughly_half() {
        let layer = build_layer(StaticRoutingProvider::new(), "dev");
        let mut hits = 0u32;
        for _ in 0..2000 {
            if layer.sample_passes(0.5) {
                hits += 1;
            }
        }
        // Wide tolerance — we only care this isn't trivially-stuck.
        assert!(
            (500..=1500).contains(&hits),
            "expected ~1000 hits, got {hits}"
        );
    }

    #[tokio::test]
    async fn dedup_window_drops_repeat_inside_window() {
        let mut rule = RoutingRule::to_channel("#x");
        rule.dedup = Some(super::super::rule::DedupRule {
            keys: vec!["user_id".to_owned()],
            window: Duration::from_mins(1),
        });
        let layer = build_layer(StaticRoutingProvider::new().with_rule("e", rule), "dev");

        let mut fields = HashMap::new();
        fields.insert("user_id".to_owned(), "u1".to_owned());

        assert!(layer.dedup_passes(
            "e",
            &["user_id".to_owned()],
            Duration::from_mins(1),
            &fields
        ));
        // Second call inside the window is suppressed.
        assert!(!layer.dedup_passes(
            "e",
            &["user_id".to_owned()],
            Duration::from_mins(1),
            &fields
        ));
    }

    #[tokio::test]
    async fn dedup_window_admits_different_key_values() {
        let layer = build_layer(StaticRoutingProvider::new(), "dev");

        let mut fields_a = HashMap::new();
        fields_a.insert("user_id".to_owned(), "u1".to_owned());
        let mut fields_b = HashMap::new();
        fields_b.insert("user_id".to_owned(), "u2".to_owned());

        assert!(layer.dedup_passes(
            "e",
            &["user_id".to_owned()],
            Duration::from_mins(1),
            &fields_a
        ));
        assert!(layer.dedup_passes(
            "e",
            &["user_id".to_owned()],
            Duration::from_mins(1),
            &fields_b
        ));
    }

    #[test]
    fn format_line_sorts_event_specific_fields_alphabetically() {
        let mut fields = HashMap::new();
        fields.insert("zeta".to_owned(), "z".to_owned());
        fields.insert("alpha".to_owned(), "a".to_owned());
        let line = format_line("evt", &fields);
        assert_eq!(line, "*evt*\nalpha=a, zeta=z");
    }

    #[test]
    fn format_line_with_no_fields_renders_event_only() {
        let line = format_line("evt", &HashMap::new());
        assert_eq!(line, "*evt*");
    }

    #[test]
    fn format_line_lifts_message_to_headline() {
        let mut fields = HashMap::new();
        fields.insert("message".to_owned(), "user authenticated".to_owned());
        fields.insert("user_id".to_owned(), "abc".to_owned());
        let line = format_line("user.login", &fields);
        assert_eq!(line, "*user.login* — user authenticated\nuser_id=abc");
    }

    #[test]
    fn format_line_drops_http_plumbing_fields() {
        let mut fields = HashMap::new();
        fields.insert("uri".to_owned(), "/oauth/token".to_owned());
        fields.insert("method".to_owned(), "POST".to_owned());
        fields.insert("version".to_owned(), "HTTP/1.1".to_owned());
        fields.insert("grant_type".to_owned(), "password".to_owned());
        fields.insert("username".to_owned(), "x@y".to_owned());
        fields.insert("route".to_owned(), "oauth2_token".to_owned());
        fields.insert("id".to_owned(), "some-uuid".to_owned());
        fields.insert("user_id".to_owned(), "u1".to_owned());
        let line = format_line("user.login", &fields);
        assert_eq!(line, "*user.login*\nuser_id=u1");
    }

    #[test]
    fn format_line_buries_identity_fields_after_event_specific() {
        let mut fields = HashMap::new();
        fields.insert("coach_slug".to_owned(), "marathon".to_owned());
        fields.insert("user_id".to_owned(), "u1".to_owned());
        fields.insert("tenant_id".to_owned(), "t1".to_owned());
        let line = format_line("coach.selected", &fields);
        assert_eq!(
            line,
            "*coach.selected*\ncoach_slug=marathon, tenant_id=t1, user_id=u1"
        );
    }

    #[test]
    fn format_line_humanises_latency_ms() {
        let mut fields = HashMap::new();
        fields.insert("latency_ms".to_owned(), "120011".to_owned());
        fields.insert("model".to_owned(), "claude-opus".to_owned());
        let line = format_line("embacle.call_completed", &fields);
        assert_eq!(
            line,
            "*embacle.call_completed*\nlatency_ms=2m, model=claude-opus"
        );
    }

    #[test]
    fn format_line_passes_through_unparseable_ms_value() {
        let mut fields = HashMap::new();
        fields.insert("latency_ms".to_owned(), "not-a-number".to_owned());
        let line = format_line("evt", &fields);
        assert_eq!(line, "*evt*\nlatency_ms=not-a-number");
    }

    #[test]
    fn format_line_leads_with_user_email_then_trails_ids() {
        let mut fields = HashMap::new();
        fields.insert("user_id".to_owned(), "u1".to_owned());
        fields.insert("tenant_id".to_owned(), "t1".to_owned());
        fields.insert("provider".to_owned(), "strava".to_owned());
        fields.insert("user_email".to_owned(), "jane@acme.com".to_owned());
        let line = format_line("provider.fetch_started", &fields);
        // Identity leads, event signal next, *_id identifiers trail.
        assert_eq!(
            line,
            "*provider.fetch_started*\nuser_email=jane@acme.com, provider=strava, tenant_id=t1, user_id=u1"
        );
    }

    #[test]
    fn format_line_prepends_emoji_to_headline() {
        let mut fields = HashMap::new();
        fields.insert("message".to_owned(), "user authenticated".to_owned());
        fields.insert("emoji".to_owned(), "🔑".to_owned());
        fields.insert("user_id".to_owned(), "u1".to_owned());
        let line = format_line("user.login", &fields);
        // `emoji` is consumed into the headline, never shown as a key=value pair.
        assert_eq!(line, "🔑 *user.login* — user authenticated\nuser_id=u1");
    }

    #[test]
    fn event_blocks_splits_signal_section_from_muted_id_context() {
        let mut fields = HashMap::new();
        fields.insert("message".to_owned(), "user asked a question".to_owned());
        fields.insert("emoji".to_owned(), "💬".to_owned());
        fields.insert("persona".to_owned(), "casual".to_owned());
        fields.insert("user_email".to_owned(), "jane@acme.com".to_owned());
        fields.insert("user_id".to_owned(), "u1".to_owned());
        fields.insert("tenant_id".to_owned(), "t1".to_owned());
        fields.insert("conversation_id".to_owned(), "c1".to_owned());

        let blocks = event_blocks("chat.question_asked", &fields);
        let arr = blocks.as_array().expect("blocks is an array");
        assert_eq!(arr.len(), 2, "expected a section + a context block");

        let section = arr[0]["text"]["text"].as_str().expect("section text");
        assert_eq!(
            section,
            "💬 *chat.question_asked* — user asked a question\nuser_email=jane@acme.com, persona=casual"
        );
        assert_eq!(arr[0]["type"], "section");

        // Identifiers are demoted to the muted context block, kept in full.
        assert_eq!(arr[1]["type"], "context");
        let ctx = arr[1]["elements"][0]["text"]
            .as_str()
            .expect("context text");
        assert_eq!(ctx, "conversation_id=c1  ·  tenant_id=t1  ·  user_id=u1");
    }

    #[test]
    fn event_blocks_omits_context_when_no_id_fields() {
        let mut fields = HashMap::new();
        fields.insert("message".to_owned(), "circuit opened".to_owned());
        fields.insert("provider".to_owned(), "cohere".to_owned());
        let blocks = event_blocks("llm.circuit_opened", &fields);
        let arr = blocks.as_array().expect("blocks is an array");
        assert_eq!(arr.len(), 1, "no *_id fields → no context block");
        assert_eq!(arr[0]["type"], "section");
    }

    #[test]
    fn event_blocks_humanises_latency_in_section() {
        let mut fields = HashMap::new();
        fields.insert("latency_ms".to_owned(), "120011".to_owned());
        fields.insert("model".to_owned(), "claude-opus".to_owned());
        let blocks = event_blocks("embacle.call_completed", &fields);
        let arr = blocks.as_array().expect("blocks is an array");
        let section = arr[0]["text"]["text"].as_str().expect("section text");
        assert_eq!(
            section,
            "*embacle.call_completed*\nlatency_ms=2m, model=claude-opus"
        );
    }

    /// Shared log of `(event, fields)` the enricher observed, for assertions.
    type SeenLog = Arc<Mutex<Vec<(String, HashMap<String, String>)>>>;

    /// Records every `(event, fields)` it is handed and injects a `user_email`,
    /// so a subscriber-driven test can assert the layer runs the enricher on the
    /// merged span+event field map before the sinks see it.
    struct RecordingEnricher {
        seen: SeenLog,
    }

    impl NotifyEnricher for RecordingEnricher {
        fn enrich(&self, event: &str, fields: &mut HashMap<String, String>) {
            if let Ok(mut guard) = self.seen.lock() {
                guard.push((event.to_owned(), fields.clone()));
            }
            fields.insert("user_email".to_owned(), "jane@acme.com".to_owned());
        }
    }

    #[tokio::test]
    async fn enricher_runs_on_merged_fields_before_dispatch() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let layer = NotifyLayer::builder(
            dummy_slack(),
            Arc::new(
                StaticRoutingProvider::new().with_rule("user.login", RoutingRule::to_channel("#x")),
            ),
            "dev".into(),
        )
        .with_enricher(Arc::new(RecordingEnricher {
            seen: Arc::clone(&seen),
        }))
        .build();

        let subscriber = tracing_subscriber::registry().with(layer);
        with_default(subscriber, || {
            let span = tracing::info_span!("req", user_id = "u1", tenant_id = "t1");
            span.in_scope(|| {
                tracing::info!(target: "notify", event = "user.login", "user authenticated");
            });
        });

        let guard = seen.lock().expect("mutex not poisoned"); // Safe: test assertion
        assert_eq!(guard.len(), 1, "enricher invoked exactly once");
        let (event, fields) = &guard[0];
        assert_eq!(event, "user.login");
        // Span fields are merged in before the enricher runs.
        assert_eq!(fields.get("user_id").map(String::as_str), Some("u1"));
        assert_eq!(fields.get("tenant_id").map(String::as_str), Some("t1"));
        // The enricher had not yet injected its own field when recording.
        assert!(!fields.contains_key("user_email"));
    }

    #[test]
    fn format_duration_ms_covers_each_bucket() {
        assert_eq!(format_duration_ms(0), "0ms");
        assert_eq!(format_duration_ms(999), "999ms");
        assert_eq!(format_duration_ms(1_000), "1s");
        assert_eq!(format_duration_ms(1_500), "1.500s");
        assert_eq!(format_duration_ms(60_000), "1m");
        assert_eq!(format_duration_ms(90_000), "1m 30s");
        assert_eq!(format_duration_ms(3_600_000), "1h");
        assert_eq!(format_duration_ms(3_661_000), "1h 1m");
    }

    #[test]
    fn format_batch_summarises_count() {
        let lines = vec![
            BatchedLine {
                queued_at: Instant::now(),
                text: "*evt* — k=1".into(),
            },
            BatchedLine {
                queued_at: Instant::now(),
                text: "*evt* — k=2".into(),
            },
        ];
        let out = format_batch("evt", &lines);
        assert!(out.starts_with("*evt* — batched x2"));
        assert!(out.contains("k=1"));
        assert!(out.contains("k=2"));
    }

    #[tokio::test]
    async fn enqueue_batch_accumulates_lines() {
        let mut rule = RoutingRule::to_channel("#x");
        rule.batch = Some(super::super::rule::BatchRule {
            interval: Duration::from_mins(1),
        });
        let layer = build_layer(StaticRoutingProvider::new().with_rule("e", rule), "dev");
        layer.dispatch("e", &HashMap::new());
        layer.dispatch("e", &HashMap::new());
        layer.dispatch("e", &HashMap::new());

        let guard = layer.inner.batches.lock().expect("mutex not poisoned"); // Safe: test assertion
        let buffer = guard.by_event.get("e").expect("buffer present"); // Safe: test assertion
        assert_eq!(buffer.lines.len(), 3);
        assert_eq!(buffer.channel, "#x");
        assert_eq!(buffer.interval, Duration::from_mins(1));
    }

    #[tokio::test]
    async fn flush_ripe_batches_drains_only_old_buffers() {
        let mut rule = RoutingRule::to_channel("#x");
        rule.batch = Some(super::super::rule::BatchRule {
            interval: Duration::from_millis(10),
        });
        let layer = build_layer(StaticRoutingProvider::new().with_rule("e", rule), "dev");
        layer.dispatch("e", &HashMap::new());
        // Not ripe yet:
        flush_ripe_batches(&layer.inner);
        {
            let guard = layer.inner.batches.lock().expect("mutex not poisoned"); // Safe: test assertion
            assert_eq!(guard.by_event.get("e").map(|b| b.lines.len()), Some(1));
        }
        sleep(Duration::from_millis(25)).await;
        flush_ripe_batches(&layer.inner);
        {
            let guard = layer.inner.batches.lock().expect("mutex not poisoned"); // Safe: test assertion
            assert_eq!(guard.by_event.get("e").map(|b| b.lines.len()), Some(0));
        }
    }
}
