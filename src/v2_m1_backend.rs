//! V2-M1 asynchronous backend adapter with explicit cancellation outcomes.
//!
//! Cua-specific MCP tool names and response shapes terminate in this module.
//! A propagated cancellation is deliberately reported as *indeterminate* rather
//! than as proof that a desktop action did not happen.

use crate::backend::{
    BackendCallCancelled, BackendCallTimedOut, ComputerUseBackend, cua::CuaBackend,
};
use crate::v2_m0::{
    CAPABILITY_SCHEMA_VERSION, CapabilityAdvertisement, DeviceCapability, DeviceCommand,
    DeviceResult, MAX_SCREENSHOT_BYTES, MAX_TYPE_TEXT_BYTES,
};
use crate::v2_m0_transport::CancellationDisposition;
use crate::v2_observability::SafeErrorCode;
use anyhow::Error as AnyError;
use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use rmcp::model::{CallToolResult, JsonObject};
use serde_json::Value;
use std::fmt;
use std::time::Duration;
use tokio::sync::watch;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendExecutionOutcome {
    Completed(DeviceResult),
    CancellationPropagatedIndeterminate,
    TimedOutIndeterminate,
}

impl BackendExecutionOutcome {
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Completed(_) => "completed",
            Self::CancellationPropagatedIndeterminate => "cancellation_propagated_indeterminate",
            Self::TimedOutIndeterminate => "timed_out_indeterminate",
        }
    }

    pub fn cancellation_disposition(&self) -> Option<CancellationDisposition> {
        match self {
            Self::Completed(_) => None,
            Self::CancellationPropagatedIndeterminate | Self::TimedOutIndeterminate => {
                Some(CancellationDisposition::IndeterminateAfterPropagation)
            }
        }
    }
}

/// Replacement seam for a typed Computer Use executor.
///
/// Implementations must return `Completed` only when their backend contract has enough
/// evidence for an ordinary result. Cancellation or timeout after a side effect may have
/// started must remain an indeterminate outcome; the CUMG Agent/Hub own quarantine and
/// explicit resolution.
#[async_trait]
pub trait ComputerUseBackendAdapter: Send + Sync {
    fn advertisement(&self) -> CapabilityAdvertisement;

    async fn connect(&self) -> Result<(), M1BackendError>;

    async fn shutdown(&self) -> Result<(), M1BackendError>;

    async fn execute(
        &self,
        command: &DeviceCommand,
        cancellation: watch::Receiver<bool>,
    ) -> Result<BackendExecutionOutcome, M1BackendError>;
}

#[derive(Clone)]
pub struct CuaMcpAdapter {
    backend: CuaBackend,
    backend_version: String,
    platform: String,
    revision: u64,
}

impl std::fmt::Debug for CuaMcpAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CuaMcpAdapter")
            .field("backend_version", &self.backend_version)
            .field("platform", &self.platform)
            .field("revision", &self.revision)
            .finish_non_exhaustive()
    }
}

