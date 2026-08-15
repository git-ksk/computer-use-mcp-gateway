//! Standard OpenTelemetry/OTLP bootstrap and payload-safe V2 telemetry helpers.
//!
//! OTLP is opt-in through the standard `OTEL_EXPORTER_OTLP_*_ENDPOINT`
//! environment variables. Exporter protocol, headers, timeouts, and signal
//! overrides are resolved by `opentelemetry-otlp` from the standard OTel env
//! variables. Default structured logs stay local and payload-free.
//!
//! Metric helpers are intentionally typed. Callers cannot attach operation IDs,
//! device IDs, principals, paths, command names, or other high-cardinality data
//! as metric attributes.

use crate::{
    v2_execution_safety::IndeterminateReason, v2_m0::DeviceCapability,
    v2_m0_execution::IndeterminateResolution, v2_m0_transport::CancellationDisposition,
};
use anyhow::{Context, Result};
use opentelemetry::{KeyValue, global, trace::TracerProvider as _};
use opentelemetry_sdk::{Resource, metrics::SdkMeterProvider, trace::SdkTracerProvider};
use std::time::Duration;
use tracing_subscriber::{EnvFilter, layer::SubscriberExt as _, util::SubscriberInitExt as _};

pub trait SafeErrorCode {
    fn safe_error_code(&self) -> &'static str;
}

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionRejectReason {
    RateLimit,
    ConcurrencyLimit,
    WrongDevice,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestRejectReason {
    RateLimit,
    ConcurrencyLimit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationOutcome {
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PersistenceComponent {
    Hub,
    Agent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthFailureReason {
    InvalidToken,
    Unavailable,
    InsufficientScope,
    AuthorizationDenied,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendFailureReason {
    Connect,
    Tool,
    Timeout,
    AmbiguousOutcome,
    Reconnect,
}

pub fn agent_session_started() {
    increment_counter("cumg.v2.agent_session_started", &[]);
}

pub fn agent_session_rejected(reason: SessionRejectReason) {
    increment_counter(
        "cumg.v2.agent_session_rejected",
        &[KeyValue::new("reason", session_reject_reason_name(reason))],
    );
}

pub fn northbound_request_rejected(reason: RequestRejectReason) {
    increment_counter(
        "cumg.v2.northbound_request_rejected",
        &[KeyValue::new("reason", request_reject_reason_name(reason))],
    );
}

pub fn reconnect_attempt() {
    increment_counter("cumg.v2.reconnect_attempt", &[]);
}

pub fn reconnect_exhausted() {
    increment_counter("cumg.v2.reconnect_exhausted", &[]);
}

pub fn operation_completed(capability: DeviceCapability, outcome: OperationOutcome) {
    increment_counter(
        "cumg.v2.operation_completed",
        &[
            KeyValue::new("capability", capability_name(capability)),
            KeyValue::new("outcome", operation_outcome_name(outcome)),
        ],
    );
}

pub fn operation_indeterminate(reason: IndeterminateReason) {
    increment_counter(
        "cumg.v2.operation_indeterminate",
        &[KeyValue::new("reason", indeterminate_reason_name(reason))],
    );
}

pub fn quarantine_created() {
    increment_counter("cumg.v2.quarantine_created", &[]);
}

pub fn quarantine_resolved() {
    increment_counter("cumg.v2.quarantine_resolved", &[]);
}

pub fn persistence_failure(component: PersistenceComponent) {
    increment_counter(
        "cumg.v2.persistence_failure",
        &[KeyValue::new(
            "component",
            persistence_component_name(component),
        )],
    );
}

pub fn auth_failure(reason: AuthFailureReason) {
    increment_counter(
        "cumg.v2.auth_failure",
        &[KeyValue::new("reason", auth_failure_reason_name(reason))],
    );
}

pub fn backend_failure(reason: BackendFailureReason) {
    increment_counter(
        "cumg.v2.backend_failure",
        &[KeyValue::new("reason", backend_failure_reason_name(reason))],
    );
}

pub fn stale_result_rejected() {
    increment_counter("cumg.v2.stale_result_rejected", &[]);
}

fn increment_counter(name: &'static str, attributes: &[KeyValue]) {
    global::meter("computer-use-mcp-gateway")
        .u64_counter(name)
        .build()
        .add(1, attributes);
}

pub const fn capability_name(capability: DeviceCapability) -> &'static str {
    match capability {
        DeviceCapability::ListApplications => "list_applications",
        DeviceCapability::ScreenGeometry => "screen_geometry",
        DeviceCapability::Screenshot => "screenshot",
        DeviceCapability::PointerClick => "pointer_click",
        DeviceCapability::PointerDrag => "pointer_drag",
        DeviceCapability::TypeText => "type_text",
        DeviceCapability::ExecuteProcess => "execute_process",
        DeviceCapability::Shell => "shell",
        DeviceCapability::ReadFile => "read_file",
        DeviceCapability::ListDirectory => "list_directory",
        DeviceCapability::ListWindows => "list_windows",
        DeviceCapability::LaunchApplication => "launch_application",
        DeviceCapability::InspectWindow => "inspect_window",
        DeviceCapability::VerifyUiState => "verify_ui_state",
        DeviceCapability::TerminateApplication => "terminate_application",
        DeviceCapability::ActivateWindow => "activate_window",
        DeviceCapability::SetWindowFrame => "set_window_frame",
        DeviceCapability::InvokeMenu => "invoke_menu",
        DeviceCapability::KeyboardInput => "keyboard_input",
        DeviceCapability::Scroll => "scroll",
        DeviceCapability::ClipboardRead => "clipboard_read",
        DeviceCapability::ClipboardWrite => "clipboard_write",
        DeviceCapability::PointerPosition => "pointer_position",
        DeviceCapability::MovePointer => "move_pointer",
        DeviceCapability::SetUiValue => "set_ui_value",
        DeviceCapability::CaptureRegion => "capture_region",
        DeviceCapability::DesktopScope => "desktop_scope",
        DeviceCapability::BrowserInspect => "browser_inspect",
        DeviceCapability::BrowserPrepare => "browser_prepare",
        DeviceCapability::BrowserNavigate => "browser_navigate",
        DeviceCapability::BrowserClick => "browser_click",
        DeviceCapability::BrowserType => "browser_type",
        DeviceCapability::BrowserDialog => "browser_dialog",
        DeviceCapability::BrowserPointer => "browser_pointer",
        DeviceCapability::BrowserUploadFile => "browser_upload_file",
        DeviceCapability::BrowserDownload => "browser_download",
    }
}

pub const fn indeterminate_reason_name(reason: IndeterminateReason) -> &'static str {
    match reason {
        IndeterminateReason::CancellationUnproven => "cancellation_unproven",
        IndeterminateReason::BackendTimedOut => "backend_timed_out",
        IndeterminateReason::BackendOutcomeUnproven => "backend_outcome_unproven",
        IndeterminateReason::ConnectionLost => "connection_lost",
        IndeterminateReason::HubRestartAfterDispatch => "hub_restart_after_dispatch",
        IndeterminateReason::AgentRestartAfterDispatch => "agent_restart_after_dispatch",
        IndeterminateReason::ResultDeliveryLost => "result_delivery_lost",
    }
}

pub const fn resolution_name(resolution: &IndeterminateResolution) -> &'static str {
    match resolution {
        IndeterminateResolution::ConfirmedCompleted => "confirmed_completed",
        IndeterminateResolution::ConfirmedNotExecuted => "confirmed_not_executed",
    }
}

pub const fn cancellation_disposition_name(disposition: &CancellationDisposition) -> &'static str {
    match disposition {
        CancellationDisposition::CancelledBeforeExecution => "cancelled_before_execution",
        CancellationDisposition::CancellationRequested => "cancellation_requested",
        CancellationDisposition::IndeterminateAfterPropagation => "indeterminate_after_propagation",
        CancellationDisposition::AlreadyTerminal => "already_terminal",
    }
}

const fn session_reject_reason_name(reason: SessionRejectReason) -> &'static str {
    match reason {
        SessionRejectReason::RateLimit => "rate_limit",
        SessionRejectReason::ConcurrencyLimit => "concurrency_limit",
        SessionRejectReason::WrongDevice => "wrong_device",
    }
}

