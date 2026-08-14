//! Backend-neutral V2-M0 adapter contract and Cua CLI PoC adapter.
//!
//! Hub↔Agent messages use `DeviceCommand`/`DeviceResult`; backend-specific tool
//! names and response shapes terminate here. This lets conformance tests exercise
//! the semantic contract without making Cua's wire surface the V2 protocol.

use crate::v2_m0::{
    CAPABILITY_SCHEMA_VERSION, CapabilityAdvertisement, DeviceCapability, DeviceCommand,
    DeviceResult,
};
use serde_json::Value;
use std::fmt;
use std::process::Command;

pub trait BackendAdapter {
    fn advertisement(&self) -> CapabilityAdvertisement;
    fn execute(&mut self, command: &DeviceCommand) -> Result<DeviceResult, BackendAdapterError>;
}

pub fn validate_adapter_advertisement(
    advertisement: &CapabilityAdvertisement,
) -> Result<(), BackendAdapterError> {
    if advertisement.backend.trim().is_empty()
        || advertisement.backend_version.trim().is_empty()
        || advertisement.platform.trim().is_empty()
    {
        return Err(BackendAdapterError::InvalidAdvertisement);
    }
    if advertisement.capability_schema_version != CAPABILITY_SCHEMA_VERSION {
        return Err(BackendAdapterError::UnsupportedCapabilitySchema {
            got: advertisement.capability_schema_version,
        });
    }
    let mut seen = std::collections::HashSet::new();
    if advertisement
        .supported
        .iter()
        .any(|capability| !seen.insert(*capability))
    {
        return Err(BackendAdapterError::DuplicateCapability);
    }
    Ok(())
}

pub fn validate_adapter_result(
    command: &DeviceCommand,
    result: &DeviceResult,
) -> Result<(), BackendAdapterError> {
    if result.matches_command(command) {
        Ok(())
    } else {
        Err(BackendAdapterError::ResultTypeMismatch)
    }
}

#[derive(Debug, Clone)]
pub struct CuaCliAdapter {
    executable: String,
    backend_version: String,
    platform: String,
    revision: u64,
}

impl CuaCliAdapter {
    pub fn detect(
        executable: impl Into<String>,
        platform: impl Into<String>,
        revision: u64,
    ) -> Result<Self, BackendAdapterError> {
        let executable = executable.into();
        let output = Command::new(&executable)
            .arg("--version")
            .output()
            .map_err(BackendAdapterError::Io)?;
        if !output.status.success() {
            return Err(BackendAdapterError::BackendFailed(format!(
                "version command exited with {}",
                output.status
            )));
        }
        let stdout = String::from_utf8(output.stdout).map_err(BackendAdapterError::Utf8)?;
        let backend_version = stdout
            .split_whitespace()
            .last()
            .ok_or(BackendAdapterError::MalformedResponse("missing version"))?
            .to_owned();
        Ok(Self {
            executable,
            backend_version,
            platform: platform.into(),
            revision,
        })
    }

    fn call(&self, tool: &str, arguments: &str) -> Result<Value, BackendAdapterError> {
        let output = Command::new(&self.executable)
            .args(["call", tool, arguments])
            .output()
            .map_err(BackendAdapterError::Io)?;
        if !output.status.success() {
            return Err(BackendAdapterError::BackendFailed(format!(
                "{tool} exited with {}",
                output.status
            )));
        }
        serde_json::from_slice(&output.stdout).map_err(BackendAdapterError::Json)
    }
}

