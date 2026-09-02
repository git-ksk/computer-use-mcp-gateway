//! Core Cua execution for resolved V2 browser commands.
//!
//! This module deliberately accepts only `BrowserBackendCommand`, which means
//! Hub-side CUMG refs have already been validated and resolved. Upload/download
//! remain fail-closed until their separate staging/transfer boundary is wired.

use crate::backend::{
    BackendCallCancelled, BackendCallResponseLost, BackendCallTimedOut, ComputerUseBackend,
    cua::CuaBackend,
};
use crate::v2_browser::{
    BrowserDialogAction, BrowserDialogDelivery, BrowserInputRoute, BrowserPointerAction,
    BrowserPrepareProfileMode, BrowserTypeMode, MAX_BROWSER_PROFILE_NAME_BYTES,
    MAX_BROWSER_PROMPT_TEXT_BYTES, MAX_BROWSER_QUERY_BYTES, MAX_BROWSER_SCROLL_DELTA_CSS_PX,
    MAX_BROWSER_TEXT_BYTES, MAX_BROWSER_URL_BYTES,
};
use crate::v2_browser_normalize::{
    BrowserNormalizeError, normalize_cua_browser_binding, normalize_cua_browser_snapshot,
};
use crate::v2_browser_runtime::{
    BrowserBackendClickTarget, BrowserBackendCommand, BrowserBackendResult,
    BrowserBackendScreenshot, BrowserBackendSemanticRef, BrowserBackendTab, BrowserMutationEffect,
};
use crate::v2_browser_staging::ResolvedStagedUpload;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use rmcp::model::{CallToolResult, JsonObject};
use serde_json::{Map, Value, json};
use std::fmt;
use tokio::sync::watch;

pub(crate) const MAX_BROWSER_SCREENSHOT_BYTES: usize = 16 * 1024 * 1024;
const MAX_CUA_ISOLATED_PROFILE_NAME_BYTES: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BrowserExecutionOutcome {
    Completed(BrowserBackendResult),
    CancellationPropagatedIndeterminate,
    TimedOutIndeterminate,
    BackendOutcomeIndeterminate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowserRefusalReason {
    RouteUnavailable,
    RequiresSetup,
    BindingAmbiguous,
    BindingStale,
    WrongTarget,
    TabRequired,
    TabNotFound,
    RefStale,
    InputTrustUnavailable,
    EndpointOwnerMismatch,
    ConsentRequired,
    ConsentRevoked,
    ReconnectExhausted,
    InputIncomplete,
    ActionUnavailable,
    OriginOutsideScope,
    Other,
}

impl BrowserRefusalReason {
    pub const fn safe_code(self) -> &'static str {
        match self {
            Self::RouteUnavailable => "browser_route_unavailable",
            Self::RequiresSetup => "browser_requires_setup",
            Self::BindingAmbiguous => "browser_binding_ambiguous",
            Self::BindingStale => "browser_binding_stale",
            Self::WrongTarget => "browser_wrong_target_refused",
            Self::TabRequired => "browser_tab_required",
            Self::TabNotFound => "browser_tab_not_found",
            Self::RefStale => "browser_ref_stale",
            Self::InputTrustUnavailable => "browser_input_trust_unavailable",
            Self::EndpointOwnerMismatch => "browser_endpoint_owner_mismatch",
            Self::ConsentRequired => "browser_consent_required",
            Self::ConsentRevoked => "browser_consent_revoked",
            Self::ReconnectExhausted => "browser_reconnect_exhausted",
            Self::InputIncomplete => "browser_input_incomplete",
            Self::ActionUnavailable => "browser_action_unavailable",
            Self::OriginOutsideScope => "browser_origin_outside_scope",
            Self::Other => "browser_refused",
        }
    }
}

#[derive(Debug)]
pub(crate) enum BrowserExecutionError {
    InvalidRequest(&'static str),
    Backend(anyhow::Error),
    BackendToolError,
    BackendRefused(BrowserRefusalReason),
    Normalize(BrowserNormalizeError),
    InvalidResult(&'static str),
    UnsupportedTransferBoundary,
}

impl fmt::Display for BrowserExecutionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRequest(message) | Self::InvalidResult(message) => f.write_str(message),
            Self::Backend(_) => f.write_str("browser backend failure"),
            Self::BackendToolError => f.write_str("browser backend tool failure"),
            Self::BackendRefused(reason) => write!(f, "browser backend refused: {reason:?}"),
            Self::Normalize(error) => write!(f, "browser result normalization failed: {error}"),
            Self::UnsupportedTransferBoundary => {
                f.write_str("browser transfer boundary is not enabled")
            }
        }
    }
}

impl std::error::Error for BrowserExecutionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Backend(error) => Some(error.as_ref()),
            Self::Normalize(error) => Some(error),
            _ => None,
        }
    }
}

impl From<BrowserNormalizeError> for BrowserExecutionError {
    fn from(value: BrowserNormalizeError) -> Self {
        Self::Normalize(value)
    }
}

pub(crate) async fn execute_cua_browser(
    backend: &CuaBackend,
    command: &BrowserBackendCommand,
    cancellation: watch::Receiver<bool>,
) -> Result<BrowserExecutionOutcome, BrowserExecutionError> {
    validate_command(command)?;
    if matches!(
        command,
        BrowserBackendCommand::Upload { .. } | BrowserBackendCommand::Download { .. }
    ) {
        return Err(BrowserExecutionError::UnsupportedTransferBoundary);
    }

    let (tool, arguments) = map_command(command)?;
    let raw = match backend.call_tool(tool, arguments, cancellation).await {
        Ok(result) => result,
        Err(error) if error.downcast_ref::<BackendCallCancelled>().is_some() => {
            return Ok(BrowserExecutionOutcome::CancellationPropagatedIndeterminate);
        }
        Err(error) if error.downcast_ref::<BackendCallTimedOut>().is_some() => {
            return Ok(BrowserExecutionOutcome::TimedOutIndeterminate);
        }
        Err(error) if error.downcast_ref::<BackendCallResponseLost>().is_some() => {
            if browser_command_is_read_only(command) {
                return Err(BrowserExecutionError::Backend(error));
            }
            return Ok(BrowserExecutionOutcome::BackendOutcomeIndeterminate);
        }
        Err(error) => return Err(BrowserExecutionError::Backend(error)),
    };
    if let Some(reason) = refusal_reason(&raw) {
        return Err(BrowserExecutionError::BackendRefused(reason));
    }
    if raw.is_error == Some(true) {
        if browser_command_is_read_only(command) {
            return Err(BrowserExecutionError::BackendToolError);
        }
        return Ok(BrowserExecutionOutcome::BackendOutcomeIndeterminate);
    }
    match normalize_completed(command, &raw) {
        Ok(result) => Ok(BrowserExecutionOutcome::Completed(result)),
        Err(_error) if !browser_command_is_read_only(command) => {
            Ok(BrowserExecutionOutcome::BackendOutcomeIndeterminate)
        }
        Err(error) => Err(error),
    }
}

fn browser_command_is_read_only(command: &BrowserBackendCommand) -> bool {
    matches!(
        command,
        BrowserBackendCommand::Bind { .. }
            | BrowserBackendCommand::Inspect { .. }
            | BrowserBackendCommand::Dialog {
                action: BrowserDialogAction::Inspect,
                ..
            }
    )
}

const CUA_BROWSER_DOWNLOAD_APPROVAL_ARG: &str = "_cua_browser_download_mcp_host_approved";

