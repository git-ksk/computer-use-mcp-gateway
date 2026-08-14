//! Cua-specific mapping for the backend-neutral V2 browser contract.
//!
//! Cua tool names, raw browser ids, backend page refs, local filesystem paths,
//! and the Cua MCP-host download approval marker terminate in this module. The
//! northbound surface must resolve CUMG opaque refs before calling these helpers.

use crate::v2_browser::{
    BrowserClickRequest, BrowserClickTarget, BrowserContractError, BrowserDialogAction,
    BrowserDialogDelivery, BrowserDialogRequest, BrowserDownloadRequest, BrowserInputRoute,
    BrowserInspectRequest, BrowserNavigateRequest, BrowserPointerAction, BrowserPointerRequest,
    BrowserPrepareProfileMode, BrowserPrepareRequest, BrowserTypeMode, BrowserTypeRequest,
    BrowserUploadRequest,
};
use serde_json::{Map, Value, json};
use std::fmt;
use std::path::Path;

const MAX_BACKEND_BROWSER_HANDLE_BYTES: usize = 4 * 1024;
const MAX_RESOLVED_LOCAL_PATH_BYTES: usize = 16 * 1024;
const CUA_DOWNLOAD_HOST_APPROVAL_ARG: &str = "_cua_browser_download_mcp_host_approved";

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct ResolvedBrowserTarget {
    target_id: String,
    tab_id: String,
}

impl ResolvedBrowserTarget {
    pub(crate) fn new(target_id: String, tab_id: String) -> Result<Self, CuaBrowserMappingError> {
        validate_backend_handle(&target_id)?;
        validate_backend_handle(&tab_id)?;
        Ok(Self { target_id, tab_id })
    }

    fn insert(&self, args: &mut Map<String, Value>) {
        args.insert("target_id".into(), json!(self.target_id));
        args.insert("tab_id".into(), json!(self.tab_id));
    }
}

impl fmt::Debug for ResolvedBrowserTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ResolvedBrowserTarget")
            .field("target_id", &"[redacted]")
            .field("tab_id", &"[redacted]")
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct ResolvedBrowserHandle(String);

impl ResolvedBrowserHandle {
    pub(crate) fn new(value: String) -> Result<Self, CuaBrowserMappingError> {
        validate_backend_handle(&value)?;
        Ok(Self(value))
    }

    fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ResolvedBrowserHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ResolvedBrowserHandle([redacted])")
    }
}

/// A local path obtained only after resolving a CUMG-issued upload file ref.
/// This constructor deliberately does not accept a northbound request type.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct ResolvedUploadFilePath(String);

impl ResolvedUploadFilePath {
    pub(crate) fn from_file_ref_resolution(value: String) -> Result<Self, CuaBrowserMappingError> {
        validate_resolved_absolute_path(&value)?;
        Ok(Self(value))
    }

    fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ResolvedUploadFilePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ResolvedUploadFilePath([redacted])")
    }
}

/// A canonical destination root obtained only after resolving a CUMG-issued
/// local-write capability. Canonical-directory proof is owned by that registry;
/// Cua independently re-validates the directory as defense in depth.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct ResolvedDownloadRoot(String);

impl ResolvedDownloadRoot {
    pub(crate) fn from_destination_ref_resolution(
        value: String,
    ) -> Result<Self, CuaBrowserMappingError> {
        validate_resolved_absolute_path(&value)?;
        Ok(Self(value))
    }

    fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ResolvedDownloadRoot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ResolvedDownloadRoot([redacted])")
    }
}

/// Evidence that CUMG, acting as the MCP host for Cua, has already admitted an
/// exact BrowserDownload capability for this operation. This is not a bearer
/// credential and is never accepted from northbound request JSON.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CuaDownloadHostApproval {
    NotApproved,
    ApprovedAfterExactCapabilityAuthorization,
}

#[derive(Clone, PartialEq)]
pub(crate) struct CuaBrowserCall {
    tool: &'static str,
    arguments: Map<String, Value>,
}

impl CuaBrowserCall {
    pub(crate) fn tool(&self) -> &'static str {
        self.tool
    }

    pub(crate) fn arguments(&self) -> &Map<String, Value> {
        &self.arguments
    }
}