impl CuaMcpAdapter {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        command: impl Into<String>,
        args: Vec<String>,
        backend_version: impl Into<String>,
        platform: impl Into<String>,
        revision: u64,
        connect_timeout: Duration,
        tool_timeout: Duration,
        reconnect_attempts: u32,
        reconnect_backoff: Duration,
    ) -> Self {
        Self {
            backend: CuaBackend::new(
                command,
                args,
                connect_timeout,
                tool_timeout,
                reconnect_attempts,
                reconnect_backoff,
            ),
            backend_version: backend_version.into(),
            platform: platform.into(),
            revision,
        }
    }

    pub fn advertisement(&self) -> CapabilityAdvertisement {
        CapabilityAdvertisement {
            backend: "cua".into(),
            backend_version: self.backend_version.clone(),
            platform: self.platform.clone(),
            capability_schema_version: CAPABILITY_SCHEMA_VERSION,
            revision: self.revision,
            supported: vec![
                DeviceCapability::ListApplications,
                DeviceCapability::ScreenGeometry,
                DeviceCapability::Screenshot,
                DeviceCapability::PointerClick,
                DeviceCapability::PointerDrag,
                DeviceCapability::TypeText,
            ],
        }
    }

    pub async fn connect(&self) -> Result<(), M1BackendError> {
        self.backend.connect().await.map_err(|error| {
            crate::v2_observability::backend_failure(
                crate::v2_observability::BackendFailureReason::Connect,
            );
            M1BackendError::Backend(error)
        })
    }

    pub async fn shutdown(&self) -> Result<(), M1BackendError> {
        self.backend
            .shutdown()
            .await
            .map_err(M1BackendError::Backend)
    }

    pub async fn execute(
        &self,
        command: &DeviceCommand,
        cancellation: watch::Receiver<bool>,
    ) -> Result<BackendExecutionOutcome, M1BackendError> {
        let (tool, arguments) = map_command(command)?;
        let raw = match self.backend.call_tool(tool, arguments, cancellation).await {
            Ok(result) => result,
            Err(error) if error.downcast_ref::<BackendCallCancelled>().is_some() => {
                return Ok(BackendExecutionOutcome::CancellationPropagatedIndeterminate);
            }
            Err(error) if error.downcast_ref::<BackendCallTimedOut>().is_some() => {
                crate::v2_observability::backend_failure(
                    crate::v2_observability::BackendFailureReason::Timeout,
                );
                return Ok(BackendExecutionOutcome::TimedOutIndeterminate);
            }
            Err(error) => {
                crate::v2_observability::backend_failure(
                    crate::v2_observability::BackendFailureReason::Tool,
                );
                return Err(M1BackendError::Backend(error));
            }
        };
        if raw.is_error == Some(true) {
            crate::v2_observability::backend_failure(
                crate::v2_observability::BackendFailureReason::Tool,
            );
            return Err(M1BackendError::BackendToolError);
        }
        let result = match command {
            DeviceCommand::PointerClick { .. } => DeviceResult::PointerClickCompleted,
            DeviceCommand::PointerDrag { .. } => DeviceResult::PointerDragCompleted,
            DeviceCommand::TypeText { .. } => DeviceResult::TypeTextCompleted,
            DeviceCommand::Screenshot => normalize_screenshot_result(&raw)?,
            _ => {
                let value = structured_value(&raw)?;
                normalize_result(command, &value)?
            }
        };
        Ok(BackendExecutionOutcome::Completed(result))
    }
}

#[async_trait]
impl ComputerUseBackendAdapter for CuaMcpAdapter {
    fn advertisement(&self) -> CapabilityAdvertisement {
        CuaMcpAdapter::advertisement(self)
    }

    async fn connect(&self) -> Result<(), M1BackendError> {
        CuaMcpAdapter::connect(self).await
    }

    async fn shutdown(&self) -> Result<(), M1BackendError> {
        CuaMcpAdapter::shutdown(self).await
    }

    async fn execute(
        &self,
        command: &DeviceCommand,
        cancellation: watch::Receiver<bool>,
    ) -> Result<BackendExecutionOutcome, M1BackendError> {
        CuaMcpAdapter::execute(self, command, cancellation).await
    }
}

fn map_command(
    command: &DeviceCommand,
) -> Result<(&'static str, Option<JsonObject>), M1BackendError> {
    match command {
        DeviceCommand::ListApplications => Ok(("list_apps", None)),
        DeviceCommand::ScreenGeometry => Ok(("get_screen_size", None)),
        DeviceCommand::Screenshot => Ok(("get_desktop_state", None)),
        DeviceCommand::PointerClick { x, y, button } => Ok((
            "click",
            serde_json::json!({
                "x": x,
                "y": y,
                "button": pointer_button_name(*button),
                "scope": "desktop"
            })
            .as_object()
            .cloned(),
        )),
        DeviceCommand::PointerDrag {
            from_x,
            from_y,
            to_x,
            to_y,
            duration_ms,
        } => {
            if *duration_ms == 0 || *duration_ms > 10_000 {
                return Err(M1BackendError::InvalidRequest(
                    "pointer drag duration must be within 1..=10000 ms",
                ));
            }
            Ok((
                "drag",
                serde_json::json!({
                    "from_x": from_x,
                    "from_y": from_y,
                    "to_x": to_x,
                    "to_y": to_y,
                    "duration_ms": duration_ms,
                    "scope": "desktop"
                })
                .as_object()
                .cloned(),
            ))
        }
        DeviceCommand::TypeText { text } => {
            if text.is_empty() || text.len() > MAX_TYPE_TEXT_BYTES {
                return Err(M1BackendError::InvalidRequest(
                    "typed text must be within 1..=32768 UTF-8 bytes",
                ));
            }
            Ok((
                "type_text",
                serde_json::json!({
                    "text": text,
                    "scope": "desktop"
                })
                .as_object()
                .cloned(),
            ))
        }
        DeviceCommand::ExecuteProcess { .. } => Err(M1BackendError::UnsupportedCommand(
            DeviceCapability::ExecuteProcess,
        )),
        DeviceCommand::Shell { .. } => {
            Err(M1BackendError::UnsupportedCommand(DeviceCapability::Shell))
        }
        DeviceCommand::ReadFile { .. } => Err(M1BackendError::UnsupportedCommand(
            DeviceCapability::ReadFile,
        )),
        DeviceCommand::ListDirectory { .. } => Err(M1BackendError::UnsupportedCommand(
            DeviceCapability::ListDirectory,
        )),
    }
}