pub(crate) async fn execute_cua_browser_upload(
    backend: &CuaBackend,
    command: &BrowserBackendCommand,
    files: &[ResolvedStagedUpload],
    cancellation: watch::Receiver<bool>,
) -> Result<BrowserExecutionOutcome, BrowserExecutionError> {
    validate_command(command)?;
    let BrowserBackendCommand::Upload {
        context_id,
        backend_target_id,
        backend_tab_id,
        backend_element_ref,
        staged_files,
    } = command
    else {
        return Err(BrowserExecutionError::UnsupportedTransferBoundary);
    };
    if files.len() != staged_files.len() {
        return Err(BrowserExecutionError::InvalidRequest(
            "browser upload resolution mismatch",
        ));
    }
    let mut args = Map::new();
    target_tab_args(&mut args, context_id, backend_target_id, backend_tab_id);
    args.insert("ref".into(), json!(backend_element_ref));
    args.insert(
        "files".into(),
        Value::Array(
            files
                .iter()
                .map(|file| json!(file.canonical_path()))
                .collect(),
        ),
    );
    let raw = match backend
        .call_tool("browser_set_input_files", Some(args), cancellation)
        .await
    {
        Ok(result) => result,
        Err(error) if error.downcast_ref::<BackendCallCancelled>().is_some() => {
            return Ok(BrowserExecutionOutcome::CancellationPropagatedIndeterminate);
        }
        Err(error) if error.downcast_ref::<BackendCallTimedOut>().is_some() => {
            return Ok(BrowserExecutionOutcome::TimedOutIndeterminate);
        }
        Err(error) if error.downcast_ref::<BackendCallResponseLost>().is_some() => {
            return Ok(BrowserExecutionOutcome::BackendOutcomeIndeterminate);
        }
        Err(error) => return Err(BrowserExecutionError::Backend(error)),
    };
    if let Some(reason) = refusal_reason(&raw) {
        return Err(BrowserExecutionError::BackendRefused(reason));
    }
    if raw.is_error == Some(true) {
        return Ok(BrowserExecutionOutcome::BackendOutcomeIndeterminate);
    }
    let completion = (|| -> Result<BrowserBackendResult, BrowserExecutionError> {
        let structured = structured_value(&raw)?;
        require_exact_target_tab_ok(&structured, backend_target_id, backend_tab_id)?;
        if structured.get("ref").and_then(Value::as_str) != Some(backend_element_ref.as_str()) {
            return Err(BrowserExecutionError::InvalidResult(
                "browser upload result ref mismatch",
            ));
        }
        let file_count = structured
            .get("file_count")
            .and_then(Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
            .ok_or(BrowserExecutionError::InvalidResult(
                "browser upload result omitted file_count",
            ))?;
        if usize::try_from(file_count).ok() != Some(files.len()) {
            return Err(BrowserExecutionError::InvalidResult(
                "browser upload file_count mismatch",
            ));
        }
        Ok(BrowserBackendResult::UploadAssigned { file_count })
    })();
    match completion {
        Ok(result) => Ok(BrowserExecutionOutcome::Completed(result)),
        Err(_) => Ok(BrowserExecutionOutcome::BackendOutcomeIndeterminate),
    }
}

pub(crate) async fn execute_cua_browser_download(
    backend: &CuaBackend,
    command: &BrowserBackendCommand,
    destination_root: &str,
    cancellation: watch::Receiver<bool>,
) -> Result<BrowserExecutionOutcome, BrowserExecutionError> {
    validate_command(command)?;
    let BrowserBackendCommand::Download {
        context_id,
        backend_target_id,
        backend_tab_id,
        backend_element_ref,
        max_bytes,
        ..
    } = command
    else {
        return Err(BrowserExecutionError::UnsupportedTransferBoundary);
    };
    if destination_root.is_empty() || destination_root.contains('\0') {
        return Err(BrowserExecutionError::InvalidRequest(
            "invalid Agent-private browser download root",
        ));
    }
    let mut args = Map::new();
    target_tab_args(&mut args, context_id, backend_target_id, backend_tab_id);
    args.insert("ref".into(), json!(backend_element_ref));
    args.insert("destination_root".into(), json!(destination_root));
    // CUMG has already granted the exact dangerous BrowserDownload capability.
    // This private adapter flag bridges that explicit host approval to Cua; it is
    // never part of the northbound schema and never caller-controlled.
    args.insert(CUA_BROWSER_DOWNLOAD_APPROVAL_ARG.into(), json!(true));
    let raw = match backend
        .call_tool("browser_download", Some(args), cancellation)
        .await
    {
        Ok(result) => result,
        Err(error) if error.downcast_ref::<BackendCallCancelled>().is_some() => {
            return Ok(BrowserExecutionOutcome::CancellationPropagatedIndeterminate);
        }
        Err(error) if error.downcast_ref::<BackendCallTimedOut>().is_some() => {
            return Ok(BrowserExecutionOutcome::TimedOutIndeterminate);
        }
        Err(error) if error.downcast_ref::<BackendCallResponseLost>().is_some() => {
            return Ok(BrowserExecutionOutcome::BackendOutcomeIndeterminate);
        }
        Err(error) => return Err(BrowserExecutionError::Backend(error)),
    };
    if let Some(reason) = refusal_reason(&raw) {
        return Err(BrowserExecutionError::BackendRefused(reason));
    }
    if raw.is_error == Some(true) {
        return Ok(BrowserExecutionOutcome::BackendOutcomeIndeterminate);
    }
    let completion = (|| -> Result<BrowserBackendResult, BrowserExecutionError> {
        let structured = structured_value(&raw)?;
        if structured.get("status").and_then(Value::as_str) != Some("completed") {
            return Err(BrowserExecutionError::InvalidResult(
                "browser download did not prove completion",
            ));
        }
        let backend_download_id = structured
            .get("download_id")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty() && value.len() <= 512 && !value.contains('\0'))
            .ok_or(BrowserExecutionError::InvalidResult(
                "browser download omitted a bounded opaque id",
            ))?
            .to_owned();
        let bytes_written = structured.get("bytes").and_then(Value::as_u64).ok_or(
            BrowserExecutionError::InvalidResult("browser download omitted byte count"),
        )?;
        if bytes_written > *max_bytes
            || bytes_written > crate::v2_browser::MAX_BROWSER_DOWNLOAD_BYTES
        {
            return Err(BrowserExecutionError::InvalidResult(
                "browser download exceeded the CUMG byte bound",
            ));
        }
        Ok(BrowserBackendResult::DownloadStaged {
            backend_download_id,
            bytes_written,
        })
    })();
    match completion {
        Ok(result) => Ok(BrowserExecutionOutcome::Completed(result)),
        Err(_) => Ok(BrowserExecutionOutcome::BackendOutcomeIndeterminate),
    }
}

