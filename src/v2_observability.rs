//! Standard OpenTelemetry/OTLP bootstrap for V2 daemons.
//!
//! OTLP is opt-in through the standard `OTEL_EXPORTER_OTLP_*_ENDPOINT`
//! environment variables. Exporter protocol, headers, timeouts, and signal
//! overrides are resolved by `opentelemetry-otlp` from the standard OTel env
//! variables. Default structured logs stay local and payload-free.

use anyhow::{Context, Result};
use opentelemetry::{KeyValue, global, trace::TracerProvider as _};
use opentelemetry_sdk::{Resource, metrics::SdkMeterProvider, trace::SdkTracerProvider};
use std::time::Duration;
use tracing_subscriber::{EnvFilter, layer::SubscriberExt as _, util::SubscriberInitExt as _};

pub struct ObservabilityGuard {
    tracer_provider: Option<SdkTracerProvider>,
    meter_provider: Option<SdkMeterProvider>,
}

impl Drop for ObservabilityGuard {
    fn drop(&mut self) {
        if let Some(provider) = &self.tracer_provider {
            let _ = provider.shutdown_with_timeout(Duration::from_secs(5));
        }
        if let Some(provider) = &self.meter_provider {
            let _ = provider.shutdown_with_timeout(Duration::from_secs(5));
        }
    }
}

pub fn init(service_name: &'static str) -> Result<ObservabilityGuard> {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let signals = OtlpSignals::from_environment();
    if signals.disabled || (!signals.traces && !signals.metrics) {
        tracing_subscriber::registry()
            .with(filter)
            .with(tracing_subscriber::fmt::layer())
            .try_init()
            .context("failed to initialize tracing subscriber")?;
        return Ok(ObservabilityGuard {
            tracer_provider: None,
            meter_provider: None,
        });
    }

    let resource = Resource::builder().with_service_name(service_name).build();
    let (tracer_provider, tracer) = if signals.traces {
        // `build()` intentionally delegates transport/protocol/endpoint/header/
        // timeout selection to opentelemetry-otlp's standard environment resolver.
        let exporter = opentelemetry_otlp::SpanExporter::builder()
            .build()
            .context("failed to build OTLP span exporter")?;
        let provider = SdkTracerProvider::builder()
            .with_resource(resource.clone())
            .with_batch_exporter(exporter)
            .build();
        let tracer = provider.tracer(service_name);
        (Some(provider), Some(tracer))
    } else {
        (None, None)
    };

    let meter_provider = if signals.metrics {
        let exporter = opentelemetry_otlp::MetricExporter::builder()
            .build()
            .context("failed to build OTLP metric exporter")?;
        let provider = SdkMeterProvider::builder()
            .with_resource(resource)
            .with_periodic_exporter(exporter)
            .build();
        global::set_meter_provider(provider.clone());
        Some(provider)
    } else {
        None
    };

    tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer())
        .with(tracer.map(|tracer| tracing_opentelemetry::layer().with_tracer(tracer)))
        .try_init()
        .context("failed to initialize OpenTelemetry tracing subscriber")?;

    Ok(ObservabilityGuard {
        tracer_provider,
        meter_provider,
    })
}

pub fn increment_counter(name: &'static str, attributes: &[KeyValue]) {
    global::meter("computer-use-mcp-gateway")
        .u64_counter(name)
        .build()
        .add(1, attributes);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct OtlpSignals {
    disabled: bool,
    traces: bool,
    metrics: bool,
}

impl OtlpSignals {
    fn from_environment() -> Self {
        let disabled = std::env::var("OTEL_SDK_DISABLED")
            .ok()
            .is_some_and(|value| value.eq_ignore_ascii_case("true"));
        let generic = std::env::var_os("OTEL_EXPORTER_OTLP_ENDPOINT").is_some();
        Self {
            disabled,
            traces: generic || std::env::var_os("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT").is_some(),
            metrics: generic || std::env::var_os("OTEL_EXPORTER_OTLP_METRICS_ENDPOINT").is_some(),
        }
    }
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn compiled_otlp_transport_defaults_to_grpc() {
        assert_eq!(
            opentelemetry_otlp::Protocol::default(),
            opentelemetry_otlp::Protocol::Grpc
        );
        opentelemetry_otlp::SpanExporter::builder()
            .with_tonic()
            .build()
            .expect("compiled OTLP gRPC trace exporter");
        opentelemetry_otlp::MetricExporter::builder()
            .with_tonic()
            .build()
            .expect("compiled OTLP gRPC metric exporter");
    }

    #[test]
    fn standard_otlp_environment_names_are_the_only_activation_inputs() {
        let source = include_str!("v2_observability.rs");
        assert!(source.contains("OTEL_EXPORTER_OTLP_ENDPOINT"));
        assert!(source.contains("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT"));
        assert!(source.contains("OTEL_EXPORTER_OTLP_METRICS_ENDPOINT"));
        assert!(source.contains("OTEL_SDK_DISABLED"));
        assert!(!source.contains(&["CUMG", "OTLP", "TOKEN"].join("_")));
        assert!(!source.contains(&["CUMG", "OTLP", "ENDPOINT"].join("_")));
    }
}
