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
    DeviceResult,
};
use crate::v2_m0_transport::CancellationDisposition;
use anyhow::Error as AnyError;
use rmcp::model::{CallToolResult, JsonObject};
use serde_json::{Map, Value, json};
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
    pub fn cancellation_disposition(&self) -> Option<CancellationDisposition> {
        match self {
            Self::Completed(_) => None,
            Self::CancellationPropagatedIndeterminate | Self::TimedOutIndeterminate => {
                Some(CancellationDisposition::IndeterminateAfterPropagation)
            }
        }
    }
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
            ],
        }
    }

    pub async fn connect(&self) -> Result<(), M1BackendError> {
        self.backend
            .connect()
            .await
            .map_err(M1BackendError::Backend)
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
                return Ok(BackendExecutionOutcome::TimedOutIndeterminate);
            }
            Err(error) => return Err(M1BackendError::Backend(error)),
        };
        if raw.is_error == Some(true) {
            return Err(M1BackendError::BackendToolError);
        }
        let value = structured_value(&raw)?;
        let result = normalize_result(command, &value)?;
        Ok(BackendExecutionOutcome::Completed(result))
    }
}

fn map_command(
    command: &DeviceCommand,
) -> Result<(&'static str, Option<JsonObject>), M1BackendError> {
    match command {
        DeviceCommand::ListApplications => Ok(("list_apps", None)),
        DeviceCommand::ScreenGeometry => Ok(("get_screen_size", None)),
        DeviceCommand::PointerClick { .. } => Err(M1BackendError::UnsupportedCommand(
            DeviceCapability::PointerClick,
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
        DeviceCommand::PointerClick { .. } => Err(M1BackendError::UnsupportedCommand(
            DeviceCapability::PointerClick,
        )),
    }
}

fn json_u32(value: &Value, key: &'static str) -> Result<u32, M1BackendError> {
    let raw = value
        .get(key)
        .and_then(Value::as_u64)
        .ok_or(M1BackendError::MalformedResponse(key))?;
    u32::try_from(raw).map_err(|_| M1BackendError::NumericOverflow)
}

#[derive(Debug)]
pub enum M1BackendError {
    Backend(AnyError),
    BackendToolError,
    MalformedResponse(&'static str),
    NumericOverflow,
    UnsupportedCommand(DeviceCapability),
}

impl fmt::Display for M1BackendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
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
        CuaMcpAdapter::new(
            "python3",
            args,
            "fixture",
            "test",
            1,
            Duration::from_secs(5),
            Duration::from_secs(30),
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
                .execute(&DeviceCommand::ScreenGeometry, cancel_rx)
                .await
                .unwrap(),
            BackendExecutionOutcome::Completed(DeviceResult::ScreenGeometry {
                width_points: 1920,
                height_points: 1080,
                scale_factor_milli: 2000,
            })
        );
        adapter.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn propagated_backend_cancellation_is_classified_indeterminate() {
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
            "--slow-list-apps".into(),
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
                .execute(&DeviceCommand::ListApplications, cancel_rx)
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
}