fn validate_command(command: &BrowserBackendCommand) -> Result<(), BrowserExecutionError> {
    let context = command.context_id();
    if !valid_context_id(context) {
        return Err(BrowserExecutionError::InvalidRequest(
            "invalid browser interaction context id",
        ));
    }
    match command {
        BrowserBackendCommand::Prepare {
            process_id,
            window_id,
            profile_mode,
            profile_name,
            ..
        } => {
            if *process_id == 0 {
                return Err(BrowserExecutionError::InvalidRequest(
                    "browser process id must be non-zero",
                ));
            }
            match profile_mode {
                BrowserPrepareProfileMode::ExistingProfile if window_id.is_none() => {
                    return Err(BrowserExecutionError::InvalidRequest(
                        "existing-profile preparation requires an exact window",
                    ));
                }
                BrowserPrepareProfileMode::IsolatedNamed => {
                    let name =
                        profile_name
                            .as_deref()
                            .ok_or(BrowserExecutionError::InvalidRequest(
                                "named profile requires a name",
                            ))?;
                    if name.is_empty()
                        || name.len() > MAX_BROWSER_PROFILE_NAME_BYTES
                        || name.len() > MAX_CUA_ISOLATED_PROFILE_NAME_BYTES
                        || name.chars().any(char::is_control)
                        || !name
                            .bytes()
                            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
                    {
                        return Err(BrowserExecutionError::InvalidRequest(
                            "invalid isolated profile name",
                        ));
                    }
                }
                _ => {}
            }
        }
        BrowserBackendCommand::Bind {
            process_id,
            window_id,
            ..
        } if *process_id == 0 || *window_id == 0 => {
            return Err(BrowserExecutionError::InvalidRequest(
                "browser bind needs a non-zero process and window id",
            ));
        }
        BrowserBackendCommand::Inspect {
            backend_target_id,
            backend_tab_id,
            backend_scope_ref,
            query,
            backend_continuation,
            ..
        } => {
            validate_handle(backend_target_id)?;
            validate_handle(backend_tab_id)?;
            if let Some(reference) = backend_scope_ref {
                validate_handle(reference)?;
            }
            if let Some(continuation) = backend_continuation {
                validate_handle(continuation)?;
            }
            if let Some(query) = query {
                validate_bounded_text(query, MAX_BROWSER_QUERY_BYTES, "browser query is invalid")?;
            }
        }
        BrowserBackendCommand::Navigate {
            backend_target_id,
            backend_tab_id,
            url,
            ..
        } => {
            validate_target_tab(backend_target_id, backend_tab_id)?;
            validate_url(url)?;
        }
        BrowserBackendCommand::Click {
            backend_target_id,
            backend_tab_id,
            target,
            input_route,
            ..
        } => {
            validate_target_tab(backend_target_id, backend_tab_id)?;
            match target {
                BrowserBackendClickTarget::Element {
                    backend_element_ref,
                } => validate_handle(backend_element_ref)?,
                BrowserBackendClickTarget::ViewportCss { .. }
                    if *input_route == BrowserInputRoute::DomEvent =>
                {
                    return Err(BrowserExecutionError::InvalidRequest(
                        "synthetic browser click requires an element ref",
                    ));
                }
                BrowserBackendClickTarget::ViewportCss { .. } => {}
            }
        }
        BrowserBackendCommand::Type {
            backend_target_id,
            backend_tab_id,
            backend_element_ref,
            text,
            ..
        } => {
            validate_target_tab(backend_target_id, backend_tab_id)?;
            validate_handle(backend_element_ref)?;
            if text.len() > MAX_BROWSER_TEXT_BYTES {
                return Err(BrowserExecutionError::InvalidRequest(
                    "browser text exceeds the bound",
                ));
            }
        }
        BrowserBackendCommand::Dialog {
            backend_target_id,
            backend_tab_id,
            backend_dialog_id,
            prompt_text,
            action,
            delivery,
            ..
        } => {
            validate_target_tab(backend_target_id, backend_tab_id)?;
            match action {
                BrowserDialogAction::Inspect => {
                    if backend_dialog_id.is_some()
                        || prompt_text.is_some()
                        || *delivery != BrowserDialogDelivery::Background
                    {
                        return Err(BrowserExecutionError::InvalidRequest(
                            "browser dialog inspect must not carry resolution authority",
                        ));
                    }
                }
                BrowserDialogAction::Accept | BrowserDialogAction::Dismiss => {
                    validate_handle(backend_dialog_id.as_deref().ok_or(
                        BrowserExecutionError::InvalidRequest(
                            "browser dialog resolution requires a dialog ref",
                        ),
                    )?)?;
                    if let Some(prompt) = prompt_text {
                        if *action != BrowserDialogAction::Accept {
                            return Err(BrowserExecutionError::InvalidRequest(
                                "prompt text is valid only when accepting a dialog",
                            ));
                        }
                        if prompt.len() > MAX_BROWSER_PROMPT_TEXT_BYTES {
                            return Err(BrowserExecutionError::InvalidRequest(
                                "browser prompt text exceeds the bound",
                            ));
                        }
                    }
                }
            }
        }
        BrowserBackendCommand::Pointer {
            backend_target_id,
            backend_tab_id,
            backend_element_ref,
            action,
            backend_destination_ref,
            delta_x,
            delta_y,
            ..
        } => {
            validate_target_tab(backend_target_id, backend_tab_id)?;
            validate_handle(backend_element_ref)?;
            if delta_x.unsigned_abs() > MAX_BROWSER_SCROLL_DELTA_CSS_PX as u32
                || delta_y.unsigned_abs() > MAX_BROWSER_SCROLL_DELTA_CSS_PX as u32
            {
                return Err(BrowserExecutionError::InvalidRequest(
                    "browser pointer delta exceeds the bound",
                ));
            }
            if *action == BrowserPointerAction::Drag {
                validate_handle(backend_destination_ref.as_deref().ok_or(
                    BrowserExecutionError::InvalidRequest(
                        "browser drag requires a destination ref",
                    ),
                )?)?;
            } else if backend_destination_ref.is_some() {
                return Err(BrowserExecutionError::InvalidRequest(
                    "destination ref is valid only for browser drag",
                ));
            }
        }
        BrowserBackendCommand::Upload {
            backend_target_id,
            backend_tab_id,
            backend_element_ref,
            staged_files,
            ..
        } => {
            validate_target_tab(backend_target_id, backend_tab_id)?;
            validate_handle(backend_element_ref)?;
            if staged_files.is_empty()
                || staged_files.len() > crate::v2_browser::MAX_BROWSER_UPLOAD_FILES
            {
                return Err(BrowserExecutionError::InvalidRequest(
                    "invalid browser upload set",
                ));
            }
            for file in staged_files {
                validate_handle(&file.backend_file_handle)?;
            }
        }
        BrowserBackendCommand::Download {
            backend_target_id,
            backend_tab_id,
            backend_element_ref,
            destination_name,
            max_bytes,
            ..
        } => {
            validate_target_tab(backend_target_id, backend_tab_id)?;
            validate_handle(backend_element_ref)?;
            crate::v2_browser::validate_download_destination_name(destination_name).map_err(
                |_| {
                    BrowserExecutionError::InvalidRequest(
                        "invalid browser download destination name",
                    )
                },
            )?;
            if *max_bytes == 0 || *max_bytes > crate::v2_browser::MAX_BROWSER_DOWNLOAD_BYTES {
                return Err(BrowserExecutionError::InvalidRequest(
                    "invalid browser download byte bound",
                ));
            }
        }
        BrowserBackendCommand::Bind { .. } => {}
    }
    Ok(())
}

fn map_command(
    command: &BrowserBackendCommand,
) -> Result<(&'static str, Option<JsonObject>), BrowserExecutionError> {
    let mut args = Map::new();
    match command {
        BrowserBackendCommand::Prepare {
            context_id,
            process_id,
            window_id,
            allow_launch,
            profile_mode,
            profile_name,
        } => {
            args.insert("pid".into(), json!(process_id));
            args.insert("session".into(), json!(context_id));
            match profile_mode {
                BrowserPrepareProfileMode::IsolatedNew => {
                    args.insert("allow_launch".into(), json!(allow_launch));
                    args.insert("profile".into(), json!({"mode": "isolated_new"}));
                }
                BrowserPrepareProfileMode::IsolatedNamed => {
                    args.insert("allow_launch".into(), json!(allow_launch));
                    args.insert(
                        "profile".into(),
                        json!({
                            "mode": "isolated_named",
                            "name": profile_name.as_deref().expect("validated profile name"),
                        }),
                    );
                }
                BrowserPrepareProfileMode::ExistingProfile => {
                    args.insert(
                        "window_id".into(),
                        json!(window_id.expect("validated window")),
                    );
                    args.insert("strategy".into(), json!({"kind": "existing_profile"}));
                }
            }
            Ok(("browser_prepare", Some(args)))
        }
        BrowserBackendCommand::Bind {
            context_id,
            process_id,
            window_id,
        } => Ok((
            "get_browser_state",
            object(json!({
                "pid": process_id,
                "window_id": window_id,
                "session": context_id,
            })),
        )),
        BrowserBackendCommand::Inspect {
            context_id,
            backend_target_id,
            backend_tab_id,
            backend_scope_ref,
            query,
            backend_continuation,
            include_screenshot,
        } => {
            target_tab_args(&mut args, context_id, backend_target_id, backend_tab_id);
            args.insert("snapshot_format".into(), json!("semantic_v2"));
            args.insert("include_screenshot".into(), json!(include_screenshot));
            if let Some(reference) = backend_scope_ref {
                args.insert("scope_ref".into(), json!(reference));
            }
            if let Some(query) = query {
                args.insert("query".into(), json!(query));
            }
            if let Some(continuation) = backend_continuation {
                args.insert("continuation".into(), json!(continuation));
            }
            Ok(("get_browser_state", Some(args)))
        }
        BrowserBackendCommand::Navigate {
            context_id,
            backend_target_id,
            backend_tab_id,
            url,
        } => {
            target_tab_args(&mut args, context_id, backend_target_id, backend_tab_id);
            args.insert("url".into(), json!(url));
            Ok(("browser_navigate", Some(args)))
        }
        BrowserBackendCommand::Click {
            context_id,
            backend_target_id,
            backend_tab_id,
            target,
            input_route,
        } => {
            target_tab_args(&mut args, context_id, backend_target_id, backend_tab_id);
            args.insert("input_route".into(), json!(input_route_name(*input_route)));
            match target {
                BrowserBackendClickTarget::Element {
                    backend_element_ref,
                } => {
                    args.insert("ref".into(), json!(backend_element_ref));
                }
                BrowserBackendClickTarget::ViewportCss { x, y } => {
                    args.insert("x".into(), json!(x));
                    args.insert("y".into(), json!(y));
                }
            }
            Ok(("browser_click", Some(args)))
        }
        BrowserBackendCommand::Type {
            context_id,
            backend_target_id,
            backend_tab_id,
            backend_element_ref,
            text,
            mode,
            replace,
        } => {
            target_tab_args(&mut args, context_id, backend_target_id, backend_tab_id);
            args.insert("ref".into(), json!(backend_element_ref));
            args.insert("text".into(), json!(text));
            args.insert(
                "mode".into(),
                json!(match mode {
                    BrowserTypeMode::InsertText => "insert_text",
                    BrowserTypeMode::Keystrokes => "keystrokes",
                }),
            );
            args.insert("replace".into(), json!(replace));
            Ok(("browser_type", Some(args)))
        }
        BrowserBackendCommand::Dialog {
            context_id,
            backend_target_id,
            backend_tab_id,
            backend_dialog_id,
            action,
            prompt_text,
            delivery,
        } => {
            target_tab_args(&mut args, context_id, backend_target_id, backend_tab_id);
            args.insert(
                "action".into(),
                json!(match action {
                    BrowserDialogAction::Inspect => "inspect",
                    BrowserDialogAction::Accept => "accept",
                    BrowserDialogAction::Dismiss => "dismiss",
                }),
            );
            if let Some(dialog_id) = backend_dialog_id {
                args.insert("dialog_id".into(), json!(dialog_id));
            }
            if *action != BrowserDialogAction::Inspect {
                args.insert(
                    "delivery_mode".into(),
                    json!(match delivery {
                        BrowserDialogDelivery::Background => "background",
                        BrowserDialogDelivery::Foreground => "foreground",
                    }),
                );
            }
            if let Some(prompt) = prompt_text {
                args.insert("prompt_text".into(), json!(prompt));
            }
            Ok(("browser_dialog", Some(args)))
        }
        BrowserBackendCommand::Pointer {
            context_id,
            backend_target_id,
            backend_tab_id,
            backend_element_ref,
            action,
            backend_destination_ref,
            delta_x,
            delta_y,
            input_route,
        } => {
            target_tab_args(&mut args, context_id, backend_target_id, backend_tab_id);
            args.insert("ref".into(), json!(backend_element_ref));
            args.insert("input_route".into(), json!(input_route_name(*input_route)));
            args.insert(
                "action".into(),
                json!(match action {
                    BrowserPointerAction::Hover => "hover",
                    BrowserPointerAction::RightClick => "right_click",
                    BrowserPointerAction::DoubleClick => "double_click",
                    BrowserPointerAction::Scroll => "scroll",
                    BrowserPointerAction::Drag => "drag",
                }),
            );
            if *action == BrowserPointerAction::Scroll {
                args.insert("delta_x".into(), json!(delta_x));
                args.insert("delta_y".into(), json!(delta_y));
            }
            if let Some(destination) = backend_destination_ref {
                args.insert("destination_ref".into(), json!(destination));
            }
            Ok(("browser_pointer", Some(args)))
        }
        BrowserBackendCommand::Upload { .. } | BrowserBackendCommand::Download { .. } => {
            Err(BrowserExecutionError::UnsupportedTransferBoundary)
        }
    }
}

