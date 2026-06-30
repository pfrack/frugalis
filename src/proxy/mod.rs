pub(crate) mod handlers;
pub(crate) mod responses_handler;
pub(crate) mod streaming;
pub(crate) mod upstream;
pub(crate) mod util;

#[cfg(feature = "otel")]
use opentelemetry::KeyValue;

/// Per-request metrics accumulator. Drop records `requests_total` and
/// `request_duration_seconds` on the shared `Metrics` handle (if present).
#[cfg(feature = "otel")]
pub(crate) struct RequestMetrics {
    metrics: Option<crate::telemetry::Metrics>,
    method: &'static str,
    route: &'static str,
    start: std::time::Instant,
    status: axum::http::StatusCode,
}

#[cfg(feature = "otel")]
impl RequestMetrics {
    pub(crate) fn new(
        metrics: Option<crate::telemetry::Metrics>,
        method: &'static str,
        route: &'static str,
    ) -> Self {
        Self {
            metrics,
            method,
            route,
            start: std::time::Instant::now(),
            status: axum::http::StatusCode::OK,
        }
    }
    pub(crate) fn set_status(&mut self, status: axum::http::StatusCode) {
        self.status = status;
    }
}

#[cfg(feature = "otel")]
impl Drop for RequestMetrics {
    fn drop(&mut self) {
        if let Some(ref m) = self.metrics {
            let attrs = [
                KeyValue::new("method", self.method),
                KeyValue::new("route", self.route),
                KeyValue::new("status", self.status.as_u16().to_string()),
            ];
            m.requests_total.add(1, &attrs);
            m.request_duration_seconds
                .record(self.start.elapsed().as_secs_f64(), &attrs);
        }
    }
}