const fn request_reject_reason_name(reason: RequestRejectReason) -> &'static str {
    match reason {
        RequestRejectReason::RateLimit => "rate_limit",
        RequestRejectReason::ConcurrencyLimit => "concurrency_limit",
    }
}

const fn operation_outcome_name(outcome: OperationOutcome) -> &'static str {
    match outcome {
        OperationOutcome::Completed => "completed",
        OperationOutcome::Failed => "failed",
        OperationOutcome::Cancelled => "cancelled",
    }
}

const fn persistence_component_name(component: PersistenceComponent) -> &'static str {
    match component {
        PersistenceComponent::Hub => "hub",
        PersistenceComponent::Agent => "agent",
    }
}

const fn auth_failure_reason_name(reason: AuthFailureReason) -> &'static str {
    match reason {
        AuthFailureReason::InvalidToken => "invalid_token",
        AuthFailureReason::Unavailable => "unavailable",
        AuthFailureReason::InsufficientScope => "insufficient_scope",
        AuthFailureReason::AuthorizationDenied => "authorization_denied",
    }
}

const fn backend_failure_reason_name(reason: BackendFailureReason) -> &'static str {
    match reason {
        BackendFailureReason::Connect => "connect",
        BackendFailureReason::Tool => "tool",
        BackendFailureReason::Timeout => "timeout",
        BackendFailureReason::AmbiguousOutcome => "ambiguous_outcome",
        BackendFailureReason::Reconnect => "reconnect",
    }
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
    use super::*;

    const METRIC_ATTRIBUTE_KEYS: &[&str] = &["reason", "capability", "outcome", "component"];

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

    #[test]
    fn metric_attributes_are_low_cardinality_only() {
        assert_eq!(
            METRIC_ATTRIBUTE_KEYS,
            &["reason", "capability", "outcome", "component"]
        );
        let source = include_str!("v2_observability.rs");
        let mut remainder = source;
        while let Some(start) = remainder.find("KeyValue::new(\"") {
            let after = &remainder[start + "KeyValue::new(\"".len()..];
            let end = after.find('"').expect("metric attribute key closes");
            let key = &after[..end];
            assert!(
                METRIC_ATTRIBUTE_KEYS.contains(&key),
                "unreviewed metric attribute key: {key}"
            );
            remainder = &after[end + 1..];
        }
        for forbidden in [
            "operation_id",
            "device_id",
            "principal",
            "subject",
            "path",
            "command",
            "tool",
        ] {
            assert!(!METRIC_ATTRIBUTE_KEYS.contains(&forbidden));
        }
    }

    #[test]
    fn metric_value_domains_are_closed_enums() {
        assert_eq!(
            session_reject_reason_name(SessionRejectReason::RateLimit),
            "rate_limit"
        );
        assert_eq!(
            request_reject_reason_name(RequestRejectReason::ConcurrencyLimit),
            "concurrency_limit"
        );
        assert_eq!(
            operation_outcome_name(OperationOutcome::Completed),
            "completed"
        );
        assert_eq!(
            persistence_component_name(PersistenceComponent::Agent),
            "agent"
        );
        assert_eq!(
            auth_failure_reason_name(AuthFailureReason::AuthorizationDenied),
            "authorization_denied"
        );
        assert_eq!(
            backend_failure_reason_name(BackendFailureReason::AmbiguousOutcome),
            "ambiguous_outcome"
        );
    }
}