fn normalize_completed(
    command: &BrowserBackendCommand,
    raw: &CallToolResult,
) -> Result<BrowserBackendResult, BrowserExecutionError> {
    let structured = structured_value(raw)?;
    match command {
        BrowserBackendCommand::Prepare { .. } => {
            require_status_ok(&structured)?;
            let prepared = structured.get("prepared").and_then(Value::as_bool).ok_or(
                BrowserExecutionError::InvalidResult(
                    "browser prepare result omitted prepared state",
                ),
            )?;
            let prepared_process_id = structured
                .get("prepared_pid")
                .and_then(Value::as_u64)
                .map(u32::try_from)
                .transpose()
                .map_err(|_| BrowserExecutionError::InvalidResult("invalid prepared process id"))?;
            let side_effect_count = normalize_prepare_side_effect_count(&structured)?;
            Ok(BrowserBackendResult::Prepared {
                prepared,
                prepared_process_id,
                side_effect_count,
            })
        }
        BrowserBackendCommand::Bind {
            process_id,
            window_id,
            ..
        } => {
            let binding = normalize_cua_browser_binding(&structured)?;
            Ok(BrowserBackendResult::Bound {
                backend_target_id: binding.backend_target_id().to_owned(),
                process_id: *process_id,
                window_id: *window_id,
                tabs: binding
                    .tabs()
                    .iter()
                    .map(|tab| BrowserBackendTab {
                        backend_tab_id: tab.backend_tab_id().to_owned(),
                        title: tab.title.clone(),
                        url: tab.url.clone(),
                        active: tab.active,
                    })
                    .collect(),
            })
        }
        BrowserBackendCommand::Inspect {
            backend_target_id,
            backend_tab_id,
            include_screenshot,
            ..
        } => {
            let snapshot =
                normalize_cua_browser_snapshot(&structured, backend_target_id, backend_tab_id)?;
            let backend_continuation = snapshot.backend_continuation().map(str::to_owned);
            let screenshot = if *include_screenshot {
                Some(normalize_screenshot(raw, &structured)?)
            } else {
                None
            };
            Ok(BrowserBackendResult::Snapshot {
                backend_snapshot_id: snapshot.backend_snapshot_id().to_owned(),
                outline: snapshot.outline,
                action_refs: snapshot
                    .action_refs
                    .into_iter()
                    .map(|reference| BrowserBackendSemanticRef {
                        backend_ref: reference.backend_ref().to_owned(),
                        role: reference.role,
                        name: reference.name,
                        value: reference.value,
                        states: reference.states,
                        actions: reference.actions,
                        frame: reference.frame,
                        visibility: reference.visibility,
                    })
                    .collect(),
                content_refs: snapshot
                    .content_refs
                    .into_iter()
                    .map(|reference| BrowserBackendSemanticRef {
                        backend_ref: reference.backend_ref().to_owned(),
                        role: reference.role,
                        name: reference.name,
                        value: reference.value,
                        states: reference.states,
                        actions: Vec::new(),
                        frame: reference.frame,
                        visibility: reference.visibility,
                    })
                    .collect(),
                complete: snapshot.complete,
                omitted: snapshot.omitted,
                backend_continuation,
                screenshot,
            })
        }
        BrowserBackendCommand::Navigate {
            backend_target_id,
            backend_tab_id,
            url,
            ..
        } => {
            require_exact_target_tab_ok(&structured, backend_target_id, backend_tab_id)?;
            if structured.get("url").and_then(Value::as_str) != Some(url.as_str())
                || structured.get("refs_invalidated").and_then(Value::as_bool) != Some(true)
            {
                return Err(BrowserExecutionError::InvalidResult(
                    "browser navigation result did not prove the requested document transition",
                ));
            }
            Ok(BrowserBackendResult::NavigationCompleted)
        }
        BrowserBackendCommand::Click {
            backend_target_id,
            backend_tab_id,
            input_route,
            ..
        } => {
            let outcome = if is_closed_action_result(&structured) {
                normalize_browser_action_result(&structured, expected_action_route(*input_route))?
            } else {
                require_exact_target_tab_ok(&structured, backend_target_id, backend_tab_id)?;
                BrowserClosedActionOutcome::Legacy
            };
            Ok(BrowserBackendResult::ClickCompleted {
                effect: match outcome {
                    BrowserClosedActionOutcome::Confirmed => BrowserMutationEffect::Dispatched,
                    BrowserClosedActionOutcome::Unverifiable => BrowserMutationEffect::Unverifiable,
                    BrowserClosedActionOutcome::Legacy => {
                        if *input_route == BrowserInputRoute::DomEvent {
                            BrowserMutationEffect::Unverifiable
                        } else {
                            BrowserMutationEffect::Dispatched
                        }
                    }
                },
            })
        }
        BrowserBackendCommand::Type {
            backend_target_id,
            backend_tab_id,
            ..
        } => {
            if is_closed_action_result(&structured) {
                normalize_browser_action_result(&structured, "trusted_input")?;
            } else {
                require_exact_target_tab_ok(&structured, backend_target_id, backend_tab_id)?;
            }
            Ok(BrowserBackendResult::TypeCompleted)
        }
        BrowserBackendCommand::Dialog {
            backend_target_id,
            backend_tab_id,
            backend_dialog_id,
            action,
            ..
        } => {
            require_exact_target_tab_ok(&structured, backend_target_id, backend_tab_id)?;
            if *action == BrowserDialogAction::Inspect {
                let present = structured.get("present").and_then(Value::as_bool).ok_or(
                    BrowserExecutionError::InvalidResult(
                        "browser dialog inspect omitted present state",
                    ),
                )?;
                let backend_dialog_id = structured
                    .get("dialog_id")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                let kind = structured
                    .get("kind")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                if present != backend_dialog_id.is_some() || present != kind.is_some() {
                    return Err(BrowserExecutionError::InvalidResult(
                        "browser dialog inspect returned inconsistent state",
                    ));
                }
                if let Some(kind) = kind.as_deref() {
                    if !matches!(kind, "alert" | "confirm" | "prompt" | "beforeunload") {
                        return Err(BrowserExecutionError::InvalidResult(
                            "browser dialog inspect returned an unknown kind",
                        ));
                    }
                }
                if let Some(dialog_id) = backend_dialog_id.as_deref() {
                    validate_handle(dialog_id)?;
                }
                Ok(BrowserBackendResult::DialogObserved {
                    present,
                    backend_dialog_id,
                    kind,
                })
            } else {
                let expected_dialog_id =
                    backend_dialog_id
                        .as_deref()
                        .ok_or(BrowserExecutionError::InvalidResult(
                            "browser dialog resolution omitted requested identity",
                        ))?;
                let expected_action = match action {
                    BrowserDialogAction::Accept => "accept",
                    BrowserDialogAction::Dismiss => "dismiss",
                    BrowserDialogAction::Inspect => unreachable!("inspect handled above"),
                };
                if structured.get("dialog_id").and_then(Value::as_str) != Some(expected_dialog_id)
                    || structured.get("action").and_then(Value::as_str) != Some(expected_action)
                {
                    return Err(BrowserExecutionError::InvalidResult(
                        "browser dialog result did not prove the requested resolution",
                    ));
                }
                Ok(BrowserBackendResult::DialogCompleted)
            }
        }
        BrowserBackendCommand::Pointer {
            backend_target_id,
            backend_tab_id,
            input_route,
            ..
        } => {
            if is_closed_action_result(&structured) {
                normalize_browser_action_result(&structured, expected_action_route(*input_route))?;
            } else {
                require_exact_target_tab_ok(&structured, backend_target_id, backend_tab_id)?;
            }
            Ok(BrowserBackendResult::PointerCompleted)
        }
        BrowserBackendCommand::Upload { .. } | BrowserBackendCommand::Download { .. } => {
            Err(BrowserExecutionError::UnsupportedTransferBoundary)
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BrowserClosedActionOutcome {
    Confirmed,
    Unverifiable,
    Legacy,
}

fn expected_action_route(input_route: BrowserInputRoute) -> &'static str {
    match input_route {
        BrowserInputRoute::Trusted => "trusted_input",
        BrowserInputRoute::DomEvent => "dom",
    }
}

fn is_closed_action_result(value: &Value) -> bool {
    value.get("effect").is_some() || value.get("route").is_some()
}

fn normalize_browser_action_result(
    value: &Value,
    expected_route: &str,
) -> Result<BrowserClosedActionOutcome, BrowserExecutionError> {
    const ALLOWED_TOP_LEVEL: &[&str] = &["effect", "route", "delivery", "evidence", "escalation"];
    let object = value
        .as_object()
        .ok_or(BrowserExecutionError::InvalidResult(
            "browser action result was not an object",
        ))?;
    if object
        .keys()
        .any(|key| !ALLOWED_TOP_LEVEL.contains(&key.as_str()))
    {
        return Err(BrowserExecutionError::InvalidResult(
            "browser action result contained unknown fields",
        ));
    }
    let route =
        object
            .get("route")
            .and_then(Value::as_str)
            .ok_or(BrowserExecutionError::InvalidResult(
                "browser action result omitted route",
            ))?;
    if route != expected_route {
        return Err(BrowserExecutionError::InvalidResult(
            "browser action result used an unexpected route",
        ));
    }
    let effect = object.get("effect").and_then(Value::as_str).ok_or(
        BrowserExecutionError::InvalidResult("browser action result omitted effect"),
    )?;
    if let Some(escalation) = object.get("escalation").filter(|value| !value.is_null()) {
        let escalation = escalation
            .as_object()
            .ok_or(BrowserExecutionError::InvalidResult(
                "browser action escalation was not an object",
            ))?;
        if escalation
            .keys()
            .any(|key| !matches!(key.as_str(), "target" | "reason"))
        {
            return Err(BrowserExecutionError::InvalidResult(
                "browser action escalation contained unknown fields",
            ));
        }
        let verification_only = effect == "unverifiable"
            && escalation.get("target").and_then(Value::as_str) == Some("page")
            && escalation.get("reason").and_then(Value::as_str) == Some("effect_unconfirmed");
        if !verification_only {
            return Err(BrowserExecutionError::BackendRefused(
                BrowserRefusalReason::Other,
            ));
        }
    }
    if let Some(delivery) = object.get("delivery").filter(|value| !value.is_null()) {
        let delivery = delivery
            .as_object()
            .ok_or(BrowserExecutionError::InvalidResult(
                "browser action delivery was not an object",
            ))?;
        if delivery
            .keys()
            .any(|key| !matches!(key.as_str(), "mode" | "delivered_count"))
        {
            return Err(BrowserExecutionError::InvalidResult(
                "browser action delivery contained unknown fields",
            ));
        }
        if delivery.get("mode").and_then(Value::as_str) != Some("background") {
            return Err(BrowserExecutionError::InvalidResult(
                "browser action delivery was not the browser background route",
            ));
        }
        if let Some(count) = delivery.get("delivered_count") {
            let count = count.as_u64().ok_or(BrowserExecutionError::InvalidResult(
                "browser action delivered_count was invalid",
            ))?;
            u32::try_from(count).map_err(|_| {
                BrowserExecutionError::InvalidResult(
                    "browser action delivered_count exceeded the bound",
                )
            })?;
        }
    }
    let evidence_count =
        if let Some(evidence) = object.get("evidence").filter(|value| !value.is_null()) {
            let evidence = evidence
                .as_array()
                .ok_or(BrowserExecutionError::InvalidResult(
                    "browser action evidence was not an array",
                ))?;
            if evidence.len() > 8 {
                return Err(BrowserExecutionError::InvalidResult(
                    "browser action evidence exceeded the bound",
                ));
            }
            for item in evidence {
                let item = item
                    .as_object()
                    .ok_or(BrowserExecutionError::InvalidResult(
                        "browser action evidence item was not an object",
                    ))?;
                if item.len() != 1
                    || !matches!(
                        item.get("kind").and_then(Value::as_str),
                        Some("value_readback" | "window_change")
                    )
                {
                    return Err(BrowserExecutionError::InvalidResult(
                        "browser action evidence was outside the closed vocabulary",
                    ));
                }
            }
            evidence.len()
        } else {
            0
        };
    match effect {
        "confirmed" if evidence_count > 0 => Ok(BrowserClosedActionOutcome::Confirmed),
        "confirmed" => Err(BrowserExecutionError::InvalidResult(
            "confirmed browser action omitted evidence",
        )),
        "unverifiable" => Ok(BrowserClosedActionOutcome::Unverifiable),
        "partial" => Err(BrowserExecutionError::BackendRefused(
            BrowserRefusalReason::InputIncomplete,
        )),
        "refused" | "suspected_noop" => Err(BrowserExecutionError::BackendRefused(
            BrowserRefusalReason::Other,
        )),
        _ => Err(BrowserExecutionError::InvalidResult(
            "browser action result contained an unknown effect",
        )),
    }
}

fn require_exact_target_tab_ok(
    structured: &Value,
    backend_target_id: &str,
    backend_tab_id: &str,
) -> Result<(), BrowserExecutionError> {
    require_status_ok(structured)?;
    if structured.get("target_id").and_then(Value::as_str) != Some(backend_target_id)
        || structured.get("tab_id").and_then(Value::as_str) != Some(backend_tab_id)
    {
        return Err(BrowserExecutionError::InvalidResult(
            "browser result did not echo the exact target and tab",
        ));
    }
    Ok(())
}

fn normalize_prepare_side_effect_count(structured: &Value) -> Result<u32, BrowserExecutionError> {
    const KNOWN: &[&str] = &[
        "launched_browser",
        "restarted_browser",
        "created_profile",
        "reused_driver_profile",
        "copied_profile_data",
        "changed_preferences",
        "displayed_consent_prompt",
        "opened_setup_page",
        "closed_setup_page",
        "enabled_remote_debugging",
        "used_bounded_pixel_fallback",
        "focused_setup_address_field",
        "foregrounded_window",
        "injected_global_input",
    ];
    let side_effects = structured
        .get("side_effects")
        .and_then(Value::as_object)
        .ok_or(BrowserExecutionError::InvalidResult(
            "browser prepare result omitted side-effect proof",
        ))?;
    if side_effects.len() > KNOWN.len()
        || side_effects
            .keys()
            .any(|key| !KNOWN.contains(&key.as_str()))
    {
        return Err(BrowserExecutionError::InvalidResult(
            "browser prepare result contained unknown side-effect proof",
        ));
    }
    side_effects.values().try_fold(0_u32, |count, value| {
        let enabled = value.as_bool().ok_or(BrowserExecutionError::InvalidResult(
            "browser prepare side-effect proof was not boolean",
        ))?;
        Ok(count + u32::from(enabled))
    })
}

fn normalize_screenshot(
    raw: &CallToolResult,
    structured: &Value,
) -> Result<BrowserBackendScreenshot, BrowserExecutionError> {
    let screenshot = structured
        .get("screenshot")
        .and_then(Value::as_object)
        .ok_or(BrowserExecutionError::InvalidResult(
            "browser screenshot metadata missing",
        ))?;
    if screenshot.get("mime_type").and_then(Value::as_str) != Some("image/png")
        || screenshot.get("coordinate_space").and_then(Value::as_str) != Some("viewport_css_px")
    {
        return Err(BrowserExecutionError::InvalidResult(
            "invalid browser screenshot metadata",
        ));
    }
    let width_pixels = positive_u32(screenshot.get("width"), "invalid browser screenshot width")?;
    let height_pixels = positive_u32(
        screenshot.get("height"),
        "invalid browser screenshot height",
    )?;
    let viewport_css_width = positive_u32(
        screenshot.get("viewport_css_width"),
        "invalid browser viewport width",
    )?;
    let viewport_css_height = positive_u32(
        screenshot.get("viewport_css_height"),
        "invalid browser viewport height",
    )?;
    let scale_x = positive_scale_millionths(
        screenshot.get("pixel_to_css_scale_x"),
        "invalid browser screenshot x scale",
    )?;
    let scale_y = positive_scale_millionths(
        screenshot.get("pixel_to_css_scale_y"),
        "invalid browser screenshot y scale",
    )?;
    let data_base64 = image_data_base64(raw)?;
    let decoded = STANDARD
        .decode(&data_base64)
        .map_err(|_| BrowserExecutionError::InvalidResult("invalid browser screenshot base64"))?;
    if decoded.is_empty() || decoded.len() > MAX_BROWSER_SCREENSHOT_BYTES {
        return Err(BrowserExecutionError::InvalidResult(
            "browser screenshot exceeds the bound",
        ));
    }
    if !decoded.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Err(BrowserExecutionError::InvalidResult(
            "browser screenshot is not a PNG",
        ));
    }
    Ok(BrowserBackendScreenshot {
        data_base64,
        mime_type: "image/png".into(),
        width_pixels,
        height_pixels,
        viewport_css_width,
        viewport_css_height,
        pixel_to_css_scale_x_millionths: scale_x,
        pixel_to_css_scale_y_millionths: scale_y,
    })
}

fn refusal_reason(raw: &CallToolResult) -> Option<BrowserRefusalReason> {
    let structured = raw.structured_content.as_ref()?;
    if structured.get("status").and_then(Value::as_str) == Some("refused") {
        let code = structured
            .get("refusal")?
            .get("code")?
            .as_str()
            .unwrap_or_default();
        return Some(closed_refusal_reason(code).unwrap_or(BrowserRefusalReason::Other));
    }
    if !matches!(
        structured.get("effect").and_then(Value::as_str),
        Some("refused" | "partial")
    ) {
        return None;
    }
    let serialized = serde_json::to_value(raw).ok()?;
    let text = serialized
        .get("content")?
        .as_array()?
        .iter()
        .filter_map(|item| item.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join(
            "
",
        );
    CLOSED_BROWSER_REFUSAL_CODES
        .iter()
        .find(|code| text.contains(**code))
        .and_then(|code| closed_refusal_reason(code))
        .or(Some(BrowserRefusalReason::Other))
}

const CLOSED_BROWSER_REFUSAL_CODES: &[&str] = &[
    "browser_route_unavailable",
    "browser_requires_setup",
    "browser_binding_ambiguous",
    "browser_binding_stale",
    "browser_wrong_target_refused",
    "browser_tab_required",
    "browser_tab_not_found",
    "browser_ref_stale",
    "browser_input_trust_unavailable",
    "browser_endpoint_owner_mismatch",
    "browser_consent_required",
    "browser_consent_revoked",
    "browser_reconnect_exhausted",
    "browser_input_incomplete",
    "browser_action_unavailable",
    "browser_origin_outside_scope",
];

fn closed_refusal_reason(code: &str) -> Option<BrowserRefusalReason> {
    Some(match code {
        "browser_route_unavailable" => BrowserRefusalReason::RouteUnavailable,
        "browser_requires_setup" => BrowserRefusalReason::RequiresSetup,
        "browser_binding_ambiguous" => BrowserRefusalReason::BindingAmbiguous,
        "browser_binding_stale" => BrowserRefusalReason::BindingStale,
        "browser_wrong_target_refused" | "browser_wrong_target" => {
            BrowserRefusalReason::WrongTarget
        }
        "browser_tab_required" => BrowserRefusalReason::TabRequired,
        "browser_tab_not_found" => BrowserRefusalReason::TabNotFound,
        "browser_ref_stale" => BrowserRefusalReason::RefStale,
        "browser_input_trust_unavailable" => BrowserRefusalReason::InputTrustUnavailable,
        "browser_endpoint_owner_mismatch" => BrowserRefusalReason::EndpointOwnerMismatch,
        "browser_consent_required" => BrowserRefusalReason::ConsentRequired,
        "browser_consent_revoked" => BrowserRefusalReason::ConsentRevoked,
        "browser_reconnect_exhausted" => BrowserRefusalReason::ReconnectExhausted,
        "browser_input_incomplete" => BrowserRefusalReason::InputIncomplete,
        "browser_action_unavailable" => BrowserRefusalReason::ActionUnavailable,
        "browser_origin_outside_scope" => BrowserRefusalReason::OriginOutsideScope,
        _ => return None,
    })
}

fn structured_value(raw: &CallToolResult) -> Result<Value, BrowserExecutionError> {
    raw.structured_content
        .clone()
        .ok_or(BrowserExecutionError::InvalidResult(
            "browser backend result omitted structured content",
        ))
}

fn require_status_ok(value: &Value) -> Result<(), BrowserExecutionError> {
    if value.get("status").and_then(Value::as_str) == Some("ok") {
        Ok(())
    } else {
        Err(BrowserExecutionError::InvalidResult(
            "browser backend result was not successful",
        ))
    }
}

fn image_data_base64(raw: &CallToolResult) -> Result<String, BrowserExecutionError> {
    let value = serde_json::to_value(raw).map_err(|_| {
        BrowserExecutionError::InvalidResult("browser result could not be serialized")
    })?;
    let content = value.get("content").and_then(Value::as_array).ok_or(
        BrowserExecutionError::InvalidResult("browser result omitted content"),
    )?;
    for item in content {
        if item.get("type").and_then(Value::as_str) == Some("image")
            && item.get("mimeType").and_then(Value::as_str) == Some("image/png")
        {
            return item
                .get("data")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .ok_or(BrowserExecutionError::InvalidResult(
                    "browser image omitted data",
                ));
        }
    }
    Err(BrowserExecutionError::InvalidResult(
        "browser result omitted PNG content",
    ))
}

fn positive_u32(
    value: Option<&Value>,
    message: &'static str,
) -> Result<u32, BrowserExecutionError> {
    let value = value.ok_or(BrowserExecutionError::InvalidResult(message))?;
    let raw = if let Some(raw) = value.as_u64() {
        raw
    } else {
        let raw = value
            .as_f64()
            .filter(|raw| raw.is_finite() && *raw > 0.0 && raw.fract() == 0.0)
            .ok_or(BrowserExecutionError::InvalidResult(message))?;
        if raw > u32::MAX as f64 {
            return Err(BrowserExecutionError::InvalidResult(message));
        }
        raw as u64
    };
    let value = u32::try_from(raw).map_err(|_| BrowserExecutionError::InvalidResult(message))?;
    if value == 0 {
        return Err(BrowserExecutionError::InvalidResult(message));
    }
    Ok(value)
}

fn positive_scale_millionths(
    value: Option<&Value>,
    message: &'static str,
) -> Result<u32, BrowserExecutionError> {
    let scale = value
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite() && *value > 0.0 && *value <= 100.0)
        .ok_or(BrowserExecutionError::InvalidResult(message))?;
    let millionths = (scale * 1_000_000.0).round();
    if !(1.0..=u32::MAX as f64).contains(&millionths) {
        return Err(BrowserExecutionError::InvalidResult(message));
    }
    Ok(millionths as u32)
}

fn target_tab_args(
    args: &mut Map<String, Value>,
    context_id: &str,
    backend_target_id: &str,
    backend_tab_id: &str,
) {
    args.insert("target_id".into(), json!(backend_target_id));
    args.insert("tab_id".into(), json!(backend_tab_id));
    args.insert("session".into(), json!(context_id));
}

fn input_route_name(route: BrowserInputRoute) -> &'static str {
    match route {
        BrowserInputRoute::Trusted => "trusted",
        BrowserInputRoute::DomEvent => "dom_event",
    }
}

