//! Observability module — Prometheus metrics registry, `/metrics` endpoint, and OTLP tracing.
//!
//! Exposes gateway metrics in Prometheus text exposition format via `GET /metrics`.
//! Returns HTTP 503 if the metrics subsystem fails to encode.
//!
//! Also provides `init_tracing` to set up the full tracing stack with optional
//! OpenTelemetry OTLP exporter for distributed tracing.

use axum::http::StatusCode;
use axum::response::IntoResponse;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::trace::TracerProvider;
use prometheus::{
    Encoder, Histogram, HistogramOpts, HistogramVec, IntCounter, IntCounterVec, IntGaugeVec, Opts,
    Registry, TextEncoder,
};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

use crate::config::OtelConfig;

/// Errors that can occur during tracing initialization.
#[derive(Debug, thiserror::Error)]
pub enum TracingError {
    #[error("Failed to create OTLP exporter: {0}")]
    ExporterError(String),
    #[error("Failed to install global tracing subscriber: {0}")]
    SubscriberError(String),
}

/// Initializes the full tracing stack.
///
/// - If `config.enabled` is `true`: sets up `tracing-opentelemetry` with an OTLP gRPC
///   exporter pointing to `config.otlp_endpoint`, layered on top of `tracing_subscriber`
///   with `EnvFilter` and console fmt output.
/// - If `config.enabled` is `false`: uses only the fmt subscriber with `EnvFilter`
///   (console logging).
///
/// This function should be called once at startup before any spans are created.
pub fn init_tracing(config: &OtelConfig) -> Result<(), TracingError> {
    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info"));

    if config.enabled {
        // Build OTLP exporter using gRPC (tonic) pointing to the configured endpoint
        let exporter = opentelemetry_otlp::SpanExporter::builder()
            .with_tonic()
            .with_endpoint(&config.otlp_endpoint)
            .build()
            .map_err(|e| TracingError::ExporterError(e.to_string()))?;

        // Create a TracerProvider with the OTLP batch exporter (Tokio runtime)
        let provider = TracerProvider::builder()
            .with_batch_exporter(exporter, opentelemetry_sdk::runtime::Tokio)
            .build();

        let tracer = provider.tracer(config.service_name.clone());

        // Create the OpenTelemetry tracing layer
        let otel_layer = tracing_opentelemetry::layer().with_tracer(tracer);

        // Compose: EnvFilter + fmt (console) + OpenTelemetry layer
        tracing_subscriber::registry()
            .with(env_filter)
            .with(tracing_subscriber::fmt::layer())
            .with(otel_layer)
            .try_init()
            .map_err(|e| TracingError::SubscriberError(e.to_string()))?;

        tracing::info!(
            otlp_endpoint = %config.otlp_endpoint,
            service_name = %config.service_name,
            "OpenTelemetry tracing initialized with OTLP exporter"
        );
    } else {
        // OTel disabled: just console logging with EnvFilter
        tracing_subscriber::registry()
            .with(env_filter)
            .with(tracing_subscriber::fmt::layer())
            .try_init()
            .map_err(|e| TracingError::SubscriberError(e.to_string()))?;

        tracing::info!("Tracing initialized (console only, OpenTelemetry disabled)");
    }

    Ok(())
}

/// Holds all Prometheus metric instances for the gateway.
#[derive(Clone)]
pub struct Metrics {
    /// Total gateway requests, labeled by route, client, and HTTP status.
    pub requests_total: IntCounterVec,
    /// Total LLM tokens processed, labeled by direction (input/output), model, and client_id.
    pub llm_tokens_total: IntCounterVec,
    /// Histogram of context compression ratios.
    pub compression_ratio: Histogram,
    /// Histogram of backend (LLM provider) latency in seconds.
    pub backend_latency: HistogramVec,
    /// Histogram of gateway internal overhead in seconds.
    pub gateway_overhead: Histogram,
    /// Histogram of total request duration, labeled by route and provider.
    pub request_duration: HistogramVec,
    /// Histogram of gateway internal overhead (total - backend) in seconds.
    pub internal_overhead: Histogram,
    /// Histogram of payload translation duration in seconds.
    pub payload_translation: Histogram,
    /// Histogram of compaction duration in seconds.
    pub compaction_duration: Histogram,
    /// Counter of failover events, labeled by provider and reason.
    pub failover_total: IntCounterVec,
    /// Gauge of circuit breaker state per provider (0=closed, 1=open, 2=half-open).
    pub circuit_breaker_state: IntGaugeVec,
    /// Counter of provider errors, labeled by provider and status_code.
    pub provider_errors_total: IntCounterVec,
    /// Total number of compaction operations applied.
    pub compaction_applied_total: IntCounter,
    /// Total number of compaction operations skipped, labeled by reason.
    pub compaction_skipped_total: IntCounterVec,
    /// Total original tokens seen before compaction.
    pub context_original_tokens: IntCounter,
    /// Total final tokens after compaction.
    pub context_final_tokens: IntCounter,
    /// Total tokens saved by compaction.
    pub context_saved_tokens_total: IntCounter,
    /// Counter of model substitution events, labeled by requested_model, resolved_model, reason.
    pub model_substitution_total: IntCounterVec,
    /// Counter of fallback mode activations, labeled by original_provider, fallback_provider, reason.
    pub fallback_mode_total: IntCounterVec,
    /// The Prometheus registry holding all metrics.
    pub registry: Registry,
}