impl BackendAdapter for CuaCliAdapter {
    fn advertisement(&self) -> CapabilityAdvertisement {
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

    fn execute(&mut self, command: &DeviceCommand) -> Result<DeviceResult, BackendAdapterError> {
        let result = match command {
            DeviceCommand::ListApplications => {
                let value = self.call("list_apps", "{}")?;
                let count = value
                    .get("apps")
                    .and_then(Value::as_array)
                    .ok_or(BackendAdapterError::MalformedResponse(
                        "list_apps missing apps array",
                    ))?
                    .len();
                DeviceResult::Applications {
                    count: u64::try_from(count)
                        .map_err(|_| BackendAdapterError::NumericOverflow)?,
                }
            }
            DeviceCommand::ScreenGeometry => {
                let value = self.call("get_screen_size", "{}")?;
                let width_points = json_u32(&value, "width")?;
                let height_points = json_u32(&value, "height")?;
                let scale = value.get("scale_factor").and_then(Value::as_f64).ok_or(
                    BackendAdapterError::MalformedResponse("get_screen_size missing scale_factor"),
                )?;
                if !scale.is_finite() || scale <= 0.0 || scale > (u32::MAX as f64 / 1000.0) {
                    return Err(BackendAdapterError::MalformedResponse(
                        "invalid scale_factor",
                    ));
                }
                DeviceResult::ScreenGeometry {
                    width_points,
                    height_points,
                    scale_factor_milli: (scale * 1000.0).round() as u32,
                }
            }
            DeviceCommand::Screenshot | DeviceCommand::ScreenshotContextual { .. } => {
                return Err(BackendAdapterError::UnsupportedCommand(
                    DeviceCapability::Screenshot,
                ));
            }
            DeviceCommand::PointerClick { .. } => {
                return Err(BackendAdapterError::UnsupportedCommand(
                    DeviceCapability::PointerClick,
                ));
            }
            DeviceCommand::PointerDrag { .. } => {
                return Err(BackendAdapterError::UnsupportedCommand(
                    DeviceCapability::PointerDrag,
                ));
            }
            DeviceCommand::TypeText { .. } => {
                return Err(BackendAdapterError::UnsupportedCommand(
                    DeviceCapability::TypeText,
                ));
            }
            DeviceCommand::ExecuteProcess { .. } => {
                return Err(BackendAdapterError::UnsupportedCommand(
                    DeviceCapability::ExecuteProcess,
                ));
            }
            DeviceCommand::Shell { .. } => {
                return Err(BackendAdapterError::UnsupportedCommand(
                    DeviceCapability::Shell,
                ));
            }
            DeviceCommand::ReadFile { .. } => {
                return Err(BackendAdapterError::UnsupportedCommand(
                    DeviceCapability::ReadFile,
                ));
            }
            DeviceCommand::ListDirectory { .. } => {
                return Err(BackendAdapterError::UnsupportedCommand(
                    DeviceCapability::ListDirectory,
                ));
            }
            DeviceCommand::SetUiValue { .. } => {
                return Err(BackendAdapterError::UnsupportedCommand(
                    DeviceCapability::SetUiValue,
                ));
            }
            DeviceCommand::CaptureRegion { .. } => {
                return Err(BackendAdapterError::UnsupportedCommand(
                    DeviceCapability::CaptureRegion,
                ));
            }
            DeviceCommand::ExpandInteractionScope { .. } => {
                return Err(BackendAdapterError::UnsupportedCommand(
                    DeviceCapability::DesktopScope,
                ));
            }
            DeviceCommand::StageBrowserUploadFile { .. } | DeviceCommand::Browser { .. } => {
                return Err(BackendAdapterError::UnsupportedCommand(
                    command.capability(),
                ));
            }
            DeviceCommand::ListWindows { .. } => {
                return Err(BackendAdapterError::UnsupportedCommand(
                    DeviceCapability::ListWindows,
                ));
            }
            DeviceCommand::LaunchApplication { .. } => {
                return Err(BackendAdapterError::UnsupportedCommand(
                    DeviceCapability::LaunchApplication,
                ));
            }
            DeviceCommand::InspectWindow { .. } | DeviceCommand::InspectWindowContextual { .. } => {
                return Err(BackendAdapterError::UnsupportedCommand(
                    DeviceCapability::InspectWindow,
                ));
            }
            DeviceCommand::VerifyUiState { .. } | DeviceCommand::VerifyUiStateContextual { .. } => {
                return Err(BackendAdapterError::UnsupportedCommand(
                    DeviceCapability::VerifyUiState,
                ));
            }
            DeviceCommand::PointerClickAdvanced { .. }
            | DeviceCommand::PointerDragAdvanced { .. }
            | DeviceCommand::TypeTextAdvanced { .. }
            | DeviceCommand::TerminateApplication { .. }
            | DeviceCommand::ActivateWindow { .. }
            | DeviceCommand::SetWindowFrame { .. }
            | DeviceCommand::InvokeMenu { .. }
            | DeviceCommand::KeyboardInput { .. }
            | DeviceCommand::Scroll { .. }
            | DeviceCommand::ClipboardRead { .. }
            | DeviceCommand::ClipboardWrite { .. }
            | DeviceCommand::PointerPosition { .. }
            | DeviceCommand::MovePointer { .. } => {
                return Err(BackendAdapterError::UnsupportedCommand(
                    command.capability(),
                ));
            }
        };
        validate_adapter_result(command, &result)?;
        Ok(result)
    }
}

fn json_u32(value: &Value, key: &'static str) -> Result<u32, BackendAdapterError> {
    let raw = value
        .get(key)
        .and_then(Value::as_u64)
        .ok_or(BackendAdapterError::MalformedResponse(key))?;
    u32::try_from(raw).map_err(|_| BackendAdapterError::NumericOverflow)
}

#[derive(Debug)]
pub enum BackendAdapterError {
    Io(std::io::Error),
    Utf8(std::string::FromUtf8Error),
    Json(serde_json::Error),
    BackendFailed(String),
    MalformedResponse(&'static str),
    NumericOverflow,
    InvalidAdvertisement,
    UnsupportedCapabilitySchema { got: u16 },
    DuplicateCapability,
    UnsupportedCommand(DeviceCapability),
    ResultTypeMismatch,
}

impl fmt::Display for BackendAdapterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for BackendAdapterError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v2_m0::PointerButton;