fn object(value: Value) -> Option<JsonObject> {
    value.as_object().cloned()
}

fn validate_target_tab(target: &str, tab: &str) -> Result<(), BrowserExecutionError> {
    validate_handle(target)?;
    validate_handle(tab)
}

fn validate_handle(value: &str) -> Result<(), BrowserExecutionError> {
    if value.is_empty() || value.len() > 4 * 1024 || value.chars().any(char::is_control) {
        return Err(BrowserExecutionError::InvalidRequest(
            "invalid resolved browser backend handle",
        ));
    }
    Ok(())
}

fn validate_bounded_text(
    value: &str,
    limit: usize,
    message: &'static str,
) -> Result<(), BrowserExecutionError> {
    if value.len() > limit || value.contains('\0') {
        return Err(BrowserExecutionError::InvalidRequest(message));
    }
    Ok(())
}

fn validate_url(value: &str) -> Result<(), BrowserExecutionError> {
    if value.is_empty()
        || value.len() > MAX_BROWSER_URL_BYTES
        || value.trim() != value
        || value.chars().any(char::is_control)
    {
        return Err(BrowserExecutionError::InvalidRequest(
            "invalid browser navigation URL",
        ));
    }
    let lower = value.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("about:")
    {
        Ok(())
    } else {
        Err(BrowserExecutionError::InvalidRequest(
            "browser navigation scheme is not allowed",
        ))
    }
}