fn structured_value(result: &CallToolResult) -> Result<Value, M1BackendError> {
    if let Some(value) = &result.structured_content {
        return Ok(value.clone());
    }
    for content in &result.content {
        if let Some(text) = content.as_text() {
            if let Ok(value) = serde_json::from_str(&text.text) {
                return Ok(value);
            }
        }
    }
    Err(M1BackendError::MalformedResponse(
        "backend returned no structured JSON result",
    ))
}

fn normalize_result(
    command: &DeviceCommand,
    value: &Value,
) -> Result<DeviceResult, M1BackendError> {
    match command {
        DeviceCommand::ListApplications => {
            let apps = value
                .get("apps")
                .and_then(Value::as_array)
                .ok_or(M1BackendError::MalformedResponse("list_apps missing apps"))?;
            Ok(DeviceResult::Applications {
                count: u64::try_from(apps.len()).map_err(|_| M1BackendError::NumericOverflow)?,
            })
        }
        DeviceCommand::ScreenGeometry => {
            let width_points = json_u32(value, "width")?;
            let height_points = json_u32(value, "height")?;
            let scale = value.get("scale_factor").and_then(Value::as_f64).ok_or(
                M1BackendError::MalformedResponse("get_screen_size missing scale_factor"),
            )?;
            if !scale.is_finite() || scale <= 0.0 || scale > (u32::MAX as f64 / 1000.0) {
                return Err(M1BackendError::MalformedResponse("invalid scale_factor"));
            }
            Ok(DeviceResult::ScreenGeometry {
                width_points,
                height_points,
                scale_factor_milli: (scale * 1000.0).round() as u32,
            })
        }
        DeviceCommand::PointerClick { .. }
        | DeviceCommand::PointerDrag { .. }
        | DeviceCommand::TypeText { .. } => Err(M1BackendError::MalformedResponse(
            "interaction result should not require response normalization",
        )),
        DeviceCommand::Screenshot => Err(M1BackendError::MalformedResponse(
            "screenshot result requires image-content normalization",
        )),
        DeviceCommand::ExecuteProcess { .. } => Err(M1BackendError::UnsupportedCommand(
            DeviceCapability::ExecuteProcess,
        )),
        DeviceCommand::Shell { .. } => {
            Err(M1BackendError::UnsupportedCommand(DeviceCapability::Shell))
        }
        DeviceCommand::ReadFile { .. } => Err(M1BackendError::UnsupportedCommand(
            DeviceCapability::ReadFile,
        )),
        DeviceCommand::ListDirectory { .. } => Err(M1BackendError::UnsupportedCommand(
            DeviceCapability::ListDirectory,
        )),
    }
}

fn normalize_screenshot_result(result: &CallToolResult) -> Result<DeviceResult, M1BackendError> {
    let value = structured_value(result)?;
    let mime_type = value
        .get("screenshot_mime_type")
        .and_then(Value::as_str)
        .ok_or(M1BackendError::MalformedResponse(
            "get_desktop_state missing screenshot_mime_type",
        ))?;
    if mime_type != "image/png" {
        return Err(M1BackendError::MalformedResponse(
            "get_desktop_state screenshot is not PNG",
        ));
    }
    let width_pixels = json_u32(&value, "screenshot_width")?;
    let height_pixels = json_u32(&value, "screenshot_height")?;
    if width_pixels == 0 || height_pixels == 0 {
        return Err(M1BackendError::MalformedResponse(
            "get_desktop_state screenshot dimensions must be positive",
        ));
    }

    let mut images = result
        .content
        .iter()
        .filter_map(|content| content.as_image());
    let image = images.next().ok_or(M1BackendError::MalformedResponse(
        "get_desktop_state missing image content",
    ))?;
    if images.next().is_some() || image.mime_type != "image/png" {
        return Err(M1BackendError::MalformedResponse(
            "get_desktop_state returned ambiguous image content",
        ));
    }
    let max_encoded = MAX_SCREENSHOT_BYTES.div_ceil(3) * 4;
    if image.data.len() > max_encoded {
        return Err(M1BackendError::ScreenshotTooLarge);
    }
    let decoded = STANDARD
        .decode(image.data.as_bytes())
        .map_err(|_| M1BackendError::MalformedResponse("screenshot base64 is invalid"))?;
    if decoded.len() > MAX_SCREENSHOT_BYTES {
        return Err(M1BackendError::ScreenshotTooLarge);
    }
    if !decoded.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Err(M1BackendError::MalformedResponse(
            "screenshot content is not a PNG",
        ));
    }

    Ok(DeviceResult::Screenshot {
        data_base64: image.data.clone(),
        mime_type: image.mime_type.clone(),
        width_pixels,
        height_pixels,
    })
}