    struct AlphaFixture;

    impl BackendAdapter for AlphaFixture {
        fn advertisement(&self) -> CapabilityAdvertisement {
            CapabilityAdvertisement {
                backend: "fixture-alpha".into(),
                backend_version: "1".into(),
                platform: "fixture".into(),
                capability_schema_version: CAPABILITY_SCHEMA_VERSION,
                revision: 1,
                supported: vec![DeviceCapability::ListApplications],
            }
        }

        fn execute(
            &mut self,
            command: &DeviceCommand,
        ) -> Result<DeviceResult, BackendAdapterError> {
            match command {
                DeviceCommand::ListApplications => Ok(DeviceResult::Applications { count: 3 }),
                other => Err(BackendAdapterError::UnsupportedCommand(other.capability())),
            }
        }
    }

    struct BetaFixture;

    impl BackendAdapter for BetaFixture {
        fn advertisement(&self) -> CapabilityAdvertisement {
            CapabilityAdvertisement {
                backend: "fixture-beta".into(),
                backend_version: "9".into(),
                platform: "different-fixture".into(),
                capability_schema_version: CAPABILITY_SCHEMA_VERSION,
                revision: 4,
                supported: vec![DeviceCapability::ScreenGeometry],
            }
        }

        fn execute(
            &mut self,
            command: &DeviceCommand,
        ) -> Result<DeviceResult, BackendAdapterError> {
            match command {
                DeviceCommand::ScreenGeometry => Ok(DeviceResult::ScreenGeometry {
                    width_points: 800,
                    height_points: 600,
                    scale_factor_milli: 2000,
                }),
                other => Err(BackendAdapterError::UnsupportedCommand(other.capability())),
            }
        }
    }

    fn assert_conforms<A: BackendAdapter>(
        adapter: &mut A,
        command: DeviceCommand,
    ) -> Result<(), BackendAdapterError> {
        let advertisement = adapter.advertisement();
        validate_adapter_advertisement(&advertisement)?;
        if !advertisement.supports(command.capability()) {
            return Err(BackendAdapterError::UnsupportedCommand(
                command.capability(),
            ));
        }
        let result = adapter.execute(&command)?;
        validate_adapter_result(&command, &result)
    }

    #[test]
    fn multiple_backend_shapes_conform_to_the_same_semantic_contract() {
        assert_conforms(&mut AlphaFixture, DeviceCommand::ListApplications).unwrap();
        assert_conforms(&mut BetaFixture, DeviceCommand::ScreenGeometry).unwrap();
    }

    #[test]
    fn advertisement_rejects_duplicate_or_unknown_schema_surface() {
        let mut duplicate = AlphaFixture.advertisement();
        duplicate.supported.push(DeviceCapability::ListApplications);
        assert!(matches!(
            validate_adapter_advertisement(&duplicate),
            Err(BackendAdapterError::DuplicateCapability)
        ));
        duplicate.supported.pop();
        duplicate.capability_schema_version += 1;
        assert!(matches!(
            validate_adapter_advertisement(&duplicate),
            Err(BackendAdapterError::UnsupportedCapabilitySchema { .. })
        ));
    }

    #[test]
    fn result_type_mismatch_fails_closed() {
        assert!(matches!(
            validate_adapter_result(
                &DeviceCommand::PointerClick {
                    x: 10,
                    y: 20,
                    button: PointerButton::Left,
                },
                &DeviceResult::Applications { count: 1 },
            ),
            Err(BackendAdapterError::ResultTypeMismatch)
        ));
    }
}
