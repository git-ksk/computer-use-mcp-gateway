//! Backend-neutral V2 browser semantic contract.
//!
//! This module deliberately contains no Cua/CDP tool names or authorization
//! artifacts. Runtime adapters translate these bounded CUMG values to a live
//! backend after normal principal/device capability authorization and
//! InteractionContext generation/revision fencing.

use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fmt;

pub const MAX_BROWSER_URL_BYTES: usize = 16 * 1024;
pub const MAX_BROWSER_QUERY_BYTES: usize = 4 * 1024;
pub const MAX_BROWSER_CONTINUATION_BYTES: usize = 4 * 1024;
pub const MAX_BROWSER_REF_BYTES: usize = 512;
pub const MAX_BROWSER_TEXT_BYTES: usize = 64 * 1024;
pub const MAX_BROWSER_PROMPT_TEXT_BYTES: usize = 64 * 1024;
pub const MAX_BROWSER_PROFILE_NAME_BYTES: usize = 128;
pub const MAX_BROWSER_UPLOAD_FILES: usize = 32;
pub const MAX_BROWSER_UPLOAD_FILE_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_BROWSER_UPLOAD_BASE64_BYTES: usize = MAX_BROWSER_UPLOAD_FILE_BYTES.div_ceil(3) * 4;
pub const MAX_BROWSER_UPLOAD_NAME_BYTES: usize = 255;
pub const MAX_BROWSER_DOWNLOAD_FILES: usize = 32;
pub const MAX_BROWSER_DOWNLOAD_BYTES: u64 = 16 * 1024 * 1024;
pub const MAX_BROWSER_DOWNLOAD_NAME_BYTES: usize = 255;
pub const MAX_BROWSER_DOWNLOAD_BASE64_BYTES: usize =
    (MAX_BROWSER_DOWNLOAD_BYTES as usize).div_ceil(3) * 4;