fn pointer_button_name(button: crate::v2_m0::PointerButton) -> &'static str {
    match button {
        crate::v2_m0::PointerButton::Left => "left",
        crate::v2_m0::PointerButton::Right => "right",
        crate::v2_m0::PointerButton::Middle => "middle",
    }
}

fn json_u32(value: &Value, key: &'static str) -> Result<u32, M1BackendError> {
    let raw = value
        .get(key)
        .and_then(Value::as_u64)
        .ok_or(M1BackendError::MalformedResponse(key))?;
    u32::try_from(raw).map_err(|_| M1BackendError::NumericOverflow)
}

pub enum M1BackendError {
    Backend(AnyError),
    BackendToolError,
    MalformedResponse(&'static str),
    NumericOverflow,
    ScreenshotTooLarge,
    InvalidRequest(&'static str),
    UnsupportedCommand(DeviceCapability),
}

impl SafeErrorCode for M1BackendError {
    fn safe_error_code(&self) -> &'static str {
        match self {
            Self::Backend(_) => "backend_failure",
            Self::BackendToolError => "backend_tool_error",
            Self::MalformedResponse(_) => "backend_malformed_response",
            Self::NumericOverflow => "backend_numeric_overflow",
            Self::ScreenshotTooLarge => "backend_screenshot_too_large",
            Self::InvalidRequest(_) => "backend_invalid_request",
            Self::UnsupportedCommand(_) => "backend_unsupported_command",
        }
    }
}

impl fmt::Debug for M1BackendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.safe_error_code())
    }
}

impl fmt::Display for M1BackendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.safe_error_code())
    }
}

impl std::error::Error for M1BackendError {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;
    use std::time::{SystemTime, UNIX_EPOCH};
    use tokio::time::{sleep, timeout};

