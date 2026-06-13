use opentelemetry::{
    metrics::{Counter, Histogram, MeterProvider as _},
    trace::TracerProvider as _,
};
use opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge;
use opentelemetry_otlp::{LogExporter, MetricExporter, Protocol, SpanExporter, WithExportConfig};
use opentelemetry_sdk::{
    logs::SdkLoggerProvider,
    metrics::SdkMeterProvider,
    trace::SdkTracerProvider,
    Resource,
};
use tracing::Subscriber;
use tracing_subscriber::{layer::Layer, registry::LookupSpan};

/// Holds the three OTel providers for lifecycle management.
///
/// Providers are kept alive for the process lifetime and shut down in order
/// (traces → logs → metrics) when `OtelGuard::shutdown()` is called.
pub struct OtelGuard {
    tracer_provider: SdkTracerProvider,
    meter_provider: SdkMeterProvider,
    logger_provider: SdkLoggerProvider,
}

/// Pre-allocated metric instruments accessed from handler code.
///
/// Each instrument is created once during `init()` and reused across all
/// requests — recording a value is a cheap atomic operation.
#[derive(Clone)]
pub struct Metrics {
    pub requests_total: Counter<u64>,
    pub request_duration_seconds: Histogram<f64>,
    pub classification_total: Counter<u64>,
    pub upstream_duration_seconds: Histogram<f64>,
}

/// Initialize OpenTelemetry providers and metric instruments.
///
/// Reads `OTEL_ENABLED` env var — returns `None` if unset, empty, or `"false"`.
/// Standard OTel env vars (`OTEL_EXPORTER_OTLP_ENDPOINT`,
/// `OTEL_EXPORTER_OTLP_HEADERS`, `OTEL_SERVICE_NAME`) are auto-detected by the
/// exporter builders.
pub fn init(service_name: &str) -> Option<(OtelGuard, Metrics)> {
    let enabled = std::env::var("OTEL_ENABLED")
        .ok()
        .map(|v| v == "true" || v == "1")
        .unwrap_or(false);
    if !enabled {
        return None;
    }

    // leak() is intentional here — the Resource builder requires &'static str,
    // and the allocation (one string per process lifetime, < 100 bytes) is bounded.
    // Do not copy this pattern for large or high-frequency allocations.
    let svc_name: &'static str = std::env::var("OTEL_SERVICE_NAME")
        .unwrap_or_else(|_| service_name.to_string())
        .leak();

    let resource = Resource::builder()
        .with_service_name(svc_name)
        .build();

    let trace_exporter = match SpanExporter::builder()
        .with_http()
        .with_protocol(Protocol::HttpBinary)
        .build()
    {
        Ok(e) => e,
        Err(e) => {
            eprintln!("OTLP SpanExporter failed to build: {e}");
            return None;
        }
    };

    let tracer_provider = SdkTracerProvider::builder()
        .with_batch_exporter(trace_exporter)
        .with_resource(resource.clone())
        .build();

    let log_exporter = match LogExporter::builder()
        .with_http()
        .with_protocol(Protocol::HttpBinary)
        .build()
    {
        Ok(e) => e,
        Err(e) => {
            eprintln!("OTLP LogExporter failed to build: {e}");
            return None;
        }
    };

    let logger_provider = SdkLoggerProvider::builder()
        .with_batch_exporter(log_exporter)
        .with_resource(resource.clone())
        .build();

    let metric_exporter = match MetricExporter::builder()
        .with_http()
        .with_protocol(Protocol::HttpBinary)
        .build()
    {
        Ok(e) => e,
        Err(e) => {
            eprintln!("OTLP MetricExporter failed to build: {e}");
            return None;
        }
    };

    let meter_provider = SdkMeterProvider::builder()
        .with_periodic_exporter(metric_exporter)
        .with_resource(resource)
        .build();

    let meter = meter_provider.meter(svc_name);

    let requests_total = meter
        .u64_counter("cerebrum.requests.total")
        .with_description("Total number of requests received")
        .build();

    let request_duration_seconds = meter
        .f64_histogram("cerebrum.request.duration_seconds")
        .with_description("Request duration in seconds")
        .with_unit("s")
        .build();

    let classification_total = meter
        .u64_counter("cerebrum.classification.total")
        .with_description("Total number of classifications performed")
        .build();

    let upstream_duration_seconds = meter
        .f64_histogram("cerebrum.upstream.duration_seconds")
        .with_description("Upstream request duration in seconds")
        .with_unit("s")
        .build();

    let guard = OtelGuard {
        tracer_provider,
        meter_provider,
        logger_provider,
    };

    let metrics = Metrics {
        requests_total,
        request_duration_seconds,
        classification_total,
        upstream_duration_seconds,
    };

    Some((guard, metrics))
}

impl OtelGuard {
    /// Build a tracing-subscriber `Layer` for OTel trace export.
    ///
    /// The returned layer bridges all `tracing` spans to the OTel tracer
    /// provider held by this guard. Callers should add it to the subscriber
    /// registry alongside the existing `fmt` layer.
    pub fn trace_layer<S>(&self, svc_name: &'static str) -> Box<dyn Layer<S> + Send + Sync + 'static>
    where
        S: Subscriber + for<'span> LookupSpan<'span> + Send + Sync + 'static,
    {
        let tracer = self.tracer_provider.tracer(svc_name);
        Box::new(tracing_opentelemetry::OpenTelemetryLayer::new(tracer))
    }

    /// Build a tracing-subscriber `Layer` for OTel log export.
    ///
    /// The returned layer bridges all `tracing` events at `info`+ level to the
    /// OTel logger provider held by this guard.
    pub fn log_layer<S>(&self) -> Box<dyn Layer<S> + Send + Sync + 'static>
    where
        S: Subscriber + for<'a> LookupSpan<'a> + Send + Sync + 'static,
    {
        Box::new(OpenTelemetryTracingBridge::new(&self.logger_provider))
    }

    /// Shut down all OTel providers in the correct order.
    ///
    /// Order: traces → logs → metrics (recommended by OTel docs so that log
    /// processors can emit self-diagnostic metrics during their own shutdown).
    pub fn shutdown(self) {
        if let Err(e) = self.tracer_provider.shutdown() {
            tracing::warn!("OTel tracer provider shutdown error: {e}");
        }
        if let Err(e) = self.logger_provider.shutdown() {
            tracing::warn!("OTel logger provider shutdown error: {e}");
        }
        if let Err(e) = self.meter_provider.shutdown() {
            tracing::warn!("OTel meter provider shutdown error: {e}");
        }
    }
}
