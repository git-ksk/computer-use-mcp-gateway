//! V2-M1 asynchronous backend adapter with explicit cancellation outcomes.
//!
//! Cua-specific MCP tool names and response shapes terminate in this module.
//! A propagated cancellation is deliberately reported as *indeterminate* rather
//! than as proof that a desktop action did not happen.

use crate::backend::{
    BackendCallCancelled, BackendCallResponseLost, BackendCallTimedOut, ComputerUseBackend,
    cua::CuaBackend,
};
use crate::v2_browser_execute::{
    BrowserExecutionError, BrowserExecutionOutcome, BrowserRefusalReason, execute_cua_browser,
    execute_cua_browser_download, execute_cua_browser_upload,
};
use crate::v2_browser_staging::{PreparedBrowserDownload, ResolvedStagedUpload};
use crate::v2_m0::{
    CAPABILITY_SCHEMA_VERSION, CapabilityAdvertisement, DeviceCapability, DeviceCommand,
    DeviceResult, InputDeliveryMode, InputTarget, KeyboardModifier, MAX_CLIPBOARD_TEXT_BYTES,
    MAX_CLIPBOARD_TYPE_BYTES, MAX_CLIPBOARD_TYPES, MAX_KEYBOARD_MODIFIERS, MAX_MENU_PATH_SEGMENTS,
    MAX_MENU_SEGMENT_BYTES, MAX_SCREENSHOT_BYTES, MAX_TYPE_TEXT_BYTES, MAX_UI_ELEMENTS,
    MAX_UI_PREDICATES, MAX_UI_QUERY_BYTES, MAX_UI_REF_BYTES, MAX_UI_TEXT_BYTES, MAX_WINDOW_RESULTS,
    PointerTarget, ScrollDirection, ScrollGranularity, ScrollTarget, UiElement, UiElementAction,
    UiElementSelector, UiImage, UiPredicate, UiPredicateResult, UiRect, UiRole, VerificationStatus,
    WindowInfo,
};
use crate::v2_m0_transport::CancellationDisposition;
use crate::v2_observability::SafeErrorCode;
use anyhow::Error as AnyError;
use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use rmcp::model::{CallToolResult, JsonObject};
use serde_json::Value;
use std::time::Duration;
use std::{collections::HashMap, fmt};
use tokio::sync::watch;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendExecutionOutcome {
    Completed(DeviceResult),
    CancellationPropagatedIndeterminate,
    TimedOutIndeterminate,
    BackendOutcomeIndeterminate,
}