impl Metrics {
    /// Creates and registers all gateway metrics with a new Prometheus registry.
    ///
    /// # Panics
    /// Panics if metric registration fails (indicates a programming error such as
    /// duplicate metric names).
    pub fn new() -> Self {
        let registry = Registry::new();

        let requests_total = IntCounterVec::new(
            Opts::new(
                "melis_gateway_requests_total",
                "Total number of requests handled by the gateway",
            ),
            &["route", "client", "status"],
        )
        .expect("failed to create melis_gateway_requests_total metric");

        let llm_tokens_total = IntCounterVec::new(
            Opts::new(
                "melis_llm_tokens_total",
                "Total LLM tokens processed",
            ),
            &["direction", "model", "client_id"],
        )
        .expect("failed to create melis_llm_tokens_total metric");

        let compression_ratio = Histogram::with_opts(HistogramOpts::new(
            "melis_context_compression_ratio",
            "Context compression ratio (final_tokens / original_tokens)",
        ))
        .expect("failed to create melis_context_compression_ratio metric");

        let backend_latency_buckets = vec![0.01, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0];
        let backend_latency = HistogramVec::new(
            HistogramOpts::new(
                "melis_backend_latency_seconds",
                "Backend (LLM provider) request latency in seconds",
            )
            .buckets(backend_latency_buckets),
            &["provider"],
        )
        .expect("failed to create melis_backend_latency_seconds metric");

        let gateway_overhead_buckets = vec![0.0005, 0.001, 0.002, 0.005, 0.01, 0.05];
        let gateway_overhead = Histogram::with_opts(
            HistogramOpts::new(
                "melis_gateway_overhead_seconds",
                "Gateway internal processing overhead in seconds",
            )
            .buckets(gateway_overhead_buckets.clone()),
        )
        .expect("failed to create melis_gateway_overhead_seconds metric");

        let request_duration_buckets = vec![0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0];
        let request_duration = HistogramVec::new(
            HistogramOpts::new(
                "melis_request_duration_seconds",
                "Total request duration in seconds",
            )
            .buckets(request_duration_buckets),
            &["route", "provider"],
        )
        .expect("failed to create melis_request_duration_seconds metric");

        let internal_overhead = Histogram::with_opts(
            HistogramOpts::new(
                "melis_gateway_internal_overhead_seconds",
                "Gateway internal overhead (total - backend) in seconds",
            )
            .buckets(gateway_overhead_buckets),
        )
        .expect("failed to create melis_gateway_internal_overhead_seconds metric");

        let payload_translation_buckets = vec![0.0001, 0.0005, 0.001, 0.005, 0.01];
        let payload_translation = Histogram::with_opts(
            HistogramOpts::new(
                "melis_payload_translation_seconds",
                "Payload translation duration in seconds",
            )
            .buckets(payload_translation_buckets),
        )
        .expect("failed to create melis_payload_translation_seconds metric");

        let compaction_duration_buckets = vec![0.0001, 0.0005, 0.001, 0.005, 0.01, 0.05];
        let compaction_duration = Histogram::with_opts(
            HistogramOpts::new(
                "melis_compaction_duration_seconds",
                "Context compaction duration in seconds",
            )
            .buckets(compaction_duration_buckets),
        )
        .expect("failed to create melis_compaction_duration_seconds metric");

        let failover_total = IntCounterVec::new(
            Opts::new(
                "melis_failover_total",
                "Total number of failover events",
            ),
            &["provider", "reason"],
        )
        .expect("failed to create melis_failover_total metric");

        let circuit_breaker_state = IntGaugeVec::new(
            Opts::new(
                "melis_circuit_breaker_state",
                "Circuit breaker state per provider (0=closed, 1=open, 2=half-open)",
            ),
            &["provider"],
        )
        .expect("failed to create melis_circuit_breaker_state metric");

        let provider_errors_total = IntCounterVec::new(
            Opts::new(
                "melis_provider_errors_total",
                "Total provider errors by provider and status code",
            ),
            &["provider", "status_code"],
        )
        .expect("failed to create melis_provider_errors_total metric");

        let compaction_applied_total = IntCounter::with_opts(Opts::new(
            "melis_compaction_applied_total",
            "Total number of compaction operations applied",
        ))
        .expect("failed to create melis_compaction_applied_total metric");

        let compaction_skipped_total = IntCounterVec::new(
            Opts::new(
                "melis_compaction_skipped_total",
                "Total number of compaction operations skipped",
            ),
            &["reason"],
        )
        .expect("failed to create melis_compaction_skipped_total metric");

        let context_original_tokens = IntCounter::with_opts(Opts::new(
            "melis_context_original_tokens",
            "Total original tokens seen before compaction",
        ))
        .expect("failed to create melis_context_original_tokens metric");

        let context_final_tokens = IntCounter::with_opts(Opts::new(
            "melis_context_final_tokens",
            "Total final tokens after compaction",
        ))
        .expect("failed to create melis_context_final_tokens metric");

        let context_saved_tokens_total = IntCounter::with_opts(Opts::new(
            "melis_context_saved_tokens_total",
            "Total tokens saved by compaction",
        ))
        .expect("failed to create melis_context_saved_tokens_total metric");

        let model_substitution_total = IntCounterVec::new(
            Opts::new(
                "melis_model_substitution_total",
                "Total model substitution events",
            ),
            &["requested_model", "resolved_model", "reason"],
        )
        .expect("failed to create melis_model_substitution_total metric");

        let fallback_mode_total = IntCounterVec::new(
            Opts::new(
                "melis_fallback_mode_total",
                "Total fallback mode activations",
            ),
            &["original_provider", "fallback_provider", "reason"],
        )
        .expect("failed to create melis_fallback_mode_total metric");

        registry
            .register(Box::new(requests_total.clone()))
            .expect("failed to register melis_gateway_requests_total");
        registry
            .register(Box::new(llm_tokens_total.clone()))
            .expect("failed to register melis_llm_tokens_total");
        registry
            .register(Box::new(compression_ratio.clone()))
            .expect("failed to register melis_context_compression_ratio");
        registry
            .register(Box::new(backend_latency.clone()))
            .expect("failed to register melis_backend_latency_seconds");
        registry
            .register(Box::new(gateway_overhead.clone()))
            .expect("failed to register melis_gateway_overhead_seconds");
        registry
            .register(Box::new(request_duration.clone()))
            .expect("failed to register melis_request_duration_seconds");
        registry
            .register(Box::new(internal_overhead.clone()))
            .expect("failed to register melis_gateway_internal_overhead_seconds");
        registry
            .register(Box::new(payload_translation.clone()))
            .expect("failed to register melis_payload_translation_seconds");
        registry
            .register(Box::new(compaction_duration.clone()))
            .expect("failed to register melis_compaction_duration_seconds");
        registry
            .register(Box::new(failover_total.clone()))
            .expect("failed to register melis_failover_total");
        registry
            .register(Box::new(circuit_breaker_state.clone()))
            .expect("failed to register melis_circuit_breaker_state");
        registry
            .register(Box::new(provider_errors_total.clone()))
            .expect("failed to register melis_provider_errors_total");
        registry
            .register(Box::new(compaction_applied_total.clone()))
            .expect("failed to register melis_compaction_applied_total");
        registry
            .register(Box::new(compaction_skipped_total.clone()))
            .expect("failed to register melis_compaction_skipped_total");
        registry
            .register(Box::new(context_original_tokens.clone()))
            .expect("failed to register melis_context_original_tokens");
        registry
            .register(Box::new(context_final_tokens.clone()))
            .expect("failed to register melis_context_final_tokens");
        registry
            .register(Box::new(context_saved_tokens_total.clone()))
            .expect("failed to register melis_context_saved_tokens_total");
        registry
            .register(Box::new(model_substitution_total.clone()))
            .expect("failed to register melis_model_substitution_total");
        registry
            .register(Box::new(fallback_mode_total.clone()))
            .expect("failed to register melis_fallback_mode_total");

        // Initialize metrics with known labels so they appear in /metrics even before first use
        provider_errors_total.with_label_values(&["ollama", "none"]).inc_by(0);
        provider_errors_total.with_label_values(&["openai", "none"]).inc_by(0);
        failover_total.with_label_values(&["none", "none"]).inc_by(0);
        circuit_breaker_state.with_label_values(&["ollama"]).set(0);
        compaction_skipped_total.with_label_values(&["below_threshold"]).inc_by(0);
        model_substitution_total.with_label_values(&["none", "none", "none"]).inc_by(0);
        fallback_mode_total.with_label_values(&["none", "none", "none"]).inc_by(0);

        Self {
            requests_total,
            llm_tokens_total,
            compression_ratio,
            backend_latency,
            gateway_overhead,
            request_duration,
            internal_overhead,
            payload_translation,
            compaction_duration,
            failover_total,
            circuit_breaker_state,
            provider_errors_total,
            compaction_applied_total,
            compaction_skipped_total,
            context_original_tokens,
            context_final_tokens,
            context_saved_tokens_total,
            model_substitution_total,
            fallback_mode_total,
            registry,
        }
    }
}