pub const MAX_BROWSER_SCROLL_DELTA_CSS_PX: i32 = 100_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserPrepareProfileMode {
    IsolatedNew,
    IsolatedNamed,
    ExistingProfile,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserPrepareRequest {
    pub context_id: String,
    pub process_id: u32,
    pub window_id: Option<u64>,
    pub allow_launch: bool,
    pub profile_mode: BrowserPrepareProfileMode,
    pub profile_name: Option<String>,
}

impl BrowserPrepareRequest {
    pub fn validate(&self) -> Result<(), BrowserContractError> {
        validate_context_id(&self.context_id)?;
        if self.process_id == 0 {
            return Err(BrowserContractError::InvalidProcessId);
        }
        match self.profile_mode {
            BrowserPrepareProfileMode::IsolatedNew => {
                if self.profile_name.is_some() {
                    return Err(BrowserContractError::InvalidProfile);
                }
            }
            BrowserPrepareProfileMode::IsolatedNamed => {
                let name = self
                    .profile_name
                    .as_deref()
                    .ok_or(BrowserContractError::InvalidProfile)?;
                validate_profile_name(name)?;
            }
            BrowserPrepareProfileMode::ExistingProfile => {
                // Existing-profile attachment is exact-window only. Backend or
                // trusted-host authorization remains southbound/operator policy;
                // no approval token is accepted through this CUMG request.
                if self.window_id.is_none() || self.allow_launch || self.profile_name.is_some() {
                    return Err(BrowserContractError::InvalidProfile);
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserBindRequest {
    pub context_id: String,
    pub process_id: u32,
    pub window_id: u64,
}

impl BrowserBindRequest {
    pub fn validate(&self) -> Result<(), BrowserContractError> {
        validate_context_id(&self.context_id)?;
        if self.process_id == 0 || self.window_id == 0 {
            return Err(BrowserContractError::InvalidWindowBinding);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserInspectRequest {
    pub context_id: String,
    pub target_ref: String,
    pub tab_ref: String,
    pub scope_ref: Option<String>,
    pub query: Option<String>,
    pub continuation_ref: Option<String>,
    pub include_screenshot: bool,
}

impl BrowserInspectRequest {
    pub fn validate(&self) -> Result<(), BrowserContractError> {
        validate_context_id(&self.context_id)?;
        validate_public_ref(&self.target_ref)?;
        validate_public_ref(&self.tab_ref)?;
        if let Some(scope_ref) = &self.scope_ref {
            validate_public_ref(scope_ref)?;
        }
        if let Some(query) = &self.query {
            validate_bounded_nonempty(query, MAX_BROWSER_QUERY_BYTES)?;
        }
        if let Some(continuation) = &self.continuation_ref {
            validate_bounded_nonempty(continuation, MAX_BROWSER_CONTINUATION_BYTES)?;
            if self.scope_ref.is_some() || self.query.is_some() {
                return Err(BrowserContractError::InvalidContinuation);
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserNavigateRequest {
    pub context_id: String,
    pub target_ref: String,
    pub tab_ref: String,
    pub url: String,
}

impl BrowserNavigateRequest {
    pub fn validate(&self) -> Result<(), BrowserContractError> {
        validate_context_id(&self.context_id)?;
        validate_public_ref(&self.target_ref)?;
        validate_public_ref(&self.tab_ref)?;
        validate_browser_url(&self.url)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserInputRoute {
    Trusted,
    DomEvent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BrowserClickTarget {
    Element { element_ref: String },
    ViewportCss { x: i32, y: i32 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserClickRequest {
    pub context_id: String,
    pub target_ref: String,
    pub tab_ref: String,
    pub target: BrowserClickTarget,
    pub input_route: BrowserInputRoute,
}

impl BrowserClickRequest {
    pub fn validate(&self) -> Result<(), BrowserContractError> {
        validate_context_id(&self.context_id)?;
        validate_public_ref(&self.target_ref)?;
        validate_public_ref(&self.tab_ref)?;
        match &self.target {
            BrowserClickTarget::Element { element_ref } => validate_public_ref(element_ref)?,
            BrowserClickTarget::ViewportCss { .. } => {
                if self.input_route == BrowserInputRoute::DomEvent {
                    return Err(BrowserContractError::SyntheticRouteRequiresRef);
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserTypeMode {
    InsertText,
    Keystrokes,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserTypeRequest {
    pub context_id: String,
    pub target_ref: String,
    pub tab_ref: String,
    pub element_ref: String,
    pub text: String,
    pub mode: BrowserTypeMode,
    pub replace: bool,
}

impl BrowserTypeRequest {
    pub fn validate(&self) -> Result<(), BrowserContractError> {
        validate_context_id(&self.context_id)?;
        validate_public_ref(&self.target_ref)?;
        validate_public_ref(&self.tab_ref)?;
        validate_public_ref(&self.element_ref)?;
        if self.text.len() > MAX_BROWSER_TEXT_BYTES {
            return Err(BrowserContractError::ValueTooLarge);
        }
        if self.text.is_empty() && !self.replace {
            return Err(BrowserContractError::EmptyValue);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserDialogAction {
    Inspect,
    Accept,
    Dismiss,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserDialogDelivery {
    Background,
    Foreground,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserDialogRequest {
    pub context_id: String,
    pub target_ref: String,
    pub tab_ref: String,
    pub dialog_ref: Option<String>,
    pub action: BrowserDialogAction,
    pub prompt_text: Option<String>,
    pub delivery: BrowserDialogDelivery,
}

impl BrowserDialogRequest {
    pub fn validate(&self) -> Result<(), BrowserContractError> {
        validate_context_id(&self.context_id)?;
        validate_public_ref(&self.target_ref)?;
        validate_public_ref(&self.tab_ref)?;
        match self.action {
            BrowserDialogAction::Inspect => {
                if self.dialog_ref.is_some()
                    || self.prompt_text.is_some()
                    || self.delivery != BrowserDialogDelivery::Background
                {
                    return Err(BrowserContractError::InvalidDialogAction);
                }
            }
            BrowserDialogAction::Accept | BrowserDialogAction::Dismiss => {
                let dialog_ref = self
                    .dialog_ref
                    .as_deref()
                    .ok_or(BrowserContractError::InvalidDialogAction)?;
                validate_public_ref(dialog_ref)?;
                if let Some(prompt) = &self.prompt_text {
                    if self.action != BrowserDialogAction::Accept {
                        return Err(BrowserContractError::PromptRequiresAccept);
                    }
                    if prompt.len() > MAX_BROWSER_PROMPT_TEXT_BYTES {
                        return Err(BrowserContractError::ValueTooLarge);
                    }
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserPointerAction {
    Hover,
    RightClick,
    DoubleClick,
    Scroll,
    Drag,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserPointerRequest {
    pub context_id: String,
    pub target_ref: String,
    pub tab_ref: String,
    pub element_ref: String,
    pub action: BrowserPointerAction,
    pub destination_ref: Option<String>,
    pub delta_x: i32,
    pub delta_y: i32,
    pub input_route: BrowserInputRoute,
}

impl BrowserPointerRequest {
    pub fn validate(&self) -> Result<(), BrowserContractError> {
        validate_context_id(&self.context_id)?;
        validate_public_ref(&self.target_ref)?;
        validate_public_ref(&self.tab_ref)?;
        validate_public_ref(&self.element_ref)?;
        if self.delta_x.unsigned_abs() > MAX_BROWSER_SCROLL_DELTA_CSS_PX as u32
            || self.delta_y.unsigned_abs() > MAX_BROWSER_SCROLL_DELTA_CSS_PX as u32
        {
            return Err(BrowserContractError::InvalidPointerDelta);
        }
        match self.action {
            BrowserPointerAction::Drag => {
                let destination = self
                    .destination_ref
                    .as_deref()
                    .ok_or(BrowserContractError::DestinationRequired)?;
                validate_public_ref(destination)?;
            }
            BrowserPointerAction::Scroll => {
                if self.destination_ref.is_some() || (self.delta_x == 0 && self.delta_y == 0) {
                    return Err(BrowserContractError::InvalidPointerAction);
                }
            }
            BrowserPointerAction::Hover
            | BrowserPointerAction::RightClick
            | BrowserPointerAction::DoubleClick => {
                if self.destination_ref.is_some() || self.delta_x != 0 || self.delta_y != 0 {
                    return Err(BrowserContractError::InvalidPointerAction);
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserStageUploadRequest {
    pub context_id: String,
    /// Caller-visible basename only. No local source path is accepted.
    pub file_name: String,
    /// File bytes transported through the dedicated bounded upload staging boundary.
    pub data_base64: String,
}

impl BrowserStageUploadRequest {
    pub fn validate(&self) -> Result<usize, BrowserContractError> {
        validate_context_id(&self.context_id)?;
        validate_transfer_basename(
            &self.file_name,
            MAX_BROWSER_UPLOAD_NAME_BYTES,
            BrowserContractError::InvalidUploadName,
        )?;
        if self.data_base64.len() > MAX_BROWSER_UPLOAD_BASE64_BYTES {
            return Err(BrowserContractError::UploadFileTooLarge);
        }
        let decoded = STANDARD
            .decode(self.data_base64.as_bytes())
            .map_err(|_| BrowserContractError::InvalidUploadData)?;
        if decoded.len() > MAX_BROWSER_UPLOAD_FILE_BYTES {
            return Err(BrowserContractError::UploadFileTooLarge);
        }
        Ok(decoded.len())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserUploadRequest {
    pub context_id: String,
    pub target_ref: String,
    pub tab_ref: String,
    pub element_ref: String,
    /// CUMG-issued file refs only. Local paths are intentionally absent.
    pub file_refs: Vec<String>,
}

impl BrowserUploadRequest {
    pub fn validate(&self) -> Result<(), BrowserContractError> {
        validate_context_id(&self.context_id)?;
        validate_public_ref(&self.target_ref)?;
        validate_public_ref(&self.tab_ref)?;
        validate_public_ref(&self.element_ref)?;
        if self.file_refs.is_empty() || self.file_refs.len() > MAX_BROWSER_UPLOAD_FILES {
            return Err(BrowserContractError::InvalidUploadSet);
        }
        let mut unique = HashSet::with_capacity(self.file_refs.len());
        for file_ref in &self.file_refs {
            validate_public_ref(file_ref)?;
            if !unique.insert(file_ref) {
                return Err(BrowserContractError::DuplicateFileRef);
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserDownloadRequest {
    pub context_id: String,
    pub target_ref: String,
    pub tab_ref: String,
    pub element_ref: String,
    /// Caller-chosen logical basename inside Agent-private download staging.
    /// No caller-selected host directory or raw local path is accepted.
    pub destination_name: String,
    pub max_bytes: u64,
    pub overwrite: bool,
}

impl BrowserDownloadRequest {
    pub fn validate(&self) -> Result<(), BrowserContractError> {
        validate_context_id(&self.context_id)?;
        validate_public_ref(&self.target_ref)?;
        validate_public_ref(&self.tab_ref)?;
        validate_public_ref(&self.element_ref)?;
        validate_download_destination_name(&self.destination_name)?;
        if self.max_bytes == 0 || self.max_bytes > MAX_BROWSER_DOWNLOAD_BYTES {
            return Err(BrowserContractError::InvalidDownloadBound);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserAction {
    Click,
    Type,
    Pointer,
    Scroll,
    Upload,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserTabSummary {
    pub tab_ref: String,
    pub title: Option<String>,
    pub url: Option<String>,
    pub active: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserBindingResult {
    pub target_ref: String,
    pub process_id: u32,
    pub window_id: u64,
    pub exact: bool,
    pub mutation_allowed: bool,
    pub tabs: Vec<BrowserTabSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserSemanticRef {
    pub element_ref: String,
    pub role: String,
    pub name: Option<String>,
    pub value: Option<String>,
    pub states: Vec<String>,
    pub actions: Vec<BrowserAction>,
    pub frame: String,
    pub visibility: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserSnapshotResult {
    pub snapshot_ref: String,
    pub outline: String,
    pub action_refs: Vec<BrowserSemanticRef>,
    pub content_refs: Vec<BrowserSemanticRef>,
    pub complete: bool,
    pub omitted: u32,
    pub continuation_ref: Option<String>,
    pub screenshot_base64: Option<String>,
    pub screenshot_width: Option<u32>,
    pub screenshot_height: Option<u32>,
    pub viewport_css_width: Option<u32>,
    pub viewport_css_height: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserDialogResult {
    pub present: bool,
    pub dialog_ref: Option<String>,
    pub kind: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserStagedUploadResult {
    pub file_ref: String,
    pub bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserDownloadResult {
    pub download_ref: String,
    pub destination_name: String,
    pub bytes_written: u64,
    /// Bounded bytes returned from Agent-private staging; never a host path.
    pub data_base64: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrowserContractError {
    InvalidContextId,
    InvalidProcessId,
    InvalidWindowBinding,
    InvalidProfile,
    InvalidProfileName,
    InvalidRef,
    InvalidUrl,
    InvalidContinuation,
    SyntheticRouteRequiresRef,
    EmptyValue,
    ValueTooLarge,
    PromptRequiresAccept,
    InvalidDialogAction,
    DestinationRequired,
    InvalidPointerAction,
    InvalidPointerDelta,
    InvalidUploadSet,
    DuplicateFileRef,
    InvalidUploadName,
    InvalidUploadData,
    UploadFileTooLarge,
    InvalidDownloadBound,
    InvalidDownloadName,
}

impl fmt::Display for BrowserContractError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for BrowserContractError {}

fn validate_context_id(value: &str) -> Result<(), BrowserContractError> {
    let Some(hex) = value.strip_prefix("ctx_") else {
        return Err(BrowserContractError::InvalidContextId);
    };
    if hex.len() != 32
        || !hex
            .as_bytes()
            .iter()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(BrowserContractError::InvalidContextId);
    }
    Ok(())
}

fn validate_public_ref(value: &str) -> Result<(), BrowserContractError> {
    if value.is_empty() || value.len() > MAX_BROWSER_REF_BYTES || !value.starts_with("ref_") {
        return Err(BrowserContractError::InvalidRef);
    }
    Ok(())
}

fn validate_bounded_nonempty(value: &str, max_bytes: usize) -> Result<(), BrowserContractError> {
    if value.trim().is_empty() {
        return Err(BrowserContractError::EmptyValue);
    }
    if value.len() > max_bytes {
        return Err(BrowserContractError::ValueTooLarge);
    }
    Ok(())
}

fn validate_profile_name(value: &str) -> Result<(), BrowserContractError> {
    if value.is_empty()
        || value.len() > MAX_BROWSER_PROFILE_NAME_BYTES
        || value == "."
        || value == ".."
        || value
            .bytes()
            .any(|byte| !(byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')))
    {
        return Err(BrowserContractError::InvalidProfileName);
    }
    Ok(())
}

pub(crate) fn validate_download_destination_name(value: &str) -> Result<(), BrowserContractError> {
    validate_transfer_basename(
        value,
        MAX_BROWSER_DOWNLOAD_NAME_BYTES,
        BrowserContractError::InvalidDownloadName,
    )
}

pub(crate) fn validate_upload_file_name(value: &str) -> Result<(), BrowserContractError> {
    validate_transfer_basename(
        value,
        MAX_BROWSER_UPLOAD_NAME_BYTES,
        BrowserContractError::InvalidUploadName,
    )
}

fn validate_transfer_basename(
    value: &str,
    max_bytes: usize,
    error: BrowserContractError,
) -> Result<(), BrowserContractError> {
    if value.is_empty()
        || value.len() > max_bytes
        || value.trim() != value
        || value == "."
        || value == ".."
        || value.chars().any(char::is_control)
        || value
            .chars()
            .any(|ch| matches!(ch, '/' | '\\' | '<' | '>' | ':' | '"' | '|' | '?' | '*'))
        || value.ends_with('.')
    {
        return Err(error.clone());
    }

    let reserved_stem = value
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    let reserved = matches!(reserved_stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (reserved_stem.len() == 4
            && (reserved_stem.starts_with("COM") || reserved_stem.starts_with("LPT"))
            && matches!(reserved_stem.as_bytes()[3], b'1'..=b'9'));
    if reserved {
        return Err(error);
    }
    Ok(())
}

fn validate_browser_url(value: &str) -> Result<(), BrowserContractError> {
    if value.is_empty()
        || value.len() > MAX_BROWSER_URL_BYTES
        || value.trim() != value
        || value.chars().any(char::is_control)
    {
        return Err(BrowserContractError::InvalidUrl);
    }
    let lower = value.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("about:")
    {
        Ok(())
    } else {
        Err(BrowserContractError::InvalidUrl)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONTEXT: &str = "ctx_0123456789abcdef0123456789abcdef";

    fn reference(index: u8) -> String {
        format!("ref_{index:032x}")
    }

    #[test]
    fn browser_navigation_allows_only_reviewed_url_schemes() {
        for url in [
            "https://example.com",
            "http://localhost:8080/a",
            "about:blank",
        ] {
            BrowserNavigateRequest {
                context_id: CONTEXT.into(),
                target_ref: reference(1),
                tab_ref: reference(2),
                url: url.into(),
            }
            .validate()
            .unwrap();
        }
        for url in [
            "file:///tmp/a",
            "javascript:alert(1)",
            "data:text/plain,x",
            " https://example.com",
        ] {
            assert_eq!(
                BrowserNavigateRequest {
                    context_id: CONTEXT.into(),
                    target_ref: reference(1),
                    tab_ref: reference(2),
                    url: url.into(),
                }
                .validate(),
                Err(BrowserContractError::InvalidUrl)
            );
        }
    }

    #[test]
    fn synthetic_click_requires_a_current_element_ref() {
        let request = BrowserClickRequest {
            context_id: CONTEXT.into(),
            target_ref: reference(1),
            tab_ref: reference(2),
            target: BrowserClickTarget::ViewportCss { x: 10, y: 20 },
            input_route: BrowserInputRoute::DomEvent,
        };
        assert_eq!(
            request.validate(),
            Err(BrowserContractError::SyntheticRouteRequiresRef)
        );
    }

    #[test]
    fn existing_profile_prepare_never_accepts_launch_or_northbound_approval_material() {
        BrowserPrepareRequest {
            context_id: CONTEXT.into(),
            process_id: 42,
            window_id: Some(7),
            allow_launch: false,
            profile_mode: BrowserPrepareProfileMode::ExistingProfile,
            profile_name: None,
        }
        .validate()
        .unwrap();
        assert_eq!(
            BrowserPrepareRequest {
                context_id: CONTEXT.into(),
                process_id: 42,
                window_id: Some(7),
                allow_launch: true,
                profile_mode: BrowserPrepareProfileMode::ExistingProfile,
                profile_name: None,
            }
            .validate(),
            Err(BrowserContractError::InvalidProfile)
        );
    }

    #[test]
    fn upload_staging_accepts_bytes_and_safe_basename_but_no_path() {
        let request = BrowserStageUploadRequest {
            context_id: CONTEXT.into(),
            file_name: "report.txt".into(),
            data_base64: STANDARD.encode(b"hello"),
        };
        assert_eq!(request.validate().unwrap(), 5);
        assert_eq!(
            BrowserStageUploadRequest {
                file_name: "../secret.txt".into(),
                ..request.clone()
            }
            .validate(),
            Err(BrowserContractError::InvalidUploadName)
        );
        assert_eq!(
            BrowserStageUploadRequest {
                data_base64: "not base64***".into(),
                ..request
            }
            .validate(),
            Err(BrowserContractError::InvalidUploadData)
        );
    }

    #[test]
    fn upload_accepts_only_bounded_unique_cumg_refs() {
        BrowserUploadRequest {
            context_id: CONTEXT.into(),
            target_ref: reference(1),
            tab_ref: reference(2),
            element_ref: reference(3),
            file_refs: vec![reference(4), reference(5)],
        }
        .validate()
        .unwrap();

        let duplicated = reference(4);
        assert_eq!(
            BrowserUploadRequest {
                context_id: CONTEXT.into(),
                target_ref: reference(1),
                tab_ref: reference(2),
                element_ref: reference(3),
                file_refs: vec![duplicated.clone(), duplicated],
            }
            .validate(),
            Err(BrowserContractError::DuplicateFileRef)
        );
    }

    #[test]
    fn download_uses_agent_private_staging_and_explicit_size_bound() {
        BrowserDownloadRequest {
            context_id: CONTEXT.into(),
            target_ref: reference(1),
            tab_ref: reference(2),
            element_ref: reference(3),
            destination_name: "download.bin".into(),
            max_bytes: 8 * 1024 * 1024,
            overwrite: false,
        }
        .validate()
        .unwrap();

        assert_eq!(
            BrowserDownloadRequest {
                context_id: CONTEXT.into(),
                target_ref: reference(1),
                tab_ref: reference(2),
                element_ref: reference(3),
                destination_name: "download.bin".into(),
                max_bytes: 0,
                overwrite: true,
            }
            .validate(),
            Err(BrowserContractError::InvalidDownloadBound)
        );

        for invalid_name in [
            "../escape",
            "nested/file",
            "nested\\file",
            "CON.txt",
            "name.",
        ] {
            assert_eq!(
                BrowserDownloadRequest {
                    context_id: CONTEXT.into(),
                    target_ref: reference(1),
                    tab_ref: reference(2),
                    element_ref: reference(3),
                    destination_name: invalid_name.into(),
                    max_bytes: 1024,
                    overwrite: false,
                }
                .validate(),
                Err(BrowserContractError::InvalidDownloadName)
            );
        }
    }

    #[test]
    fn continuation_cannot_be_mixed_with_a_new_query_or_scope() {
        assert_eq!(
            BrowserInspectRequest {
                context_id: CONTEXT.into(),
                target_ref: reference(1),
                tab_ref: reference(2),
                scope_ref: None,
                query: Some("settings".into()),
                continuation_ref: Some(reference(3)),
                include_screenshot: false,
            }
            .validate(),
            Err(BrowserContractError::InvalidContinuation)
        );
    }

    #[test]
    fn pointer_shapes_are_exact_and_do_not_smuggle_extra_state() {
        BrowserPointerRequest {
            context_id: CONTEXT.into(),
            target_ref: reference(1),
            tab_ref: reference(2),
            element_ref: reference(3),
            action: BrowserPointerAction::Drag,
            destination_ref: Some(reference(4)),
            delta_x: 0,
            delta_y: 0,
            input_route: BrowserInputRoute::DomEvent,
        }
        .validate()
        .unwrap();

        assert_eq!(
            BrowserPointerRequest {
                context_id: CONTEXT.into(),
                target_ref: reference(1),
                tab_ref: reference(2),
                element_ref: reference(3),
                action: BrowserPointerAction::Scroll,
                destination_ref: None,
                delta_x: 0,
                delta_y: 0,
                input_route: BrowserInputRoute::Trusted,
            }
            .validate(),
            Err(BrowserContractError::InvalidPointerAction)
        );
    }
}