impl fmt::Debug for CuaBrowserCall {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CuaBrowserCall")
            .field("tool", &self.tool)
            .field("arguments", &"[redacted backend browser arguments]")
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CuaBrowserMappingError {
    InvalidContract(BrowserContractError),
    InvalidBackendHandle,
    ResolutionMismatch,
    InvalidResolvedPath,
}

impl fmt::Display for CuaBrowserMappingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for CuaBrowserMappingError {}

impl From<BrowserContractError> for CuaBrowserMappingError {
    fn from(value: BrowserContractError) -> Self {
        Self::InvalidContract(value)
    }
}

pub(crate) fn map_browser_prepare(
    request: &BrowserPrepareRequest,
) -> Result<CuaBrowserCall, CuaBrowserMappingError> {
    request.validate()?;
    let mut args = Map::new();
    args.insert("pid".into(), json!(request.process_id));
    args.insert("session".into(), json!(request.context_id));
    match request.profile_mode {
        BrowserPrepareProfileMode::IsolatedNew => {
            args.insert("allow_launch".into(), json!(request.allow_launch));
            args.insert("profile".into(), json!({"mode": "isolated_new"}));
        }
        BrowserPrepareProfileMode::IsolatedNamed => {
            args.insert("allow_launch".into(), json!(request.allow_launch));
            args.insert(
                "profile".into(),
                json!({
                    "mode": "isolated_named",
                    "name": request.profile_name.as_deref().expect("validated named profile"),
                }),
            );
        }
        BrowserPrepareProfileMode::ExistingProfile => {
            args.insert(
                "window_id".into(),
                json!(request.window_id.expect("validated existing-profile window")),
            );
            args.insert("strategy".into(), json!({"kind": "existing_profile"}));
            // Existing-profile runtime grants/host authorization are intentionally
            // not synthesized here. Cua remains free to refuse the preparation.
        }
    }
    Ok(call("browser_prepare", args))
}

pub(crate) fn map_browser_bind(
    context_id: &str,
    process_id: u32,
    window_id: u64,
) -> Result<CuaBrowserCall, CuaBrowserMappingError> {
    crate::v2_browser::BrowserBindRequest {
        context_id: context_id.to_owned(),
        process_id,
        window_id,
    }
    .validate()?;
    Ok(call(
        "get_browser_state",
        object(json!({
            "pid": process_id,
            "window_id": window_id,
            "session": context_id,
        })),
    ))
}

pub(crate) fn map_browser_inspect(
    request: &BrowserInspectRequest,
    target: &ResolvedBrowserTarget,
    backend_scope_ref: Option<&ResolvedBrowserHandle>,
    backend_continuation: Option<&ResolvedBrowserHandle>,
) -> Result<CuaBrowserCall, CuaBrowserMappingError> {
    request.validate()?;
    if request.scope_ref.is_some() != backend_scope_ref.is_some()
        || request.continuation_ref.is_some() != backend_continuation.is_some()
    {
        return Err(CuaBrowserMappingError::ResolutionMismatch);
    }
    let mut args = Map::new();
    target.insert(&mut args);
    args.insert("session".into(), json!(request.context_id));
    args.insert("snapshot_format".into(), json!("semantic_v2"));
    args.insert("include_screenshot".into(), json!(request.include_screenshot));
    if let Some(query) = &request.query {
        args.insert("query".into(), json!(query));
    }
    if let Some(scope_ref) = backend_scope_ref {
        args.insert("scope_ref".into(), json!(scope_ref.as_str()));
    }
    if let Some(continuation) = backend_continuation {
        args.insert("continuation".into(), json!(continuation.as_str()));
    }
    Ok(call("get_browser_state", args))
}

pub(crate) fn map_browser_navigate(
    request: &BrowserNavigateRequest,
    target: &ResolvedBrowserTarget,
) -> Result<CuaBrowserCall, CuaBrowserMappingError> {
    request.validate()?;
    let mut args = Map::new();
    target.insert(&mut args);
    args.insert("session".into(), json!(request.context_id));
    args.insert("url".into(), json!(request.url));
    Ok(call("browser_navigate", args))
}

pub(crate) fn map_browser_click(
    request: &BrowserClickRequest,
    target: &ResolvedBrowserTarget,
    backend_element_ref: Option<&ResolvedBrowserHandle>,
) -> Result<CuaBrowserCall, CuaBrowserMappingError> {
    request.validate()?;
    let mut args = Map::new();
    target.insert(&mut args);
    args.insert("session".into(), json!(request.context_id));
    args.insert("input_route".into(), json!(input_route(request.input_route)));
    match &request.target {
        BrowserClickTarget::Element { .. } => {
            let element = backend_element_ref.ok_or(CuaBrowserMappingError::ResolutionMismatch)?;
            args.insert("ref".into(), json!(element.as_str()));
        }
        BrowserClickTarget::ViewportCss { x, y } => {
            if backend_element_ref.is_some() {
                return Err(CuaBrowserMappingError::ResolutionMismatch);
            }
            args.insert("x".into(), json!(x));
            args.insert("y".into(), json!(y));
        }
    }
    Ok(call("browser_click", args))
}

pub(crate) fn map_browser_type(
    request: &BrowserTypeRequest,
    target: &ResolvedBrowserTarget,
    backend_element_ref: &ResolvedBrowserHandle,
) -> Result<CuaBrowserCall, CuaBrowserMappingError> {
    request.validate()?;
    let mut args = Map::new();
    target.insert(&mut args);
    args.insert("session".into(), json!(request.context_id));
    args.insert("ref".into(), json!(backend_element_ref.as_str()));
    args.insert("text".into(), json!(request.text));
    args.insert(
        "mode".into(),
        json!(match request.mode {
            BrowserTypeMode::InsertText => "insert_text",
            BrowserTypeMode::Keystrokes => "keystrokes",
        }),
    );
    args.insert("replace".into(), json!(request.replace));
    Ok(call("browser_type", args))
}

pub(crate) fn map_browser_dialog(
    request: &BrowserDialogRequest,
    target: &ResolvedBrowserTarget,
    backend_dialog_id: &ResolvedBrowserHandle,
) -> Result<CuaBrowserCall, CuaBrowserMappingError> {
    request.validate()?;
    let mut args = Map::new();
    target.insert(&mut args);
    args.insert("session".into(), json!(request.context_id));
    args.insert("dialog_id".into(), json!(backend_dialog_id.as_str()));
    args.insert(
        "action".into(),
        json!(match request.action {
            BrowserDialogAction::Accept => "accept",
            BrowserDialogAction::Dismiss => "dismiss",
        }),
    );
    args.insert(
        "delivery_mode".into(),
        json!(match request.delivery {
            BrowserDialogDelivery::Background => "background",
            BrowserDialogDelivery::Foreground => "foreground",
        }),
    );
    if let Some(prompt_text) = &request.prompt_text {
        args.insert("prompt_text".into(), json!(prompt_text));
    }
    Ok(call("browser_dialog", args))
}

pub(crate) fn map_browser_pointer(
    request: &BrowserPointerRequest,
    target: &ResolvedBrowserTarget,
    backend_element_ref: &ResolvedBrowserHandle,
    backend_destination_ref: Option<&ResolvedBrowserHandle>,
) -> Result<CuaBrowserCall, CuaBrowserMappingError> {
    request.validate()?;
    if request.destination_ref.is_some() != backend_destination_ref.is_some() {
        return Err(CuaBrowserMappingError::ResolutionMismatch);
    }
    let mut args = Map::new();
    target.insert(&mut args);
    args.insert("session".into(), json!(request.context_id));
    args.insert("ref".into(), json!(backend_element_ref.as_str()));
    args.insert("input_route".into(), json!(input_route(request.input_route)));
    args.insert(
        "action".into(),
        json!(match request.action {
            BrowserPointerAction::Hover => "hover",
            BrowserPointerAction::RightClick => "right_click",
            BrowserPointerAction::DoubleClick => "double_click",
            BrowserPointerAction::Scroll => "scroll",
            BrowserPointerAction::Drag => "drag",
        }),
    );
    if request.action == BrowserPointerAction::Scroll {
        args.insert("delta_x".into(), json!(request.delta_x));
        args.insert("delta_y".into(), json!(request.delta_y));
    }
    if let Some(destination) = backend_destination_ref {
        args.insert("destination_ref".into(), json!(destination.as_str()));
    }
    Ok(call("browser_pointer", args))
}

pub(crate) fn map_browser_upload(
    request: &BrowserUploadRequest,
    target: &ResolvedBrowserTarget,
    backend_element_ref: &ResolvedBrowserHandle,
    files: &[ResolvedUploadFilePath],
) -> Result<CuaBrowserCall, CuaBrowserMappingError> {
    request.validate()?;
    if files.len() != request.file_refs.len() {
        return Err(CuaBrowserMappingError::ResolutionMismatch);
    }
    let mut args = Map::new();
    target.insert(&mut args);
    args.insert("session".into(), json!(request.context_id));
    args.insert("ref".into(), json!(backend_element_ref.as_str()));
    args.insert(
        "paths".into(),
        Value::Array(files.iter().map(|path| json!(path.as_str())).collect()),
    );
    Ok(call("browser_set_input_files", args))
}

pub(crate) fn map_browser_download(
    request: &BrowserDownloadRequest,
    target: &ResolvedBrowserTarget,
    backend_element_ref: &ResolvedBrowserHandle,
    destination_root: &ResolvedDownloadRoot,
    approval: CuaDownloadHostApproval,
) -> Result<CuaBrowserCall, CuaBrowserMappingError> {
    request.validate()?;
    let mut args = Map::new();
    target.insert(&mut args);
    args.insert("session".into(), json!(request.context_id));
    args.insert("ref".into(), json!(backend_element_ref.as_str()));
    args.insert("destination_root".into(), json!(destination_root.as_str()));
    if approval == CuaDownloadHostApproval::ApprovedAfterExactCapabilityAuthorization {
        // CUMG is the MCP host of the Cua backend. This marker is emitted only
        // after CUMG's own exact BrowserDownload authorization/admission path;
        // it is never copied from caller JSON and never used for browser_prepare.
        args.insert(CUA_DOWNLOAD_HOST_APPROVAL_ARG.into(), json!(true));
    }
    Ok(call("browser_download", args))
}

fn input_route(route: BrowserInputRoute) -> &'static str {
    match route {
        BrowserInputRoute::Trusted => "trusted",
        BrowserInputRoute::DomEvent => "dom_event",
    }
}