fn valid_context_id(value: &str) -> bool {
    value.len() == 36
        && value.starts_with("ctx_")
        && value[4..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v2_browser_runtime::BrowserBackendClickTarget;

    const CONTEXT: &str = "ctx_0123456789abcdef0123456789abcdef";

    #[test]
    fn transfer_commands_fail_before_any_cua_call_mapping() {
        let command = BrowserBackendCommand::Download {
            context_id: CONTEXT.into(),
            backend_target_id: "target".into(),
            backend_tab_id: "tab".into(),
            backend_element_ref: "p1:1".into(),
            destination_name: "download.bin".into(),
            max_bytes: 1024,
            overwrite: false,
        };
        assert!(matches!(
            map_command(&command),
            Err(BrowserExecutionError::UnsupportedTransferBoundary)
        ));
    }

    #[test]
    fn dom_event_coordinate_click_is_rejected_before_backend_dispatch() {
        let command = BrowserBackendCommand::Click {
            context_id: CONTEXT.into(),
            backend_target_id: "target".into(),
            backend_tab_id: "tab".into(),
            target: BrowserBackendClickTarget::ViewportCss { x: 1, y: 2 },
            input_route: BrowserInputRoute::DomEvent,
        };
        assert!(matches!(
            validate_command(&command),
            Err(BrowserExecutionError::InvalidRequest(_))
        ));
    }

    #[test]
    fn existing_profile_prepare_contains_no_synthesized_approval_artifact() {
        let command = BrowserBackendCommand::Prepare {
            context_id: CONTEXT.into(),
            process_id: 42,
            window_id: Some(7),
            allow_launch: false,
            profile_mode: BrowserPrepareProfileMode::ExistingProfile,
            profile_name: None,
        };
        let (tool, args) = map_command(&command).unwrap();
        assert_eq!(tool, "browser_prepare");
        let args = args.unwrap();
        assert_eq!(args["strategy"], json!({"kind": "existing_profile"}));
        assert!(!args.contains_key("approval_token"));
        assert!(!args.contains_key("_cua_browser_download_mcp_host_approved"));
    }

    #[test]
    fn exact_cua_refusal_is_recognized_without_mcp_error_flag() {
        let raw: CallToolResult = serde_json::from_value(json!({
            "content": [{"type": "text", "text": "refused (browser_consent_required): approval required"}],
            "structuredContent": {
                "status": "refused",
                "refusal": {
                    "code": "browser_consent_required",
                    "message": "approval required",
                    "detail": {"provider_private": "must not escape"}
                }
            }
        }))
        .unwrap();
        assert_eq!(raw.is_error, None);
        assert_eq!(
            refusal_reason(&raw),
            Some(BrowserRefusalReason::ConsentRequired)
        );
    }

    #[test]
    fn safe_refusal_mapping_uses_only_code_not_provider_message() {
        let raw: CallToolResult = serde_json::from_value(json!({
            "content": [],
            "isError": true,
            "structuredContent": {
                "status": "refused",
                "refusal": {
                    "code": "browser_ref_stale",
                    "message": "provider-secret-message"
                }
            }
        }))
        .unwrap();
        assert_eq!(refusal_reason(&raw), Some(BrowserRefusalReason::RefStale));
        assert!(!format!("{:?}", refusal_reason(&raw)).contains("provider-secret-message"));
    }

    #[test]
    fn prepare_normalizes_exact_0193_side_effect_object_without_exposing_details() {
        let command = BrowserBackendCommand::Prepare {
            context_id: CONTEXT.into(),
            process_id: 42,
            window_id: None,
            allow_launch: true,
            profile_mode: BrowserPrepareProfileMode::IsolatedNew,
            profile_name: None,
        };
        let mut raw = CallToolResult::success(vec![]);
        raw.structured_content = Some(json!({
            "status": "ok",
            "prepared": true,
            "prepared_pid": 43,
            "side_effects": {
                "launched_browser": true,
                "restarted_browser": false,
                "created_profile": true,
                "reused_driver_profile": false,
                "copied_profile_data": false,
                "changed_preferences": false,
                "displayed_consent_prompt": false,
                "opened_setup_page": false,
                "closed_setup_page": false,
                "enabled_remote_debugging": false,
                "used_bounded_pixel_fallback": false,
                "focused_setup_address_field": false,
                "foregrounded_window": false,
                "injected_global_input": false
            }
        }));
        assert_eq!(
            normalize_completed(&command, &raw).unwrap(),
            BrowserBackendResult::Prepared {
                prepared: true,
                prepared_process_id: Some(43),
                side_effect_count: 2,
            }
        );
    }

    #[test]
    fn cua_named_profile_limit_fails_before_backend_dispatch() {
        let command = BrowserBackendCommand::Prepare {
            context_id: CONTEXT.into(),
            process_id: 42,
            window_id: None,
            allow_launch: true,
            profile_mode: BrowserPrepareProfileMode::IsolatedNamed,
            profile_name: Some("a".repeat(MAX_CUA_ISOLATED_PROFILE_NAME_BYTES + 1)),
        };
        assert!(matches!(
            validate_command(&command),
            Err(BrowserExecutionError::InvalidRequest(_))
        ));
    }

    #[test]
    fn dialog_inspect_maps_without_resolution_authority() {
        let command = BrowserBackendCommand::Dialog {
            context_id: CONTEXT.into(),
            backend_target_id: "target".into(),
            backend_tab_id: "tab".into(),
            backend_dialog_id: None,
            action: BrowserDialogAction::Inspect,
            prompt_text: None,
            delivery: BrowserDialogDelivery::Background,
        };
        validate_command(&command).unwrap();
        let (tool, args) = map_command(&command).unwrap();
        assert_eq!(tool, "browser_dialog");
        let args = args.unwrap();
        assert_eq!(args["action"], json!("inspect"));
        assert_eq!(args["target_id"], json!("target"));
        assert_eq!(args["tab_id"], json!("tab"));
        assert_eq!(args["session"], json!(CONTEXT));
        assert!(!args.contains_key("dialog_id"));
        assert!(!args.contains_key("delivery_mode"));
        assert!(!args.contains_key("prompt_text"));
    }

    #[test]
    fn dialog_inspect_normalizes_exact_0193_shape() {
        let command = BrowserBackendCommand::Dialog {
            context_id: CONTEXT.into(),
            backend_target_id: "target".into(),
            backend_tab_id: "tab".into(),
            backend_dialog_id: None,
            action: BrowserDialogAction::Inspect,
            prompt_text: None,
            delivery: BrowserDialogDelivery::Background,
        };
        let mut raw = CallToolResult::success(vec![]);
        raw.structured_content = Some(json!({
            "status": "ok",
            "target_id": "target",
            "tab_id": "tab",
            "present": true,
            "dialog_id": "dialog-7:3",
            "kind": "prompt"
        }));
        assert_eq!(
            normalize_completed(&command, &raw).unwrap(),
            BrowserBackendResult::DialogObserved {
                present: true,
                backend_dialog_id: Some("dialog-7:3".into()),
                kind: Some("prompt".into()),
            }
        );
    }

    #[test]
    fn dialog_inspect_rejects_inconsistent_backend_state() {
        let command = BrowserBackendCommand::Dialog {
            context_id: CONTEXT.into(),
            backend_target_id: "target".into(),
            backend_tab_id: "tab".into(),
            backend_dialog_id: None,
            action: BrowserDialogAction::Inspect,
            prompt_text: None,
            delivery: BrowserDialogDelivery::Background,
        };
        for structured in [
            json!({"status":"ok","target_id":"target","tab_id":"tab","present":false,"dialog_id":"dialog-1","kind":"alert"}),
            json!({"status":"ok","target_id":"target","tab_id":"tab","present":true,"dialog_id":"dialog-1"}),
            json!({"status":"ok","target_id":"target","tab_id":"tab","present":true,"kind":"alert"}),
            json!({"status":"ok","target_id":"target","tab_id":"tab","present":true,"dialog_id":"dialog-1","kind":"custom"}),
        ] {
            let mut raw = CallToolResult::success(vec![]);
            raw.structured_content = Some(structured);
            assert!(matches!(
                normalize_completed(&command, &raw),
                Err(BrowserExecutionError::InvalidResult(_))
            ));
        }
    }

    #[test]
    fn current_cua_closed_action_result_is_accepted_only_for_exact_route() {
        let valid = json!({
            "effect": "unverifiable",
            "route": "trusted_input",
            "delivery": {"mode": "background", "delivered_count": 10}
        });
        assert_eq!(
            normalize_browser_action_result(&valid, "trusted_input").unwrap(),
            BrowserClosedActionOutcome::Unverifiable
        );
        assert!(matches!(
            normalize_browser_action_result(&valid, "dom"),
            Err(BrowserExecutionError::InvalidResult(_))
        ));
    }

    #[test]
    fn closed_browser_action_result_rejects_non_background_delivery() {
        for mode in ["foreground", "unknown", "not_applicable"] {
            let value = json!({
                "effect": "unverifiable",
                "route": "trusted_input",
                "delivery": {"mode": mode}
            });
            assert!(matches!(
                normalize_browser_action_result(&value, "trusted_input"),
                Err(BrowserExecutionError::InvalidResult(_))
            ));
        }
    }

    #[test]
    fn closed_dom_action_accepts_only_page_verification_recommendation() {
        let value = json!({
            "effect": "unverifiable",
            "route": "dom",
            "delivery": {"mode": "background"},
            "escalation": {"target": "page", "reason": "effect_unconfirmed"}
        });
        assert_eq!(
            normalize_browser_action_result(&value, "dom").unwrap(),
            BrowserClosedActionOutcome::Unverifiable
        );
        for escalation in [
            json!({"target":"foreground","reason":"delivery_failed"}),
            json!({"target":"pixel","reason":"effect_unconfirmed"}),
            json!({"target":"session","reason":"route_unavailable"}),
            json!({"target":"page","reason":"suspected_noop"}),
        ] {
            let value = json!({
                "effect":"unverifiable",
                "route":"dom",
                "delivery":{"mode":"background"},
                "escalation": escalation
            });
            assert!(matches!(
                normalize_browser_action_result(&value, "dom"),
                Err(BrowserExecutionError::BackendRefused(_))
            ));
        }
    }

    #[test]
    fn closed_action_result_rejects_escalation_partial_noop_and_unknown_fields() {
        for value in [
            json!({
                "effect":"unverifiable","route":"trusted_input",
                "escalation":{"target":"foreground","reason":"delivery_failed"}
            }),
            json!({
                "effect":"partial","route":"trusted_input",
                "delivery":{"mode":"background","delivered_count":1}
            }),
            json!({"effect":"suspected_noop","route":"trusted_input"}),
        ] {
            assert!(matches!(
                normalize_browser_action_result(&value, "trusted_input"),
                Err(BrowserExecutionError::BackendRefused(_))
            ));
        }
        assert!(matches!(
            normalize_browser_action_result(
                &json!({"effect":"unverifiable","route":"trusted_input","provider":"secret"}),
                "trusted_input"
            ),
            Err(BrowserExecutionError::InvalidResult(_))
        ));
    }

    #[test]
    fn projected_action_refusal_recovers_only_closed_code_from_text() {
        let raw: CallToolResult = serde_json::from_value(json!({
            "content": [{
                "type":"text",
                "text":"refused (browser_input_trust_unavailable): provider-private explanation"
            }],
            "structuredContent": {
                "effect":"refused",
                "route":"trusted_input",
                "escalation":{"target":"page","reason":"route_unavailable"}
            }
        }))
        .unwrap();
        assert_eq!(
            refusal_reason(&raw),
            Some(BrowserRefusalReason::InputTrustUnavailable)
        );
    }

    #[test]
    fn mutation_success_requires_exact_target_and_tab_echo() {
        let command = BrowserBackendCommand::Navigate {
            context_id: CONTEXT.into(),
            backend_target_id: "target".into(),
            backend_tab_id: "tab".into(),
            url: "https://example.com".into(),
        };
        for structured in [
            json!({
                "status":"ok","target_id":"wrong","tab_id":"tab",
                "url":"https://example.com","refs_invalidated":true
            }),
            json!({
                "status":"ok","target_id":"target","tab_id":"wrong",
                "url":"https://example.com","refs_invalidated":true
            }),
            json!({
                "status":"ok","target_id":"target","tab_id":"tab",
                "url":"https://other.example","refs_invalidated":true
            }),
            json!({
                "status":"ok","target_id":"target","tab_id":"tab",
                "url":"https://example.com","refs_invalidated":false
            }),
        ] {
            let mut raw = CallToolResult::success(vec![]);
            raw.structured_content = Some(structured);
            assert!(matches!(
                normalize_completed(&command, &raw),
                Err(BrowserExecutionError::InvalidResult(_))
            ));
        }
    }

    #[test]
    fn dialog_resolution_requires_exact_dialog_and_action_echo() {
        let command = BrowserBackendCommand::Dialog {
            context_id: CONTEXT.into(),
            backend_target_id: "target".into(),
            backend_tab_id: "tab".into(),
            backend_dialog_id: Some("dialog-7".into()),
            action: BrowserDialogAction::Accept,
            prompt_text: None,
            delivery: BrowserDialogDelivery::Background,
        };
        for structured in [
            json!({
                "status":"ok","target_id":"target","tab_id":"tab",
                "dialog_id":"dialog-8","kind":"alert","action":"accept"
            }),
            json!({
                "status":"ok","target_id":"target","tab_id":"tab",
                "dialog_id":"dialog-7","kind":"alert","action":"dismiss"
            }),
        ] {
            let mut raw = CallToolResult::success(vec![]);
            raw.structured_content = Some(structured);
            assert!(matches!(
                normalize_completed(&command, &raw),
                Err(BrowserExecutionError::InvalidResult(_))
            ));
        }
    }

    #[test]
    fn dialog_resolution_preserves_explicit_delivery_without_fallback() {
        let command = BrowserBackendCommand::Dialog {
            context_id: CONTEXT.into(),
            backend_target_id: "target".into(),
            backend_tab_id: "tab".into(),
            backend_dialog_id: Some("dialog-7:3".into()),
            action: BrowserDialogAction::Accept,
            prompt_text: Some("answer".into()),
            delivery: BrowserDialogDelivery::Foreground,
        };
        let (_, args) = map_command(&command).unwrap();
        let args = args.unwrap();
        assert_eq!(args["action"], json!("accept"));
        assert_eq!(args["dialog_id"], json!("dialog-7:3"));
        assert_eq!(args["delivery_mode"], json!("foreground"));
        assert_eq!(args["prompt_text"], json!("answer"));
    }

    #[test]
    fn resolved_runtime_limits_do_not_exceed_public_browser_contract() {
        let oversized_query = "q".repeat(MAX_BROWSER_QUERY_BYTES + 1);
        let command = BrowserBackendCommand::Inspect {
            context_id: CONTEXT.into(),
            backend_target_id: "target".into(),
            backend_tab_id: "tab".into(),
            backend_scope_ref: None,
            query: Some(oversized_query),
            backend_continuation: None,
            include_screenshot: false,
        };
        assert!(matches!(
            validate_command(&command),
            Err(BrowserExecutionError::InvalidRequest(_))
        ));

        let command = BrowserBackendCommand::Pointer {
            context_id: CONTEXT.into(),
            backend_target_id: "target".into(),
            backend_tab_id: "tab".into(),
            backend_element_ref: "p1:1".into(),
            action: BrowserPointerAction::Scroll,
            backend_destination_ref: None,
            delta_x: MAX_BROWSER_SCROLL_DELTA_CSS_PX + 1,
            delta_y: 0,
            input_route: BrowserInputRoute::Trusted,
        };
        assert!(matches!(
            validate_command(&command),
            Err(BrowserExecutionError::InvalidRequest(_))
        ));
    }

    #[test]
    fn valid_browser_commands_map_only_to_typed_cua_tools() {
        let navigate = BrowserBackendCommand::Navigate {
            context_id: CONTEXT.into(),
            backend_target_id: "target".into(),
            backend_tab_id: "tab".into(),
            url: "https://example.com".into(),
        };
        let (tool, args) = map_command(&navigate).unwrap();
        assert_eq!(tool, "browser_navigate");
        let args = args.unwrap();
        assert_eq!(args.get("url"), Some(&json!("https://example.com")));

        let click = BrowserBackendCommand::Click {
            context_id: CONTEXT.into(),
            backend_target_id: "target".into(),
            backend_tab_id: "tab".into(),
            target: BrowserBackendClickTarget::Element {
                backend_element_ref: "p1:1".into(),
            },
            input_route: BrowserInputRoute::DomEvent,
        };
        assert_eq!(map_command(&click).unwrap().0, "browser_click");
    }
}