impl Default for Metrics {
    fn default() -> Self {
        Self::new()
    }
}

/// Axum handler for `GET /metrics`.
///
/// Encodes all registered Prometheus metrics in text exposition format.
/// Returns HTTP 503 Service Unavailable if the encoding fails.
pub async fn metrics_handler(
    state: axum::extract::State<crate::state::AppState>,
) -> impl IntoResponse {
    let encoder = TextEncoder::new();
    let metric_families = state.metrics.registry.gather();

    let mut buffer = Vec::new();
    match encoder.encode(&metric_families, &mut buffer) {
        Ok(()) => (
            StatusCode::OK,
            [(
                axum::http::header::CONTENT_TYPE,
                "text/plain; version=0.0.4; charset=utf-8",
            )],
            buffer,
        )
            .into_response(),
        Err(e) => {
            tracing::error!(error = %e, "Failed to encode Prometheus metrics");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                format!("Metrics subsystem error: {}", e),
            )
                .into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_metrics_creation_succeeds() {
        let metrics = Metrics::new();
        // Verify all metrics are registered by gathering them
        let families = metrics.registry.gather();
        assert!(!families.is_empty(), "Expected registered metric families");
    }

    #[test]
    fn test_increment_requests_total() {
        let metrics = Metrics::new();
        metrics
            .requests_total
            .with_label_values(&["/v1/chat/completions", "client-1", "200"])
            .inc();
        metrics
            .requests_total
            .with_label_values(&["/v1/chat/completions", "client-1", "200"])
            .inc();

        let val = metrics
            .requests_total
            .with_label_values(&["/v1/chat/completions", "client-1", "200"])
            .get();
        assert_eq!(val, 2);
    }

    #[test]
    fn test_increment_llm_tokens_total() {
        let metrics = Metrics::new();
        metrics
            .llm_tokens_total
            .with_label_values(&["input", "gpt-4o", "client-abc"])
            .inc_by(150);

        let val = metrics
            .llm_tokens_total
            .with_label_values(&["input", "gpt-4o", "client-abc"])
            .get();
        assert_eq!(val, 150);
    }

    #[test]
    fn test_observe_compression_ratio() {
        let metrics = Metrics::new();
        metrics.compression_ratio.observe(0.75);
        metrics.compression_ratio.observe(0.60);

        let count = metrics.compression_ratio.get_sample_count();
        assert_eq!(count, 2);
    }

    #[test]
    fn test_observe_backend_latency() {
        let metrics = Metrics::new();
        metrics
            .backend_latency
            .with_label_values(&["openai"])
            .observe(0.123);

        let count = metrics
            .backend_latency
            .with_label_values(&["openai"])
            .get_sample_count();
        assert_eq!(count, 1);
    }

    #[test]
    fn test_observe_gateway_overhead() {
        let metrics = Metrics::new();
        metrics.gateway_overhead.observe(0.001);

        let count = metrics.gateway_overhead.get_sample_count();
        assert_eq!(count, 1);
    }

    #[test]
    fn test_metrics_encoding_produces_valid_output() {
        let metrics = Metrics::new();
        // Record some data
        metrics
            .requests_total
            .with_label_values(&["/v1/chat/completions", "test-client", "200"])
            .inc();
        metrics.compression_ratio.observe(0.8);
        metrics.gateway_overhead.observe(0.002);

        let encoder = TextEncoder::new();
        let metric_families = metrics.registry.gather();
        let mut buffer = Vec::new();
        let result = encoder.encode(&metric_families, &mut buffer);

        assert!(result.is_ok(), "Encoding should succeed");
        let output = String::from_utf8(buffer).expect("Output should be valid UTF-8");

        // Verify Prometheus text format markers
        assert!(
            output.contains("melis_gateway_requests_total"),
            "Output should contain requests_total metric"
        );
        assert!(
            output.contains("melis_context_compression_ratio"),
            "Output should contain compression_ratio metric"
        );
        assert!(
            output.contains("melis_gateway_overhead_seconds"),
            "Output should contain gateway_overhead metric"
        );
        assert!(
            output.contains("# HELP"),
            "Output should contain HELP comments"
        );
        assert!(
            output.contains("# TYPE"),
            "Output should contain TYPE annotations"
        );
    }
}