fn call(tool: &'static str, arguments: Map<String, Value>) -> CuaBrowserCall {
    CuaBrowserCall { tool, arguments }
}

fn object(value: Value) -> Map<String, Value> {
    value.as_object().cloned().expect("static JSON object")
}

fn validate_backend_handle(value: &str) -> Result<(), CuaBrowserMappingError> {
    if value.is_empty()
        || value.len() > MAX_BACKEND_BROWSER_HANDLE_BYTES
        || value.chars().any(char::is_control)
    {
        return Err(CuaBrowserMappingError::InvalidBackendHandle);
    }
    Ok(())
}

fn validate_resolved_absolute_path(value: &str) -> Result<(), CuaBrowserMappingError> {
    if value.is_empty()
        || value.len() > MAX_RESOLVED_LOCAL_PATH_BYTES
        || value.chars().any(char::is_control)
        || !Path::new(value).is_absolute()
    {
        return Err(CuaBrowserMappingError::InvalidResolvedPath);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v2_browser::{
        BrowserClickTarget, BrowserDownloadRequest, BrowserPointerAction, BrowserPrepareProfileMode,
        BrowserPrepareRequest, BrowserUploadRequest,
    };

    const CONTEXT: &str = "ctx_0123456789abcdef0123456789abcdef";

    fn public_ref(index: u8) -> String {
        format!("ref_{index:032x}")
    }

    fn target() -> ResolvedBrowserTarget {
        ResolvedBrowserTarget::new("backend-target-secret".into(), "backend-tab-secret".into())
            .unwrap()
    }

    fn handle(name: &str) -> ResolvedBrowserHandle {
        ResolvedBrowserHandle::new(name.into()).unwrap()
    }

    #[test]
    fn bind_uses_context_as_cua_session_without_exposing_backend_ids_northbound() {
        let call = map_browser_bind(CONTEXT, 42, 7).unwrap();
        assert_eq!(call.tool(), "get_browser_state");
        assert_eq!(call.arguments()["pid"], json!(42));
        assert_eq!(call.arguments()["window_id"], json!(7));
        assert_eq!(call.arguments()["session"], json!(CONTEXT));
    }

    #[test]
    fn existing_profile_prepare_never_synthesizes_a_grant_or_approval_token() {
        let call = map_browser_prepare(&BrowserPrepareRequest {
            context_id: CONTEXT.into(),
            process_id: 42,
            window_id: Some(7),
            allow_launch: false,
            profile_mode: BrowserPrepareProfileMode::ExistingProfile,
            profile_name: None,
        })
        .unwrap();
        assert_eq!(call.tool(), "browser_prepare");
        assert_eq!(call.arguments()["strategy"], json!({"kind": "existing_profile"}));
        assert!(!call.arguments().contains_key("approval_token"));
        assert!(!call.arguments().contains_key(CUA_DOWNLOAD_HOST_APPROVAL_ARG));
    }

    #[test]
    fn synthetic_click_receives_only_the_resolved_backend_element_ref() {
        let request = BrowserClickRequest {
            context_id: CONTEXT.into(),
            target_ref: public_ref(1),
            tab_ref: public_ref(2),
            target: BrowserClickTarget::Element {
                element_ref: public_ref(3),
            },
            input_route: BrowserInputRoute::DomEvent,
        };
        let call = map_browser_click(&request, &target(), Some(&handle("backend-element"))).unwrap();
        assert_eq!(call.tool(), "browser_click");
        assert_eq!(call.arguments()["ref"], json!("backend-element"));
        assert_ne!(call.arguments()["ref"], json!(public_ref(3)));
        assert_eq!(call.arguments()["input_route"], json!("dom_event"));
    }

    #[test]
    fn inspect_ref_presence_must_match_completed_cumg_resolution() {
        let request = BrowserInspectRequest {
            context_id: CONTEXT.into(),
            target_ref: public_ref(1),
            tab_ref: public_ref(2),
            scope_ref: Some(public_ref(3)),
            query: None,
            continuation_ref: None,
            include_screenshot: false,
        };
        assert_eq!(
            map_browser_inspect(&request, &target(), None, None),
            Err(CuaBrowserMappingError::ResolutionMismatch)
        );
    }

    #[test]
    fn upload_never_accepts_arbitrary_paths_from_the_browser_request() {
        let request = BrowserUploadRequest {
            context_id: CONTEXT.into(),
            target_ref: public_ref(1),
            tab_ref: public_ref(2),
            element_ref: public_ref(3),
            file_refs: vec![public_ref(4)],
        };
        let file = ResolvedUploadFilePath::from_file_ref_resolution("/tmp/proven-upload.txt".into())
            .unwrap();
        let call = map_browser_upload(&request, &target(), &handle("backend-upload"), &[file])
            .unwrap();
        assert_eq!(call.arguments()["paths"], json!(["/tmp/proven-upload.txt"]));
        assert!(!serde_json::to_string(&request).unwrap().contains("/tmp/proven-upload.txt"));
    }

    #[test]
    fn download_host_approval_marker_is_fail_closed_and_not_caller_controlled() {
        let request = BrowserDownloadRequest {
            context_id: CONTEXT.into(),
            target_ref: public_ref(1),
            tab_ref: public_ref(2),
            element_ref: public_ref(3),
            destination_root_ref: public_ref(4),
            max_bytes: 1024,
            overwrite: false,
        };
        let root = ResolvedDownloadRoot::from_destination_ref_resolution("/tmp".into()).unwrap();
        let denied = map_browser_download(
            &request,
            &target(),
            &handle("backend-download"),
            &root,
            CuaDownloadHostApproval::NotApproved,
        )
        .unwrap();
        assert!(!denied.arguments().contains_key(CUA_DOWNLOAD_HOST_APPROVAL_ARG));

        let approved = map_browser_download(
            &request,
            &target(),
            &handle("backend-download"),
            &root,
            CuaDownloadHostApproval::ApprovedAfterExactCapabilityAuthorization,
        )
        .unwrap();
        assert_eq!(approved.arguments()[CUA_DOWNLOAD_HOST_APPROVAL_ARG], json!(true));
        assert!(!serde_json::to_value(&request)
            .unwrap()
            .as_object()
            .unwrap()
            .contains_key(CUA_DOWNLOAD_HOST_APPROVAL_ARG));
    }

    #[test]
    fn pointer_destination_resolution_is_exact() {
        let request = BrowserPointerRequest {
            context_id: CONTEXT.into(),
            target_ref: public_ref(1),
            tab_ref: public_ref(2),
            element_ref: public_ref(3),
            action: BrowserPointerAction::Drag,
            destination_ref: Some(public_ref(4)),
            delta_x: 0,
            delta_y: 0,
            input_route: BrowserInputRoute::DomEvent,
        };
        assert_eq!(
            map_browser_pointer(&request, &target(), &handle("source"), None),
            Err(CuaBrowserMappingError::ResolutionMismatch)
        );
        let call = map_browser_pointer(
            &request,
            &target(),
            &handle("source"),
            Some(&handle("destination")),
        )
        .unwrap();
        assert_eq!(call.arguments()["destination_ref"], json!("destination"));
    }

    #[test]
    fn adapter_debug_output_redacts_backend_browser_material_and_paths() {
        let target = target();
        assert!(!format!("{target:?}").contains("backend-target-secret"));
        let path = ResolvedUploadFilePath::from_file_ref_resolution("/tmp/secret-upload.txt".into())
            .unwrap();
        assert!(!format!("{path:?}").contains("secret-upload"));
        let call = map_browser_bind(CONTEXT, 42, 7).unwrap();
        assert!(!format!("{call:?}").contains(CONTEXT));
    }
}