    async fn wait_for_file(path: &Path) {
        timeout(Duration::from_secs(5), async {
            while !path.exists() {
                sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("fixture marker was created");
    }

    fn fixture(args: Vec<String>) -> CuaMcpAdapter {
        fixture_with_timeout(args, Duration::from_secs(30))
    }

    fn fixture_with_timeout(args: Vec<String>, tool_timeout: Duration) -> CuaMcpAdapter {
        CuaMcpAdapter::new(
            "python3",
            args,
            "fixture",
            "test",
            1,
            Duration::from_secs(5),
            tool_timeout,
            1,
            Duration::from_millis(10),
        )
    }

    #[tokio::test]
    async fn normalizes_fixture_results_to_backend_neutral_types() {
        let adapter = fixture(vec!["scripts/mock_mcp_backend.py".into()]);
        adapter.connect().await.unwrap();
        let (_cancel_tx, cancel_rx) = watch::channel(false);
        assert_eq!(
            adapter
                .execute(&DeviceCommand::ListApplications, cancel_rx.clone())
                .await
                .unwrap(),
            BackendExecutionOutcome::Completed(DeviceResult::Applications { count: 2 })
        );
        assert_eq!(
            adapter
                .execute(&DeviceCommand::ScreenGeometry, cancel_rx.clone())
                .await
                .unwrap(),
            BackendExecutionOutcome::Completed(DeviceResult::ScreenGeometry {
                width_points: 1920,
                height_points: 1080,
                scale_factor_milli: 2000,
            })
        );
        assert_eq!(
            adapter
                .execute(&DeviceCommand::Screenshot, cancel_rx.clone())
                .await
                .unwrap(),
            BackendExecutionOutcome::Completed(DeviceResult::Screenshot {
                data_base64: "iVBORw0KGgo=".into(),
                mime_type: "image/png".into(),
                width_pixels: 2,
                height_pixels: 1,
            })
        );
        assert_eq!(
            adapter
                .execute(
                    &DeviceCommand::TypeText {
                        text: "fixture text".into(),
                    },
                    cancel_rx,
                )
                .await
                .unwrap(),
            BackendExecutionOutcome::Completed(DeviceResult::TypeTextCompleted)
        );
        adapter.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn type_text_propagated_cancellation_is_classified_indeterminate() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir();
        let call_marker = directory.join(format!("cumg-m1-call-{}-{nonce}", std::process::id()));
        let cancel_marker =
            directory.join(format!("cumg-m1-cancel-{}-{nonce}", std::process::id()));
        let adapter = fixture(vec![
            "scripts/mock_mcp_backend.py".into(),
            "--slow-type-text".into(),
            "--call-marker".into(),
            call_marker.to_string_lossy().into_owned(),
            "--cancel-marker".into(),
            cancel_marker.to_string_lossy().into_owned(),
        ]);
        adapter.connect().await.unwrap();
        let (cancel_tx, cancel_rx) = watch::channel(false);
        let caller = adapter.clone();
        let call = tokio::spawn(async move {
            caller
                .execute(
                    &DeviceCommand::TypeText {
                        text: "cancel me".into(),
                    },
                    cancel_rx,
                )
                .await
        });
        wait_for_file(&call_marker).await;
        cancel_tx.send(true).unwrap();
        let outcome = timeout(Duration::from_secs(5), call)
            .await
            .expect("adapter cancellation returns")
            .expect("adapter task joins")
            .expect("cancellation is a classified outcome");
        assert_eq!(
            outcome,
            BackendExecutionOutcome::CancellationPropagatedIndeterminate
        );
        assert_eq!(
            outcome.cancellation_disposition(),
            Some(CancellationDisposition::IndeterminateAfterPropagation)
        );
        wait_for_file(&cancel_marker).await;
        assert_eq!(
            fs::read_to_string(&call_marker).unwrap(),
            fs::read_to_string(&cancel_marker).unwrap(),
            "the backend cancellation must target the exact in-flight MCP request"
        );
        adapter.shutdown().await.unwrap();
        let _ = fs::remove_file(call_marker);
        let _ = fs::remove_file(cancel_marker);
    }

    #[tokio::test]
    async fn type_text_timeout_is_classified_indeterminate() {
        let adapter = fixture_with_timeout(
            vec![
                "scripts/mock_mcp_backend.py".into(),
                "--slow-type-text".into(),
            ],
            Duration::from_millis(100),
        );
        adapter.connect().await.unwrap();
        let (_cancel_tx, cancel_rx) = watch::channel(false);
        let outcome = adapter
            .execute(
                &DeviceCommand::TypeText {
                    text: "timeout me".into(),
                },
                cancel_rx,
            )
            .await
            .expect("timeout is a classified backend outcome");
        assert_eq!(outcome, BackendExecutionOutcome::TimedOutIndeterminate);
        adapter.shutdown().await.unwrap();
    }

    #[test]
    fn screenshot_normalization_rejects_missing_or_non_png_image_content() {
        let mut missing = CallToolResult::success(vec![rmcp::model::ContentBlock::text("summary")]);
        missing.structured_content = Some(serde_json::json!({
            "screenshot_width": 2,
            "screenshot_height": 1,
            "screenshot_mime_type": "image/png"
        }));
        assert!(matches!(
            normalize_screenshot_result(&missing),
            Err(M1BackendError::MalformedResponse(_))
        ));

        let mut wrong = CallToolResult::success(vec![rmcp::model::ContentBlock::image(
            "iVBORw0KGgo=",
            "image/jpeg",
        )]);
        wrong.structured_content = missing.structured_content.clone();
        assert!(matches!(
            normalize_screenshot_result(&wrong),
            Err(M1BackendError::MalformedResponse(_))
        ));
    }

    #[test]
    fn cua_mapping_for_type_text_is_typed_and_bounded() {
        let command = DeviceCommand::TypeText {
            text: "hello".into(),
        };
        let (tool, args) = map_command(&command).unwrap();
        assert_eq!(tool, "type_text");
        assert_eq!(
            Value::Object(args.unwrap()),
            serde_json::json!({"text": "hello", "scope": "desktop"})
        );
        assert!(matches!(
            map_command(&DeviceCommand::TypeText {
                text: String::new()
            }),
            Err(M1BackendError::InvalidRequest(_))
        ));
    }

    #[test]
    fn backend_error_debug_and_display_do_not_expose_raw_exception() {
        let marker = "Bearer SUPER_SECRET_TOKEN signature=SECRET raw_stdout=SECRET";
        let error = M1BackendError::Backend(anyhow::anyhow!(marker));
        let debug = format!("{error:?}");
        let display = error.to_string();
        assert_eq!(debug, "backend_failure");
        assert_eq!(display, "backend_failure");
        assert!(!debug.contains(marker));
        assert!(!display.contains(marker));
    }
}