impl BackendExecutionOutcome {
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Completed(_) => "completed",
            Self::CancellationPropagatedIndeterminate => "cancellation_propagated_indeterminate",
            Self::TimedOutIndeterminate => "timed_out_indeterminate",
            Self::BackendOutcomeIndeterminate => "backend_outcome_indeterminate",
        }
    }

    pub fn cancellation_disposition(&self) -> Option<CancellationDisposition> {
        match self {
            Self::Completed(_) | Self::BackendOutcomeIndeterminate => None,
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

    /// Tear down backend-owned state for a CUMG interaction context. This is
    /// executor lifecycle, not a northbound semantic capability. Stateless
    /// backends may keep the default no-op implementation.
    async fn end_interaction_session(&self, _context_id: &str) -> Result<(), M1BackendError> {
        Ok(())
    }

    async fn execute(
        &self,
        command: &DeviceCommand,
        cancellation: watch::Receiver<bool>,
    ) -> Result<BackendExecutionOutcome, M1BackendError>;

    async fn execute_browser_upload(
        &self,
        _command: &crate::v2_browser_runtime::BrowserBackendCommand,
        _files: &[ResolvedStagedUpload],
        _cancellation: watch::Receiver<bool>,
    ) -> Result<BackendExecutionOutcome, M1BackendError> {
        Err(M1BackendError::UnsupportedCommand(
            DeviceCapability::BrowserUploadFile,
        ))
    }

    async fn execute_browser_download(
        &self,
        _command: &crate::v2_browser_runtime::BrowserBackendCommand,
        _prepared: &PreparedBrowserDownload,
        _cancellation: watch::Receiver<bool>,
    ) -> Result<BackendExecutionOutcome, M1BackendError> {
        Err(M1BackendError::UnsupportedCommand(
            DeviceCapability::BrowserDownload,
        ))
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
        let backend_version = backend_version.into();
        let backend = CuaBackend::new(
            command,
            args,
            connect_timeout,
            tool_timeout,
            reconnect_attempts,
            reconnect_backoff,
        );
        let backend = if backend_version == "external" {
            backend
        } else {
            backend.with_expected_server_version(backend_version.clone())
        };
        Self {
            backend,
            backend_version,
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
                DeviceCapability::ListWindows,
                DeviceCapability::LaunchApplication,
                DeviceCapability::InspectWindow,
                DeviceCapability::VerifyUiState,
                DeviceCapability::TerminateApplication,
                DeviceCapability::ActivateWindow,
                DeviceCapability::SetWindowFrame,
                DeviceCapability::InvokeMenu,
                DeviceCapability::KeyboardInput,
                DeviceCapability::Scroll,
                DeviceCapability::ClipboardRead,
                DeviceCapability::ClipboardWrite,
                DeviceCapability::PointerPosition,
                DeviceCapability::MovePointer,
                DeviceCapability::SetUiValue,
                DeviceCapability::CaptureRegion,
                DeviceCapability::DesktopScope,
                DeviceCapability::BrowserInspect,
                DeviceCapability::BrowserPrepare,
                DeviceCapability::BrowserNavigate,
                DeviceCapability::BrowserClick,
                DeviceCapability::BrowserType,
                DeviceCapability::BrowserDialog,
                DeviceCapability::BrowserPointer,
                DeviceCapability::BrowserUploadFile,
                DeviceCapability::BrowserDownload,
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

    pub async fn end_interaction_session(&self, context_id: &str) -> Result<(), M1BackendError> {
        let valid = context_id.len() == 36
            && context_id.starts_with("ctx_")
            && context_id[4..]
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
        if !valid {
            return Err(M1BackendError::InvalidRequest(
                "invalid interaction context id",
            ));
        }
        let (_cancel_tx, cancellation) = watch::channel(false);
        let arguments = serde_json::json!({"session": context_id})
            .as_object()
            .cloned();
        let result = self
            .backend
            .call_tool("end_session", arguments, cancellation)
            .await
            .map_err(|error| {
                crate::v2_observability::backend_failure(
                    crate::v2_observability::BackendFailureReason::Tool,
                );
                M1BackendError::Backend(error)
            })?;
        if result.is_error == Some(true) {
            crate::v2_observability::backend_failure(
                crate::v2_observability::BackendFailureReason::Tool,
            );
            return Err(M1BackendError::BackendToolError);
        }
        Ok(())
    }

    pub async fn execute_browser_upload(
        &self,
        command: &crate::v2_browser_runtime::BrowserBackendCommand,
        files: &[ResolvedStagedUpload],
        cancellation: watch::Receiver<bool>,
    ) -> Result<BackendExecutionOutcome, M1BackendError> {
        match execute_cua_browser_upload(&self.backend, command, files, cancellation).await {
            Ok(BrowserExecutionOutcome::Completed(result)) => {
                if !result.matches_command(command) {
                    return Err(M1BackendError::MalformedResponse(
                        "browser upload result type did not match command",
                    ));
                }
                Ok(BackendExecutionOutcome::Completed(DeviceResult::Browser {
                    result,
                }))
            }
            Ok(BrowserExecutionOutcome::CancellationPropagatedIndeterminate) => {
                Ok(BackendExecutionOutcome::CancellationPropagatedIndeterminate)
            }
            Ok(BrowserExecutionOutcome::TimedOutIndeterminate) => {
                Ok(BackendExecutionOutcome::TimedOutIndeterminate)
            }
            Ok(BrowserExecutionOutcome::BackendOutcomeIndeterminate) => {
                Ok(BackendExecutionOutcome::BackendOutcomeIndeterminate)
            }
            Err(BrowserExecutionError::InvalidRequest(message)) => {
                Err(M1BackendError::InvalidRequest(message))
            }
            Err(BrowserExecutionError::Backend(error)) => Err(M1BackendError::Backend(error)),
            Err(BrowserExecutionError::BackendToolError) => Err(M1BackendError::BackendToolError),
            Err(BrowserExecutionError::BackendRefused(reason)) => {
                Err(M1BackendError::BrowserRefused(reason))
            }
            Err(BrowserExecutionError::Normalize(_)) => Err(M1BackendError::MalformedResponse(
                "browser upload result normalization failed",
            )),
            Err(BrowserExecutionError::InvalidResult(message)) => {
                Err(M1BackendError::MalformedResponse(message))
            }
            Err(BrowserExecutionError::UnsupportedTransferBoundary) => Err(
                M1BackendError::UnsupportedCommand(DeviceCapability::BrowserUploadFile),
            ),
        }
    }

    pub async fn execute_browser_download(
        &self,
        command: &crate::v2_browser_runtime::BrowserBackendCommand,
        prepared: &PreparedBrowserDownload,
        cancellation: watch::Receiver<bool>,
    ) -> Result<BackendExecutionOutcome, M1BackendError> {
        match execute_cua_browser_download(
            &self.backend,
            command,
            prepared.canonical_root(),
            cancellation,
        )
        .await
        {
            Ok(BrowserExecutionOutcome::Completed(result)) => {
                if !matches!(
                    result,
                    crate::v2_browser_runtime::BrowserBackendResult::DownloadStaged { .. }
                ) || !result.matches_command(command)
                {
                    return Err(M1BackendError::MalformedResponse(
                        "browser download result type did not match command",
                    ));
                }
                Ok(BackendExecutionOutcome::Completed(DeviceResult::Browser {
                    result,
                }))
            }
            Ok(BrowserExecutionOutcome::CancellationPropagatedIndeterminate) => {
                Ok(BackendExecutionOutcome::CancellationPropagatedIndeterminate)
            }
            Ok(BrowserExecutionOutcome::TimedOutIndeterminate) => {
                Ok(BackendExecutionOutcome::TimedOutIndeterminate)
            }
            Ok(BrowserExecutionOutcome::BackendOutcomeIndeterminate) => {
                Ok(BackendExecutionOutcome::BackendOutcomeIndeterminate)
            }
            Err(BrowserExecutionError::InvalidRequest(message)) => {
                Err(M1BackendError::InvalidRequest(message))
            }
            Err(BrowserExecutionError::Backend(error)) => Err(M1BackendError::Backend(error)),
            Err(BrowserExecutionError::BackendToolError) => Err(M1BackendError::BackendToolError),
            Err(BrowserExecutionError::BackendRefused(reason)) => {
                Err(M1BackendError::BrowserRefused(reason))
            }
            Err(BrowserExecutionError::Normalize(_)) => Err(M1BackendError::MalformedResponse(
                "browser download result normalization failed",
            )),
            Err(BrowserExecutionError::InvalidResult(message)) => {
                Err(M1BackendError::MalformedResponse(message))
            }
            Err(BrowserExecutionError::UnsupportedTransferBoundary) => Err(
                M1BackendError::UnsupportedCommand(DeviceCapability::BrowserDownload),
            ),
        }
    }

    pub async fn execute(
        &self,
        command: &DeviceCommand,
        cancellation: watch::Receiver<bool>,
    ) -> Result<BackendExecutionOutcome, M1BackendError> {
        if let DeviceCommand::Browser { command } = command {
            return match execute_cua_browser(&self.backend, command, cancellation).await {
                Ok(BrowserExecutionOutcome::Completed(result)) => {
                    if !result.matches_command(command) {
                        return Err(M1BackendError::MalformedResponse(
                            "browser result type did not match command",
                        ));
                    }
                    Ok(BackendExecutionOutcome::Completed(DeviceResult::Browser {
                        result,
                    }))
                }
                Ok(BrowserExecutionOutcome::CancellationPropagatedIndeterminate) => {
                    Ok(BackendExecutionOutcome::CancellationPropagatedIndeterminate)
                }
                Ok(BrowserExecutionOutcome::TimedOutIndeterminate) => {
                    Ok(BackendExecutionOutcome::TimedOutIndeterminate)
                }
                Ok(BrowserExecutionOutcome::BackendOutcomeIndeterminate) => {
                    Ok(BackendExecutionOutcome::BackendOutcomeIndeterminate)
                }
                Err(BrowserExecutionError::InvalidRequest(message)) => {
                    Err(M1BackendError::InvalidRequest(message))
                }
                Err(BrowserExecutionError::Backend(error)) => Err(M1BackendError::Backend(error)),
                Err(BrowserExecutionError::BackendToolError) => {
                    Err(M1BackendError::BackendToolError)
                }
                Err(BrowserExecutionError::BackendRefused(reason)) => {
                    Err(M1BackendError::BrowserRefused(reason))
                }
                Err(BrowserExecutionError::Normalize(_)) => Err(M1BackendError::MalformedResponse(
                    "browser result normalization failed",
                )),
                Err(BrowserExecutionError::InvalidResult(message)) => {
                    Err(M1BackendError::MalformedResponse(message))
                }
                Err(BrowserExecutionError::UnsupportedTransferBoundary) => Err(
                    M1BackendError::UnsupportedCommand(command_capability(command)),
                ),
            };
        }
        let context_session_to_refresh = match command {
            DeviceCommand::ExpandInteractionScope { context_id, .. }
            | DeviceCommand::VerifyUiStateContextual { context_id, .. } => Some(context_id),
            _ => None,
        };
        if let Some(context_id) = context_session_to_refresh {
            // Cua's session idle TTL is shorter than CUMG's InteractionContext lifetime.
            // A Human handoff can therefore reclaim the backend session while the
            // CUMG context remains valid. start_session is idempotent and revives
            // only this already-admitted context at window scope before verification.
            let start_args = serde_json::json!({
                "session": context_id,
                "capture_scope": "auto",
            })
            .as_object()
            .cloned();
            let started = match self
                .backend
                .call_tool("start_session", start_args, cancellation.clone())
                .await
            {
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
                Err(error) if error.downcast_ref::<BackendCallResponseLost>().is_some() => {
                    crate::v2_observability::backend_failure(
                        crate::v2_observability::BackendFailureReason::AmbiguousOutcome,
                    );
                    return Ok(BackendExecutionOutcome::BackendOutcomeIndeterminate);
                }
                Err(error) => {
                    crate::v2_observability::backend_failure(
                        crate::v2_observability::BackendFailureReason::Tool,
                    );
                    return Err(M1BackendError::Backend(error));
                }
            };
            if started.is_error == Some(true) {
                crate::v2_observability::backend_failure(
                    crate::v2_observability::BackendFailureReason::AmbiguousOutcome,
                );
                return Ok(BackendExecutionOutcome::BackendOutcomeIndeterminate);
            }
        }
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
            Err(error) if error.downcast_ref::<BackendCallResponseLost>().is_some() => {
                crate::v2_observability::backend_failure(
                    crate::v2_observability::BackendFailureReason::AmbiguousOutcome,
                );
                if command.is_read_only() {
                    return Err(M1BackendError::Backend(error));
                }
                return Ok(BackendExecutionOutcome::BackendOutcomeIndeterminate);
            }
            Err(error) => {
                crate::v2_observability::backend_failure(
                    crate::v2_observability::BackendFailureReason::Tool,
                );
                return Err(M1BackendError::Backend(error));
            }
        };
        if raw.is_error == Some(true) {
            crate::v2_observability::backend_failure(if command.is_read_only() {
                crate::v2_observability::BackendFailureReason::Tool
            } else {
                crate::v2_observability::BackendFailureReason::AmbiguousOutcome
            });
            if command.is_read_only() {
                return Err(M1BackendError::BackendToolError);
            }
            return Ok(BackendExecutionOutcome::BackendOutcomeIndeterminate);
        }
        let result: Result<DeviceResult, M1BackendError> = (|| {
            Ok(match command {
                DeviceCommand::PointerClick { .. } | DeviceCommand::PointerClickAdvanced { .. } => {
                    DeviceResult::PointerClickCompleted
                }
                DeviceCommand::PointerDrag { .. } | DeviceCommand::PointerDragAdvanced { .. } => {
                    DeviceResult::PointerDragCompleted
                }
                DeviceCommand::TypeText { .. } | DeviceCommand::TypeTextAdvanced { .. } => {
                    DeviceResult::TypeTextCompleted
                }
                DeviceCommand::TerminateApplication { process_id } => {
                    DeviceResult::ApplicationTerminated {
                        process_id: *process_id,
                    }
                }
                DeviceCommand::ActivateWindow { .. } => {
                    normalize_window_activation_result(command, &raw)?
                }
                DeviceCommand::SetWindowFrame {
                    process_id,
                    window_id,
                    bounds,
                    ..
                } => DeviceResult::WindowFrameSet {
                    process_id: *process_id,
                    window_id: *window_id,
                    bounds: bounds.clone(),
                },
                DeviceCommand::InvokeMenu { .. } => DeviceResult::MenuInvoked,
                DeviceCommand::KeyboardInput { .. } => DeviceResult::KeyboardInputCompleted,
                DeviceCommand::Scroll { .. } => DeviceResult::ScrollCompleted,
                DeviceCommand::ClipboardRead { .. } => normalize_clipboard_read_result(&raw)?,
                DeviceCommand::ClipboardWrite { .. } => normalize_clipboard_write_result(&raw)?,
                DeviceCommand::PointerPosition { .. } => normalize_pointer_position_result(&raw)?,
                DeviceCommand::MovePointer { .. } => DeviceResult::PointerMoveCompleted,
                DeviceCommand::SetUiValue { .. } => DeviceResult::UiValueSet,
                DeviceCommand::CaptureRegion { .. } => DeviceResult::RegionCaptured {
                    image: normalize_region_capture_result(&raw)?,
                },
                DeviceCommand::ExpandInteractionScope { .. } => {
                    DeviceResult::InteractionScopeExpanded
                }
                DeviceCommand::Screenshot | DeviceCommand::ScreenshotContextual { .. } => {
                    normalize_screenshot_result(&raw)?
                }
                DeviceCommand::InspectWindow { .. }
                | DeviceCommand::InspectWindowContextual { .. } => {
                    normalize_window_snapshot_result(command, &raw)?
                }
                DeviceCommand::VerifyUiState { .. }
                | DeviceCommand::VerifyUiStateContextual { .. } => {
                    normalize_ui_verification_result(command, &raw)?
                }
                _ => {
                    let value = structured_value(&raw)?;
                    normalize_result(command, &value)?
                }
            })
        })();
        match result {
            Ok(result) => Ok(BackendExecutionOutcome::Completed(result)),
            Err(_error) if !command.is_read_only() => {
                crate::v2_observability::backend_failure(
                    crate::v2_observability::BackendFailureReason::AmbiguousOutcome,
                );
                Ok(BackendExecutionOutcome::BackendOutcomeIndeterminate)
            }
            Err(error) => Err(error),
        }
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

    async fn end_interaction_session(&self, context_id: &str) -> Result<(), M1BackendError> {
        CuaMcpAdapter::end_interaction_session(self, context_id).await
    }

    async fn execute(
        &self,
        command: &DeviceCommand,
        cancellation: watch::Receiver<bool>,
    ) -> Result<BackendExecutionOutcome, M1BackendError> {
        CuaMcpAdapter::execute(self, command, cancellation).await
    }

    async fn execute_browser_upload(
        &self,
        command: &crate::v2_browser_runtime::BrowserBackendCommand,
        files: &[ResolvedStagedUpload],
        cancellation: watch::Receiver<bool>,
    ) -> Result<BackendExecutionOutcome, M1BackendError> {
        CuaMcpAdapter::execute_browser_upload(self, command, files, cancellation).await
    }

    async fn execute_browser_download(
        &self,
        command: &crate::v2_browser_runtime::BrowserBackendCommand,
        prepared: &PreparedBrowserDownload,
        cancellation: watch::Receiver<bool>,
    ) -> Result<BackendExecutionOutcome, M1BackendError> {
        CuaMcpAdapter::execute_browser_download(self, command, prepared, cancellation).await
    }
}

fn map_command(
    command: &DeviceCommand,
) -> Result<(&'static str, Option<JsonObject>), M1BackendError> {
    match command {
        DeviceCommand::ListApplications => Ok(("list_apps", None)),
        DeviceCommand::ScreenGeometry => Ok(("get_screen_size", None)),
        DeviceCommand::Screenshot => Ok(("get_desktop_state", None)),
        DeviceCommand::ScreenshotContextual { context_id } => Ok((
            "get_desktop_state",
            serde_json::json!({"session": context_id})
                .as_object()
                .cloned(),
        )),
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
        DeviceCommand::PointerClickAdvanced {
            context_id,
            target,
            button,
            click_count,
            action,
            modifiers,
            delivery,
        } => {
            if !(1..=3).contains(click_count) {
                return Err(M1BackendError::InvalidRequest(
                    "click count must be within 1..=3",
                ));
            }
            let modifiers = map_keyboard_modifiers(modifiers)?;
            if !modifiers.is_empty() && *delivery != InputDeliveryMode::Foreground {
                return Err(M1BackendError::InvalidRequest(
                    "modified click requires explicit foreground delivery",
                ));
            }
            let mut args = pointer_target_arguments(target, *delivery)?;
            args.insert(
                "delivery_mode".into(),
                serde_json::json!(delivery_mode_name(*delivery)),
            );
            insert_context_session(&mut args, context_id.as_deref())?;
            if matches!(target, PointerTarget::Element { .. }) && *click_count == 2 {
                if *button != crate::v2_m0::PointerButton::Left
                    || action.is_some()
                    || !modifiers.is_empty()
                {
                    return Err(M1BackendError::InvalidRequest(
                        "invalid element double click options",
                    ));
                }
                return Ok(("double_click", Some(args)));
            }
            if matches!(target, PointerTarget::Element { .. }) {
                if *click_count != 1 {
                    return Err(M1BackendError::InvalidRequest(
                        "element click count must be 1 or 2",
                    ));
                }
                if let Some(action) = action {
                    args.insert(
                        "action".into(),
                        serde_json::json!(ui_element_action_name(*action)),
                    );
                }
            } else if action.is_some() {
                return Err(M1BackendError::InvalidRequest(
                    "UI element action requires an element target",
                ));
            }
            args.insert(
                "button".into(),
                serde_json::json!(pointer_button_name(*button)),
            );
            args.insert("count".into(), serde_json::json!(click_count));
            args.insert("modifier".into(), serde_json::json!(modifiers));
            Ok(("click", Some(args)))
        }
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
        DeviceCommand::PointerDragAdvanced {
            context_id,
            from,
            to,
            button,
            modifiers,
            delivery,
            duration_ms,
            steps,
        } => {
            if *duration_ms > 10_000 || !(1..=200).contains(steps) {
                return Err(M1BackendError::InvalidRequest(
                    "invalid advanced pointer drag bounds",
                ));
            }
            let modifiers = map_keyboard_modifiers(modifiers)?;
            let mut args = drag_target_arguments(from, to, *delivery)?;
            args.insert(
                "button".into(),
                serde_json::json!(pointer_button_name(*button)),
            );
            args.insert("modifier".into(), serde_json::json!(modifiers));
            args.insert(
                "delivery_mode".into(),
                serde_json::json!(delivery_mode_name(*delivery)),
            );
            args.insert("duration_ms".into(), serde_json::json!(duration_ms));
            args.insert("steps".into(), serde_json::json!(steps));
            insert_context_session(&mut args, context_id.as_deref())?;
            Ok(("drag", Some(args)))
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
        DeviceCommand::TypeTextAdvanced {
            context_id,
            text,
            target,
            delivery,
            delay_ms,
        } => {
            if text.is_empty() || text.len() > MAX_TYPE_TEXT_BYTES || *delay_ms > 200 {
                return Err(M1BackendError::InvalidRequest(
                    "invalid targeted text input bounds",
                ));
            }
            let mut args = input_target_arguments(target, *delivery)?;
            args.insert("text".into(), serde_json::json!(text));
            args.insert("delay_ms".into(), serde_json::json!(delay_ms));
            args.insert(
                "delivery_mode".into(),
                serde_json::json!(delivery_mode_name(*delivery)),
            );
            insert_context_session(&mut args, context_id.as_deref())?;
            Ok(("type_text", Some(args)))
        }
        DeviceCommand::ListWindows {
            process_id,
            on_screen_only,
        } => Ok((
            "list_windows",
            serde_json::json!({
                "pid": process_id,
                "on_screen_only": on_screen_only,
            })
            .as_object()
            .cloned(),
        )),
        DeviceCommand::LaunchApplication {
            identifier,
            name,
            targets,
            new_instance,
        } => {
            validate_application_selector(identifier.as_deref(), name.as_deref(), targets)?;
            Ok((
                "launch_app",
                serde_json::json!({
                    "bundle_id": identifier,
                    "name": name,
                    "urls": targets,
                    "creates_new_application_instance": new_instance,
                })
                .as_object()
                .cloned(),
            ))
        }
        DeviceCommand::InspectWindow {
            process_id,
            window_id,
            query,
            max_elements,
            max_depth,
            include_screenshot,
        } => map_inspect_window(
            None,
            *process_id,
            *window_id,
            query,
            *max_elements,
            *max_depth,
            *include_screenshot,
        ),
        DeviceCommand::InspectWindowContextual {
            context_id,
            process_id,
            window_id,
            query,
            max_elements,
            max_depth,
            include_screenshot,
        } => map_inspect_window(
            Some(context_id.as_str()),
            *process_id,
            *window_id,
            query,
            *max_elements,
            *max_depth,
            *include_screenshot,
        ),
        DeviceCommand::VerifyUiState {
            process_id,
            window_id,
            predicates,
            timeout_ms,
            stable_samples,
            include_screenshot,
        } => map_verify_ui_state(
            None,
            *process_id,
            *window_id,
            predicates,
            *timeout_ms,
            *stable_samples,
            *include_screenshot,
        ),
        DeviceCommand::VerifyUiStateContextual {
            context_id,
            process_id,
            window_id,
            predicates,
            timeout_ms,
            stable_samples,
            include_screenshot,
        } => map_verify_ui_state(
            Some(context_id.as_str()),
            *process_id,
            *window_id,
            predicates,
            *timeout_ms,
            *stable_samples,
            *include_screenshot,
        ),
        DeviceCommand::TerminateApplication { process_id } => {
            if *process_id == 0 {
                return Err(M1BackendError::InvalidRequest(
                    "process_id must be positive",
                ));
            }
            Ok((
                "kill_app",
                serde_json::json!({"pid": process_id}).as_object().cloned(),
            ))
        }
        DeviceCommand::ActivateWindow {
            process_id,
            window_id,
        } => {
            if *process_id == 0 || window_id == &Some(0) {
                return Err(M1BackendError::InvalidRequest("invalid activation target"));
            }
            let mut args = JsonObject::new();
            args.insert("pid".into(), serde_json::json!(process_id));
            if let Some(window_id) = window_id {
                args.insert("window_id".into(), serde_json::json!(window_id));
            }
            Ok(("bring_to_front", Some(args)))
        }
        DeviceCommand::SetWindowFrame {
            context_id,
            process_id,
            window_id,
            bounds,
        } => {
            validate_window_target(*process_id, *window_id)?;
            if bounds.width == 0 || bounds.height == 0 {
                return Err(M1BackendError::InvalidRequest(
                    "window frame must be positive",
                ));
            }
            let mut args = serde_json::json!({
                "pid": process_id,
                "window_id": window_id,
                "x": bounds.x,
                "y": bounds.y,
                "width": bounds.width,
                "height": bounds.height,
            })
            .as_object()
            .cloned()
            .expect("object literal");
            insert_context_session(&mut args, context_id.as_deref())?;
            Ok(("set_window_frame", Some(args)))
        }
        DeviceCommand::InvokeMenu {
            context_id,
            process_id,
            window_id,
            path,
        } => {
            validate_window_target(*process_id, *window_id)?;
            if path.is_empty()
                || path.len() > MAX_MENU_PATH_SEGMENTS
                || path.iter().any(|segment| {
                    segment.trim().is_empty() || segment.len() > MAX_MENU_SEGMENT_BYTES
                })
            {
                return Err(M1BackendError::InvalidRequest(
                    "invalid application menu path",
                ));
            }
            let mut args = serde_json::json!({
                "pid": process_id,
                "window_id": window_id,
                "path": path,
            })
            .as_object()
            .cloned()
            .expect("object literal");
            insert_context_session(&mut args, context_id.as_deref())?;
            Ok(("invoke_menu", Some(args)))
        }
        DeviceCommand::KeyboardInput {
            context_id,
            key,
            modifiers,
            target,
            delivery,
        } => {
            validate_keyboard_key(key)?;
            let modifiers = map_keyboard_modifiers(modifiers)?;
            let mut args = input_target_arguments(target, *delivery)?;
            args.insert("key".into(), serde_json::json!(key));
            args.insert("modifiers".into(), serde_json::json!(modifiers));
            args.insert(
                "delivery_mode".into(),
                serde_json::json!(delivery_mode_name(*delivery)),
            );
            insert_context_session(&mut args, context_id.as_deref())?;
            Ok(("press_key", Some(args)))
        }
        DeviceCommand::Scroll {
            context_id,
            direction,
            granularity,
            amount,
            target,
            delivery,
        } => {
            if !(1..=50).contains(amount) {
                return Err(M1BackendError::InvalidRequest(
                    "scroll amount must be within 1..=50",
                ));
            }
            let mut args = scroll_target_arguments(target, *delivery)?;
            args.insert(
                "direction".into(),
                serde_json::json!(scroll_direction_name(*direction)),
            );
            args.insert(
                "by".into(),
                serde_json::json!(scroll_granularity_name(*granularity)),
            );
            args.insert("amount".into(), serde_json::json!(amount));
            args.insert(
                "delivery_mode".into(),
                serde_json::json!(delivery_mode_name(*delivery)),
            );
            insert_context_session(&mut args, context_id.as_deref())?;
            Ok(("scroll", Some(args)))
        }
        DeviceCommand::ClipboardRead {
            context_id,
            include_text,
        } => {
            let mut args = serde_json::json!({"include_text": include_text})
                .as_object()
                .cloned()
                .expect("object literal");
            insert_context_session(&mut args, context_id.as_deref())?;
            Ok(("clipboard_read", Some(args)))
        }
        DeviceCommand::ClipboardWrite { context_id, text } => {
            if text.len() > MAX_CLIPBOARD_TEXT_BYTES {
                return Err(M1BackendError::InvalidRequest(
                    "clipboard text is too large",
                ));
            }
            let mut args = serde_json::json!({"text": text})
                .as_object()
                .cloned()
                .expect("object literal");
            insert_context_session(&mut args, context_id.as_deref())?;
            Ok(("clipboard_write", Some(args)))
        }
        DeviceCommand::PointerPosition { context_id } => {
            let mut args = JsonObject::new();
            insert_context_session(&mut args, context_id.as_deref())?;
            Ok(("get_cursor_position", (!args.is_empty()).then_some(args)))
        }
        DeviceCommand::MovePointer { context_id, x, y } => Ok((
            "move_cursor",
            serde_json::json!({"session": context_id, "x": x, "y": y, "scope": "desktop"})
                .as_object()
                .cloned(),
        )),
        DeviceCommand::SetUiValue {
            context_id,
            process_id,
            window_id,
            element_ref,
            value,
        } => {
            validate_window_target(*process_id, *window_id)?;
            if element_ref.is_empty()
                || element_ref.len() > MAX_UI_REF_BYTES
                || value.len() > MAX_TYPE_TEXT_BYTES
            {
                return Err(M1BackendError::InvalidRequest("invalid UI value request"));
            }
            Ok((
                "set_value",
                serde_json::json!({
                    "session": context_id,
                    "pid": process_id,
                    "window_id": window_id,
                    "element_token": element_ref,
                    "value": value,
                })
                .as_object()
                .cloned(),
            ))
        }
        DeviceCommand::CaptureRegion {
            context_id,
            process_id,
            window_id,
            bounds,
        } => {
            validate_window_target(*process_id, *window_id)?;
            if bounds.width == 0 || bounds.height == 0 {
                return Err(M1BackendError::InvalidRequest(
                    "capture region must be positive",
                ));
            }
            let x2 = i64::from(bounds.x) + i64::from(bounds.width);
            let y2 = i64::from(bounds.y) + i64::from(bounds.height);
            if x2 > i64::from(i32::MAX) || y2 > i64::from(i32::MAX) {
                return Err(M1BackendError::InvalidRequest(
                    "capture region overflows coordinates",
                ));
            }
            let mut args = serde_json::json!({
                "pid": process_id,
                "window_id": window_id,
                "x1": bounds.x,
                "y1": bounds.y,
                "x2": x2 as i32,
                "y2": y2 as i32,
            })
            .as_object()
            .cloned()
            .expect("object literal");
            insert_context_session(&mut args, context_id.as_deref())?;
            Ok(("zoom", Some(args)))
        }
        DeviceCommand::ExpandInteractionScope { context_id, reason } => {
            if reason.is_empty() || reason.len() > 200 {
                return Err(M1BackendError::InvalidRequest(
                    "invalid scope escalation reason",
                ));
            }
            Ok((
                "escalate_session",
                serde_json::json!({
                    "session": context_id,
                    "reason": "other",
                    "detail": reason,
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
        DeviceCommand::StageBrowserUploadFile { .. } | DeviceCommand::Browser { .. } => {
            Err(M1BackendError::UnsupportedCommand(command.capability()))
        }
    }
}

fn command_capability(
    command: &crate::v2_browser_runtime::BrowserBackendCommand,
) -> DeviceCapability {
    match command.capability() {
        crate::v2_browser_runtime::BrowserRuntimeCapability::Inspect => {
            DeviceCapability::BrowserInspect
        }
        crate::v2_browser_runtime::BrowserRuntimeCapability::Prepare => {
            DeviceCapability::BrowserPrepare
        }
        crate::v2_browser_runtime::BrowserRuntimeCapability::Navigate => {
            DeviceCapability::BrowserNavigate
        }
        crate::v2_browser_runtime::BrowserRuntimeCapability::Click => {
            DeviceCapability::BrowserClick
        }
        crate::v2_browser_runtime::BrowserRuntimeCapability::Type => DeviceCapability::BrowserType,
        crate::v2_browser_runtime::BrowserRuntimeCapability::Dialog => {
            DeviceCapability::BrowserDialog
        }
        crate::v2_browser_runtime::BrowserRuntimeCapability::Pointer => {
            DeviceCapability::BrowserPointer
        }
        crate::v2_browser_runtime::BrowserRuntimeCapability::UploadFile => {
            DeviceCapability::BrowserUploadFile
        }
        crate::v2_browser_runtime::BrowserRuntimeCapability::Download => {
            DeviceCapability::BrowserDownload
        }
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
        | DeviceCommand::PointerClickAdvanced { .. }
        | DeviceCommand::PointerDrag { .. }
        | DeviceCommand::PointerDragAdvanced { .. }
        | DeviceCommand::TypeText { .. }
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
        | DeviceCommand::MovePointer { .. }
        | DeviceCommand::SetUiValue { .. }
        | DeviceCommand::CaptureRegion { .. }
        | DeviceCommand::ExpandInteractionScope { .. } => Err(M1BackendError::MalformedResponse(
            "specialized Computer Use result should not use generic normalization",
        )),
        DeviceCommand::Screenshot | DeviceCommand::ScreenshotContextual { .. } => {
            Err(M1BackendError::MalformedResponse(
                "screenshot result requires image-content normalization",
            ))
        }
        DeviceCommand::ListWindows { .. } => normalize_windows_result(value),
        DeviceCommand::LaunchApplication { .. } => normalize_application_launch_result(value),
        DeviceCommand::InspectWindow { .. } | DeviceCommand::InspectWindowContextual { .. } => {
            Err(M1BackendError::MalformedResponse(
                "window snapshot requires full tool-result normalization",
            ))
        }
        DeviceCommand::VerifyUiState { .. } | DeviceCommand::VerifyUiStateContextual { .. } => {
            Err(M1BackendError::MalformedResponse(
                "UI verification requires full tool-result normalization",
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
        DeviceCommand::StageBrowserUploadFile { .. } | DeviceCommand::Browser { .. } => {
            Err(M1BackendError::UnsupportedCommand(command.capability()))
        }
    }
}

fn map_inspect_window(
    context_id: Option<&str>,
    process_id: u32,
    window_id: u64,
    query: &Option<String>,
    max_elements: u32,
    max_depth: u32,
    include_screenshot: bool,
) -> Result<(&'static str, Option<JsonObject>), M1BackendError> {
    validate_window_target(process_id, window_id)?;
    if query
        .as_ref()
        .is_some_and(|value| value.len() > MAX_UI_QUERY_BYTES)
        || max_elements == 0
        || (max_elements as usize) > MAX_UI_ELEMENTS
        || max_depth == 0
        || max_depth > 64
    {
        return Err(M1BackendError::InvalidRequest(
            "invalid window inspection bounds",
        ));
    }
    let mut args = serde_json::json!({
        "pid": process_id,
        "window_id": window_id,
        "query": query,
        "max_elements": max_elements,
        "max_depth": max_depth,
        "include_screenshot": include_screenshot,
    })
    .as_object()
    .cloned()
    .expect("object literal");
    insert_context_session(&mut args, context_id)?;
    Ok(("get_window_state", Some(args)))
}

fn map_verify_ui_state(
    context_id: Option<&str>,
    process_id: u32,
    window_id: u64,
    predicates: &[UiPredicate],
    timeout_ms: u64,
    stable_samples: u8,
    include_screenshot: bool,
) -> Result<(&'static str, Option<JsonObject>), M1BackendError> {
    validate_window_target(process_id, window_id)?;
    if predicates.is_empty()
        || predicates.len() > MAX_UI_PREDICATES
        || timeout_ms > 10_000
        || !(1..=5).contains(&stable_samples)
    {
        return Err(M1BackendError::InvalidRequest(
            "invalid UI verification bounds",
        ));
    }
    let expect = predicates
        .iter()
        .map(map_ui_predicate)
        .collect::<Result<Vec<_>, _>>()?;
    let mut args = serde_json::json!({
        "pid": process_id,
        "window_id": window_id,
        "expect": expect,
        "timeout_ms": timeout_ms,
        "stable_samples": stable_samples,
        "include_screenshot": include_screenshot,
    })
    .as_object()
    .cloned()
    .expect("object literal");
    insert_context_session(&mut args, context_id)?;
    Ok(("verify_state", Some(args)))
}

fn validate_application_selector(
    identifier: Option<&str>,
    name: Option<&str>,
    targets: &[String],
) -> Result<(), M1BackendError> {
    let valid_identifier =
        identifier.is_some_and(|value| !value.trim().is_empty() && value.len() <= 512);
    let valid_name = name.is_some_and(|value| !value.trim().is_empty() && value.len() <= 512);
    if !valid_identifier && !valid_name {
        return Err(M1BackendError::InvalidRequest(
            "application identifier or name is required",
        ));
    }
    if targets.len() > 16
        || targets
            .iter()
            .any(|target| target.is_empty() || target.len() > 4096)
    {
        return Err(M1BackendError::InvalidRequest(
            "invalid application launch targets",
        ));
    }
    Ok(())
}

fn validate_window_target(process_id: u32, window_id: u64) -> Result<(), M1BackendError> {
    if process_id == 0 || window_id == 0 {
        return Err(M1BackendError::InvalidRequest(
            "process_id and window_id must be positive",
        ));
    }
    Ok(())
}

fn map_ui_predicate(predicate: &UiPredicate) -> Result<Value, M1BackendError> {
    match predicate {
        UiPredicate::WindowExists { exists } => Ok(serde_json::json!({
            "window": { "exists": exists }
        })),
        UiPredicate::WindowBounds {
            bounds,
            tolerance_px,
        } => {
            if *tolerance_px > 100 {
                return Err(M1BackendError::InvalidRequest(
                    "window-bound tolerance must be <= 100 px",
                ));
            }
            Ok(serde_json::json!({
                "window": {
                    "bounds": {
                        "x": bounds.x,
                        "y": bounds.y,
                        "width": bounds.width,
                        "height": bounds.height,
                        "tolerance_px": tolerance_px,
                    }
                }
            }))
        }
        UiPredicate::ElementExists { selector } => Ok(serde_json::json!({
            "element": {
                "exists": true,
                "selector": map_element_selector(selector)?,
            }
        })),
        UiPredicate::ElementState {
            selector,
            enabled,
            selected,
            value_equals,
        } => {
            if enabled.is_none() && selected.is_none() && value_equals.is_none() {
                return Err(M1BackendError::InvalidRequest(
                    "element-state predicate requires a state",
                ));
            }
            if value_equals
                .as_ref()
                .is_some_and(|value| value.len() > MAX_TYPE_TEXT_BYTES)
            {
                return Err(M1BackendError::InvalidRequest(
                    "element value predicate is too large",
                ));
            }
            Ok(serde_json::json!({
                "element": {
                    "selector": map_element_selector(selector)?,
                    "enabled": enabled,
                    "selected": selected,
                    "value_equals": value_equals,
                }
            }))
        }
    }
}

fn map_element_selector(selector: &UiElementSelector) -> Result<Value, M1BackendError> {
    if selector.role.is_none() && selector.label_contains.is_none() {
        return Err(M1BackendError::InvalidRequest("empty UI element selector"));
    }
    if selector
        .label_contains
        .as_ref()
        .is_some_and(|value| value.is_empty() || value.len() > MAX_UI_QUERY_BYTES)
    {
        return Err(M1BackendError::InvalidRequest("invalid UI selector label"));
    }
    let role = selector.role.map(ui_role_to_cua).transpose()?;
    Ok(serde_json::json!({
        "role": role,
        "label_contains": selector.label_contains,
    }))
}

fn ui_role_to_cua(role: UiRole) -> Result<&'static str, M1BackendError> {
    let role = match role {
        UiRole::Window => "AXWindow",
        UiRole::Button => "AXButton",
        UiRole::Text => "AXStaticText",
        UiRole::TextField => "AXTextField",
        UiRole::Checkbox => "AXCheckBox",
        UiRole::RadioButton => "AXRadioButton",
        UiRole::Link => "AXLink",
        UiRole::Menu => "AXMenu",
        UiRole::MenuItem => "AXMenuItem",
        UiRole::Toolbar => "AXToolbar",
        UiRole::Tab => "AXTabGroup",
        UiRole::List => "AXList",
        UiRole::ListItem => "AXRow",
        UiRole::Table => "AXTable",
        UiRole::Row => "AXRow",
        UiRole::Cell => "AXCell",
        UiRole::Group => "AXGroup",
        UiRole::Image => "AXImage",
        UiRole::Slider => "AXSlider",
        UiRole::Other => {
            return Err(M1BackendError::InvalidRequest(
                "other UI role cannot be used as a selector",
            ));
        }
    };
    Ok(role)
}

fn normalize_windows_result(value: &Value) -> Result<DeviceResult, M1BackendError> {
    let windows =
        value
            .get("windows")
            .and_then(Value::as_array)
            .ok_or(M1BackendError::MalformedResponse(
                "list_windows missing windows",
            ))?;
    let mut normalized = Vec::with_capacity(windows.len().min(MAX_WINDOW_RESULTS));
    let mut truncated = false;
    for window in windows {
        let Some(window) = normalize_listed_window(window)? else {
            // Cua may report zero-area helper/agent windows in the top-level
            // observation feed. They cannot be targeted by the V2 window
            // contract, so omit them from ListWindows rather than failing the
            // entire read-only observation. All other fields are still parsed
            // and validated before omission, and exact target/snapshot paths
            // keep strict positive geometry.
            continue;
        };
        if normalized.len() == MAX_WINDOW_RESULTS {
            truncated = true;
            break;
        }
        normalized.push(window);
    }
    Ok(DeviceResult::Windows {
        windows: normalized,
        truncated,
    })
}

fn normalize_application_launch_result(value: &Value) -> Result<DeviceResult, M1BackendError> {
    let process_id = json_u32(value, "pid")?;
    let name = value
        .get("name")
        .and_then(Value::as_str)
        .ok_or(M1BackendError::MalformedResponse("launch_app missing name"))?;
    let name = bounded_string(name, MAX_UI_TEXT_BYTES, "launch_app name is too large")?;
    let identifier = value
        .get("bundle_id")
        .and_then(Value::as_str)
        .map(|value| {
            bounded_string(
                value,
                MAX_UI_REF_BYTES,
                "launch_app identifier is too large",
            )
        })
        .transpose()?;
    let state = value.get("launch_state").and_then(Value::as_object).ok_or(
        M1BackendError::MalformedResponse("launch_app missing launch_state"),
    )?;
    let process_running = state
        .get("process_running")
        .and_then(Value::as_bool)
        .ok_or(M1BackendError::MalformedResponse(
            "launch state missing process_running",
        ))?;
    let window_ready = state.get("window_ready").and_then(Value::as_bool).ok_or(
        M1BackendError::MalformedResponse("launch state missing window_ready"),
    )?;
    let windows =
        value
            .get("windows")
            .and_then(Value::as_array)
            .ok_or(M1BackendError::MalformedResponse(
                "launch_app missing windows",
            ))?;
    let windows_truncated = windows.len() > MAX_WINDOW_RESULTS;
    let windows = windows
        .iter()
        .take(MAX_WINDOW_RESULTS)
        .map(normalize_window)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(DeviceResult::ApplicationLaunched {
        process_id,
        identifier,
        name,
        process_running,
        window_ready,
        windows,
        windows_truncated,
    })
}

fn normalize_window(value: &Value) -> Result<WindowInfo, M1BackendError> {
    normalize_window_record(value, false)?.ok_or(M1BackendError::MalformedResponse(
        "rectangle must be positive",
    ))
}

fn normalize_listed_window(value: &Value) -> Result<Option<WindowInfo>, M1BackendError> {
    normalize_window_record(value, true)
}

fn normalize_window_record(
    value: &Value,
    omit_zero_area: bool,
) -> Result<Option<WindowInfo>, M1BackendError> {
    let window_id =
        value
            .get("window_id")
            .and_then(Value::as_u64)
            .ok_or(M1BackendError::MalformedResponse(
                "window missing window_id",
            ))?;
    let process_id = json_u32(value, "pid")?;
    let application = bounded_string(
        value
            .get("app_name")
            .and_then(Value::as_str)
            .unwrap_or_default(),
        MAX_UI_TEXT_BYTES,
        "window application name is too large",
    )?;
    let title = bounded_string(
        value
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or_default(),
        MAX_UI_TEXT_BYTES,
        "window title is too large",
    )?;
    let bounds = value
        .get("bounds")
        .ok_or(M1BackendError::MalformedResponse("window missing bounds"))
        .and_then(normalize_rect_allow_zero)?;
    let is_on_screen = value.get("is_on_screen").and_then(Value::as_bool).ok_or(
        M1BackendError::MalformedResponse("window missing is_on_screen"),
    )?;
    let on_current_workspace = match value.get("on_current_space") {
        Some(Value::Bool(value)) => Some(*value),
        Some(Value::Null) | None => None,
        _ => {
            return Err(M1BackendError::MalformedResponse(
                "invalid on_current_space",
            ));
        }
    };
    if bounds.width == 0 || bounds.height == 0 {
        if omit_zero_area {
            return Ok(None);
        }
        return Err(M1BackendError::MalformedResponse(
            "rectangle must be positive",
        ));
    }
    Ok(Some(WindowInfo {
        window_id,
        process_id,
        application,
        title,
        bounds,
        is_on_screen,
        on_current_workspace,
    }))
}

fn normalize_rect(value: &Value) -> Result<UiRect, M1BackendError> {
    let rect = normalize_rect_allow_zero(value)?;
    if rect.width == 0 || rect.height == 0 {
        return Err(M1BackendError::MalformedResponse(
            "rectangle must be positive",
        ));
    }
    Ok(rect)
}

fn normalize_rect_allow_zero(value: &Value) -> Result<UiRect, M1BackendError> {
    let object = value
        .as_object()
        .ok_or(M1BackendError::MalformedResponse("invalid rectangle"))?;
    let x = json_i32_number(object.get("x"), "rectangle x")?;
    let y = json_i32_number(object.get("y"), "rectangle y")?;
    let width = json_u32_number(
        object.get("width").or_else(|| object.get("w")),
        "rectangle width",
    )?;
    let height = json_u32_number(
        object.get("height").or_else(|| object.get("h")),
        "rectangle height",
    )?;
    Ok(UiRect {
        x,
        y,
        width,
        height,
    })
}

fn normalize_window_snapshot_result(
    command: &DeviceCommand,
    result: &CallToolResult,
) -> Result<DeviceResult, M1BackendError> {
    let (process_id, window_id, include_screenshot) = match command {
        DeviceCommand::InspectWindow {
            process_id,
            window_id,
            include_screenshot,
            ..
        }
        | DeviceCommand::InspectWindowContextual {
            process_id,
            window_id,
            include_screenshot,
            ..
        } => (process_id, window_id, include_screenshot),
        _ => return Err(M1BackendError::MalformedResponse("wrong snapshot command")),
    };
    let value = structured_value(result)?;
    let snapshot_ref = value.get("snapshot_id").and_then(Value::as_str).ok_or(
        M1BackendError::MalformedResponse("get_window_state missing snapshot_id"),
    )?;
    let snapshot_ref = bounded_string(
        snapshot_ref,
        MAX_UI_REF_BYTES,
        "get_window_state snapshot_id is too large",
    )?;
    let returned_pid = json_u32(&value, "pid")?;
    let returned_window_id =
        value
            .get("window_id")
            .and_then(Value::as_u64)
            .ok_or(M1BackendError::MalformedResponse(
                "get_window_state missing window_id",
            ))?;
    if returned_pid != *process_id || returned_window_id != *window_id {
        return Err(M1BackendError::MalformedResponse(
            "window snapshot target mismatch",
        ));
    }
    let raw_elements = value.get("elements").and_then(Value::as_array).ok_or(
        M1BackendError::MalformedResponse("get_window_state missing elements"),
    )?;
    if raw_elements.len() > MAX_UI_ELEMENTS {
        return Err(M1BackendError::MalformedResponse("too many UI elements"));
    }
    let refs: HashMap<u64, String> = raw_elements
        .iter()
        .filter_map(|element| {
            let index = element.get("element_index")?.as_u64()?;
            let token = element.get("element_token")?.as_str()?.to_owned();
            Some((index, token))
        })
        .collect();
    let elements = raw_elements
        .iter()
        .map(|element| normalize_ui_element(element, &refs))
        .collect::<Result<Vec<_>, _>>()?;
    let elements_complete = value
        .get("elements_complete")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let screenshot = if *include_screenshot {
        Some(extract_png_image(result)?)
    } else {
        if result
            .content
            .iter()
            .any(|content| content.as_image().is_some())
        {
            return Err(M1BackendError::MalformedResponse(
                "unexpected image for screenshot-disabled inspection",
            ));
        }
        None
    };
    Ok(DeviceResult::WindowSnapshot {
        snapshot_ref,
        process_id: returned_pid,
        window_id: returned_window_id,
        elements,
        elements_complete,
        screenshot,
    })
}

fn normalize_ui_element(
    value: &Value,
    refs: &HashMap<u64, String>,
) -> Result<UiElement, M1BackendError> {
    let element_ref = value.get("element_token").and_then(Value::as_str).ok_or(
        M1BackendError::MalformedResponse("UI element missing element_token"),
    )?;
    let element_ref = bounded_string(
        element_ref,
        MAX_UI_REF_BYTES,
        "UI element token is too large",
    )?;
    let role = value
        .get("role")
        .and_then(Value::as_str)
        .map(normalize_cua_role)
        .ok_or(M1BackendError::MalformedResponse("UI element missing role"))?;
    let label = bounded_optional_string(
        value.get("label"),
        MAX_UI_TEXT_BYTES,
        "UI element label is too large",
    )?;
    let element_value = bounded_optional_string(
        value.get("value"),
        MAX_UI_TEXT_BYTES,
        "UI element value is too large",
    )?;
    let bounds = match value.get("frame") {
        Some(Value::Object(_)) => Some(normalize_rect(&value["frame"])?),
        Some(Value::Null) | None => None,
        _ => {
            return Err(M1BackendError::MalformedResponse(
                "invalid UI element frame",
            ));
        }
    };
    let enabled = optional_bool(value.get("enabled"))?;
    let selected = optional_bool(value.get("selected"))?;
    let parent_ref = match value.get("parent_index") {
        Some(Value::Number(number)) => number.as_u64().and_then(|index| refs.get(&index).cloned()),
        Some(Value::Null) | None => None,
        _ => return Err(M1BackendError::MalformedResponse("invalid parent_index")),
    };
    let depth =
        value
            .get("depth")
            .and_then(Value::as_u64)
            .ok_or(M1BackendError::MalformedResponse(
                "UI element missing depth",
            ))?;
    let depth = u16::try_from(depth).map_err(|_| M1BackendError::NumericOverflow)?;
    Ok(UiElement {
        element_ref,
        role,
        label,
        value: element_value,
        bounds,
        enabled,
        selected,
        parent_ref,
        depth,
    })
}

fn normalize_cua_role(role: &str) -> UiRole {
    match role {
        "AXWindow" => UiRole::Window,
        "AXButton" => UiRole::Button,
        "AXStaticText" => UiRole::Text,
        "AXTextField" | "AXTextArea" => UiRole::TextField,
        "AXCheckBox" => UiRole::Checkbox,
        "AXRadioButton" => UiRole::RadioButton,
        "AXLink" => UiRole::Link,
        "AXMenu" => UiRole::Menu,
        "AXMenuItem" | "AXMenuBarItem" => UiRole::MenuItem,
        "AXToolbar" => UiRole::Toolbar,
        "AXTabGroup" => UiRole::Tab,
        "AXList" => UiRole::List,
        "AXOutline" => UiRole::List,
        "AXTable" => UiRole::Table,
        "AXRow" => UiRole::Row,
        "AXCell" => UiRole::Cell,
        "AXGroup" => UiRole::Group,
        "AXImage" => UiRole::Image,
        "AXSlider" => UiRole::Slider,
        _ => UiRole::Other,
    }
}

fn normalize_ui_verification_result(
    command: &DeviceCommand,
    result: &CallToolResult,
) -> Result<DeviceResult, M1BackendError> {
    let (expected, include_screenshot) = match command {
        DeviceCommand::VerifyUiState {
            predicates,
            include_screenshot,
            ..
        }
        | DeviceCommand::VerifyUiStateContextual {
            predicates,
            include_screenshot,
            ..
        } => (predicates, include_screenshot),
        _ => {
            return Err(M1BackendError::MalformedResponse(
                "wrong verification command",
            ));
        }
    };
    let value = structured_value(result)?;
    let status = parse_verification_status(value.get("status").and_then(Value::as_str).ok_or(
        M1BackendError::MalformedResponse("verify_state missing status"),
    )?)?;
    let stable =
        value
            .get("stable")
            .and_then(Value::as_bool)
            .ok_or(M1BackendError::MalformedResponse(
                "verify_state missing stable",
            ))?;
    let samples = json_u32(&value, "samples")?;
    let raw_predicates = value.get("predicates").and_then(Value::as_array).ok_or(
        M1BackendError::MalformedResponse("verify_state missing predicates"),
    )?;
    if raw_predicates.len() != expected.len() {
        return Err(M1BackendError::MalformedResponse(
            "verification predicate count mismatch",
        ));
    }
    let predicates = raw_predicates
        .iter()
        .map(|predicate| {
            let status = predicate
                .get("status")
                .and_then(Value::as_str)
                .ok_or(M1BackendError::MalformedResponse(
                    "predicate missing status",
                ))
                .and_then(parse_verification_status)?;
            let unknown_reason = bounded_optional_string(
                predicate.get("unknown_reason"),
                MAX_UI_TEXT_BYTES,
                "verification unknown reason is too large",
            )?;
            Ok(UiPredicateResult {
                status,
                unknown_reason,
            })
        })
        .collect::<Result<Vec<_>, M1BackendError>>()?;
    let screenshot = if *include_screenshot {
        Some(extract_png_image(result)?)
    } else {
        if result
            .content
            .iter()
            .any(|content| content.as_image().is_some())
        {
            return Err(M1BackendError::MalformedResponse(
                "unexpected image for screenshot-disabled verification",
            ));
        }
        None
    };
    Ok(DeviceResult::UiStateVerification {
        status,
        stable,
        samples,
        predicates,
        screenshot,
    })
}

fn parse_verification_status(value: &str) -> Result<VerificationStatus, M1BackendError> {
    match value {
        "satisfied" => Ok(VerificationStatus::Satisfied),
        "unsatisfied" => Ok(VerificationStatus::Unsatisfied),
        "unknown" => Ok(VerificationStatus::Unknown),
        _ => Err(M1BackendError::MalformedResponse(
            "invalid verification status",
        )),
    }
}

fn extract_png_image(result: &CallToolResult) -> Result<UiImage, M1BackendError> {
    let mut images = result
        .content
        .iter()
        .filter_map(|content| content.as_image());
    let image = images.next().ok_or(M1BackendError::MalformedResponse(
        "missing PNG image content",
    ))?;
    if images.next().is_some() || image.mime_type != "image/png" {
        return Err(M1BackendError::MalformedResponse(
            "ambiguous PNG image content",
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
    let (width_pixels, height_pixels) = png_dimensions(&decoded)?;
    Ok(UiImage {
        data_base64: image.data.clone(),
        mime_type: image.mime_type.clone(),
        width_pixels,
        height_pixels,
    })
}

fn png_dimensions(bytes: &[u8]) -> Result<(u32, u32), M1BackendError> {
    if bytes.len() < 24 || !bytes.starts_with(b"\x89PNG\r\n\x1a\n") || &bytes[12..16] != b"IHDR" {
        return Err(M1BackendError::MalformedResponse(
            "image content is not a PNG",
        ));
    }
    let width = u32::from_be_bytes(
        bytes[16..20]
            .try_into()
            .map_err(|_| M1BackendError::MalformedResponse("invalid PNG width"))?,
    );
    let height = u32::from_be_bytes(
        bytes[20..24]
            .try_into()
            .map_err(|_| M1BackendError::MalformedResponse("invalid PNG height"))?,
    );
    if width == 0 || height == 0 {
        return Err(M1BackendError::MalformedResponse(
            "PNG dimensions must be positive",
        ));
    }
    Ok((width, height))
}

fn bounded_string(
    value: &str,
    max_bytes: usize,
    error: &'static str,
) -> Result<String, M1BackendError> {
    if value.len() > max_bytes {
        return Err(M1BackendError::MalformedResponse(error));
    }
    Ok(value.to_owned())
}

fn bounded_optional_string(
    value: Option<&Value>,
    max_bytes: usize,
    error: &'static str,
) -> Result<Option<String>, M1BackendError> {
    optional_string(value)?
        .map(|value| bounded_string(&value, max_bytes, error))
        .transpose()
}

fn optional_string(value: Option<&Value>) -> Result<Option<String>, M1BackendError> {
    match value {
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(Value::Null) | None => Ok(None),
        _ => Err(M1BackendError::MalformedResponse("invalid optional string")),
    }
}

fn optional_bool(value: Option<&Value>) -> Result<Option<bool>, M1BackendError> {
    match value {
        Some(Value::Bool(value)) => Ok(Some(*value)),
        Some(Value::Null) | None => Ok(None),
        _ => Err(M1BackendError::MalformedResponse("invalid optional bool")),
    }
}

fn json_i32_number(value: Option<&Value>, name: &'static str) -> Result<i32, M1BackendError> {
    let value = value
        .and_then(Value::as_f64)
        .ok_or(M1BackendError::MalformedResponse(name))?;
    if !value.is_finite() || value < i32::MIN as f64 || value > i32::MAX as f64 {
        return Err(M1BackendError::NumericOverflow);
    }
    Ok(value.round() as i32)
}

fn json_u32_number(value: Option<&Value>, name: &'static str) -> Result<u32, M1BackendError> {
    let value = value
        .and_then(Value::as_f64)
        .ok_or(M1BackendError::MalformedResponse(name))?;
    if !value.is_finite() || value < 0.0 || value > u32::MAX as f64 {
        return Err(M1BackendError::NumericOverflow);
    }
    Ok(value.round() as u32)
}

fn insert_context_session(
    args: &mut JsonObject,
    context_id: Option<&str>,
) -> Result<(), M1BackendError> {
    if let Some(context_id) = context_id {
        if context_id.len() > MAX_UI_REF_BYTES || !context_id.starts_with("ctx_") {
            return Err(M1BackendError::InvalidRequest(
                "invalid interaction context id",
            ));
        }
        args.insert("session".into(), serde_json::json!(context_id));
    }
    Ok(())
}

fn normalize_window_activation_result(
    command: &DeviceCommand,
    result: &CallToolResult,
) -> Result<DeviceResult, M1BackendError> {
    let DeviceCommand::ActivateWindow {
        process_id,
        window_id,
    } = command
    else {
        return Err(M1BackendError::MalformedResponse(
            "wrong activation command",
        ));
    };
    let value = structured_value(result)?;
    let process_activated = value
        .get("process_activated")
        .and_then(Value::as_bool)
        .ok_or(M1BackendError::MalformedResponse(
            "activation missing process_activated",
        ))?;
    let exact_window_verified = if window_id.is_some() {
        Some(
            value
                .get("exact_window_effect")
                .and_then(Value::as_object)
                .and_then(|effect| effect.get("verified"))
                .and_then(Value::as_bool)
                .ok_or(M1BackendError::MalformedResponse(
                    "activation missing exact-window evidence",
                ))?,
        )
    } else {
        None
    };
    Ok(DeviceResult::WindowActivated {
        process_id: *process_id,
        window_id: *window_id,
        process_activated,
        exact_window_verified,
    })
}

fn normalize_region_capture_result(result: &CallToolResult) -> Result<UiImage, M1BackendError> {
    let value = structured_value(result)?;
    let mime_type = value
        .get("mime_type")
        .or_else(|| value.get("screenshot_mime_type"))
        .and_then(Value::as_str)
        .ok_or(M1BackendError::MalformedResponse(
            "region capture missing mime type",
        ))?;
    if mime_type != "image/jpeg" {
        return Err(M1BackendError::MalformedResponse(
            "region capture is not JPEG",
        ));
    }
    let width_pixels = json_u32(&value, "width")?;
    let height_pixels = json_u32(&value, "height")?;
    if width_pixels == 0 || height_pixels == 0 || width_pixels > 4096 || height_pixels > 4096 {
        return Err(M1BackendError::MalformedResponse(
            "invalid region capture dimensions",
        ));
    }
    let image_data = result
        .content
        .iter()
        .filter_map(|content| content.as_image())
        .find(|image| image.mime_type == "image/jpeg")
        .map(|image| image.data.clone())
        .or_else(|| {
            value
                .get("screenshot_png_b64")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
        .ok_or(M1BackendError::MalformedResponse(
            "region capture missing JPEG content",
        ))?;
    let max_encoded = MAX_SCREENSHOT_BYTES.div_ceil(3) * 4;
    if image_data.len() > max_encoded {
        return Err(M1BackendError::ScreenshotTooLarge);
    }
    let decoded = STANDARD
        .decode(image_data.as_bytes())
        .map_err(|_| M1BackendError::MalformedResponse("region capture base64 is invalid"))?;
    if decoded.len() > MAX_SCREENSHOT_BYTES || !decoded.starts_with(&[0xff, 0xd8, 0xff]) {
        return Err(M1BackendError::MalformedResponse(
            "region capture content is not JPEG",
        ));
    }
    Ok(UiImage {
        data_base64: image_data,
        mime_type: mime_type.to_owned(),
        width_pixels,
        height_pixels,
    })
}

fn delivery_mode_name(mode: InputDeliveryMode) -> &'static str {
    match mode {
        InputDeliveryMode::Background => "background",
        InputDeliveryMode::Foreground => "foreground",
    }
}

fn keyboard_modifier_name(modifier: KeyboardModifier) -> &'static str {
    match modifier {
        KeyboardModifier::Meta => "cmd",
        KeyboardModifier::Shift => "shift",
        KeyboardModifier::Alt => "alt",
        KeyboardModifier::Control => "ctrl",
        KeyboardModifier::Function => "fn",
    }
}

fn map_keyboard_modifiers(
    modifiers: &[KeyboardModifier],
) -> Result<Vec<&'static str>, M1BackendError> {
    if modifiers.len() > MAX_KEYBOARD_MODIFIERS {
        return Err(M1BackendError::InvalidRequest(
            "too many keyboard modifiers",
        ));
    }
    let mut seen = Vec::with_capacity(modifiers.len());
    for modifier in modifiers {
        if seen.contains(modifier) {
            return Err(M1BackendError::InvalidRequest(
                "duplicate keyboard modifier",
            ));
        }
        seen.push(*modifier);
    }
    Ok(seen.into_iter().map(keyboard_modifier_name).collect())
}

fn validate_keyboard_key(key: &str) -> Result<(), M1BackendError> {
    let named = [
        "return", "tab", "escape", "up", "down", "left", "right", "space", "delete", "home", "end",
        "pageup", "pagedown", "f1", "f2", "f3", "f4", "f5", "f6", "f7", "f8", "f9", "f10", "f11",
        "f12",
    ];
    let single_ascii = key.len() == 1 && key.as_bytes()[0].is_ascii_alphanumeric();
    if single_ascii || named.contains(&key) {
        Ok(())
    } else {
        Err(M1BackendError::InvalidRequest("unsupported keyboard key"))
    }
}

fn pointer_target_arguments(
    target: &PointerTarget,
    delivery: InputDeliveryMode,
) -> Result<JsonObject, M1BackendError> {
    let mut args = JsonObject::new();
    match target {
        PointerTarget::DesktopPhysical { x, y } => {
            if delivery == InputDeliveryMode::Foreground {
                return Err(M1BackendError::InvalidRequest(
                    "desktop pointer target cannot request foreground delivery",
                ));
            }
            args.insert("x".into(), serde_json::json!(x));
            args.insert("y".into(), serde_json::json!(y));
            args.insert("scope".into(), serde_json::json!("desktop"));
        }
        PointerTarget::WindowPhysical {
            process_id,
            window_id,
            x,
            y,
        } => {
            validate_window_target(*process_id, *window_id)?;
            args.insert("pid".into(), serde_json::json!(process_id));
            args.insert("window_id".into(), serde_json::json!(window_id));
            args.insert("x".into(), serde_json::json!(x));
            args.insert("y".into(), serde_json::json!(y));
            args.insert("scope".into(), serde_json::json!("window"));
        }
        PointerTarget::Element {
            process_id,
            window_id,
            element_ref,
        } => {
            validate_window_target(*process_id, *window_id)?;
            if element_ref.is_empty() || element_ref.len() > MAX_UI_REF_BYTES {
                return Err(M1BackendError::InvalidRequest("invalid UI element ref"));
            }
            args.insert("pid".into(), serde_json::json!(process_id));
            args.insert("window_id".into(), serde_json::json!(window_id));
            args.insert("element_token".into(), serde_json::json!(element_ref));
            args.insert("scope".into(), serde_json::json!("window"));
        }
    }
    Ok(args)
}

fn drag_target_arguments(
    from: &PointerTarget,
    to: &PointerTarget,
    delivery: InputDeliveryMode,
) -> Result<JsonObject, M1BackendError> {
    let mut args = JsonObject::new();
    match (from, to) {
        (
            PointerTarget::DesktopPhysical {
                x: from_x,
                y: from_y,
            },
            PointerTarget::DesktopPhysical { x: to_x, y: to_y },
        ) => {
            if delivery == InputDeliveryMode::Foreground {
                return Err(M1BackendError::InvalidRequest(
                    "desktop drag cannot request foreground delivery",
                ));
            }
            args.insert("from_x".into(), serde_json::json!(from_x));
            args.insert("from_y".into(), serde_json::json!(from_y));
            args.insert("to_x".into(), serde_json::json!(to_x));
            args.insert("to_y".into(), serde_json::json!(to_y));
            args.insert("scope".into(), serde_json::json!("desktop"));
        }
        (
            PointerTarget::WindowPhysical {
                process_id: from_pid,
                window_id: from_window,
                x: from_x,
                y: from_y,
            },
            PointerTarget::WindowPhysical {
                process_id: to_pid,
                window_id: to_window,
                x: to_x,
                y: to_y,
            },
        ) if from_pid == to_pid && from_window == to_window => {
            validate_window_target(*from_pid, *from_window)?;
            args.insert("pid".into(), serde_json::json!(from_pid));
            args.insert("window_id".into(), serde_json::json!(from_window));
            args.insert("from_x".into(), serde_json::json!(from_x));
            args.insert("from_y".into(), serde_json::json!(from_y));
            args.insert("to_x".into(), serde_json::json!(to_x));
            args.insert("to_y".into(), serde_json::json!(to_y));
            args.insert("scope".into(), serde_json::json!("window"));
        }
        _ => {
            return Err(M1BackendError::InvalidRequest(
                "drag endpoints must share one coordinate space and exact window",
            ));
        }
    }
    Ok(args)
}

fn input_target_arguments(
    target: &InputTarget,
    delivery: InputDeliveryMode,
) -> Result<JsonObject, M1BackendError> {
    let mut args = JsonObject::new();
    match target {
        InputTarget::Desktop => {
            if delivery == InputDeliveryMode::Foreground {
                return Err(M1BackendError::InvalidRequest(
                    "desktop input cannot request foreground delivery",
                ));
            }
            args.insert("scope".into(), serde_json::json!("desktop"));
        }
        InputTarget::Window {
            process_id,
            window_id,
        } => {
            if *process_id == 0 || window_id == &Some(0) {
                return Err(M1BackendError::InvalidRequest(
                    "invalid window input target",
                ));
            }
            if delivery == InputDeliveryMode::Foreground && window_id.is_none() {
                return Err(M1BackendError::InvalidRequest(
                    "foreground input requires an exact window",
                ));
            }
            args.insert("pid".into(), serde_json::json!(process_id));
            if let Some(window_id) = window_id {
                args.insert("window_id".into(), serde_json::json!(window_id));
            }
            args.insert("scope".into(), serde_json::json!("window"));
        }
        InputTarget::WindowPoint {
            process_id,
            window_id,
            x,
            y,
        } => {
            validate_window_target(*process_id, *window_id)?;
            args.insert("pid".into(), serde_json::json!(process_id));
            args.insert("window_id".into(), serde_json::json!(window_id));
            args.insert("x".into(), serde_json::json!(x));
            args.insert("y".into(), serde_json::json!(y));
            args.insert("scope".into(), serde_json::json!("window"));
        }
        InputTarget::Element {
            process_id,
            window_id,
            element_ref,
        } => {
            validate_window_target(*process_id, *window_id)?;
            if element_ref.is_empty() || element_ref.len() > MAX_UI_REF_BYTES {
                return Err(M1BackendError::InvalidRequest("invalid UI element ref"));
            }
            args.insert("pid".into(), serde_json::json!(process_id));
            args.insert("window_id".into(), serde_json::json!(window_id));
            args.insert("element_token".into(), serde_json::json!(element_ref));
            args.insert("scope".into(), serde_json::json!("window"));
        }
    }
    Ok(args)
}

fn scroll_target_arguments(
    target: &ScrollTarget,
    delivery: InputDeliveryMode,
) -> Result<JsonObject, M1BackendError> {
    let mut args = JsonObject::new();
    match target {
        ScrollTarget::Window {
            process_id,
            window_id,
        } => {
            if *process_id == 0 || window_id == &Some(0) {
                return Err(M1BackendError::InvalidRequest(
                    "invalid scroll window target",
                ));
            }
            if delivery == InputDeliveryMode::Foreground && window_id.is_none() {
                return Err(M1BackendError::InvalidRequest(
                    "foreground scroll requires an exact window",
                ));
            }
            args.insert("pid".into(), serde_json::json!(process_id));
            if let Some(window_id) = window_id {
                args.insert("window_id".into(), serde_json::json!(window_id));
            }
            args.insert("scope".into(), serde_json::json!("window"));
        }
        ScrollTarget::WindowPoint {
            process_id,
            window_id,
            x,
            y,
        } => {
            validate_window_target(*process_id, *window_id)?;
            args.insert("pid".into(), serde_json::json!(process_id));
            args.insert("window_id".into(), serde_json::json!(window_id));
            args.insert("x".into(), serde_json::json!(x));
            args.insert("y".into(), serde_json::json!(y));
            args.insert("scope".into(), serde_json::json!("window"));
        }
        ScrollTarget::DesktopPoint { x, y } => {
            if delivery == InputDeliveryMode::Foreground {
                return Err(M1BackendError::InvalidRequest(
                    "desktop scroll cannot request foreground delivery",
                ));
            }
            args.insert("x".into(), serde_json::json!(x));
            args.insert("y".into(), serde_json::json!(y));
            args.insert("scope".into(), serde_json::json!("desktop"));
        }
    }
    Ok(args)
}

fn scroll_direction_name(direction: ScrollDirection) -> &'static str {
    match direction {
        ScrollDirection::Up => "up",
        ScrollDirection::Down => "down",
        ScrollDirection::Left => "left",
        ScrollDirection::Right => "right",
    }
}

fn scroll_granularity_name(granularity: ScrollGranularity) -> &'static str {
    match granularity {
        ScrollGranularity::Line => "line",
        ScrollGranularity::Page => "page",
    }
}

fn normalize_clipboard_read_result(
    result: &CallToolResult,
) -> Result<DeviceResult, M1BackendError> {
    let value = structured_value(result)?;
    let types = normalize_clipboard_types(&value)?;
    let text = match value.get("text") {
        Some(Value::String(text)) if text.len() <= MAX_CLIPBOARD_TEXT_BYTES => Some(text.clone()),
        Some(Value::String(_)) => {
            return Err(M1BackendError::MalformedResponse(
                "clipboard text is too large",
            ));
        }
        Some(Value::Null) | None => None,
        _ => return Err(M1BackendError::MalformedResponse("invalid clipboard text")),
    };
    Ok(DeviceResult::ClipboardState { types, text })
}

fn normalize_clipboard_write_result(
    result: &CallToolResult,
) -> Result<DeviceResult, M1BackendError> {
    let value = structured_value(result)?;
    Ok(DeviceResult::ClipboardWritten {
        types: normalize_clipboard_types(&value)?,
    })
}

fn normalize_clipboard_types(value: &Value) -> Result<Vec<String>, M1BackendError> {
    let types =
        value
            .get("types")
            .and_then(Value::as_array)
            .ok_or(M1BackendError::MalformedResponse(
                "clipboard result missing types",
            ))?;
    if types.len() > MAX_CLIPBOARD_TYPES {
        return Err(M1BackendError::MalformedResponse(
            "too many clipboard types",
        ));
    }
    types
        .iter()
        .map(|value| {
            let value = value
                .as_str()
                .ok_or(M1BackendError::MalformedResponse("invalid clipboard type"))?;
            bounded_string(
                value,
                MAX_CLIPBOARD_TYPE_BYTES,
                "clipboard type is too large",
            )
        })
        .collect()
}

fn normalize_pointer_position_result(
    result: &CallToolResult,
) -> Result<DeviceResult, M1BackendError> {
    let value = structured_value(result)?;
    let object = value.as_object().ok_or(M1BackendError::MalformedResponse(
        "invalid pointer position",
    ))?;
    Ok(DeviceResult::PointerPosition {
        x_points: json_i32_number(object.get("x"), "pointer x")?,
        y_points: json_i32_number(object.get("y"), "pointer y")?,
    })
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

fn ui_element_action_name(action: UiElementAction) -> &'static str {
    match action {
        UiElementAction::Press => "press",
        UiElementAction::Open => "open",
        UiElementAction::ShowMenu => "show_menu",
        UiElementAction::Pick => "pick",
        UiElementAction::Confirm => "confirm",
        UiElementAction::Cancel => "cancel",
    }
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
    BrowserRefused(BrowserRefusalReason),
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
            Self::BrowserRefused(reason) => reason.safe_code(),
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

    fn fixture_python() -> String {
        std::env::var("CUMG_TEST_PYTHON").unwrap_or_else(|_| "python3".into())
    }
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
            fixture_python(),
            args,
            "1.0.0",
            "test",
            1,
            Duration::from_secs(5),
            tool_timeout,
            1,
            Duration::from_millis(10),
        )
    }

    fn transfer_state_dir(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "cumg-{label}-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ))
    }

    #[tokio::test]
    async fn post_dispatch_backend_tool_errors_are_indeterminate_only_for_mutations() {
        use crate::v2_browser::BrowserInputRoute;
        use crate::v2_browser_runtime::{BrowserBackendClickTarget, BrowserBackendCommand};

        let click_marker = transfer_state_dir("ambiguous-click-marker");
        let adapter = fixture(vec![
            "scripts/mock_cua_mcp_backend.py".into(),
            "--ambiguous-click-marker".into(),
            click_marker.to_string_lossy().into_owned(),
        ]);
        adapter.connect().await.unwrap();
        let (_cancel_tx, cancel_rx) = watch::channel(false);
        assert_eq!(
            adapter
                .execute(
                    &DeviceCommand::PointerClick {
                        x: 10,
                        y: 20,
                        button: crate::v2_m0::PointerButton::Left,
                    },
                    cancel_rx,
                )
                .await
                .unwrap(),
            BackendExecutionOutcome::BackendOutcomeIndeterminate
        );
        assert!(
            click_marker.exists(),
            "fixture must prove dispatch before error"
        );
        adapter.shutdown().await.unwrap();
        let _ = fs::remove_file(&click_marker);

        let browser_marker = transfer_state_dir("ambiguous-browser-click-marker");
        let adapter = fixture(vec![
            "scripts/mock_cua_mcp_backend.py".into(),
            "--ambiguous-browser-click-marker".into(),
            browser_marker.to_string_lossy().into_owned(),
        ]);
        adapter.connect().await.unwrap();
        let (_cancel_tx, cancel_rx) = watch::channel(false);
        let browser_click = DeviceCommand::Browser {
            command: BrowserBackendCommand::Click {
                context_id: "ctx_0123456789abcdef0123456789abcdef".into(),
                backend_target_id: "target".into(),
                backend_tab_id: "tab".into(),
                target: BrowserBackendClickTarget::Element {
                    backend_element_ref: "p1:1".into(),
                },
                input_route: BrowserInputRoute::DomEvent,
            },
        };
        assert_eq!(
            adapter.execute(&browser_click, cancel_rx).await.unwrap(),
            BackendExecutionOutcome::BackendOutcomeIndeterminate
        );
        assert!(
            browser_marker.exists(),
            "browser fixture must prove dispatch before error"
        );
        adapter.shutdown().await.unwrap();
        let _ = fs::remove_file(&browser_marker);

        let response_loss_marker = transfer_state_dir("response-loss-click-marker");
        let adapter = fixture(vec![
            "scripts/mock_cua_mcp_backend.py".into(),
            "--drop-after-click-marker".into(),
            response_loss_marker.to_string_lossy().into_owned(),
        ]);
        adapter.connect().await.unwrap();
        let (_cancel_tx, cancel_rx) = watch::channel(false);
        assert_eq!(
            adapter
                .execute(
                    &DeviceCommand::PointerClick {
                        x: 30,
                        y: 40,
                        button: crate::v2_m0::PointerButton::Left,
                    },
                    cancel_rx,
                )
                .await
                .unwrap(),
            BackendExecutionOutcome::BackendOutcomeIndeterminate
        );
        assert!(
            response_loss_marker.exists(),
            "fixture must prove dispatch before response loss"
        );
        adapter.shutdown().await.unwrap();
        let _ = fs::remove_file(&response_loss_marker);

        let adapter = fixture(vec![
            "scripts/mock_cua_mcp_backend.py".into(),
            "--fail-list-apps".into(),
        ]);
        adapter.connect().await.unwrap();
        let (_cancel_tx, cancel_rx) = watch::channel(false);
        assert!(matches!(
            adapter
                .execute(&DeviceCommand::ListApplications, cancel_rx)
                .await,
            Err(M1BackendError::BackendToolError)
        ));
        adapter.shutdown().await.unwrap();
    }

    #[test]
    fn cua_advertises_complete_browser_core_and_transfer_surface() {
        let advertisement = fixture(vec![]).advertisement();
        for capability in [
            DeviceCapability::BrowserInspect,
            DeviceCapability::BrowserPrepare,
            DeviceCapability::BrowserNavigate,
            DeviceCapability::BrowserClick,
            DeviceCapability::BrowserType,
            DeviceCapability::BrowserDialog,
            DeviceCapability::BrowserPointer,
        ] {
            assert!(advertisement.supports(capability), "missing {capability:?}");
        }
        assert!(advertisement.supports(DeviceCapability::BrowserUploadFile));
        assert!(advertisement.supports(DeviceCapability::BrowserDownload));
    }

    #[tokio::test]
    async fn cua_transfer_adapter_uses_only_agent_private_staging() {
        use crate::v2_browser_runtime::{
            BrowserBackendCommand, BrowserBackendResult, BrowserStagedUploadFile,
        };
        use crate::v2_browser_staging::{BrowserDownloadStagingBroker, BrowserUploadStagingBroker};
        use base64::{Engine as _, engine::general_purpose::STANDARD};

        let state = transfer_state_dir("transfer-happy");
        fs::create_dir_all(&state).unwrap();
        let upload = BrowserUploadStagingBroker::new(&state).unwrap();
        let staged = upload
            .stage(
                "ctx_0123456789abcdef0123456789abcdef",
                1,
                1,
                "payload.txt",
                &STANDARD.encode(b"upload"),
                6,
            )
            .unwrap();
        let resolved = upload
            .resolve(&staged.handle, "ctx_0123456789abcdef0123456789abcdef", 1, 1)
            .unwrap();
        let upload_command = BrowserBackendCommand::Upload {
            context_id: "ctx_0123456789abcdef0123456789abcdef".into(),
            backend_target_id: "target".into(),
            backend_tab_id: "tab".into(),
            backend_element_ref: "p1:upload".into(),
            staged_files: vec![BrowserStagedUploadFile {
                backend_file_handle: staged.handle,
            }],
        };
        let adapter = fixture(vec!["scripts/mock_cua_mcp_backend.py".into()]);
        adapter.connect().await.unwrap();
        let (_cancel_tx, cancel_rx) = watch::channel(false);
        assert_eq!(
            adapter
                .execute_browser_upload(&upload_command, &[resolved], cancel_rx.clone())
                .await
                .unwrap(),
            BackendExecutionOutcome::Completed(DeviceResult::Browser {
                result: BrowserBackendResult::UploadAssigned { file_count: 1 },
            })
        );

        let download = BrowserDownloadStagingBroker::new(&state).unwrap();
        let prepared = download
            .prepare(
                "ctx_0123456789abcdef0123456789abcdef",
                1,
                1,
                "artifact.txt",
                1024,
                false,
            )
            .unwrap();
        let download_command = BrowserBackendCommand::Download {
            context_id: "ctx_0123456789abcdef0123456789abcdef".into(),
            backend_target_id: "target".into(),
            backend_tab_id: "tab".into(),
            backend_element_ref: "p1:download".into(),
            destination_name: "artifact.txt".into(),
            max_bytes: 1024,
            overwrite: false,
        };
        let result = adapter
            .execute_browser_download(&download_command, &prepared, cancel_rx)
            .await
            .unwrap();
        let (download_id, bytes) = match result {
            BackendExecutionOutcome::Completed(DeviceResult::Browser {
                result:
                    BrowserBackendResult::DownloadStaged {
                        backend_download_id,
                        bytes_written,
                    },
            }) => (backend_download_id, bytes_written),
            other => panic!("unexpected download result: {other:?}"),
        };
        let finalized = download
            .finalize(
                prepared.operation_handle(),
                "ctx_0123456789abcdef0123456789abcdef",
                1,
                1,
                &download_id,
                bytes,
            )
            .unwrap();
        assert_eq!(
            STANDARD.decode(&finalized.data_base64).unwrap(),
            b"fixture-download"
        );
        assert!(!format!("{finalized:?}").contains(prepared.canonical_root()));
        adapter.shutdown().await.unwrap();
        let _ = fs::remove_dir_all(state);
    }

    #[tokio::test]
    async fn cua_transfer_backend_failures_after_dispatch_are_indeterminate() {
        use crate::v2_browser_runtime::{BrowserBackendCommand, BrowserStagedUploadFile};
        use crate::v2_browser_staging::{BrowserDownloadStagingBroker, BrowserUploadStagingBroker};
        use base64::{Engine as _, engine::general_purpose::STANDARD};

        let state = transfer_state_dir("transfer-failure");
        fs::create_dir_all(&state).unwrap();
        let upload = BrowserUploadStagingBroker::new(&state).unwrap();
        let staged = upload
            .stage(
                "ctx_0123456789abcdef0123456789abcdef",
                1,
                1,
                "payload.txt",
                &STANDARD.encode(b"upload"),
                6,
            )
            .unwrap();
        let resolved = upload
            .resolve(&staged.handle, "ctx_0123456789abcdef0123456789abcdef", 1, 1)
            .unwrap();
        let upload_command = BrowserBackendCommand::Upload {
            context_id: "ctx_0123456789abcdef0123456789abcdef".into(),
            backend_target_id: "target".into(),
            backend_tab_id: "tab".into(),
            backend_element_ref: "p1:upload".into(),
            staged_files: vec![BrowserStagedUploadFile {
                backend_file_handle: staged.handle,
            }],
        };
        let upload_adapter = fixture(vec![
            "scripts/mock_cua_mcp_backend.py".into(),
            "--fail-browser-upload".into(),
        ]);
        upload_adapter.connect().await.unwrap();
        let (_tx, rx) = watch::channel(false);
        assert!(matches!(
            upload_adapter
                .execute_browser_upload(&upload_command, &[resolved], rx)
                .await,
            Ok(BackendExecutionOutcome::BackendOutcomeIndeterminate)
        ));
        upload_adapter.shutdown().await.unwrap();

        let download = BrowserDownloadStagingBroker::new(&state).unwrap();
        let prepared = download
            .prepare(
                "ctx_0123456789abcdef0123456789abcdef",
                1,
                1,
                "artifact.txt",
                1024,
                false,
            )
            .unwrap();
        let command = BrowserBackendCommand::Download {
            context_id: "ctx_0123456789abcdef0123456789abcdef".into(),
            backend_target_id: "target".into(),
            backend_tab_id: "tab".into(),
            backend_element_ref: "p1:download".into(),
            destination_name: "artifact.txt".into(),
            max_bytes: 1024,
            overwrite: false,
        };
        let download_adapter = fixture(vec![
            "scripts/mock_cua_mcp_backend.py".into(),
            "--fail-browser-download".into(),
        ]);
        download_adapter.connect().await.unwrap();
        let (_tx, rx) = watch::channel(false);
        assert!(matches!(
            download_adapter
                .execute_browser_download(&command, &prepared, rx)
                .await,
            Ok(BackendExecutionOutcome::BackendOutcomeIndeterminate)
        ));
        download_adapter.shutdown().await.unwrap();
        let _ = fs::remove_dir_all(state);
    }

    #[tokio::test]
    async fn cua_download_timeout_and_cancellation_are_indeterminate() {
        use crate::v2_browser_runtime::BrowserBackendCommand;
        use crate::v2_browser_staging::BrowserDownloadStagingBroker;

        let state = transfer_state_dir("transfer-indeterminate");
        fs::create_dir_all(&state).unwrap();
        let download = BrowserDownloadStagingBroker::new(&state).unwrap();
        let command = BrowserBackendCommand::Download {
            context_id: "ctx_0123456789abcdef0123456789abcdef".into(),
            backend_target_id: "target".into(),
            backend_tab_id: "tab".into(),
            backend_element_ref: "p1:download".into(),
            destination_name: "artifact.txt".into(),
            max_bytes: 1024,
            overwrite: false,
        };

        let prepared = download
            .prepare(
                "ctx_0123456789abcdef0123456789abcdef",
                1,
                1,
                "artifact.txt",
                1024,
                false,
            )
            .unwrap();
        let timeout_adapter = fixture_with_timeout(
            vec![
                "scripts/mock_cua_mcp_backend.py".into(),
                "--slow-browser-download".into(),
            ],
            Duration::from_millis(100),
        );
        timeout_adapter.connect().await.unwrap();
        let (_tx, rx) = watch::channel(false);
        assert_eq!(
            timeout_adapter
                .execute_browser_download(&command, &prepared, rx)
                .await
                .unwrap(),
            BackendExecutionOutcome::TimedOutIndeterminate
        );
        timeout_adapter.shutdown().await.unwrap();
        download
            .abort(
                prepared.operation_handle(),
                "ctx_0123456789abcdef0123456789abcdef",
                1,
                1,
            )
            .unwrap();

        let marker = state.join("download-called");
        let prepared = download
            .prepare(
                "ctx_0123456789abcdef0123456789abcdef",
                1,
                1,
                "artifact.txt",
                1024,
                false,
            )
            .unwrap();
        let cancel_adapter = fixture(vec![
            "scripts/mock_cua_mcp_backend.py".into(),
            "--slow-browser-download".into(),
            "--transfer-marker".into(),
            marker.to_string_lossy().into_owned(),
        ]);
        cancel_adapter.connect().await.unwrap();
        let (cancel_tx, rx) = watch::channel(false);
        let adapter = cancel_adapter.clone();
        let command_for_task = command.clone();
        let prepared_for_task = prepared.clone();
        let task = tokio::spawn(async move {
            adapter
                .execute_browser_download(&command_for_task, &prepared_for_task, rx)
                .await
        });
        wait_for_file(&marker).await;
        cancel_tx.send(true).unwrap();
        assert_eq!(
            task.await.unwrap().unwrap(),
            BackendExecutionOutcome::CancellationPropagatedIndeterminate
        );
        cancel_adapter.shutdown().await.unwrap();
        let _ = fs::remove_dir_all(state);
    }

    #[tokio::test]
    async fn cua_upload_timeout_and_cancellation_are_indeterminate() {
        use crate::v2_browser_runtime::{BrowserBackendCommand, BrowserStagedUploadFile};
        use crate::v2_browser_staging::BrowserUploadStagingBroker;
        use base64::{Engine as _, engine::general_purpose::STANDARD};

        let state = transfer_state_dir("upload-indeterminate");
        fs::create_dir_all(&state).unwrap();
        let upload = BrowserUploadStagingBroker::new(&state).unwrap();
        let staged = upload
            .stage(
                "ctx_0123456789abcdef0123456789abcdef",
                1,
                1,
                "payload.txt",
                &STANDARD.encode(b"upload"),
                6,
            )
            .unwrap();
        let resolved = upload
            .resolve(&staged.handle, "ctx_0123456789abcdef0123456789abcdef", 1, 1)
            .unwrap();
        let command = BrowserBackendCommand::Upload {
            context_id: "ctx_0123456789abcdef0123456789abcdef".into(),
            backend_target_id: "target".into(),
            backend_tab_id: "tab".into(),
            backend_element_ref: "p1:upload".into(),
            staged_files: vec![BrowserStagedUploadFile {
                backend_file_handle: staged.handle,
            }],
        };

        let timeout_adapter = fixture_with_timeout(
            vec![
                "scripts/mock_cua_mcp_backend.py".into(),
                "--slow-browser-upload".into(),
            ],
            Duration::from_millis(100),
        );
        timeout_adapter.connect().await.unwrap();
        let (_tx, rx) = watch::channel(false);
        assert_eq!(
            timeout_adapter
                .execute_browser_upload(&command, std::slice::from_ref(&resolved), rx)
                .await
                .unwrap(),
            BackendExecutionOutcome::TimedOutIndeterminate
        );
        timeout_adapter.shutdown().await.unwrap();

        let marker = state.join("upload-called");
        let cancel_adapter = fixture(vec![
            "scripts/mock_cua_mcp_backend.py".into(),
            "--slow-browser-upload".into(),
            "--transfer-marker".into(),
            marker.to_string_lossy().into_owned(),
        ]);
        cancel_adapter.connect().await.unwrap();
        let (cancel_tx, rx) = watch::channel(false);
        let adapter = cancel_adapter.clone();
        let command_for_task = command.clone();
        let resolved_for_task = resolved.clone();
        let task = tokio::spawn(async move {
            adapter
                .execute_browser_upload(&command_for_task, &[resolved_for_task], rx)
                .await
        });
        wait_for_file(&marker).await;
        cancel_tx.send(true).unwrap();
        assert_eq!(
            task.await.unwrap().unwrap(),
            BackendExecutionOutcome::CancellationPropagatedIndeterminate
        );
        cancel_adapter.shutdown().await.unwrap();
        let _ = fs::remove_dir_all(state);
    }

    #[tokio::test]
    #[ignore = "trusted-mac real-Cua browser transfer acceptance"]
    async fn real_cua_browser_transfer_acceptance() {
        use crate::v2_browser_runtime::{
            BrowserBackendCommand, BrowserBackendResult, BrowserStagedUploadFile,
        };
        use crate::v2_browser_staging::{BrowserDownloadStagingBroker, BrowserUploadStagingBroker};
        use base64::{Engine as _, engine::general_purpose::STANDARD};

        assert_eq!(
            std::env::var("CUMG_V2_BROWSER_TRANSFER_E2E_ACK").as_deref(),
            Ok("1"),
            "explicit trusted-Mac acknowledgement required"
        );
        let command = std::env::var("CUMG_V2_CUA_COMMAND")
            .unwrap_or_else(|_| "/Users/sawadakousuke/.local/bin/cua-driver".into());
        let target = std::env::var("CUMG_V2_BROWSER_TARGET_ID").unwrap();
        let tab = std::env::var("CUMG_V2_BROWSER_TAB_ID").unwrap();
        let upload_ref = std::env::var("CUMG_V2_BROWSER_UPLOAD_REF").unwrap();
        let download_ref = std::env::var("CUMG_V2_BROWSER_DOWNLOAD_REF").unwrap();
        let context = "ctx_0123456789abcdef0123456789abcdef";
        let state = transfer_state_dir("real-transfer");
        fs::create_dir_all(&state).unwrap();

        let adapter = CuaMcpAdapter::new(
            command,
            vec!["mcp".into()],
            "0.19.3",
            "macos",
            1,
            Duration::from_secs(10),
            Duration::from_secs(40),
            1,
            Duration::from_millis(50),
        );
        adapter.connect().await.unwrap();
        let (_cancel_tx, cancel_rx) = watch::channel(false);

        let upload_staging = BrowserUploadStagingBroker::new(&state).unwrap();
        let staged = upload_staging
            .stage(
                context,
                1,
                1,
                "harmless-upload.txt",
                &STANDARD.encode(b"harmless-real-cua-upload\n"),
                25,
            )
            .unwrap();
        let resolved = upload_staging
            .resolve(&staged.handle, context, 1, 1)
            .unwrap();
        let upload_command = BrowserBackendCommand::Upload {
            context_id: context.into(),
            backend_target_id: target.clone(),
            backend_tab_id: tab.clone(),
            backend_element_ref: upload_ref,
            staged_files: vec![BrowserStagedUploadFile {
                backend_file_handle: staged.handle.clone(),
            }],
        };
        assert_eq!(
            adapter
                .execute_browser_upload(&upload_command, &[resolved], cancel_rx.clone())
                .await
                .unwrap(),
            BackendExecutionOutcome::Completed(DeviceResult::Browser {
                result: BrowserBackendResult::UploadAssigned { file_count: 1 },
            })
        );
        upload_staging
            .consume_handles(&[staged.handle], context, 1, 1)
            .unwrap();

        let download_staging = BrowserDownloadStagingBroker::new(&state).unwrap();
        let prepared = download_staging
            .prepare(context, 1, 1, "artifact.txt", 1024, false)
            .unwrap();
        let download_command = BrowserBackendCommand::Download {
            context_id: context.into(),
            backend_target_id: target.clone(),
            backend_tab_id: tab.clone(),
            backend_element_ref: download_ref,
            destination_name: "artifact.txt".into(),
            max_bytes: 1024,
            overwrite: false,
        };
        let staged_result = adapter
            .execute_browser_download(&download_command, &prepared, cancel_rx)
            .await
            .unwrap();
        let (download_id, bytes) = match staged_result {
            BackendExecutionOutcome::Completed(DeviceResult::Browser {
                result:
                    BrowserBackendResult::DownloadStaged {
                        backend_download_id,
                        bytes_written,
                    },
            }) => (backend_download_id, bytes_written),
            other => panic!("unexpected real-Cua download result: {other:?}"),
        };
        let finalized = download_staging
            .finalize(
                prepared.operation_handle(),
                context,
                1,
                1,
                &download_id,
                bytes,
            )
            .unwrap();
        assert_eq!(
            STANDARD.decode(&finalized.data_base64).unwrap(),
            b"deterministic-browser-download\n"
        );
        assert_eq!(finalized.destination_name, "artifact.txt");
        assert!(finalized.backend_download_handle.starts_with("download_"));

        let refused = download_staging
            .prepare(context, 1, 1, "refused.txt", 1024, false)
            .unwrap();
        let stale_command = BrowserBackendCommand::Download {
            context_id: context.into(),
            backend_target_id: target,
            backend_tab_id: tab,
            backend_element_ref: "p999999:999999".into(),
            destination_name: "refused.txt".into(),
            max_bytes: 1024,
            overwrite: false,
        };
        let (_failure_tx, failure_rx) = watch::channel(false);
        assert!(matches!(
            adapter
                .execute_browser_download(&stale_command, &refused, failure_rx)
                .await,
            Err(M1BackendError::BrowserRefused(_))
        ));
        download_staging
            .abort(refused.operation_handle(), context, 1, 1)
            .unwrap();

        adapter.end_interaction_session(context).await.unwrap();
        adapter.shutdown().await.unwrap();
        let _ = fs::remove_dir_all(state);
    }

    #[tokio::test]
    #[ignore = "trusted-mac real-Cua issue #47 browser alert acceptance"]
    async fn real_cua_browser_alert_backend_error_is_indeterminate() {
        use crate::v2_browser::{BrowserAction, BrowserInputRoute};
        use crate::v2_browser_runtime::{
            BrowserBackendClickTarget, BrowserBackendCommand, BrowserBackendResult,
        };

        assert_eq!(
            std::env::var("CUMG_V2_ISSUE47_E2E_ACK").as_deref(),
            Ok("1"),
            "explicit trusted-Mac acknowledgement required"
        );
        let command = std::env::var("CUMG_V2_CUA_COMMAND")
            .unwrap_or_else(|_| "/Users/sawadakousuke/.local/bin/cua-driver".into());
        let process_id: u32 = std::env::var("CUMG_V2_ISSUE47_BROWSER_PID")
            .expect("CUMG_V2_ISSUE47_BROWSER_PID is required")
            .parse()
            .expect("browser pid must be u32");
        let context = format!("ctx_{:032x}", rand::random::<u128>());
        let adapter = CuaMcpAdapter::new(
            command,
            vec!["mcp".into()],
            "0.19.3",
            "macos",
            1,
            Duration::from_secs(10),
            Duration::from_secs(30),
            1,
            Duration::from_millis(50),
        );
        adapter.connect().await.unwrap();
        let (_cancel_tx, cancel_rx) = watch::channel(false);

        let mut window_id = None;
        for _ in 0..30 {
            let windows = adapter
                .execute(
                    &DeviceCommand::ListWindows {
                        process_id: Some(process_id),
                        on_screen_only: true,
                    },
                    cancel_rx.clone(),
                )
                .await
                .unwrap();
            if let BackendExecutionOutcome::Completed(DeviceResult::Windows { windows, .. }) =
                windows
                && let Some(window) = windows.into_iter().find(|window| {
                    window.is_on_screen
                        && window.title == "CUMG issue47"
                        && window.bounds.width >= 400
                        && window.bounds.height >= 300
                })
            {
                window_id = Some(window.window_id);
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let window_id =
            window_id.expect("isolated issue #47 Chrome window did not become observable");

        let bound = adapter
            .execute(
                &DeviceCommand::Browser {
                    command: BrowserBackendCommand::Bind {
                        context_id: context.clone(),
                        process_id,
                        window_id,
                    },
                },
                cancel_rx.clone(),
            )
            .await
            .unwrap();
        let (target, tab) = match bound {
            BackendExecutionOutcome::Completed(DeviceResult::Browser {
                result:
                    BrowserBackendResult::Bound {
                        backend_target_id,
                        tabs,
                        ..
                    },
            }) => {
                let tab = tabs
                    .iter()
                    .find(|tab| tab.active == Some(true))
                    .or_else(|| tabs.first())
                    .cloned()
                    .expect("bound browser must expose a tab");
                (backend_target_id, tab.backend_tab_id)
            }
            other => panic!("unexpected browser bind result: {other:?}"),
        };

        let inspected = adapter
            .execute(
                &DeviceCommand::Browser {
                    command: BrowserBackendCommand::Inspect {
                        context_id: context.clone(),
                        backend_target_id: target.clone(),
                        backend_tab_id: tab.clone(),
                        backend_scope_ref: None,
                        query: Some("alert".into()),
                        backend_continuation: None,
                        include_screenshot: false,
                    },
                },
                cancel_rx.clone(),
            )
            .await
            .unwrap();
        let alert_ref = match inspected {
            BackendExecutionOutcome::Completed(DeviceResult::Browser {
                result: BrowserBackendResult::Snapshot { action_refs, .. },
            }) => action_refs
                .into_iter()
                .find(|reference| {
                    reference.actions.contains(&BrowserAction::Click)
                        && reference
                            .name
                            .as_deref()
                            .is_some_and(|name| name.eq_ignore_ascii_case("alert"))
                })
                .map(|reference| reference.backend_ref)
                .expect("fixture alert button must expose a click ref"),
            other => panic!("unexpected browser inspect result: {other:?}"),
        };

        let click = adapter
            .execute(
                &DeviceCommand::Browser {
                    command: BrowserBackendCommand::Click {
                        context_id: context.clone(),
                        backend_target_id: target,
                        backend_tab_id: tab,
                        target: BrowserBackendClickTarget::Element {
                            backend_element_ref: alert_ref,
                        },
                        input_route: BrowserInputRoute::DomEvent,
                    },
                },
                cancel_rx.clone(),
            )
            .await
            .unwrap();
        assert_eq!(click, BackendExecutionOutcome::BackendOutcomeIndeterminate);

        // The exact issue #47 condition is a backend failure after a visible
        // effect. Prove the JS alert is actually present independently through
        // the native window surface; do not infer it from the browser error.
        let mut observed_alert = false;
        for _ in 0..20 {
            let windows = adapter
                .execute(
                    &DeviceCommand::ListWindows {
                        process_id: Some(process_id),
                        on_screen_only: true,
                    },
                    cancel_rx.clone(),
                )
                .await
                .unwrap();
            if let BackendExecutionOutcome::Completed(DeviceResult::Windows { windows, .. }) =
                windows
            {
                for window in windows {
                    if window.window_id == window_id || window.bounds.width < 200 {
                        continue;
                    }
                    let inspected = adapter
                        .execute(
                            &DeviceCommand::InspectWindow {
                                process_id,
                                window_id: window.window_id,
                                query: Some("GATEWAY_ALERT_OK".into()),
                                max_elements: 64,
                                max_depth: 8,
                                include_screenshot: false,
                            },
                            cancel_rx.clone(),
                        )
                        .await;
                    if let Ok(BackendExecutionOutcome::Completed(DeviceResult::WindowSnapshot {
                        elements,
                        ..
                    })) = inspected
                        && elements.iter().any(|element| {
                            element
                                .label
                                .as_deref()
                                .is_some_and(|label| label.contains("GATEWAY_ALERT_OK"))
                                || element
                                    .value
                                    .as_deref()
                                    .is_some_and(|value| value.contains("GATEWAY_ALERT_OK"))
                        })
                    {
                        observed_alert = true;
                        break;
                    }
                }
            }
            if observed_alert {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(
            observed_alert,
            "browser click must have produced the visible alert before the backend error"
        );

        adapter.end_interaction_session(&context).await.unwrap();
        adapter.shutdown().await.unwrap();
    }

    #[tokio::test]
    #[ignore = "trusted-mac real-Cua native element action acceptance"]
    async fn real_cua_native_element_action_acceptance() {
        assert_eq!(
            std::env::var("CUMG_V2_NATIVE_ELEMENT_E2E_ACK").as_deref(),
            Ok("1"),
            "explicit trusted-Mac acknowledgement required"
        );
        let command = std::env::var("CUMG_V2_CUA_COMMAND")
            .unwrap_or_else(|_| "/Users/sawadakousuke/.local/bin/cua-driver".into());
        let context = format!("ctx_{:032x}", rand::random::<u128>());
        let adapter = CuaMcpAdapter::new(
            command,
            vec!["mcp".into()],
            "0.19.3",
            "macos",
            1,
            Duration::from_secs(10),
            Duration::from_secs(20),
            1,
            Duration::from_millis(50),
        );
        adapter.connect().await.unwrap();
        let (_cancel_tx, cancel_rx) = watch::channel(false);

        let launch = adapter
            .execute(
                &DeviceCommand::LaunchApplication {
                    identifier: Some("com.apple.calculator".into()),
                    name: None,
                    targets: vec![],
                    new_instance: true,
                },
                cancel_rx.clone(),
            )
            .await
            .unwrap();
        let process_id = match launch {
            BackendExecutionOutcome::Completed(DeviceResult::ApplicationLaunched {
                process_id,
                ..
            }) => process_id,
            other => panic!("unexpected Calculator launch result: {other:?}"),
        };

        let mut window_id = None;
        for _ in 0..20 {
            let windows = adapter
                .execute(
                    &DeviceCommand::ListWindows {
                        process_id: Some(process_id),
                        on_screen_only: false,
                    },
                    cancel_rx.clone(),
                )
                .await
                .unwrap();
            if let BackendExecutionOutcome::Completed(DeviceResult::Windows { windows, .. }) =
                windows
            {
                if let Some(window) = windows.into_iter().find(|window| {
                    window.window_id != 0
                        && window.is_on_screen
                        && window.bounds.width >= 150
                        && window.bounds.height >= 150
                }) {
                    window_id = Some(window.window_id);
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let window_id = window_id.expect("Calculator window did not become observable");

        let inspected = adapter
            .execute(
                &DeviceCommand::InspectWindowContextual {
                    context_id: context.clone(),
                    process_id,
                    window_id,
                    query: Some("7".into()),
                    max_elements: 128,
                    max_depth: 32,
                    include_screenshot: true,
                },
                cancel_rx.clone(),
            )
            .await
            .unwrap();
        let (element_ref, before_screenshot) = match inspected {
            BackendExecutionOutcome::Completed(DeviceResult::WindowSnapshot {
                elements,
                screenshot: Some(screenshot),
                ..
            }) => {
                let element_ref = elements
                    .into_iter()
                    .find(|element| {
                        element.role == UiRole::Button
                            && element
                                .label
                                .as_deref()
                                .is_some_and(|label| label.trim() == "7")
                    })
                    .map(|element| element.element_ref)
                    .expect("Calculator 7 button was not exposed as an actionable element");
                (element_ref, screenshot.data_base64)
            }
            other => panic!("unexpected Calculator inspect result: {other:?}"),
        };

        let wrong_window_id = window_id.checked_add(1).expect("window id overflow");
        assert!(
            adapter
                .execute(
                    &DeviceCommand::PointerClickAdvanced {
                        context_id: Some(context.clone()),
                        target: PointerTarget::Element {
                            process_id,
                            window_id: wrong_window_id,
                            element_ref: element_ref.clone(),
                        },
                        button: crate::v2_m0::PointerButton::Left,
                        click_count: 1,
                        action: Some(UiElementAction::Press),
                        modifiers: vec![],
                        delivery: InputDeliveryMode::Background,
                    },
                    cancel_rx.clone(),
                )
                .await
                .is_err(),
            "an element ref must not be usable with a different native window"
        );

        assert_eq!(
            adapter
                .execute(
                    &DeviceCommand::PointerClickAdvanced {
                        context_id: Some(context.clone()),
                        target: PointerTarget::Element {
                            process_id,
                            window_id,
                            element_ref,
                        },
                        button: crate::v2_m0::PointerButton::Left,
                        click_count: 1,
                        action: Some(UiElementAction::Press),
                        modifiers: vec![],
                        delivery: InputDeliveryMode::Background,
                    },
                    cancel_rx.clone(),
                )
                .await
                .unwrap(),
            BackendExecutionOutcome::Completed(DeviceResult::PointerClickCompleted)
        );
        tokio::time::sleep(Duration::from_millis(200)).await;

        let after = adapter
            .execute(
                &DeviceCommand::InspectWindowContextual {
                    context_id: context.clone(),
                    process_id,
                    window_id,
                    query: Some("7".into()),
                    max_elements: 128,
                    max_depth: 32,
                    include_screenshot: true,
                },
                cancel_rx.clone(),
            )
            .await
            .unwrap();
        match after {
            BackendExecutionOutcome::Completed(DeviceResult::WindowSnapshot {
                screenshot: Some(screenshot),
                ..
            }) => {
                assert_ne!(
                    screenshot.data_base64, before_screenshot,
                    "Calculator window did not visually change after the background AX element press"
                );
            }
            other => panic!("unexpected Calculator post-click inspect result: {other:?}"),
        }

        let _ = adapter
            .execute(
                &DeviceCommand::TerminateApplication { process_id },
                cancel_rx,
            )
            .await;
        adapter.end_interaction_session(&context).await.unwrap();
        adapter.shutdown().await.unwrap();
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
                    cancel_rx.clone(),
                )
                .await
                .unwrap(),
            BackendExecutionOutcome::Completed(DeviceResult::TypeTextCompleted)
        );
        assert_eq!(
            adapter
                .execute(
                    &DeviceCommand::ListWindows {
                        process_id: Some(101),
                        on_screen_only: true,
                    },
                    cancel_rx.clone(),
                )
                .await
                .unwrap(),
            BackendExecutionOutcome::Completed(DeviceResult::Windows {
                windows: vec![WindowInfo {
                    window_id: 77,
                    process_id: 101,
                    application: "Fixture A".into(),
                    title: "Main".into(),
                    bounds: UiRect {
                        x: 10,
                        y: 20,
                        width: 800,
                        height: 600
                    },
                    is_on_screen: true,
                    on_current_workspace: Some(true),
                }],
                truncated: false,
            })
        );
        assert!(matches!(
            adapter
                .execute(
                    &DeviceCommand::LaunchApplication {
                        identifier: Some("fixture.app".into()),
                        name: None,
                        targets: vec![],
                        new_instance: false,
                    },
                    cancel_rx.clone(),
                )
                .await
                .unwrap(),
            BackendExecutionOutcome::Completed(DeviceResult::ApplicationLaunched {
                process_id: 101,
                process_running: true,
                window_ready: true,
                ..
            })
        ));
        let inspected = adapter
            .execute(
                &DeviceCommand::InspectWindow {
                    process_id: 101,
                    window_id: 77,
                    query: None,
                    max_elements: 50,
                    max_depth: 10,
                    include_screenshot: true,
                },
                cancel_rx.clone(),
            )
            .await
            .unwrap();
        match inspected {
            BackendExecutionOutcome::Completed(DeviceResult::WindowSnapshot {
                snapshot_ref,
                elements,
                elements_complete,
                screenshot,
                ..
            }) => {
                assert_eq!(snapshot_ref, "sfixture1");
                assert!(elements_complete);
                assert_eq!(elements.len(), 2);
                assert_eq!(elements[0].role, UiRole::Window);
                assert_eq!(elements[1].role, UiRole::Button);
                assert_eq!(elements[1].parent_ref.as_deref(), Some("sfixture1:0"));
                let screenshot = screenshot.unwrap();
                assert_eq!(
                    (screenshot.width_pixels, screenshot.height_pixels),
                    (230, 408)
                );
            }
            other => panic!("unexpected inspection result: {other:?}"),
        }
        assert_eq!(
            adapter
                .execute(
                    &DeviceCommand::VerifyUiState {
                        process_id: 101,
                        window_id: 77,
                        predicates: vec![UiPredicate::ElementExists {
                            selector: UiElementSelector {
                                role: Some(UiRole::Button),
                                label_contains: Some("Run".into()),
                            },
                        }],
                        timeout_ms: 0,
                        stable_samples: 1,
                        include_screenshot: false,
                    },
                    cancel_rx,
                )
                .await
                .unwrap(),
            BackendExecutionOutcome::Completed(DeviceResult::UiStateVerification {
                status: VerificationStatus::Satisfied,
                stable: true,
                samples: 1,
                predicates: vec![UiPredicateResult {
                    status: VerificationStatus::Satisfied,
                    unknown_reason: None,
                }],
                screenshot: None,
            })
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
    fn desktop_semantic_mapping_preserves_scope_delivery_and_backend_neutral_modifiers() {
        let context = "ctx_0123456789abcdef0123456789abcdef".to_owned();
        let (tool, args) = map_command(&DeviceCommand::PointerClickAdvanced {
            context_id: Some(context.clone()),
            target: PointerTarget::WindowPhysical {
                process_id: 42,
                window_id: 7,
                x: 10,
                y: 20,
            },
            button: crate::v2_m0::PointerButton::Left,
            click_count: 2,
            action: None,
            modifiers: vec![KeyboardModifier::Meta],
            delivery: InputDeliveryMode::Foreground,
        })
        .unwrap();
        assert_eq!(tool, "click");
        let args = Value::Object(args.unwrap());
        assert_eq!(args["session"], context);
        assert_eq!(args["scope"], "window");
        assert_eq!(args["pid"], 42);
        assert_eq!(args["window_id"], 7);
        assert_eq!(args["modifier"], serde_json::json!(["cmd"]));
        assert_eq!(args["delivery_mode"], "foreground");
        assert_eq!(args["count"], 2);

        assert!(matches!(
            map_command(&DeviceCommand::PointerClickAdvanced {
                context_id: None,
                target: PointerTarget::WindowPhysical {
                    process_id: 42,
                    window_id: 7,
                    x: 10,
                    y: 20,
                },
                button: crate::v2_m0::PointerButton::Left,
                click_count: 1,
                action: None,
                modifiers: vec![KeyboardModifier::Meta],
                delivery: InputDeliveryMode::Background,
            }),
            Err(M1BackendError::InvalidRequest(_))
        ));
        assert!(matches!(
            map_command(&DeviceCommand::PointerDragAdvanced {
                context_id: None,
                from: PointerTarget::WindowPhysical {
                    process_id: 42,
                    window_id: 7,
                    x: 1,
                    y: 2,
                },
                to: PointerTarget::WindowPhysical {
                    process_id: 42,
                    window_id: 8,
                    x: 3,
                    y: 4,
                },
                button: crate::v2_m0::PointerButton::Left,
                modifiers: vec![],
                delivery: InputDeliveryMode::Background,
                duration_ms: 100,
                steps: 10,
            }),
            Err(M1BackendError::InvalidRequest(_))
        ));
    }

    #[test]
    fn native_element_actions_map_to_exact_cua_element_tokens() {
        let context = "ctx_0123456789abcdef0123456789abcdef".to_owned();
        let target = PointerTarget::Element {
            process_id: 42,
            window_id: 7,
            element_ref: "backend-element-token".into(),
        };

        let (tool, args) = map_command(&DeviceCommand::PointerClickAdvanced {
            context_id: Some(context.clone()),
            target: target.clone(),
            button: crate::v2_m0::PointerButton::Left,
            click_count: 1,
            action: Some(UiElementAction::Open),
            modifiers: vec![],
            delivery: InputDeliveryMode::Background,
        })
        .unwrap();
        assert_eq!(tool, "click");
        let args = Value::Object(args.unwrap());
        assert_eq!(args["session"], context);
        assert_eq!(args["pid"], 42);
        assert_eq!(args["window_id"], 7);
        assert_eq!(args["element_token"], "backend-element-token");
        assert_eq!(args["action"], "open");
        assert!(args.get("x").is_none());
        assert!(args.get("y").is_none());

        let (tool, args) = map_command(&DeviceCommand::PointerClickAdvanced {
            context_id: Some(context.clone()),
            target: target.clone(),
            button: crate::v2_m0::PointerButton::Left,
            click_count: 2,
            action: None,
            modifiers: vec![],
            delivery: InputDeliveryMode::Background,
        })
        .unwrap();
        assert_eq!(tool, "double_click");
        let args = Value::Object(args.unwrap());
        assert_eq!(args["element_token"], "backend-element-token");
        assert_eq!(args["session"], context);

        let (tool, args) = map_command(&DeviceCommand::TypeTextAdvanced {
            context_id: Some(context.clone()),
            text: "hello".into(),
            target: InputTarget::Element {
                process_id: 42,
                window_id: 7,
                element_ref: "backend-element-token".into(),
            },
            delivery: InputDeliveryMode::Background,
            delay_ms: 30,
        })
        .unwrap();
        assert_eq!(tool, "type_text");
        let args = Value::Object(args.unwrap());
        assert_eq!(args["element_token"], "backend-element-token");
        assert_eq!(args["pid"], 42);
        assert_eq!(args["window_id"], 7);

        let (tool, args) = map_command(&DeviceCommand::KeyboardInput {
            context_id: Some(context),
            key: "return".into(),
            modifiers: vec![],
            target: InputTarget::Element {
                process_id: 42,
                window_id: 7,
                element_ref: "backend-element-token".into(),
            },
            delivery: InputDeliveryMode::Background,
        })
        .unwrap();
        assert_eq!(tool, "press_key");
        let args = Value::Object(args.unwrap());
        assert_eq!(args["element_token"], "backend-element-token");
    }

    #[test]
    fn native_element_click_options_fail_closed() {
        let target = PointerTarget::Element {
            process_id: 42,
            window_id: 7,
            element_ref: "backend-element-token".into(),
        };
        assert!(matches!(
            map_command(&DeviceCommand::PointerClickAdvanced {
                context_id: Some("ctx_0123456789abcdef0123456789abcdef".into()),
                target: target.clone(),
                button: crate::v2_m0::PointerButton::Left,
                click_count: 3,
                action: None,
                modifiers: vec![],
                delivery: InputDeliveryMode::Background,
            }),
            Err(M1BackendError::InvalidRequest(_))
        ));
        assert!(matches!(
            map_command(&DeviceCommand::PointerClickAdvanced {
                context_id: Some("ctx_0123456789abcdef0123456789abcdef".into()),
                target,
                button: crate::v2_m0::PointerButton::Right,
                click_count: 2,
                action: None,
                modifiers: vec![],
                delivery: InputDeliveryMode::Background,
            }),
            Err(M1BackendError::InvalidRequest(_))
        ));
    }

    #[test]
    fn scoped_value_region_and_escalation_map_without_raw_backend_escape_hatch() {
        let context = "ctx_0123456789abcdef0123456789abcdef";
        let (tool, args) = map_command(&DeviceCommand::SetUiValue {
            context_id: context.into(),
            process_id: 42,
            window_id: 7,
            element_ref: "backend-element-token".into(),
            value: "new-value".into(),
        })
        .unwrap();
        assert_eq!(tool, "set_value");
        let args = Value::Object(args.unwrap());
        assert_eq!(args["session"], context);
        assert_eq!(args["element_token"], "backend-element-token");

        let (tool, args) = map_command(&DeviceCommand::CaptureRegion {
            context_id: Some(context.into()),
            process_id: 42,
            window_id: 7,
            bounds: UiRect {
                x: 10,
                y: 20,
                width: 100,
                height: 50,
            },
        })
        .unwrap();
        assert_eq!(tool, "zoom");
        let args = Value::Object(args.unwrap());
        assert_eq!(args["x1"], 10);
        assert_eq!(args["y1"], 20);
        assert_eq!(args["x2"], 110);
        assert_eq!(args["y2"], 70);
        assert_eq!(args["session"], context);

        let (tool, args) = map_command(&DeviceCommand::ExpandInteractionScope {
            context_id: context.into(),
            reason: "window routes exhausted".into(),
        })
        .unwrap();
        assert_eq!(tool, "escalate_session");
        let args = Value::Object(args.unwrap());
        assert_eq!(args["session"], context);
        assert_eq!(args["reason"], "other");
        assert_eq!(args["detail"], "window routes exhausted");
    }

    #[tokio::test]
    async fn end_interaction_session_stays_backend_lifecycle_and_uses_exact_context() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let marker = std::env::temp_dir().join(format!(
            "cumg-m1-end-session-{}-{nonce}",
            std::process::id()
        ));
        let adapter = fixture(vec![
            "scripts/mock_mcp_backend.py".into(),
            "--args-marker".into(),
            marker.to_string_lossy().into_owned(),
        ]);
        adapter.connect().await.unwrap();
        let context_id = "ctx_0123456789abcdef0123456789abcdef";
        adapter.end_interaction_session(context_id).await.unwrap();
        wait_for_file(&marker).await;
        let recorded: Value = serde_json::from_str(&fs::read_to_string(&marker).unwrap()).unwrap();
        assert_eq!(recorded["tool"], "end_session");
        assert_eq!(recorded["arguments"]["session"], context_id);
        assert!(
            adapter
                .end_interaction_session("bad-context")
                .await
                .is_err()
        );
        adapter.shutdown().await.unwrap();
        let _ = fs::remove_file(marker);
    }

    #[tokio::test]
    async fn contextual_verification_revives_reclaimed_cua_session_without_desktop_scope() {
        let marker = transfer_state_dir("verify-session-refresh-marker");
        let adapter = fixture(vec![
            "scripts/mock_mcp_backend.py".into(),
            "--args-marker".into(),
            marker.to_string_lossy().into_owned(),
        ]);
        adapter.connect().await.unwrap();
        let context_id = "ctx_0123456789abcdef0123456789abcdef";
        adapter.end_interaction_session(context_id).await.unwrap();

        let (_cancel_tx, cancel_rx) = watch::channel(false);
        let outcome = adapter
            .execute(
                &DeviceCommand::VerifyUiStateContextual {
                    context_id: context_id.into(),
                    process_id: 101,
                    window_id: 77,
                    predicates: vec![UiPredicate::WindowExists { exists: true }],
                    timeout_ms: 1_000,
                    stable_samples: 1,
                    include_screenshot: false,
                },
                cancel_rx,
            )
            .await
            .unwrap();
        assert!(matches!(
            outcome,
            BackendExecutionOutcome::Completed(DeviceResult::UiStateVerification {
                status: VerificationStatus::Satisfied,
                ..
            })
        ));

        wait_for_file(&marker).await;
        let recorded: Value = serde_json::from_str(&fs::read_to_string(&marker).unwrap()).unwrap();
        assert_eq!(recorded["tool"], "start_session");
        assert_eq!(recorded["arguments"]["session"], context_id);
        assert_eq!(recorded["arguments"]["capture_scope"], "auto");

        adapter.shutdown().await.unwrap();
        let _ = fs::remove_file(marker);
    }

    #[tokio::test]
    async fn contextual_verification_refresh_failure_never_dispatches_verify_state() {
        let verify_marker = transfer_state_dir("verify-after-refresh-failure-marker");
        let adapter = fixture(vec![
            "scripts/mock_mcp_backend.py".into(),
            "--fail-start-session".into(),
            "--verify-marker".into(),
            verify_marker.to_string_lossy().into_owned(),
        ]);
        adapter.connect().await.unwrap();
        let (_cancel_tx, cancel_rx) = watch::channel(false);
        let outcome = adapter
            .execute(
                &DeviceCommand::VerifyUiStateContextual {
                    context_id: "ctx_0123456789abcdef0123456789abcdef".into(),
                    process_id: 101,
                    window_id: 77,
                    predicates: vec![UiPredicate::WindowExists { exists: true }],
                    timeout_ms: 1_000,
                    stable_samples: 1,
                    include_screenshot: false,
                },
                cancel_rx,
            )
            .await
            .unwrap();
        assert_eq!(
            outcome,
            BackendExecutionOutcome::BackendOutcomeIndeterminate
        );
        assert!(
            !verify_marker.exists(),
            "verification must not dispatch after session refresh failure"
        );
        adapter.shutdown().await.unwrap();
        let _ = fs::remove_file(verify_marker);
    }

    #[tokio::test]
    async fn desktop_fixture_normalizes_activation_clipboard_pointer_region_and_scope() {
        let adapter = fixture(vec!["scripts/mock_mcp_backend.py".into()]);
        adapter.connect().await.unwrap();
        let (_cancel_tx, cancel_rx) = watch::channel(false);
        assert_eq!(
            adapter
                .execute(
                    &DeviceCommand::ActivateWindow {
                        process_id: 101,
                        window_id: Some(77),
                    },
                    cancel_rx.clone(),
                )
                .await
                .unwrap(),
            BackendExecutionOutcome::Completed(DeviceResult::WindowActivated {
                process_id: 101,
                window_id: Some(77),
                process_activated: true,
                exact_window_verified: Some(true),
            })
        );
        assert_eq!(
            adapter
                .execute(
                    &DeviceCommand::ClipboardRead {
                        context_id: None,
                        include_text: true,
                    },
                    cancel_rx.clone(),
                )
                .await
                .unwrap(),
            BackendExecutionOutcome::Completed(DeviceResult::ClipboardState {
                types: vec!["public.utf8-plain-text".into()],
                text: Some("fixture clipboard".into()),
            })
        );
        assert_eq!(
            adapter
                .execute(
                    &DeviceCommand::PointerPosition { context_id: None },
                    cancel_rx.clone(),
                )
                .await
                .unwrap(),
            BackendExecutionOutcome::Completed(DeviceResult::PointerPosition {
                x_points: 123,
                y_points: 456
            })
        );
        let region = adapter
            .execute(
                &DeviceCommand::CaptureRegion {
                    context_id: None,
                    process_id: 101,
                    window_id: 77,
                    bounds: UiRect {
                        x: 0,
                        y: 0,
                        width: 120,
                        height: 80,
                    },
                },
                cancel_rx.clone(),
            )
            .await
            .unwrap();
        assert!(matches!(
            region,
            BackendExecutionOutcome::Completed(DeviceResult::RegionCaptured {
                image: UiImage {
                    width_pixels: 144,
                    height_pixels: 96,
                    ..
                }
            })
        ));
        assert_eq!(
            adapter
                .execute(
                    &DeviceCommand::ExpandInteractionScope {
                        context_id: "ctx_0123456789abcdef0123456789abcdef".into(),
                        reason: "test".into(),
                    },
                    cancel_rx,
                )
                .await
                .unwrap(),
            BackendExecutionOutcome::Completed(DeviceResult::InteractionScopeExpanded)
        );
        adapter.shutdown().await.unwrap();
    }

    #[test]
    fn list_windows_omits_zero_area_backend_records_but_keeps_strict_validation() {
        let value = serde_json::json!({
            "windows": [
                {
                    "window_id": 1,
                    "pid": 10,
                    "app_name": "Helper",
                    "title": "",
                    "bounds": {"x": 0, "y": 0, "width": 0, "height": 0},
                    "is_on_screen": false,
                    "on_current_space": null
                },
                {
                    "window_id": 2,
                    "pid": 11,
                    "app_name": "App",
                    "title": "Main",
                    "bounds": {"x": 10, "y": 20, "width": 800, "height": 600},
                    "is_on_screen": true,
                    "on_current_space": true
                }
            ]
        });
        assert_eq!(
            normalize_windows_result(&value).unwrap(),
            DeviceResult::Windows {
                windows: vec![WindowInfo {
                    window_id: 2,
                    process_id: 11,
                    application: "App".into(),
                    title: "Main".into(),
                    bounds: UiRect {
                        x: 10,
                        y: 20,
                        width: 800,
                        height: 600,
                    },
                    is_on_screen: true,
                    on_current_workspace: Some(true),
                }],
                truncated: false,
            }
        );

        let malformed = serde_json::json!({
            "windows": [{
                "window_id": 3,
                "pid": 12,
                "app_name": "Broken",
                "title": "",
                "bounds": {"x": 0, "y": 0, "width": 0, "height": 0},
                "on_current_space": null
            }]
        });
        assert!(matches!(
            normalize_windows_result(&malformed),
            Err(M1BackendError::MalformedResponse(
                "window missing is_on_screen"
            ))
        ));

        let zero_area_target = serde_json::json!({
            "window_id": 4,
            "pid": 13,
            "app_name": "Helper",
            "title": "",
            "bounds": {"x": 0, "y": 0, "width": 0, "height": 10},
            "is_on_screen": false,
            "on_current_space": null
        });
        assert!(matches!(
            normalize_window(&zero_area_target),
            Err(M1BackendError::MalformedResponse(
                "rectangle must be positive"
            ))
        ));

        let mut bounded = Vec::new();
        for index in 0..MAX_WINDOW_RESULTS {
            bounded.push(serde_json::json!({
                "window_id": index as u64 + 10,
                "pid": 100,
                "app_name": "App",
                "title": "Window",
                "bounds": {"x": 0, "y": 0, "width": 10, "height": 10},
                "is_on_screen": true,
                "on_current_space": true
            }));
        }
        bounded.push(serde_json::json!({
            "window_id": 999_998,
            "pid": 100,
            "app_name": "Helper",
            "title": "",
            "bounds": {"x": 0, "y": 0, "width": 0, "height": 0},
            "is_on_screen": false,
            "on_current_space": null
        }));
        let at_limit =
            normalize_windows_result(&serde_json::json!({"windows": bounded.clone()})).unwrap();
        assert!(matches!(
            at_limit,
            DeviceResult::Windows {
                truncated: false,
                ..
            }
        ));

        bounded.push(serde_json::json!({
            "window_id": 999_999,
            "pid": 100,
            "app_name": "App",
            "title": "Overflow",
            "bounds": {"x": 0, "y": 0, "width": 10, "height": 10},
            "is_on_screen": true,
            "on_current_space": true
        }));
        let over_limit =
            normalize_windows_result(&serde_json::json!({"windows": bounded})).unwrap();
        assert!(matches!(
            over_limit,
            DeviceResult::Windows {
                truncated: true,
                ..
            }
        ));
    }

    #[test]
    fn gui_semantic_commands_map_to_cua_without_leaking_cua_names_northbound() {
        let (tool, args) = map_command(&DeviceCommand::ListWindows {
            process_id: Some(42),
            on_screen_only: true,
        })
        .unwrap();
        assert_eq!(tool, "list_windows");
        assert_eq!(Value::Object(args.unwrap())["pid"], 42);

        let (tool, args) = map_command(&DeviceCommand::VerifyUiState {
            process_id: 42,
            window_id: 7,
            predicates: vec![UiPredicate::ElementExists {
                selector: UiElementSelector {
                    role: Some(UiRole::Button),
                    label_contains: Some("Save".into()),
                },
            }],
            timeout_ms: 1000,
            stable_samples: 2,
            include_screenshot: false,
        })
        .unwrap();
        assert_eq!(tool, "verify_state");
        let args = Value::Object(args.unwrap());
        assert_eq!(args["expect"][0]["element"]["selector"]["role"], "AXButton");
        assert_eq!(
            args["expect"][0]["element"]["selector"]["label_contains"],
            "Save"
        );
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
