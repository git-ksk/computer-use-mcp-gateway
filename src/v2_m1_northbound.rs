//! Standard MCP Authorization boundary for the V2 Hub.
//!
//! This module deliberately keeps OAuth access tokens northbound. The bearer
//! token is validated at the HTTP resource-server boundary and reduced to an
//! [`AuthenticatedClientPrincipal`] plus OAuth scopes. Only that principal is
//! passed into the local principal -> device -> exact `DeviceCapability`
//! policy. Southbound Hub/Agent messages continue to carry only typed commands
//! and short-lived exact capability grants.

use crate::v2_observability::SafeErrorCode;
use crate::{
    v2_browser::{
        BrowserAction, BrowserBindRequest, BrowserBindingResult, BrowserClickRequest,
        BrowserClickTarget, BrowserContractError, BrowserDialogAction, BrowserDialogRequest,
        BrowserDialogResult, BrowserDownloadRequest, BrowserDownloadResult, BrowserInspectRequest,
        BrowserNavigateRequest, BrowserPointerAction, BrowserPointerRequest, BrowserPrepareRequest,
        BrowserSemanticRef, BrowserSnapshotResult, BrowserStageUploadRequest,
        BrowserStagedUploadResult, BrowserTabSummary, BrowserTypeRequest, BrowserUploadRequest,
        MAX_BROWSER_DOWNLOAD_BASE64_BYTES, MAX_BROWSER_DOWNLOAD_BYTES,
        MAX_BROWSER_DOWNLOAD_NAME_BYTES, MAX_BROWSER_PROFILE_NAME_BYTES,
        MAX_BROWSER_PROMPT_TEXT_BYTES, MAX_BROWSER_QUERY_BYTES, MAX_BROWSER_SCROLL_DELTA_CSS_PX,
        MAX_BROWSER_TEXT_BYTES, MAX_BROWSER_UPLOAD_BASE64_BYTES, MAX_BROWSER_UPLOAD_FILES,
        MAX_BROWSER_UPLOAD_NAME_BYTES, MAX_BROWSER_URL_BYTES,
    },
    v2_browser_refs::{BrowserRefError, BrowserRefRegistry, DEFAULT_MAX_BROWSER_REFS_PER_CONTEXT},
    v2_browser_runtime::{
        BrowserBackendClickTarget, BrowserBackendCommand, BrowserBackendResult,
        BrowserBackendSemanticRef, BrowserStagedUploadFile,
    },
    v2_execution_safety::{OperationOwner, RecoverableOperationResult},
    v2_interaction_context::{
        DEFAULT_MAX_REFS_PER_CONTEXT, InteractionContextBinding, InteractionContextId,
        InteractionContextLimits, InteractionContextManager, InteractionScope,
        ScopedBackendRefRegistry, ScopedRefError, ScopedRefKind,
    },
    v2_m0::{
        BrowserUploadPayload, CapabilityAdvertisement, DeviceCapability, DeviceCommand,
        DeviceResult, InputDeliveryMode, InputTarget, KeyboardModifier, MAX_CLIPBOARD_TEXT_BYTES,
        MAX_KEYBOARD_MODIFIERS, MAX_MENU_PATH_SEGMENTS, MAX_MENU_SEGMENT_BYTES,
        MAX_TYPE_TEXT_BYTES, MAX_UI_ELEMENTS, MAX_UI_PREDICATES, MAX_UI_QUERY_BYTES, PointerButton,
        PointerTarget, ProcessEnvVar, ProcessRequest, ScrollDirection, ScrollGranularity,
        ScrollTarget, ShellRequest, UiElementAction, UiPredicate, UiRect, UiRole,
    },
    v2_m0_execution::HubOperationState,
    v2_m0_trust::{AuthenticatedClientPrincipal, ClientAuthorizationPolicy, TrustError},
    v2_m1_hub::{HubCommandError, HubHandle},
    v2_usage::{UsageError, UsageLease, UsageManager, UsageOperation, UsageSettlement},
};
use async_trait::async_trait;
use axum::{
    Json, Router,
    extract::DefaultBodyLimit,
    extract::{Request, State},
    http::{
        HeaderMap, HeaderValue, StatusCode,
        header::{AUTHORIZATION, ORIGIN, WWW_AUTHENTICATE},
        request::Parts,
    },
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use reqwest::{Client, Url, redirect::Policy as RedirectPolicy};
use rmcp::{
    ErrorData as McpError, ServerHandler,
    model::{
        CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
        InitializeRequestParams, InitializeResult, JsonObject, ListToolsResult,
        PaginatedRequestParams, ProtocolVersion, ServerCapabilities, ServerInfo, Tool,
        ToolAnnotations,
    },
    service::{RequestContext, RoleServer},
    transport::streamable_http_server::{
        StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
    },
};
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::{
    borrow::Cow,
    collections::{BTreeMap, HashSet},
    fmt,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{sync::Mutex as TokioMutex, time::MissedTickBehavior};
use tracing::warn;

const DEFAULT_INTROSPECTION_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_BEARER_TOKEN_BYTES: usize = 16 * 1024;
const MAX_AUDIT_CLIENT_NAME_BYTES: usize = 128;
const MAX_AUDIT_CLIENT_VERSION_BYTES: usize = 64;
const MAX_AUDIT_CLIENT_DESCRIPTION_BYTES: usize = 256;
const TOOL_LIST_APPS: &str = "list_apps";
const TOOL_GET_SCREEN_SIZE: &str = "get_screen_size";
const TOOL_SCREENSHOT: &str = "screenshot";
const TOOL_CLICK: &str = "click";
const TOOL_DRAG: &str = "drag";
const TOOL_TYPE_TEXT: &str = "type_text";
const TOOL_EXECUTE_PROCESS: &str = "execute_process";
const TOOL_SHELL: &str = "shell";
const TOOL_GET_OPERATION: &str = "get_operation";
const TOOL_READ_FILE: &str = "read_file";
const TOOL_LIST_DIRECTORY: &str = "list_directory";
const TOOL_LIST_WINDOWS: &str = "list_windows";
const TOOL_LAUNCH_APPLICATION: &str = "launch_application";
const TOOL_INSPECT_WINDOW: &str = "inspect_window";
const TOOL_VERIFY_UI_STATE: &str = "verify_ui_state";
const TOOL_TERMINATE_APPLICATION: &str = "terminate_application";
const TOOL_ACTIVATE_WINDOW: &str = "activate_window";
const TOOL_SET_WINDOW_FRAME: &str = "set_window_frame";
const TOOL_INVOKE_MENU: &str = "invoke_menu";
const TOOL_KEYBOARD_INPUT: &str = "keyboard_input";
const TOOL_SCROLL: &str = "scroll";
const TOOL_CLIPBOARD_READ: &str = "clipboard_read";
const TOOL_CLIPBOARD_WRITE: &str = "clipboard_write";
const TOOL_GET_POINTER_POSITION: &str = "get_pointer_position";
const TOOL_MOVE_POINTER: &str = "move_pointer";
const TOOL_OPEN_INTERACTION_CONTEXT: &str = "open_interaction_context";
const TOOL_CLOSE_INTERACTION_CONTEXT: &str = "close_interaction_context";
const TOOL_EXPAND_INTERACTION_SCOPE: &str = "expand_interaction_scope";
const TOOL_SET_UI_VALUE: &str = "set_ui_value";
const TOOL_CAPTURE_REGION: &str = "capture_region";
const TOOL_BROWSER_PREPARE: &str = "browser_prepare";
const TOOL_BROWSER_BIND: &str = "browser_bind";
const TOOL_BROWSER_INSPECT: &str = "browser_inspect";
const TOOL_BROWSER_NAVIGATE: &str = "browser_navigate";
const TOOL_BROWSER_CLICK: &str = "browser_click";
const TOOL_BROWSER_TYPE: &str = "browser_type";
const TOOL_BROWSER_DIALOG: &str = "browser_dialog";
const TOOL_BROWSER_POINTER: &str = "browser_pointer";
const TOOL_BROWSER_STAGE_UPLOAD_FILE: &str = "browser_stage_upload_file";
const TOOL_BROWSER_UPLOAD_FILE: &str = "browser_upload_file";
const TOOL_BROWSER_DOWNLOAD: &str = "browser_download_file";

const CONTEXT_ELIGIBLE_CAPABILITIES: &[DeviceCapability] = &[
    DeviceCapability::Screenshot,
    DeviceCapability::PointerClick,
    DeviceCapability::PointerDrag,
    DeviceCapability::TypeText,
    DeviceCapability::ListWindows,
    DeviceCapability::LaunchApplication,
    DeviceCapability::InspectWindow,
    DeviceCapability::VerifyUiState,
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
];

#[derive(Debug, Clone)]
pub struct NorthboundMcpConfig {
    resource: String,
    authorization_server: String,
    resource_url: Url,
    metadata_url: Url,
    required_scopes: Vec<String>,
}

impl NorthboundMcpConfig {
    pub fn new(
        resource: impl Into<String>,
        authorization_server: impl Into<String>,
        required_scopes: Vec<String>,
    ) -> Result<Self, NorthboundConfigError> {
        let resource = resource.into();
        let authorization_server = authorization_server.into();
        let resource_url = validate_https_url(&resource, "MCP resource")?;
        if resource_url.query().is_some() || resource_url.fragment().is_some() {
            return Err(NorthboundConfigError::InvalidResourceUri);
        }
        let authorization_url = validate_https_url(&authorization_server, "authorization server")?;
        if authorization_url.query().is_some() || authorization_url.fragment().is_some() {
            return Err(NorthboundConfigError::InvalidAuthorizationServerUri);
        }
        if required_scopes.is_empty()
            || required_scopes
                .iter()
                .any(|scope| !valid_scope_token(scope))
        {
            return Err(NorthboundConfigError::InvalidScope);
        }
        let mut seen = HashSet::new();
        if required_scopes
            .iter()
            .any(|scope| !seen.insert(scope.clone()))
        {
            return Err(NorthboundConfigError::DuplicateScope);
        }

        let metadata_path = protected_resource_metadata_path(resource_url.path());
        let mut metadata_url = resource_url.clone();
        metadata_url.set_path(&metadata_path);
        metadata_url.set_query(None);
        metadata_url.set_fragment(None);

        Ok(Self {
            resource,
            authorization_server,
            resource_url,
            metadata_url,
            required_scopes,
        })
    }

    pub fn resource(&self) -> &str {
        &self.resource
    }

    pub fn authorization_server(&self) -> &str {
        &self.authorization_server
    }

    pub fn mcp_path(&self) -> &str {
        let path = self.resource_url.path();
        if path.is_empty() { "/" } else { path }
    }

    pub fn metadata_path(&self) -> &str {
        self.metadata_url.path()
    }

    pub fn metadata_url(&self) -> &str {
        self.metadata_url.as_str()
    }

    pub fn required_scopes(&self) -> &[String] {
        &self.required_scopes
    }

    pub fn protected_resource_metadata(&self) -> Value {
        json!({
            "resource": self.resource,
            "authorization_servers": [self.authorization_server],
            "scopes_supported": self.required_scopes,
            "bearer_methods_supported": ["header"]
        })
    }
}

#[derive(Clone)]
pub struct TrustedProxyConfig {
    resource: String,
    resource_url: Url,
    principal: AuthenticatedClientPrincipal,
}

impl fmt::Debug for TrustedProxyConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TrustedProxyConfig")
            .field("resource", &self.resource)
            .field("principal", &"[REDACTED]")
            .finish()
    }
}

impl TrustedProxyConfig {
    /// Configure an explicitly single-principal deployment behind a reviewed
    /// authenticated proxy/tunnel. The proxy owns authentication; CUMG never
    /// trusts caller-supplied identity headers in this mode.
    pub fn new(
        resource: impl Into<String>,
        issuer: impl Into<String>,
        subject: impl Into<String>,
    ) -> Result<Self, NorthboundConfigError> {
        let resource = resource.into();
        let issuer = issuer.into();
        let resource_url = validate_https_url(&resource, "MCP resource")?;
        if resource_url.query().is_some() || resource_url.fragment().is_some() {
            return Err(NorthboundConfigError::InvalidResourceUri);
        }
        validate_https_url(&issuer, "trusted proxy issuer")
            .map_err(|_| NorthboundConfigError::InvalidTrustedProxyIssuerUri)?;
        let principal = AuthenticatedClientPrincipal::new(issuer, subject)
            .map_err(|_| NorthboundConfigError::InvalidTrustedProxyPrincipal)?;
        Ok(Self {
            resource,
            resource_url,
            principal,
        })
    }

    pub fn resource(&self) -> &str {
        &self.resource
    }

    pub fn issuer(&self) -> &str {
        &self.principal.issuer
    }

    pub fn mcp_path(&self) -> &str {
        let path = self.resource_url.path();
        if path.is_empty() { "/" } else { path }
    }

    pub fn principal(&self) -> &AuthenticatedClientPrincipal {
        &self.principal
    }
}

fn validate_https_url(value: &str, _kind: &'static str) -> Result<Url, NorthboundConfigError> {
    let url = Url::parse(value).map_err(|_| NorthboundConfigError::InvalidUrl)?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(NorthboundConfigError::HttpsRequired);
    }
    Ok(url)
}

fn protected_resource_metadata_path(resource_path: &str) -> String {
    let suffix = resource_path.trim_start_matches('/');
    if suffix.is_empty() {
        "/.well-known/oauth-protected-resource".to_owned()
    } else {
        format!("/.well-known/oauth-protected-resource/{suffix}")
    }
}

fn valid_scope_token(scope: &str) -> bool {
    !scope.is_empty()
        && scope
            .as_bytes()
            .iter()
            .all(|byte| matches!(*byte, 0x21 | 0x23..=0x5b | 0x5d..=0x7e))
}

#[derive(Clone)]
pub struct OAuthIntrospectionConfig {
    pub issuer: String,
    pub resource: String,
    pub endpoint: String,
    pub client_id: String,
    pub client_secret: String,
    pub timeout: Duration,
}

impl fmt::Debug for OAuthIntrospectionConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OAuthIntrospectionConfig")
            .field("issuer", &self.issuer)
            .field("resource", &self.resource)
            .field("endpoint", &self.endpoint)
            .field("client_id", &self.client_id)
            .field("client_secret", &"[REDACTED]")
            .field("timeout", &self.timeout)
            .finish()
    }
}

impl OAuthIntrospectionConfig {
    pub fn new(
        issuer: impl Into<String>,
        resource: impl Into<String>,
        endpoint: impl Into<String>,
        client_id: impl Into<String>,
        client_secret: impl Into<String>,
    ) -> Self {
        Self {
            issuer: issuer.into(),
            resource: resource.into(),
            endpoint: endpoint.into(),
            client_id: client_id.into(),
            client_secret: client_secret.into(),
            timeout: DEFAULT_INTROSPECTION_TIMEOUT,
        }
    }
}

#[derive(Clone)]
pub struct OAuthIntrospectionVerifier {
    client: Client,
    config: OAuthIntrospectionConfig,
}

impl OAuthIntrospectionVerifier {
    pub fn new(config: OAuthIntrospectionConfig) -> Result<Self, TokenVerificationError> {
        validate_https_url(&config.issuer, "issuer")
            .map_err(|_| TokenVerificationError::InvalidConfiguration)?;
        validate_https_url(&config.resource, "resource")
            .map_err(|_| TokenVerificationError::InvalidConfiguration)?;
        validate_https_url(&config.endpoint, "introspection endpoint")
            .map_err(|_| TokenVerificationError::InvalidConfiguration)?;
        if config.client_id.trim().is_empty()
            || config.client_secret.is_empty()
            || config.timeout.is_zero()
        {
            return Err(TokenVerificationError::InvalidConfiguration);
        }
        let client = Client::builder()
            .redirect(RedirectPolicy::none())
            .timeout(config.timeout)
            .build()
            .map_err(|_| TokenVerificationError::InvalidConfiguration)?;
        Ok(Self { client, config })
    }

    fn validate_response(
        &self,
        response: IntrospectionResponse,
    ) -> Result<VerifiedAccessToken, TokenVerificationError> {
        if !response.active {
            return Err(TokenVerificationError::InvalidToken);
        }
        let subject = response
            .sub
            .filter(|subject| !subject.trim().is_empty())
            .ok_or(TokenVerificationError::InvalidToken)?;
        let audiences = response.aud.ok_or(TokenVerificationError::InvalidToken)?;
        if !audiences.matches(&self.config.resource) {
            return Err(TokenVerificationError::InvalidToken);
        }
        if let Some(exp) = response.exp
            && exp <= unix_time_secs().map_err(|_| TokenVerificationError::Unavailable)?
        {
            return Err(TokenVerificationError::InvalidToken);
        }
        if let Some(issuer) = response.iss
            && issuer != self.config.issuer
        {
            return Err(TokenVerificationError::InvalidToken);
        }

        let principal = AuthenticatedClientPrincipal::new(self.config.issuer.clone(), subject)
            .map_err(|_| TokenVerificationError::InvalidToken)?;
        let scopes = response
            .scope
            .unwrap_or_default()
            .split_ascii_whitespace()
            .map(ToOwned::to_owned)
            .collect();
        Ok(VerifiedAccessToken { principal, scopes })
    }
}

#[async_trait]
impl AccessTokenVerifier for OAuthIntrospectionVerifier {
    async fn verify(&self, token: &str) -> Result<VerifiedAccessToken, TokenVerificationError> {
        if token.is_empty() {
            return Err(TokenVerificationError::InvalidToken);
        }
        let response = self
            .client
            .post(&self.config.endpoint)
            .basic_auth(&self.config.client_id, Some(&self.config.client_secret))
            .form(&[("token", token), ("token_type_hint", "access_token")])
            .send()
            .await
            .map_err(|_| TokenVerificationError::Unavailable)?;
        if !response.status().is_success() {
            return Err(TokenVerificationError::Unavailable);
        }
        let body = response
            .json::<IntrospectionResponse>()
            .await
            .map_err(|_| TokenVerificationError::Unavailable)?;
        self.validate_response(body)
    }
}

#[derive(Debug, Deserialize)]
struct IntrospectionResponse {
    active: bool,
    sub: Option<String>,
    aud: Option<AudienceClaim>,
    scope: Option<String>,
    exp: Option<u64>,
    iss: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum AudienceClaim {
    One(String),
    Many(Vec<String>),
}

impl AudienceClaim {
    fn matches(&self, expected: &str) -> bool {
        match self {
            Self::One(value) => canonical_resource_eq(value, expected),
            Self::Many(values) => values
                .iter()
                .any(|value| canonical_resource_eq(value, expected)),
        }
    }
}

fn canonical_resource_eq(candidate: &str, expected: &str) -> bool {
    let (Ok(candidate), Ok(expected)) = (Url::parse(candidate), Url::parse(expected)) else {
        return false;
    };
    candidate.scheme().eq_ignore_ascii_case(expected.scheme())
        && candidate
            .host_str()
            .zip(expected.host_str())
            .is_some_and(|(left, right)| left.eq_ignore_ascii_case(right))
        && candidate.port_or_known_default() == expected.port_or_known_default()
        && candidate.path() == expected.path()
        && candidate.query() == expected.query()
        && candidate.fragment().is_none()
        && expected.fragment().is_none()
}

fn unix_time_secs() -> Result<u64, std::time::SystemTimeError> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())
}

fn unix_time_ms() -> Result<u64, std::time::SystemTimeError> {
    Ok(
        u64::try_from(SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis())
            .unwrap_or(u64::MAX),
    )
}

#[derive(Clone)]
pub struct VerifiedAccessToken {
    pub principal: AuthenticatedClientPrincipal,
    pub scopes: HashSet<String>,
}

impl fmt::Debug for VerifiedAccessToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VerifiedAccessToken")
            .field("principal", &"[REDACTED]")
            .field("scope_count", &self.scopes.len())
            .finish()
    }
}

#[async_trait]
pub trait AccessTokenVerifier: Send + Sync {
    async fn verify(&self, token: &str) -> Result<VerifiedAccessToken, TokenVerificationError>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenVerificationError {
    InvalidConfiguration,
    InvalidToken,
    Unavailable,
}

impl fmt::Display for TokenVerificationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for TokenVerificationError {}

#[derive(Debug, Deserialize)]
pub struct NorthboundPolicyDocument {
    pub grants: Vec<NorthboundPolicyGrant>,
}

#[derive(Debug, Deserialize)]
pub struct NorthboundPolicyGrant {
    pub issuer: String,
    pub subject: String,
    pub device_id: String,
    pub capabilities: Vec<DeviceCapability>,
}

impl NorthboundPolicyDocument {
    pub fn from_json(value: &str) -> Result<Self, NorthboundPolicyError> {
        serde_json::from_str(value).map_err(NorthboundPolicyError::Json)
    }

    pub fn build_policy(
        self,
        expected_issuer: &str,
        expected_device_id: &str,
    ) -> Result<ClientAuthorizationPolicy, NorthboundPolicyError> {
        if self.grants.is_empty() {
            return Err(NorthboundPolicyError::EmptyPolicy);
        }
        let mut policy = ClientAuthorizationPolicy::default();
        let mut seen = HashSet::new();
        for grant in self.grants {
            if grant.issuer != expected_issuer {
                return Err(NorthboundPolicyError::IssuerMismatch);
            }
            if grant.device_id != expected_device_id {
                return Err(NorthboundPolicyError::DeviceMismatch);
            }
            if grant.capabilities.is_empty() {
                return Err(NorthboundPolicyError::EmptyCapabilities);
            }
            let principal = AuthenticatedClientPrincipal::new(grant.issuer, grant.subject)
                .map_err(NorthboundPolicyError::Trust)?;
            for capability in grant.capabilities {
                let key = (
                    principal.issuer.clone(),
                    principal.subject.clone(),
                    grant.device_id.clone(),
                    capability,
                );
                if !seen.insert(key) {
                    return Err(NorthboundPolicyError::DuplicateGrant);
                }
                policy.allow_device_capability(&principal, &grant.device_id, capability);
            }
        }
        Ok(policy)
    }
}

#[derive(Debug)]
pub enum NorthboundPolicyError {
    Json(serde_json::Error),
    Trust(TrustError),
    EmptyPolicy,
    IssuerMismatch,
    DeviceMismatch,
    EmptyCapabilities,
    DuplicateGrant,
}

impl fmt::Display for NorthboundPolicyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for NorthboundPolicyError {}

#[derive(Debug, Clone)]
struct NorthboundAuthContext {
    principal: AuthenticatedClientPrincipal,
}

#[derive(Clone)]
struct NorthboundAuthState {
    verifier: Arc<dyn AccessTokenVerifier>,
    config: Arc<NorthboundMcpConfig>,
}

#[derive(Clone)]
struct TrustedProxyAuthState {
    principal: AuthenticatedClientPrincipal,
    resource_url: Url,
}

#[derive(Debug, Clone, Copy)]
enum ExactOriginError {
    BadRequest,
    Forbidden,
}

impl ExactOriginError {
    fn into_response(self) -> Response {
        match self {
            Self::BadRequest => (
                StatusCode::BAD_REQUEST,
                "Bad Request: invalid Origin header",
            )
                .into_response(),
            Self::Forbidden => (
                StatusCode::FORBIDDEN,
                "Forbidden: Origin header is not allowed",
            )
                .into_response(),
        }
    }
}

fn validate_exact_browser_origin(
    headers: &HeaderMap,
    resource_url: &Url,
) -> Result<(), ExactOriginError> {
    let mut values = headers.get_all(ORIGIN).iter();
    let Some(value) = values.next() else {
        return Ok(());
    };
    if values.next().is_some() {
        return Err(ExactOriginError::BadRequest);
    }
    let raw = value.to_str().map_err(|_| ExactOriginError::BadRequest)?;
    let origin = Url::parse(raw).map_err(|_| ExactOriginError::BadRequest)?;
    if !origin.username().is_empty()
        || origin.password().is_some()
        || origin.path() != "/"
        || origin.query().is_some()
        || origin.fragment().is_some()
    {
        return Err(ExactOriginError::BadRequest);
    }
    if origin.origin() != resource_url.origin() {
        return Err(ExactOriginError::Forbidden);
    }
    Ok(())
}

/// Build a northbound MCP resource for an explicitly single-principal deployment
/// whose loopback origin is reachable only through a reviewed authenticated proxy.
///
/// This adapter deliberately does not read `X-User`, Cloudflare identity headers,
/// or any other caller-controlled identity value. The principal comes only from
/// operator configuration. Use a signed-token/OIDC adapter for multi-principal use.
pub fn build_trusted_proxy_router(handler: V2NorthboundMcp, config: TrustedProxyConfig) -> Router {
    let mut allowed_hosts = vec![
        "localhost".to_owned(),
        "127.0.0.1".to_owned(),
        "::1".to_owned(),
    ];
    if let Some(host) = config.resource_url.host_str() {
        allowed_hosts.push(host.to_owned());
    }
    let allowed_origins = vec![config.resource_url.origin().ascii_serialization()];
    let http_config = StreamableHttpServerConfig::default()
        .with_legacy_session_mode(false)
        .with_json_response(true)
        .with_allowed_hosts(allowed_hosts)
        .with_allowed_origins(allowed_origins)
        .with_stateless_protocol_metadata_required(true);
    let service = StreamableHttpService::new(
        move || Ok(handler.clone()),
        LocalSessionManager::default().into(),
        http_config,
    );
    let state = TrustedProxyAuthState {
        principal: config.principal.clone(),
        resource_url: config.resource_url.clone(),
    };
    Router::new()
        .nest_service(config.mcp_path(), service)
        .layer(DefaultBodyLimit::max(24 * 1024 * 1024))
        .layer(middleware::from_fn_with_state(state, trusted_proxy_guard))
}

async fn trusted_proxy_guard(
    State(state): State<TrustedProxyAuthState>,
    mut request: Request,
    next: Next,
) -> Response {
    if let Err(error) = validate_exact_browser_origin(request.headers(), &state.resource_url) {
        return error.into_response();
    }
    // Authentication is completed by the reviewed proxy before this loopback
    // listener. Strip known credentials/identity hints so neither rmcp nor the
    // Hub/Agent path can accidentally consume or log them. None of these values
    // participates in principal selection.
    for name in [
        "authorization",
        "cf-access-jwt-assertion",
        "cf-access-authenticated-user-email",
        "cf-access-client-id",
        "cf-access-client-secret",
        "x-user",
        "x-authenticated-user",
    ] {
        request.headers_mut().remove(name);
    }
    request.extensions_mut().insert(NorthboundAuthContext {
        principal: state.principal,
    });
    next.run(request).await
}

pub fn build_northbound_router(
    handler: V2NorthboundMcp,
    config: NorthboundMcpConfig,
    verifier: Arc<dyn AccessTokenVerifier>,
) -> Router {
    let config = Arc::new(config);
    let mut allowed_hosts = vec![
        "localhost".to_owned(),
        "127.0.0.1".to_owned(),
        "::1".to_owned(),
    ];
    if let Some(host) = config.resource_url.host_str() {
        allowed_hosts.push(host.to_owned());
    }
    let allowed_origins = vec![config.resource_url.origin().ascii_serialization()];
    let http_config = StreamableHttpServerConfig::default()
        .with_legacy_session_mode(false)
        .with_json_response(true)
        .with_allowed_hosts(allowed_hosts)
        .with_allowed_origins(allowed_origins)
        .with_stateless_protocol_metadata_required(true);
    let service = StreamableHttpService::new(
        move || Ok(handler.clone()),
        LocalSessionManager::default().into(),
        http_config,
    );
    let auth_state = NorthboundAuthState {
        verifier,
        config: config.clone(),
    };
    let protected = Router::new()
        .nest_service(config.mcp_path(), service)
        .layer(DefaultBodyLimit::max(24 * 1024 * 1024))
        .layer(middleware::from_fn_with_state(
            auth_state,
            oauth_resource_guard,
        ));
    let metadata = config.protected_resource_metadata();
    let metadata_path = config.metadata_path().to_owned();

    Router::new()
        .route(
            &metadata_path,
            get(move || {
                let metadata = metadata.clone();
                async move { Json(metadata) }
            }),
        )
        .merge(protected)
}

async fn oauth_resource_guard(
    State(state): State<NorthboundAuthState>,
    mut request: Request,
    next: Next,
) -> Response {
    if let Err(error) = validate_exact_browser_origin(request.headers(), &state.config.resource_url)
    {
        return error.into_response();
    }
    if query_contains_access_token(request.uri().query()) {
        return oauth_error_response(
            StatusCode::BAD_REQUEST,
            None,
            "invalid_request",
            "OAuth access tokens must not be sent in the request URI",
        );
    }
    let token = match bearer_token(request.headers()) {
        Ok(Some(token)) => token,
        Ok(None) => {
            crate::v2_observability::auth_failure(
                crate::v2_observability::AuthFailureReason::InvalidToken,
            );
            let challenge = bearer_challenge(&state.config, None);
            return oauth_error_response(
                StatusCode::UNAUTHORIZED,
                Some(challenge),
                "invalid_token",
                "Bearer access token required",
            );
        }
        Err(()) => {
            return oauth_error_response(
                StatusCode::BAD_REQUEST,
                None,
                "invalid_request",
                "Malformed Authorization header",
            );
        }
    };
    let verified = match state.verifier.verify(token).await {
        Ok(verified) => verified,
        Err(TokenVerificationError::InvalidToken) => {
            crate::v2_observability::auth_failure(
                crate::v2_observability::AuthFailureReason::InvalidToken,
            );
            let challenge = bearer_challenge(&state.config, Some("invalid_token"));
            return oauth_error_response(
                StatusCode::UNAUTHORIZED,
                Some(challenge),
                "invalid_token",
                "Access token is invalid or expired",
            );
        }
        Err(TokenVerificationError::Unavailable | TokenVerificationError::InvalidConfiguration) => {
            crate::v2_observability::auth_failure(
                crate::v2_observability::AuthFailureReason::Unavailable,
            );
            warn!(
                event = "v2_northbound_auth_unavailable",
                outcome = "denied",
                error_code = "oauth_introspection_unavailable",
                "OAuth token validation unavailable"
            );
            return oauth_error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                None,
                "temporarily_unavailable",
                "Token validation is temporarily unavailable",
            );
        }
    };

    if !state
        .config
        .required_scopes()
        .iter()
        .all(|scope| verified.scopes.contains(scope))
    {
        crate::v2_observability::auth_failure(
            crate::v2_observability::AuthFailureReason::InsufficientScope,
        );
        warn!(
            event = "v2_northbound_auth_denied",
            outcome = "denied",
            error_code = "insufficient_scope",
            "OAuth token lacks required resource scope"
        );
        let challenge = bearer_challenge(&state.config, Some("insufficient_scope"));
        return oauth_error_response(
            StatusCode::FORBIDDEN,
            Some(challenge),
            "insufficient_scope",
            "Access token does not contain the required MCP resource scope",
        );
    }

    // The bearer token has served its only purpose at the resource-server boundary.
    // Strip it before rmcp captures HTTP Parts into RequestContext extensions so no
    // downstream handler (and therefore no Hub/Agent command path) can observe it.
    request.headers_mut().remove(AUTHORIZATION);
    request.extensions_mut().insert(NorthboundAuthContext {
        principal: verified.principal,
    });
    next.run(request).await
}

fn query_contains_access_token(query: Option<&str>) -> bool {
    query.is_some_and(|query| {
        query
            .split('&')
            .filter_map(|field| field.split_once('=').map(|(key, _)| key).or(Some(field)))
            .any(|key| key == "access_token")
    })
}

fn bearer_token(headers: &HeaderMap) -> Result<Option<&str>, ()> {
    let mut values = headers.get_all(AUTHORIZATION).iter();
    let Some(value) = values.next() else {
        return Ok(None);
    };
    if values.next().is_some() {
        return Err(());
    }
    let value = value.to_str().map_err(|_| ())?;
    let mut parts = value.split_ascii_whitespace();
    let scheme = parts.next().ok_or(())?;
    let token = parts.next().ok_or(())?;
    if !scheme.eq_ignore_ascii_case("Bearer")
        || token.is_empty()
        || token.len() > MAX_BEARER_TOKEN_BYTES
        || parts.next().is_some()
    {
        return Err(());
    }
    Ok(Some(token))
}

fn bearer_challenge(config: &NorthboundMcpConfig, error: Option<&str>) -> String {
    let mut challenge = String::from("Bearer");
    if let Some(error) = error {
        challenge.push_str(&format!(" error=\"{error}\","));
    }
    challenge.push_str(&format!(
        " resource_metadata=\"{}\", scope=\"{}\"",
        config.metadata_url(),
        config.required_scopes().join(" ")
    ));
    challenge
}

fn oauth_error_response(
    status: StatusCode,
    challenge: Option<String>,
    error: &'static str,
    description: &'static str,
) -> Response {
    let mut response = (
        status,
        Json(json!({ "error": error, "error_description": description })),
    )
        .into_response();
    if let Some(challenge) = challenge
        && let Ok(value) = HeaderValue::from_str(&challenge)
    {
        response.headers_mut().insert(WWW_AUTHENTICATE, value);
    }
    response
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct NorthboundClientAudit {
    name: String,
    version: String,
    description: Option<String>,
}

impl NorthboundClientAudit {
    fn from_implementation(client: &Implementation) -> Self {
        Self {
            name: bounded_audit_text(&client.name, MAX_AUDIT_CLIENT_NAME_BYTES, "unknown"),
            version: bounded_audit_text(&client.version, MAX_AUDIT_CLIENT_VERSION_BYTES, "unknown"),
            description: client
                .description
                .as_deref()
                .map(|value| bounded_audit_text(value, MAX_AUDIT_CLIENT_DESCRIPTION_BYTES, ""))
                .filter(|value| !value.is_empty()),
        }
    }
}

fn bounded_audit_text(value: &str, max_bytes: usize, fallback: &str) -> String {
    let mut output = String::with_capacity(value.len().min(max_bytes));
    for character in value.chars() {
        let character = if character.is_control() {
            ' '
        } else {
            character
        };
        if output.len() + character.len_utf8() > max_bytes {
            break;
        }
        output.push(character);
    }
    let trimmed = output.trim();
    if trimmed.is_empty() {
        fallback.to_owned()
    } else {
        trimmed.to_owned()
    }
}

fn log_northbound_client_initialized(client: &Implementation) {
    let client = NorthboundClientAudit::from_implementation(client);
    tracing::info!(
        event = "v2_northbound_client_initialized",
        client_name = %client.name,
        client_version = %client.version,
        client_description = client.description.as_deref().unwrap_or("none"),
        identity_source = "mcp_client_info_untrusted",
        "northbound MCP client metadata recorded for audit correlation"
    );
}

fn log_northbound_operation_requested(
    operation_id: &str,
    device_id: &str,
    capability: DeviceCapability,
    client: Option<Implementation>,
) {
    let client = client
        .as_ref()
        .map(NorthboundClientAudit::from_implementation)
        .unwrap_or_else(|| NorthboundClientAudit {
            name: "unknown".into(),
            version: "unknown".into(),
            description: None,
        });
    tracing::info!(
        event = "v2_northbound_operation_requested",
        operation_id,
        device_id,
        capability = crate::v2_observability::capability_name(capability),
        client_name = %client.name,
        client_version = %client.version,
        client_description = client.description.as_deref().unwrap_or("none"),
        identity_source = "mcp_client_info_untrusted",
        outcome = "authorized",
        "authorized northbound operation correlated with caller-supplied MCP client metadata"
    );
}

struct NorthboundInteractionState {
    contexts: InteractionContextManager,
    refs: ScopedBackendRefRegistry,
    browser_refs: BrowserRefRegistry,
}

impl NorthboundInteractionState {
    fn new() -> Self {
        Self {
            contexts: InteractionContextManager::new(InteractionContextLimits::default())
                .expect("static interaction-context limits are valid"),
            refs: ScopedBackendRefRegistry::new(DEFAULT_MAX_REFS_PER_CONTEXT)
                .expect("static scoped-ref limit is valid"),
            browser_refs: BrowserRefRegistry::new(DEFAULT_MAX_BROWSER_REFS_PER_CONTEXT)
                .expect("static browser-ref limit is valid"),
        }
    }

    fn prune_expired(&mut self, now_ms: u64) -> Vec<InteractionContextId> {
        let expired = self.contexts.prune(now_ms);
        for context_id in &expired {
            self.refs.invalidate_context(context_id);
            self.browser_refs.invalidate_context(context_id.as_str());
        }
        expired
    }

    fn fence_live_binding(
        &mut self,
        device_id: &str,
        generation: u64,
        revision: u64,
    ) -> Vec<InteractionContextId> {
        let mut invalidated = self
            .contexts
            .invalidate_device_generation(device_id, generation);
        self.refs
            .invalidate_device_generation(device_id, generation);
        self.browser_refs
            .invalidate_device_generation(device_id, generation);
        invalidated.extend(
            self.contexts
                .invalidate_capability_revision(device_id, revision),
        );
        self.refs
            .invalidate_capability_revision(device_id, revision);
        self.browser_refs
            .invalidate_capability_revision(device_id, revision);
        invalidated
    }
}

struct PreparedBrowserCall {
    binding: InteractionContextBinding,
    command: BrowserBackendCommand,
    public_target_ref: Option<String>,
    public_tab_ref: Option<String>,
    public_dialog_ref: Option<String>,
}

#[derive(Clone)]
pub struct V2NorthboundMcp {
    hub: HubHandle,
    authorizer: Arc<dyn DeviceCapabilityAuthorizer>,
    usage: UsageManager,
    interactions: Arc<TokioMutex<NorthboundInteractionState>>,
}

/// Replacement seam for delegated authorization and generic policy engines.
///
/// Implementations only decide the exact authenticated-principal/device/capability tuple.
/// They do not own CUMG operation identity, desktop ownership, grants, settlement, or
/// quarantine resolution.
pub trait DeviceCapabilityAuthorizer: Send + Sync {
    fn authorize_device_capability(
        &self,
        principal: &AuthenticatedClientPrincipal,
        device_id: &str,
        capability: DeviceCapability,
    ) -> Result<(), TrustError>;
}

impl DeviceCapabilityAuthorizer for ClientAuthorizationPolicy {
    fn authorize_device_capability(
        &self,
        principal: &AuthenticatedClientPrincipal,
        device_id: &str,
        capability: DeviceCapability,
    ) -> Result<(), TrustError> {
        ClientAuthorizationPolicy::authorize_device_capability(
            self, principal, device_id, capability,
        )
    }
}

impl V2NorthboundMcp {
    pub fn new(hub: HubHandle, policy: ClientAuthorizationPolicy) -> Self {
        Self::new_with_authorizer_and_usage(hub, Arc::new(policy), UsageManager::noop())
    }

    pub fn new_with_usage(
        hub: HubHandle,
        policy: ClientAuthorizationPolicy,
        usage: UsageManager,
    ) -> Self {
        Self::new_with_authorizer_and_usage(hub, Arc::new(policy), usage)
    }

    pub fn new_with_authorizer(
        hub: HubHandle,
        authorizer: Arc<dyn DeviceCapabilityAuthorizer>,
    ) -> Self {
        Self::new_with_authorizer_and_usage(hub, authorizer, UsageManager::noop())
    }

    pub fn new_with_authorizer_and_usage(
        hub: HubHandle,
        authorizer: Arc<dyn DeviceCapabilityAuthorizer>,
        usage: UsageManager,
    ) -> Self {
        Self {
            hub,
            authorizer,
            usage,
            interactions: Arc::new(TokioMutex::new(NorthboundInteractionState::new())),
        }
    }

    fn auth_context(
        context: &RequestContext<RoleServer>,
    ) -> Result<&NorthboundAuthContext, McpError> {
        let parts = context.extensions.get::<Parts>().ok_or_else(|| {
            McpError::invalid_request("Authenticated HTTP context required", None)
        })?;
        parts
            .extensions
            .get::<NorthboundAuthContext>()
            .ok_or_else(|| McpError::invalid_request("Authenticated principal required", None))
    }

    fn authorize(
        &self,
        principal: &AuthenticatedClientPrincipal,
        capability: DeviceCapability,
    ) -> Result<(), McpError> {
        self.authorizer
            .authorize_device_capability(principal, self.hub.device_id(), capability)
            .map_err(|_| {
                crate::v2_observability::auth_failure(
                    crate::v2_observability::AuthFailureReason::AuthorizationDenied,
                );
                warn!(
                    event = "v2_northbound_auth_denied",
                    capability = crate::v2_observability::capability_name(capability),
                    outcome = "denied",
                    error_code = "capability_not_authorized",
                    "northbound principal is not authorized for device capability"
                );
                McpError::invalid_request("Device capability is not authorized", None)
            })
    }

    fn context_access_allowed(
        &self,
        principal: &AuthenticatedClientPrincipal,
        capabilities: Option<&CapabilityAdvertisement>,
    ) -> bool {
        CONTEXT_ELIGIBLE_CAPABILITIES
            .iter()
            .copied()
            .any(|capability| {
                capability_is_live(capabilities, capability)
                    && self
                        .authorizer
                        .authorize_device_capability(principal, self.hub.device_id(), capability)
                        .is_ok()
            })
    }

    async fn cleanup_backend_sessions(&self, contexts: Vec<InteractionContextId>) {
        for context_id in contexts {
            if let Err(error) = self
                .hub
                .end_backend_interaction_session(context_id.as_str().to_owned())
                .await
            {
                tracing::warn!(
                    event = "v2_backend_session_cleanup_unavailable",
                    outcome = "failed",
                    error_code = error.safe_error_code(),
                    "backend interaction-session cleanup was unavailable without logging context identity"
                );
            }
        }
    }

    async fn open_interaction_context(
        &self,
        principal: &AuthenticatedClientPrincipal,
    ) -> Result<CallToolResponse, McpError> {
        let (generation, capabilities) = self
            .hub
            .current_session_binding()
            .await
            .ok_or_else(|| McpError::invalid_request("Agent is offline", None))?;
        if !self.context_access_allowed(principal, Some(&capabilities)) {
            return Err(McpError::invalid_request(
                "No authorized live Computer Use capability is available",
                None,
            ));
        }
        let now_ms = unix_time_ms()
            .map_err(|_| McpError::internal_error("System clock unavailable", None))?;
        let (binding, invalidated) = {
            let mut state = self.interactions.lock().await;
            let mut invalidated = state.prune_expired(now_ms);
            invalidated.extend(state.fence_live_binding(
                self.hub.device_id(),
                generation,
                capabilities.revision,
            ));
            let binding = state
                .contexts
                .open(
                    principal,
                    self.hub.device_id(),
                    generation,
                    capabilities.revision,
                    now_ms,
                )
                .map_err(|_| {
                    McpError::invalid_request("Interaction context could not be opened", None)
                })?;
            (binding, invalidated)
        };
        self.cleanup_backend_sessions(invalidated).await;
        let payload = json!({
            "context_id": binding.id.as_str(),
            "scope": "window_scoped",
            "device_generation": binding.device_generation,
            "capability_revision": binding.capability_revision,
        });
        Ok(CallToolResult::success(vec![ContentBlock::text(payload.to_string())]).into())
    }

    async fn close_interaction_context(
        &self,
        principal: &AuthenticatedClientPrincipal,
        context_id: &str,
    ) -> Result<CallToolResponse, McpError> {
        let id = InteractionContextId::parse(context_id)
            .map_err(|_| McpError::invalid_params("Invalid interaction context id", None))?;
        {
            let mut state = self.interactions.lock().await;
            state
                .contexts
                .close(&id, principal, self.hub.device_id())
                .map_err(|_| {
                    McpError::invalid_request(
                        "Interaction context is not owned by this principal/device",
                        None,
                    )
                })?;
            state.refs.invalidate_context(&id);
            state.browser_refs.invalidate_context(id.as_str());
        }
        let backend_session_ended = match self
            .hub
            .end_backend_interaction_session(id.as_str().to_owned())
            .await
        {
            Ok(ended) => ended,
            Err(error) => {
                tracing::warn!(
                    event = "v2_backend_session_cleanup_unavailable",
                    outcome = "failed",
                    error_code = error.safe_error_code(),
                    "backend interaction-session cleanup was unavailable without logging context identity"
                );
                false
            }
        };
        Ok(CallToolResult::success(vec![ContentBlock::text(
            json!({
                "closed": true,
                "backend_session_ended": backend_session_ended,
            })
            .to_string(),
        )])
        .into())
    }

    async fn validate_interaction_context(
        &self,
        principal: &AuthenticatedClientPrincipal,
        context_id: &str,
    ) -> Result<InteractionContextBinding, McpError> {
        let id = InteractionContextId::parse(context_id)
            .map_err(|_| McpError::invalid_params("Invalid interaction context id", None))?;
        let (generation, capabilities) = self
            .hub
            .current_session_binding()
            .await
            .ok_or_else(|| McpError::invalid_request("Agent is offline", None))?;
        let now_ms = unix_time_ms()
            .map_err(|_| McpError::internal_error("System clock unavailable", None))?;
        let (result, invalidated) = {
            let mut state = self.interactions.lock().await;
            let mut invalidated = state.prune_expired(now_ms);
            invalidated.extend(state.fence_live_binding(
                self.hub.device_id(),
                generation,
                capabilities.revision,
            ));
            let result = state.contexts.validate_and_touch(
                &id,
                principal,
                self.hub.device_id(),
                generation,
                capabilities.revision,
                now_ms,
            );
            (result, invalidated)
        };
        self.cleanup_backend_sessions(invalidated).await;
        result.map_err(|_| {
            McpError::invalid_request("Interaction context is invalid, stale, or expired", None)
        })
    }

    async fn validate_browser_interaction_context(
        &self,
        principal: &AuthenticatedClientPrincipal,
        context_id: &str,
    ) -> Result<InteractionContextBinding, McpError> {
        let binding = self
            .validate_interaction_context(principal, context_id)
            .await?;
        require_browser_window_scope(binding)
    }

    async fn prepare_contextual_command(
        &self,
        principal: &AuthenticatedClientPrincipal,
        mut command: DeviceCommand,
    ) -> Result<(DeviceCommand, Option<InteractionContextBinding>), McpError> {
        if matches!(
            command,
            DeviceCommand::Screenshot
                | DeviceCommand::PointerClick { .. }
                | DeviceCommand::PointerDrag { .. }
                | DeviceCommand::TypeText { .. }
        ) {
            return Err(McpError::invalid_params(
                "Desktop-scoped northbound input requires an interaction context",
                None,
            ));
        }
        let context_id = command_interaction_context_id(&command).map(ToOwned::to_owned);
        let Some(context_id) = context_id else {
            return Ok((command, None));
        };
        let binding = self
            .validate_interaction_context(principal, &context_id)
            .await?;
        if command_requires_desktop_scope(&command)
            && binding.scope != InteractionScope::DesktopScoped
        {
            return Err(McpError::invalid_request(
                "Interaction context has not been explicitly expanded to desktop scope",
                None,
            ));
        }
        if command_requires_window_scope(&command)
            && binding.scope != InteractionScope::WindowScoped
        {
            return Err(McpError::invalid_request(
                "Window-scoped interaction is unavailable after desktop scope expansion; close the context and open a fresh one",
                None,
            ));
        }
        if let Some(element_ref) = command_scoped_ui_element_ref_mut(&mut command) {
            let backend_ref = {
                let state = self.interactions.lock().await;
                state
                    .refs
                    .resolve(element_ref, &binding, ScopedRefKind::Element)
                    .map(str::to_owned)
                    .map_err(|_| {
                        McpError::invalid_request(
                            "UI element ref is stale or belongs to another context",
                            None,
                        )
                    })?
            };
            *element_ref = backend_ref;
        }
        Ok((command, Some(binding)))
    }

    async fn publicize_window_snapshot(
        &self,
        binding: &InteractionContextBinding,
        result: DeviceResult,
    ) -> Result<DeviceResult, McpError> {
        let DeviceResult::WindowSnapshot {
            snapshot_ref,
            process_id,
            window_id,
            mut elements,
            elements_complete,
            screenshot,
        } = result
        else {
            return Ok(result);
        };
        let mut state = self.interactions.lock().await;
        let public_snapshot = state
            .refs
            .mint(binding, ScopedRefKind::Snapshot, &snapshot_ref)
            .map_err(|_| McpError::internal_error("Scoped snapshot ref limit exceeded", None))?;
        let mut remap = BTreeMap::new();
        for element in &elements {
            let public = state
                .refs
                .mint(binding, ScopedRefKind::Element, &element.element_ref)
                .map_err(|_| McpError::internal_error("Scoped element ref limit exceeded", None))?;
            remap.insert(element.element_ref.clone(), public);
        }
        for element in &mut elements {
            let raw = element.element_ref.clone();
            element.element_ref = remap.get(&raw).cloned().ok_or_else(|| {
                McpError::internal_error("Scoped element ref rewrite failed", None)
            })?;
            element.parent_ref = element
                .parent_ref
                .as_ref()
                .and_then(|raw_parent| remap.get(raw_parent).cloned());
        }
        Ok(DeviceResult::WindowSnapshot {
            snapshot_ref: public_snapshot,
            process_id,
            window_id,
            elements,
            elements_complete,
            screenshot,
        })
    }

    async fn prepare_browser_call(
        &self,
        principal: &AuthenticatedClientPrincipal,
        tool_name: &str,
        arguments: Option<JsonObject>,
    ) -> Result<PreparedBrowserCall, McpError> {
        match tool_name {
            TOOL_BROWSER_PREPARE => {
                let request: BrowserPrepareRequest = parse_arguments(arguments)?;
                request.validate().map_err(browser_contract_error_to_mcp)?;
                let binding = self
                    .validate_browser_interaction_context(principal, &request.context_id)
                    .await?;
                Ok(PreparedBrowserCall {
                    binding,
                    command: BrowserBackendCommand::Prepare {
                        context_id: request.context_id,
                        process_id: request.process_id,
                        window_id: request.window_id,
                        allow_launch: request.allow_launch,
                        profile_mode: request.profile_mode,
                        profile_name: request.profile_name,
                    },
                    public_target_ref: None,
                    public_tab_ref: None,
                    public_dialog_ref: None,
                })
            }
            TOOL_BROWSER_BIND => {
                let request: BrowserBindRequest = parse_arguments(arguments)?;
                request.validate().map_err(browser_contract_error_to_mcp)?;
                let binding = self
                    .validate_browser_interaction_context(principal, &request.context_id)
                    .await?;
                Ok(PreparedBrowserCall {
                    binding,
                    command: BrowserBackendCommand::Bind {
                        context_id: request.context_id,
                        process_id: request.process_id,
                        window_id: request.window_id,
                    },
                    public_target_ref: None,
                    public_tab_ref: None,
                    public_dialog_ref: None,
                })
            }
            TOOL_BROWSER_INSPECT => {
                let request: BrowserInspectRequest = parse_arguments(arguments)?;
                request.validate().map_err(browser_contract_error_to_mcp)?;
                let binding = self
                    .validate_browser_interaction_context(principal, &request.context_id)
                    .await?;
                let (resolved, backend_scope_ref, backend_continuation) = {
                    let mut state = self.interactions.lock().await;
                    let resolved = state
                        .browser_refs
                        .resolve_target_tab(&binding, &request.target_ref, &request.tab_ref)
                        .map_err(browser_ref_error_to_mcp)?;
                    let backend_scope_ref = request
                        .scope_ref
                        .as_deref()
                        .map(|scope_ref| {
                            state
                                .browser_refs
                                .resolve_scope_ref(
                                    &binding,
                                    &request.target_ref,
                                    &request.tab_ref,
                                    scope_ref,
                                )
                                .map(|resolved| resolved.backend_ref)
                                .map_err(browser_ref_error_to_mcp)
                        })
                        .transpose()?;
                    let backend_continuation = request
                        .continuation_ref
                        .as_deref()
                        .map(|continuation_ref| {
                            state
                                .browser_refs
                                .consume_continuation(
                                    &binding,
                                    &request.target_ref,
                                    &request.tab_ref,
                                    continuation_ref,
                                )
                                .map_err(browser_ref_error_to_mcp)
                        })
                        .transpose()?;
                    (resolved, backend_scope_ref, backend_continuation)
                };
                Ok(PreparedBrowserCall {
                    binding,
                    command: BrowserBackendCommand::Inspect {
                        context_id: request.context_id,
                        backend_target_id: resolved.backend_target,
                        backend_tab_id: resolved.backend_tab,
                        backend_scope_ref,
                        query: request.query,
                        backend_continuation,
                        include_screenshot: request.include_screenshot,
                    },
                    public_target_ref: Some(request.target_ref),
                    public_tab_ref: Some(request.tab_ref),
                    public_dialog_ref: None,
                })
            }
            TOOL_BROWSER_NAVIGATE => {
                let request: BrowserNavigateRequest = parse_arguments(arguments)?;
                request.validate().map_err(browser_contract_error_to_mcp)?;
                let binding = self
                    .validate_browser_interaction_context(principal, &request.context_id)
                    .await?;
                let resolved = self
                    .interactions
                    .lock()
                    .await
                    .browser_refs
                    .resolve_target_tab(&binding, &request.target_ref, &request.tab_ref)
                    .map_err(browser_ref_error_to_mcp)?;
                Ok(PreparedBrowserCall {
                    binding,
                    command: BrowserBackendCommand::Navigate {
                        context_id: request.context_id,
                        backend_target_id: resolved.backend_target,
                        backend_tab_id: resolved.backend_tab,
                        url: request.url,
                    },
                    public_target_ref: Some(request.target_ref),
                    public_tab_ref: Some(request.tab_ref),
                    public_dialog_ref: None,
                })
            }
            TOOL_BROWSER_CLICK => {
                let request: BrowserClickRequest = parse_arguments(arguments)?;
                request.validate().map_err(browser_contract_error_to_mcp)?;
                let binding = self
                    .validate_browser_interaction_context(principal, &request.context_id)
                    .await?;
                let (resolved, target) = {
                    let state = self.interactions.lock().await;
                    let resolved = state
                        .browser_refs
                        .resolve_target_tab(&binding, &request.target_ref, &request.tab_ref)
                        .map_err(browser_ref_error_to_mcp)?;
                    let target = match &request.target {
                        BrowserClickTarget::Element { element_ref } => {
                            let element = state
                                .browser_refs
                                .resolve_action(
                                    &binding,
                                    &request.target_ref,
                                    &request.tab_ref,
                                    element_ref,
                                    BrowserAction::Click,
                                )
                                .map_err(browser_ref_error_to_mcp)?;
                            BrowserBackendClickTarget::Element {
                                backend_element_ref: element.backend_ref,
                            }
                        }
                        BrowserClickTarget::ViewportCss { x, y } => {
                            BrowserBackendClickTarget::ViewportCss { x: *x, y: *y }
                        }
                    };
                    (resolved, target)
                };
                Ok(PreparedBrowserCall {
                    binding,
                    command: BrowserBackendCommand::Click {
                        context_id: request.context_id,
                        backend_target_id: resolved.backend_target,
                        backend_tab_id: resolved.backend_tab,
                        target,
                        input_route: request.input_route,
                    },
                    public_target_ref: Some(request.target_ref),
                    public_tab_ref: Some(request.tab_ref),
                    public_dialog_ref: None,
                })
            }
            TOOL_BROWSER_TYPE => {
                let request: BrowserTypeRequest = parse_arguments(arguments)?;
                request.validate().map_err(browser_contract_error_to_mcp)?;
                let binding = self
                    .validate_browser_interaction_context(principal, &request.context_id)
                    .await?;
                let (resolved, element) = {
                    let state = self.interactions.lock().await;
                    let resolved = state
                        .browser_refs
                        .resolve_target_tab(&binding, &request.target_ref, &request.tab_ref)
                        .map_err(browser_ref_error_to_mcp)?;
                    let element = state
                        .browser_refs
                        .resolve_action(
                            &binding,
                            &request.target_ref,
                            &request.tab_ref,
                            &request.element_ref,
                            BrowserAction::Type,
                        )
                        .map_err(browser_ref_error_to_mcp)?;
                    (resolved, element)
                };
                Ok(PreparedBrowserCall {
                    binding,
                    command: BrowserBackendCommand::Type {
                        context_id: request.context_id,
                        backend_target_id: resolved.backend_target,
                        backend_tab_id: resolved.backend_tab,
                        backend_element_ref: element.backend_ref,
                        text: request.text,
                        mode: request.mode,
                        replace: request.replace,
                    },
                    public_target_ref: Some(request.target_ref),
                    public_tab_ref: Some(request.tab_ref),
                    public_dialog_ref: None,
                })
            }
            TOOL_BROWSER_DIALOG => {
                let request: BrowserDialogRequest = parse_arguments(arguments)?;
                request.validate().map_err(browser_contract_error_to_mcp)?;
                let binding = self
                    .validate_browser_interaction_context(principal, &request.context_id)
                    .await?;
                let (resolved, backend_dialog_id) = {
                    let state = self.interactions.lock().await;
                    let resolved = state
                        .browser_refs
                        .resolve_target_tab(&binding, &request.target_ref, &request.tab_ref)
                        .map_err(browser_ref_error_to_mcp)?;
                    let backend_dialog_id = request
                        .dialog_ref
                        .as_deref()
                        .map(|dialog_ref| {
                            state
                                .browser_refs
                                .resolve_dialog(
                                    &binding,
                                    &request.target_ref,
                                    &request.tab_ref,
                                    dialog_ref,
                                )
                                .map(|resolved| resolved.backend_ref)
                                .map_err(browser_ref_error_to_mcp)
                        })
                        .transpose()?;
                    (resolved, backend_dialog_id)
                };
                Ok(PreparedBrowserCall {
                    binding,
                    command: BrowserBackendCommand::Dialog {
                        context_id: request.context_id,
                        backend_target_id: resolved.backend_target,
                        backend_tab_id: resolved.backend_tab,
                        backend_dialog_id,
                        action: request.action,
                        prompt_text: request.prompt_text,
                        delivery: request.delivery,
                    },
                    public_target_ref: Some(request.target_ref),
                    public_tab_ref: Some(request.tab_ref),
                    public_dialog_ref: request.dialog_ref,
                })
            }
            TOOL_BROWSER_POINTER => {
                let request: BrowserPointerRequest = parse_arguments(arguments)?;
                request.validate().map_err(browser_contract_error_to_mcp)?;
                let binding = self
                    .validate_browser_interaction_context(principal, &request.context_id)
                    .await?;
                let (resolved, element, destination) = {
                    let state = self.interactions.lock().await;
                    let resolved = state
                        .browser_refs
                        .resolve_target_tab(&binding, &request.target_ref, &request.tab_ref)
                        .map_err(browser_ref_error_to_mcp)?;
                    let allowed: &[BrowserAction] =
                        if request.action == BrowserPointerAction::Scroll {
                            &[BrowserAction::Scroll, BrowserAction::Pointer]
                        } else {
                            &[BrowserAction::Pointer]
                        };
                    let element = state
                        .browser_refs
                        .resolve_any_action(
                            &binding,
                            &request.target_ref,
                            &request.tab_ref,
                            &request.element_ref,
                            allowed,
                        )
                        .map_err(browser_ref_error_to_mcp)?;
                    let destination = request
                        .destination_ref
                        .as_deref()
                        .map(|destination_ref| {
                            state
                                .browser_refs
                                .resolve_action(
                                    &binding,
                                    &request.target_ref,
                                    &request.tab_ref,
                                    destination_ref,
                                    BrowserAction::Pointer,
                                )
                                .map(|resolved| resolved.backend_ref)
                                .map_err(browser_ref_error_to_mcp)
                        })
                        .transpose()?;
                    (resolved, element, destination)
                };
                Ok(PreparedBrowserCall {
                    binding,
                    command: BrowserBackendCommand::Pointer {
                        context_id: request.context_id,
                        backend_target_id: resolved.backend_target,
                        backend_tab_id: resolved.backend_tab,
                        backend_element_ref: element.backend_ref,
                        action: request.action,
                        backend_destination_ref: destination,
                        delta_x: request.delta_x,
                        delta_y: request.delta_y,
                        input_route: request.input_route,
                    },
                    public_target_ref: Some(request.target_ref),
                    public_tab_ref: Some(request.tab_ref),
                    public_dialog_ref: None,
                })
            }
            TOOL_BROWSER_UPLOAD_FILE => {
                let request: BrowserUploadRequest = parse_arguments(arguments)?;
                request.validate().map_err(browser_contract_error_to_mcp)?;
                let binding = self
                    .validate_browser_interaction_context(principal, &request.context_id)
                    .await?;
                let (resolved, element, staged_files) = {
                    let mut state = self.interactions.lock().await;
                    let resolved = state
                        .browser_refs
                        .resolve_target_tab(&binding, &request.target_ref, &request.tab_ref)
                        .map_err(browser_ref_error_to_mcp)?;
                    let element = state
                        .browser_refs
                        .resolve_action(
                            &binding,
                            &request.target_ref,
                            &request.tab_ref,
                            &request.element_ref,
                            BrowserAction::Upload,
                        )
                        .map_err(browser_ref_error_to_mcp)?;
                    let handles = state
                        .refs
                        .consume_many(&request.file_refs, &binding, ScopedRefKind::UploadFile)
                        .map_err(scoped_ref_error_to_mcp)?;
                    let files = handles
                        .into_iter()
                        .map(|backend_file_handle| BrowserStagedUploadFile {
                            backend_file_handle,
                        })
                        .collect();
                    (resolved, element, files)
                };
                Ok(PreparedBrowserCall {
                    binding,
                    command: BrowserBackendCommand::Upload {
                        context_id: request.context_id,
                        backend_target_id: resolved.backend_target,
                        backend_tab_id: resolved.backend_tab,
                        backend_element_ref: element.backend_ref,
                        staged_files,
                    },
                    public_target_ref: Some(request.target_ref),
                    public_tab_ref: Some(request.tab_ref),
                    public_dialog_ref: None,
                })
            }
            TOOL_BROWSER_DOWNLOAD => {
                let request: BrowserDownloadRequest = parse_arguments(arguments)?;
                request.validate().map_err(browser_contract_error_to_mcp)?;
                let binding = self
                    .validate_browser_interaction_context(principal, &request.context_id)
                    .await?;
                let (resolved, element) = {
                    let state = self.interactions.lock().await;
                    let resolved = state
                        .browser_refs
                        .resolve_target_tab(&binding, &request.target_ref, &request.tab_ref)
                        .map_err(browser_ref_error_to_mcp)?;
                    // Cua's current download primitive activates an exact clickable ref and
                    // independently proves that a download begins/completes. It does not mint
                    // a separate semantic "download" action in snapshots.
                    let element = state
                        .browser_refs
                        .resolve_action(
                            &binding,
                            &request.target_ref,
                            &request.tab_ref,
                            &request.element_ref,
                            BrowserAction::Click,
                        )
                        .map_err(browser_ref_error_to_mcp)?;
                    (resolved, element)
                };
                Ok(PreparedBrowserCall {
                    binding,
                    command: BrowserBackendCommand::Download {
                        context_id: request.context_id,
                        backend_target_id: resolved.backend_target,
                        backend_tab_id: resolved.backend_tab,
                        backend_element_ref: element.backend_ref,
                        destination_name: request.destination_name,
                        max_bytes: request.max_bytes,
                        overwrite: request.overwrite,
                    },
                    public_target_ref: Some(request.target_ref),
                    public_tab_ref: Some(request.tab_ref),
                    public_dialog_ref: None,
                })
            }
            _ => Err(McpError::invalid_params("Unknown V2 browser tool", None)),
        }
    }

    async fn call_browser_stage_upload(
        &self,
        principal: &AuthenticatedClientPrincipal,
        arguments: Option<JsonObject>,
        operation_id: String,
        usage: UsageLease,
        context: &RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        let request: BrowserStageUploadRequest = match parse_arguments(arguments) {
            Ok(request) => request,
            Err(error) => {
                settle_usage_best_effort(
                    &usage,
                    UsageSettlement::Zero,
                    "invalid_browser_upload_stage",
                )
                .await;
                return Err(error);
            }
        };
        let expected_bytes = match request.validate() {
            Ok(bytes) => bytes,
            Err(error) => {
                settle_usage_best_effort(
                    &usage,
                    UsageSettlement::Zero,
                    "invalid_browser_upload_stage",
                )
                .await;
                return Err(browser_contract_error_to_mcp(error));
            }
        };
        let binding = match self
            .validate_browser_interaction_context(principal, &request.context_id)
            .await
        {
            Ok(binding) => binding,
            Err(error) => {
                settle_usage_best_effort(
                    &usage,
                    UsageSettlement::Zero,
                    "invalid_browser_upload_context",
                )
                .await;
                return Err(error);
            }
        };
        let execution = self
            .execute_command(
                principal,
                operation_id,
                DeviceCommand::StageBrowserUploadFile {
                    context_id: request.context_id,
                    file_name: request.file_name,
                    data_base64: BrowserUploadPayload::after_contract_validation(
                        request.data_base64,
                    ),
                    expected_bytes: expected_bytes as u64,
                },
                usage,
                context,
            )
            .await;
        let result = match execution {
            Ok(result) => result,
            Err(error) => return Ok(execution_error_response(error)),
        };
        let DeviceResult::BrowserUploadStaged {
            backend_file_handle,
            bytes,
        } = result
        else {
            return Err(McpError::internal_error(
                "Browser upload staging result mismatch",
                None,
            ));
        };
        if bytes != expected_bytes as u64 {
            return Err(McpError::internal_error(
                "Browser upload staging byte count mismatch",
                None,
            ));
        }
        let file_ref = self
            .interactions
            .lock()
            .await
            .refs
            .mint(&binding, ScopedRefKind::UploadFile, &backend_file_handle)
            .map_err(scoped_ref_mint_error_to_mcp)?;
        browser_json_response(
            serde_json::to_value(BrowserStagedUploadResult { file_ref, bytes }).map_err(|_| {
                McpError::internal_error("Browser upload staging response failed", None)
            })?,
        )
    }

    async fn call_browser_tool(
        &self,
        principal: &AuthenticatedClientPrincipal,
        tool_name: &str,
        arguments: Option<JsonObject>,
        operation_id: String,
        usage: UsageLease,
        context: &RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        let prepared = match self
            .prepare_browser_call(principal, tool_name, arguments)
            .await
        {
            Ok(prepared) => prepared,
            Err(error) => {
                settle_usage_best_effort(&usage, UsageSettlement::Zero, "invalid_browser_request")
                    .await;
                return Err(error);
            }
        };
        let command = DeviceCommand::Browser {
            command: prepared.command.clone(),
        };
        let result = match self
            .execute_command(principal, operation_id, command, usage, context)
            .await
        {
            Ok(result) => result,
            Err(error) => return Ok(execution_error_response(error)),
        };
        self.publicize_browser_result(prepared, result).await
    }

    async fn publicize_browser_result(
        &self,
        prepared: PreparedBrowserCall,
        result: DeviceResult,
    ) -> Result<CallToolResponse, McpError> {
        let DeviceResult::Browser { result } = result else {
            return Err(McpError::internal_error(
                "Browser result did not match the requested semantic",
                None,
            ));
        };
        if !result.matches_command(&prepared.command) {
            return Err(McpError::internal_error(
                "Browser result did not match the requested semantic",
                None,
            ));
        }
        match (result, &prepared.command) {
            (
                BrowserBackendResult::Prepared {
                    prepared,
                    prepared_process_id,
                    side_effect_count,
                },
                BrowserBackendCommand::Prepare { .. },
            ) => browser_json_response(json!({
                "type": "browser_prepared",
                "prepared": prepared,
                "prepared_process_id": prepared_process_id,
                "side_effect_count": side_effect_count,
            })),
            (
                BrowserBackendResult::Bound {
                    backend_target_id,
                    process_id,
                    window_id,
                    tabs,
                },
                BrowserBackendCommand::Bind { .. },
            ) => {
                let mut state = self.interactions.lock().await;
                let mut minted = Vec::with_capacity(tabs.len() + 1);
                let target_ref = match state
                    .browser_refs
                    .mint_target(&prepared.binding, &backend_target_id)
                {
                    Ok(reference) => {
                        minted.push(reference.clone());
                        reference
                    }
                    Err(error) => return Err(browser_ref_mint_error_to_mcp(error)),
                };
                let mut public_tabs = Vec::with_capacity(tabs.len());
                for tab in tabs {
                    match state.browser_refs.mint_tab(
                        &prepared.binding,
                        &target_ref,
                        &tab.backend_tab_id,
                    ) {
                        Ok(tab_ref) => {
                            minted.push(tab_ref.clone());
                            public_tabs.push(BrowserTabSummary {
                                tab_ref,
                                title: tab.title,
                                url: tab.url,
                                active: tab.active,
                            });
                        }
                        Err(error) => {
                            state.browser_refs.discard_refs(&minted);
                            return Err(browser_ref_mint_error_to_mcp(error));
                        }
                    }
                }
                browser_json_response(
                    serde_json::to_value(BrowserBindingResult {
                        target_ref,
                        process_id,
                        window_id,
                        exact: true,
                        mutation_allowed: true,
                        tabs: public_tabs,
                    })
                    .map_err(|_| {
                        McpError::internal_error("Browser binding serialization failed", None)
                    })?,
                )
            }
            (
                BrowserBackendResult::Snapshot {
                    backend_snapshot_id,
                    outline,
                    action_refs,
                    content_refs,
                    complete,
                    omitted,
                    backend_continuation,
                    screenshot,
                },
                BrowserBackendCommand::Inspect { .. },
            ) => {
                let target_ref = prepared
                    .public_target_ref
                    .as_deref()
                    .ok_or_else(browser_public_ref_internal_error)?;
                let tab_ref = prepared
                    .public_tab_ref
                    .as_deref()
                    .ok_or_else(browser_public_ref_internal_error)?;
                let mut state = self.interactions.lock().await;
                let mut minted = Vec::with_capacity(action_refs.len() + content_refs.len() + 2);
                let snapshot_ref = state
                    .browser_refs
                    .begin_snapshot(&prepared.binding, target_ref, tab_ref, &backend_snapshot_id)
                    .map_err(browser_ref_mint_error_to_mcp)?;
                minted.push(snapshot_ref.clone());
                let mut public_actions = Vec::with_capacity(action_refs.len());
                for reference in action_refs {
                    let public_ref = match state.browser_refs.mint_action_element(
                        &prepared.binding,
                        target_ref,
                        tab_ref,
                        &snapshot_ref,
                        &reference.backend_ref,
                        &reference.actions,
                    ) {
                        Ok(reference) => reference,
                        Err(error) => {
                            state.browser_refs.discard_refs(&minted);
                            return Err(browser_ref_mint_error_to_mcp(error));
                        }
                    };
                    minted.push(public_ref.clone());
                    public_actions.push(publicize_browser_semantic_ref(reference, public_ref));
                }
                let mut public_content = Vec::with_capacity(content_refs.len());
                for reference in content_refs {
                    let public_ref = match state.browser_refs.mint_content_element(
                        &prepared.binding,
                        target_ref,
                        tab_ref,
                        &snapshot_ref,
                        &reference.backend_ref,
                    ) {
                        Ok(reference) => reference,
                        Err(error) => {
                            state.browser_refs.discard_refs(&minted);
                            return Err(browser_ref_mint_error_to_mcp(error));
                        }
                    };
                    minted.push(public_ref.clone());
                    public_content.push(publicize_browser_semantic_ref(reference, public_ref));
                }
                let continuation_ref = if let Some(backend_continuation) = backend_continuation {
                    match state.browser_refs.mint_continuation(
                        &prepared.binding,
                        target_ref,
                        tab_ref,
                        &snapshot_ref,
                        &backend_continuation,
                    ) {
                        Ok(reference) => {
                            minted.push(reference.clone());
                            Some(reference)
                        }
                        Err(error) => {
                            state.browser_refs.discard_refs(&minted);
                            return Err(browser_ref_mint_error_to_mcp(error));
                        }
                    }
                } else {
                    None
                };
                let snapshot = BrowserSnapshotResult {
                    snapshot_ref,
                    outline,
                    action_refs: public_actions,
                    content_refs: public_content,
                    complete,
                    omitted,
                    continuation_ref,
                    screenshot_base64: screenshot.as_ref().map(|image| image.data_base64.clone()),
                    screenshot_width: screenshot.as_ref().map(|image| image.width_pixels),
                    screenshot_height: screenshot.as_ref().map(|image| image.height_pixels),
                    viewport_css_width: screenshot.as_ref().map(|image| image.viewport_css_width),
                    viewport_css_height: screenshot.as_ref().map(|image| image.viewport_css_height),
                };
                drop(state);
                let value = serde_json::to_value(snapshot).map_err(|_| {
                    McpError::internal_error("Browser snapshot serialization failed", None)
                })?;
                if let Some(image) = screenshot {
                    let mut metadata = value;
                    if let Some(object) = metadata.as_object_mut() {
                        object.insert("screenshot_base64".into(), Value::Null);
                        object.insert("screenshot_mime_type".into(), json!(image.mime_type));
                        object.insert(
                            "pixel_to_css_scale_x_millionths".into(),
                            json!(image.pixel_to_css_scale_x_millionths),
                        );
                        object.insert(
                            "pixel_to_css_scale_y_millionths".into(),
                            json!(image.pixel_to_css_scale_y_millionths),
                        );
                    }
                    Ok(CallToolResult::success(vec![
                        ContentBlock::image(image.data_base64, "image/png"),
                        ContentBlock::text(metadata.to_string()),
                    ])
                    .into())
                } else {
                    browser_json_response(value)
                }
            }
            (BrowserBackendResult::NavigationCompleted, BrowserBackendCommand::Navigate { .. }) => {
                self.invalidate_browser_document(&prepared).await?;
                browser_json_response(json!({
                    "type": "browser_navigation_completed",
                    "completed": true,
                    "fresh_snapshot_required": true,
                }))
            }
            (
                BrowserBackendResult::ClickCompleted { effect },
                BrowserBackendCommand::Click { .. },
            ) => browser_json_response(json!({
                "type": "browser_click_completed",
                "completed": true,
                "effect": effect,
                "verification_required": true,
            })),
            (BrowserBackendResult::TypeCompleted, BrowserBackendCommand::Type { .. }) => {
                browser_json_response(json!({
                    "type": "browser_type_completed",
                    "completed": true,
                    "verification_required": true,
                }))
            }
            (
                BrowserBackendResult::DialogObserved {
                    present,
                    backend_dialog_id,
                    kind,
                },
                BrowserBackendCommand::Dialog {
                    action: BrowserDialogAction::Inspect,
                    ..
                },
            ) => {
                let target_ref = prepared
                    .public_target_ref
                    .as_deref()
                    .ok_or_else(browser_public_ref_internal_error)?;
                let tab_ref = prepared
                    .public_tab_ref
                    .as_deref()
                    .ok_or_else(browser_public_ref_internal_error)?;
                let dialog_ref = if present {
                    let backend_dialog_id = backend_dialog_id.as_deref().ok_or_else(|| {
                        McpError::internal_error("Browser dialog identity was missing", None)
                    })?;
                    Some(
                        self.interactions
                            .lock()
                            .await
                            .browser_refs
                            .mint_dialog(&prepared.binding, target_ref, tab_ref, backend_dialog_id)
                            .map_err(browser_ref_mint_error_to_mcp)?,
                    )
                } else {
                    None
                };
                browser_json_response(
                    serde_json::to_value(BrowserDialogResult {
                        present,
                        dialog_ref,
                        kind,
                    })
                    .map_err(|_| {
                        McpError::internal_error("Browser dialog serialization failed", None)
                    })?,
                )
            }
            (
                BrowserBackendResult::DialogCompleted,
                BrowserBackendCommand::Dialog {
                    action: BrowserDialogAction::Accept | BrowserDialogAction::Dismiss,
                    ..
                },
            ) => {
                let target_ref = prepared
                    .public_target_ref
                    .as_deref()
                    .ok_or_else(browser_public_ref_internal_error)?;
                let tab_ref = prepared
                    .public_tab_ref
                    .as_deref()
                    .ok_or_else(browser_public_ref_internal_error)?;
                let dialog_ref = prepared
                    .public_dialog_ref
                    .as_deref()
                    .ok_or_else(browser_public_ref_internal_error)?;
                self.interactions
                    .lock()
                    .await
                    .browser_refs
                    .complete_dialog(&prepared.binding, target_ref, tab_ref, dialog_ref)
                    .map_err(browser_ref_error_to_mcp)?;
                browser_json_response(json!({
                    "type": "browser_dialog_completed",
                    "completed": true,
                }))
            }
            (BrowserBackendResult::PointerCompleted, BrowserBackendCommand::Pointer { .. }) => {
                browser_json_response(json!({
                    "type": "browser_pointer_completed",
                    "completed": true,
                    "verification_required": true,
                }))
            }
            (
                BrowserBackendResult::UploadAssigned { file_count },
                BrowserBackendCommand::Upload { .. },
            ) => browser_json_response(json!({
                "type": "browser_upload_completed",
                "completed": true,
                "file_count": file_count,
                "verification_required": true,
            })),
            (
                BrowserBackendResult::DownloadCompleted {
                    backend_download_handle,
                    destination_name,
                    bytes_written,
                    data_base64,
                },
                BrowserBackendCommand::Download {
                    destination_name: requested_name,
                    max_bytes,
                    ..
                },
            ) => {
                let decoded_bytes = STANDARD.decode(data_base64.as_bytes()).map_err(|_| {
                    McpError::internal_error(
                        "Browser download result violated the bounded contract",
                        None,
                    )
                })?;
                if destination_name != *requested_name
                    || bytes_written > *max_bytes
                    || bytes_written > MAX_BROWSER_DOWNLOAD_BYTES
                    || data_base64.len() > MAX_BROWSER_DOWNLOAD_BASE64_BYTES
                    || u64::try_from(decoded_bytes.len()).ok() != Some(bytes_written)
                {
                    return Err(McpError::internal_error(
                        "Browser download result violated the bounded contract",
                        None,
                    ));
                }
                let download_ref = self
                    .interactions
                    .lock()
                    .await
                    .refs
                    .mint(
                        &prepared.binding,
                        ScopedRefKind::DownloadFile,
                        &backend_download_handle,
                    )
                    .map_err(scoped_ref_mint_error_to_mcp)?;
                browser_json_response(
                    serde_json::to_value(BrowserDownloadResult {
                        download_ref,
                        destination_name,
                        bytes_written,
                        data_base64,
                    })
                    .map_err(|_| {
                        McpError::internal_error("Browser download serialization failed", None)
                    })?,
                )
            }
            _ => Err(McpError::internal_error(
                "Browser result did not match the requested semantic",
                None,
            )),
        }
    }

    async fn invalidate_browser_document(
        &self,
        prepared: &PreparedBrowserCall,
    ) -> Result<(), McpError> {
        let target_ref = prepared
            .public_target_ref
            .as_deref()
            .ok_or_else(browser_public_ref_internal_error)?;
        let tab_ref = prepared
            .public_tab_ref
            .as_deref()
            .ok_or_else(browser_public_ref_internal_error)?;
        self.interactions
            .lock()
            .await
            .browser_refs
            .invalidate_tab_document(&prepared.binding, target_ref, tab_ref)
            .map_err(browser_ref_error_to_mcp)
    }

    async fn tools_for(&self, principal: &AuthenticatedClientPrincipal) -> Vec<Tool> {
        let capabilities = self.hub.current_capabilities().await;
        let context_access = self.context_access_allowed(principal, capabilities.as_ref());
        all_tools()
            .into_iter()
            .filter(|tool| match tool.name.as_ref() {
                TOOL_OPEN_INTERACTION_CONTEXT | TOOL_CLOSE_INTERACTION_CONTEXT => context_access,
                TOOL_GET_OPERATION => self.recovery_access_allowed(principal),
                name => tool_capability(name).is_some_and(|capability| {
                    self.authorizer
                        .authorize_device_capability(principal, self.hub.device_id(), capability)
                        .is_ok()
                        && capability_is_live(capabilities.as_ref(), capability)
                }),
            })
            .collect()
    }

    fn recovery_access_allowed(&self, principal: &AuthenticatedClientPrincipal) -> bool {
        [DeviceCapability::ExecuteProcess, DeviceCapability::Shell]
            .into_iter()
            .any(|capability| {
                self.authorizer
                    .authorize_device_capability(principal, self.hub.device_id(), capability)
                    .is_ok()
            })
    }

    async fn get_operation(
        &self,
        principal: &AuthenticatedClientPrincipal,
        operation_id: &str,
    ) -> Result<CallToolResponse, McpError> {
        validate_operation_id(operation_id)?;
        let recovery = self
            .hub
            .operation_recovery_as(OperationOwner::from_principal(principal), operation_id)
            .await
            .map_err(operation_lookup_error_to_mcp)?;
        if !matches!(
            recovery.capability,
            DeviceCapability::ExecuteProcess | DeviceCapability::Shell
        ) {
            return Err(operation_not_found_error());
        }
        self.authorize(principal, recovery.capability)?;

        let state = public_recovery_state(recovery.state, recovery.result.as_ref());
        let mut payload = json!({
            "type": "operation_status",
            "operation_id": recovery.operation.operation_id,
            "state": state,
            "capability": crate::v2_observability::capability_name(recovery.capability),
            "original_retry_safe": false,
        });
        if let Some(reason) = recovery.indeterminate_reason {
            payload["indeterminate_reason"] =
                json!(crate::v2_observability::indeterminate_reason_name(reason));
        }
        if let Some(receipt) = recovery.receipt {
            payload["finalized_at_ms"] = json!(receipt.finalized_at_ms);
        }
        if let Some(result) = recovery.result {
            payload["result"] = recoverable_result_json(result);
        }
        Ok(CallToolResult::success(vec![ContentBlock::text(payload.to_string())]).into())
    }

    async fn execute_command(
        &self,
        principal: &AuthenticatedClientPrincipal,
        operation_id: String,
        command: DeviceCommand,
        usage: UsageLease,
        context: &RequestContext<RoleServer>,
    ) -> Result<DeviceResult, McpError> {
        let owner = OperationOwner::from_principal(principal);
        let read_only = command.is_read_only();
        let pending = match self
            .hub
            .start_command_as_with_id(
                owner.clone(),
                operation_id.clone(),
                command,
                Some(usage.clone()),
            )
            .await
        {
            Ok(pending) => pending,
            Err(error) => {
                settle_usage_best_effort(&usage, UsageSettlement::Zero, "pre_dispatch_rejected")
                    .await;
                return Err(hub_error_to_mcp(error));
            }
        };
        let mut wait = Box::pin(pending.wait());
        let renew_after = usage.renew_after();
        let renew_enabled = renew_after.is_some();
        let renew_period = renew_after.unwrap_or(Duration::from_secs(60));
        let mut renew_tick = tokio::time::interval(renew_period);
        renew_tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
        // `interval()` ticks immediately once; consume that tick so the first
        // renewal occurs only after the sidecar-provided heartbeat period.
        renew_tick.tick().await;

        loop {
            tokio::select! {
                result = &mut wait => {
                    return match result {
                        Ok(result) => {
                            let settlement = if usage.was_dispatched() {
                                UsageSettlement::Full
                            } else {
                                UsageSettlement::Zero
                            };
                            let outcome = if usage.was_dispatched() { "completed" } else { "pre_dispatch_no_effect" };
                            settle_usage_best_effort(&usage, settlement, outcome).await;
                            Ok(result.result)
                        }
                        Err(error) => {
                            let (settlement, outcome) =
                                usage_settlement_for_error(usage.was_dispatched(), read_only, &error);
                            settle_usage_best_effort(&usage, settlement, outcome).await;
                            Err(hub_error_to_mcp(error))
                        }
                    };
                },
                _ = renew_tick.tick(), if renew_enabled => {
                    if let Err(error) = usage.renew().await {
                        warn!(
                            event = "v2_usage_renew_failed",
                            operation_id,
                            outcome = "execution_state_unchanged",
                            error_code = error.safe_error_code(),
                            "usage lease renewal failed; CUMG execution remains authoritative"
                        );
                    }
                },
                _ = context.ct.cancelled() => {
                    let cancellation = self.hub.cancel_as(owner, operation_id).await;
                    let (settlement, outcome) = if usage.was_dispatched() {
                        (UsageSettlement::Full, "cancelled_after_dispatch")
                    } else {
                        (UsageSettlement::Zero, "cancelled_before_dispatch")
                    };
                    settle_usage_best_effort(&usage, settlement, outcome).await;
                    if let Err(error) = cancellation {
                        warn!(
                            event = "v2_northbound_cancel_failed",
                            outcome = "original_call_cancelled",
                            error_code = error.safe_error_code(),
                            "CUMG cancellation request failed; execution safety state remains authoritative"
                        );
                    }
                    return Err(McpError::invalid_request("Tool call was cancelled", None));
                }
            }
        }
    }
}

impl ServerHandler for V2NorthboundMcp {
    fn supported_protocol_versions(&self) -> Cow<'static, [ProtocolVersion]> {
        Cow::Owned(vec![ProtocolVersion::V_2026_07_28])
    }

    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::default();
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
        info.instructions = Some(
            "Authenticated single-device V2 Hub. Device capabilities are authorized independently from OAuth scopes."
                .into(),
        );
        info
    }

    async fn initialize(
        &self,
        request: InitializeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<InitializeResult, McpError> {
        log_northbound_client_initialized(&request.client_info);
        context.peer.set_peer_info(request.clone());
        let mut info = self.get_info();
        if self
            .supported_protocol_versions()
            .contains(&request.protocol_version)
        {
            info.protocol_version = request.protocol_version;
        } else {
            tracing::warn!(
                client_requested = %request.protocol_version,
                server_fallback = %info.protocol_version,
                "client requested unsupported protocol version; falling back to server default"
            );
        }
        Ok(info)
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        let auth = Self::auth_context(&context)?;
        Ok(ListToolsResult {
            tools: self.tools_for(&auth.principal).await,
            ..Default::default()
        })
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        all_tools()
            .into_iter()
            .find(|tool| tool.name.as_ref() == name)
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        let auth = Self::auth_context(&context)?;
        if request.name.as_ref() == TOOL_OPEN_INTERACTION_CONTEXT {
            let _: EmptyArgs = parse_arguments(request.arguments)?;
            return self.open_interaction_context(&auth.principal).await;
        }
        if request.name.as_ref() == TOOL_CLOSE_INTERACTION_CONTEXT {
            let args: ContextIdArgs = parse_arguments(request.arguments)?;
            return self
                .close_interaction_context(&auth.principal, &args.context_id)
                .await;
        }
        if request.name.as_ref() == TOOL_GET_OPERATION {
            let args: OperationIdArgs = parse_arguments(request.arguments)?;
            return self
                .get_operation(&auth.principal, &args.operation_id)
                .await;
        }
        let capability = tool_capability(request.name.as_ref())
            .ok_or_else(|| McpError::invalid_params("Unknown V2 Hub tool", None))?;
        let recoverable_process_call =
            matches!(request.name.as_ref(), TOOL_EXECUTE_PROCESS | TOOL_SHELL);
        let operation_id =
            requested_operation_id(request.name.as_ref(), request.arguments.as_ref())?
                .unwrap_or_else(|| self.hub.new_operation_id());
        // OAuth has already reduced the bearer token to this verified issuer +
        // subject. Usage admission receives only that principal identity and the
        // tool name; request arguments and bearer material never cross the seam.
        let usage = self
            .usage
            .reserve(UsageOperation {
                operation_id: operation_id.clone(),
                issuer: auth.principal.issuer.clone(),
                subject: auth.principal.subject.clone(),
                tool: request.name.to_string(),
            })
            .await
            .map_err(usage_error_to_mcp)?;

        if let Err(error) = self.authorize(&auth.principal, capability) {
            settle_usage_best_effort(&usage, UsageSettlement::Zero, "authorization_denied").await;
            return Err(error);
        }
        log_northbound_operation_requested(
            &operation_id,
            self.hub.device_id(),
            capability,
            context.client_info(),
        );

        if request.name.as_ref() == TOOL_BROWSER_STAGE_UPLOAD_FILE {
            return self
                .call_browser_stage_upload(
                    &auth.principal,
                    request.arguments,
                    operation_id,
                    usage,
                    &context,
                )
                .await;
        }

        if is_browser_tool(request.name.as_ref()) {
            return self
                .call_browser_tool(
                    &auth.principal,
                    request.name.as_ref(),
                    request.arguments,
                    operation_id,
                    usage,
                    &context,
                )
                .await;
        }

        let command_result: Result<DeviceCommand, McpError> = (|| match request.name.as_ref() {
            TOOL_LIST_APPS => Ok(DeviceCommand::ListApplications),
            TOOL_GET_SCREEN_SIZE => Ok(DeviceCommand::ScreenGeometry),
            TOOL_SCREENSHOT => {
                let args: ScreenshotArgs = parse_arguments(request.arguments)?;
                Ok(DeviceCommand::ScreenshotContextual {
                    context_id: args.context_id,
                })
            }
            TOOL_CLICK => {
                let args: ClickArgs = parse_arguments(request.arguments)?;
                let button = parse_pointer_button(args.button.as_deref())?;
                let action = args
                    .action
                    .as_deref()
                    .map(parse_ui_element_action)
                    .transpose()?;
                let advanced = args.context_id.is_some()
                    || args.coordinate_space.is_some()
                    || args.process_id.is_some()
                    || args.window_id.is_some()
                    || args.element_ref.is_some()
                    || action.is_some()
                    || args.click_count.unwrap_or(1) != 1
                    || !args.modifiers.is_empty()
                    || args.delivery.is_some();
                if !advanced {
                    let (x, y) = require_coordinate_pair(args.x, args.y)?;
                    return Ok(DeviceCommand::PointerClick { x, y, button });
                }
                let target = parse_click_target(
                    args.context_id.as_deref(),
                    args.coordinate_space.as_deref(),
                    args.process_id,
                    args.window_id,
                    args.x,
                    args.y,
                    args.element_ref,
                )?;
                validate_element_click_options(
                    &target,
                    button,
                    args.click_count.unwrap_or(1),
                    action,
                    &args.modifiers,
                )?;
                Ok(DeviceCommand::PointerClickAdvanced {
                    context_id: args.context_id,
                    target,
                    button,
                    click_count: args.click_count.unwrap_or(1),
                    action,
                    modifiers: parse_keyboard_modifiers(&args.modifiers)?,
                    delivery: parse_delivery_mode(args.delivery.as_deref())?,
                })
            }
            TOOL_DRAG => {
                let args: DragArgs = parse_arguments(request.arguments)?;
                if args.duration_ms > 10_000 {
                    return Err(McpError::invalid_params(
                        "duration_ms must be within 0..=10000",
                        None,
                    ));
                }
                let advanced = args.context_id.is_some()
                    || args.coordinate_space.is_some()
                    || args.process_id.is_some()
                    || args.window_id.is_some()
                    || args.button.is_some()
                    || !args.modifiers.is_empty()
                    || args.delivery.is_some()
                    || args.steps.is_some();
                if !advanced {
                    if args.duration_ms == 0 {
                        return Err(McpError::invalid_params(
                            "legacy duration_ms must be within 1..=10000",
                            None,
                        ));
                    }
                    return Ok(DeviceCommand::PointerDrag {
                        from_x: args.from_x,
                        from_y: args.from_y,
                        to_x: args.to_x,
                        to_y: args.to_y,
                        duration_ms: args.duration_ms,
                    });
                }
                let from = parse_pointer_target(
                    args.coordinate_space.as_deref(),
                    args.process_id,
                    args.window_id,
                    args.from_x,
                    args.from_y,
                )?;
                let to = parse_pointer_target(
                    args.coordinate_space.as_deref(),
                    args.process_id,
                    args.window_id,
                    args.to_x,
                    args.to_y,
                )?;
                Ok(DeviceCommand::PointerDragAdvanced {
                    context_id: args.context_id,
                    from,
                    to,
                    button: parse_pointer_button(args.button.as_deref())?,
                    modifiers: parse_keyboard_modifiers(&args.modifiers)?,
                    delivery: parse_delivery_mode(args.delivery.as_deref())?,
                    duration_ms: args.duration_ms,
                    steps: args.steps.unwrap_or(20),
                })
            }
            TOOL_TYPE_TEXT => {
                let args: TypeTextArgs = parse_arguments(request.arguments)?;
                if args.text.is_empty() || args.text.len() > MAX_TYPE_TEXT_BYTES {
                    return Err(McpError::invalid_params(
                        "text must be within 1..=32768 UTF-8 bytes",
                        None,
                    ));
                }
                let advanced = args.context_id.is_some()
                    || args.target_kind.is_some()
                    || args.process_id.is_some()
                    || args.window_id.is_some()
                    || args.x.is_some()
                    || args.y.is_some()
                    || args.element_ref.is_some()
                    || args.delivery.is_some()
                    || args.delay_ms.is_some();
                if !advanced {
                    return Ok(DeviceCommand::TypeText { text: args.text });
                }
                if args.element_ref.is_some() && args.context_id.is_none() {
                    return Err(McpError::invalid_params(
                        "element_ref input requires context_id",
                        None,
                    ));
                }
                Ok(DeviceCommand::TypeTextAdvanced {
                    context_id: args.context_id,
                    text: args.text,
                    target: parse_input_target(
                        args.target_kind.as_deref(),
                        args.process_id,
                        args.window_id,
                        args.x,
                        args.y,
                        args.element_ref,
                    )?,
                    delivery: parse_delivery_mode(args.delivery.as_deref())?,
                    delay_ms: args.delay_ms.unwrap_or(30),
                })
            }
            TOOL_EXECUTE_PROCESS => {
                let args: ExecuteProcessArgs = parse_arguments(request.arguments)?;
                if let Some(operation_id) = args.operation_id.as_deref() {
                    validate_operation_id(operation_id)?;
                }
                Ok(DeviceCommand::ExecuteProcess {
                    request: ProcessRequest {
                        program: args.program,
                        args: args.args,
                        cwd: args.cwd,
                        env: env_map(args.env),
                        timeout_ms: args.timeout_ms,
                    },
                })
            }
            TOOL_SHELL => {
                let args: ShellArgs = parse_arguments(request.arguments)?;
                if let Some(operation_id) = args.operation_id.as_deref() {
                    validate_operation_id(operation_id)?;
                }
                Ok(DeviceCommand::Shell {
                    request: ShellRequest {
                        command: args.command,
                        cwd: args.cwd,
                        env: env_map(args.env),
                        timeout_ms: args.timeout_ms,
                    },
                })
            }
            TOOL_READ_FILE => {
                let args: PathArgs = parse_arguments(request.arguments)?;
                Ok(DeviceCommand::ReadFile { path: args.path })
            }
            TOOL_LIST_DIRECTORY => {
                let args: PathArgs = parse_arguments(request.arguments)?;
                Ok(DeviceCommand::ListDirectory { path: args.path })
            }
            TOOL_LIST_WINDOWS => {
                let args: ListWindowsArgs = parse_arguments(request.arguments)?;
                Ok(DeviceCommand::ListWindows {
                    process_id: args.process_id,
                    on_screen_only: args.on_screen_only,
                })
            }
            TOOL_LAUNCH_APPLICATION => {
                let args: LaunchApplicationArgs = parse_arguments(request.arguments)?;
                validate_launch_args(&args)?;
                Ok(DeviceCommand::LaunchApplication {
                    identifier: args.identifier,
                    name: args.name,
                    targets: args.targets,
                    new_instance: args.new_instance,
                })
            }
            TOOL_INSPECT_WINDOW => {
                let args: InspectWindowArgs = parse_arguments(request.arguments)?;
                validate_window_args(args.process_id, args.window_id)?;
                if args
                    .query
                    .as_ref()
                    .is_some_and(|query| query.len() > MAX_UI_QUERY_BYTES)
                    || args.max_elements == 0
                    || (args.max_elements as usize) > MAX_UI_ELEMENTS
                    || args.max_depth == 0
                    || args.max_depth > 64
                {
                    return Err(McpError::invalid_params(
                        "Invalid window inspection bounds",
                        None,
                    ));
                }
                if let Some(context_id) = args.context_id {
                    Ok(DeviceCommand::InspectWindowContextual {
                        context_id,
                        process_id: args.process_id,
                        window_id: args.window_id,
                        query: args.query,
                        max_elements: args.max_elements,
                        max_depth: args.max_depth,
                        include_screenshot: args.include_screenshot,
                    })
                } else {
                    Ok(DeviceCommand::InspectWindow {
                        process_id: args.process_id,
                        window_id: args.window_id,
                        query: args.query,
                        max_elements: args.max_elements,
                        max_depth: args.max_depth,
                        include_screenshot: args.include_screenshot,
                    })
                }
            }
            TOOL_VERIFY_UI_STATE => {
                let args: VerifyUiStateArgs = parse_arguments(request.arguments)?;
                validate_window_args(args.process_id, args.window_id)?;
                validate_ui_predicates(&args.expect)?;
                if args.timeout_ms > 10_000 || !(1..=5).contains(&args.stable_samples) {
                    return Err(McpError::invalid_params(
                        "Invalid UI verification bounds",
                        None,
                    ));
                }
                if let Some(context_id) = args.context_id {
                    Ok(DeviceCommand::VerifyUiStateContextual {
                        context_id,
                        process_id: args.process_id,
                        window_id: args.window_id,
                        predicates: args.expect,
                        timeout_ms: args.timeout_ms,
                        stable_samples: args.stable_samples,
                        include_screenshot: args.include_screenshot,
                    })
                } else {
                    Ok(DeviceCommand::VerifyUiState {
                        process_id: args.process_id,
                        window_id: args.window_id,
                        predicates: args.expect,
                        timeout_ms: args.timeout_ms,
                        stable_samples: args.stable_samples,
                        include_screenshot: args.include_screenshot,
                    })
                }
            }
            TOOL_TERMINATE_APPLICATION => {
                let args: ProcessIdArgs = parse_arguments(request.arguments)?;
                require_positive_process_id(args.process_id)?;
                Ok(DeviceCommand::TerminateApplication {
                    process_id: args.process_id,
                })
            }
            TOOL_ACTIVATE_WINDOW => {
                let args: ActivateWindowArgs = parse_arguments(request.arguments)?;
                require_positive_process_id(args.process_id)?;
                if args.window_id == Some(0) {
                    return Err(McpError::invalid_params("window_id must be positive", None));
                }
                Ok(DeviceCommand::ActivateWindow {
                    process_id: args.process_id,
                    window_id: args.window_id,
                })
            }
            TOOL_SET_WINDOW_FRAME => {
                let args: SetWindowFrameArgs = parse_arguments(request.arguments)?;
                validate_window_args(args.process_id, args.window_id)?;
                if args.width == 0 || args.height == 0 {
                    return Err(McpError::invalid_params(
                        "window frame width and height must be positive",
                        None,
                    ));
                }
                Ok(DeviceCommand::SetWindowFrame {
                    context_id: args.context_id,
                    process_id: args.process_id,
                    window_id: args.window_id,
                    bounds: UiRect {
                        x: args.x,
                        y: args.y,
                        width: args.width,
                        height: args.height,
                    },
                })
            }
            TOOL_INVOKE_MENU => {
                let args: InvokeMenuArgs = parse_arguments(request.arguments)?;
                validate_window_args(args.process_id, args.window_id)?;
                validate_menu_path(&args.path)?;
                Ok(DeviceCommand::InvokeMenu {
                    context_id: args.context_id,
                    process_id: args.process_id,
                    window_id: args.window_id,
                    path: args.path,
                })
            }
            TOOL_KEYBOARD_INPUT => {
                let args: KeyboardInputArgs = parse_arguments(request.arguments)?;
                validate_keyboard_key_input(&args.key)?;
                if args.element_ref.is_some() && args.context_id.is_none() {
                    return Err(McpError::invalid_params(
                        "element_ref input requires context_id",
                        None,
                    ));
                }
                Ok(DeviceCommand::KeyboardInput {
                    context_id: args.context_id,
                    key: args.key,
                    modifiers: parse_keyboard_modifiers(&args.modifiers)?,
                    target: parse_input_target(
                        args.target_kind.as_deref(),
                        args.process_id,
                        args.window_id,
                        args.x,
                        args.y,
                        args.element_ref,
                    )?,
                    delivery: parse_delivery_mode(args.delivery.as_deref())?,
                })
            }
            TOOL_SCROLL => {
                let args: ScrollArgs = parse_arguments(request.arguments)?;
                if !(1..=50).contains(&args.amount) {
                    return Err(McpError::invalid_params(
                        "amount must be within 1..=50",
                        None,
                    ));
                }
                Ok(DeviceCommand::Scroll {
                    context_id: args.context_id,
                    direction: parse_scroll_direction(&args.direction)?,
                    granularity: parse_scroll_granularity(args.granularity.as_deref())?,
                    amount: args.amount,
                    target: parse_scroll_target(
                        args.target_kind.as_deref(),
                        args.process_id,
                        args.window_id,
                        args.x,
                        args.y,
                    )?,
                    delivery: parse_delivery_mode(args.delivery.as_deref())?,
                })
            }
            TOOL_CLIPBOARD_READ => {
                let args: ClipboardReadArgs = parse_arguments(request.arguments)?;
                Ok(DeviceCommand::ClipboardRead {
                    context_id: args.context_id,
                    include_text: args.include_text,
                })
            }
            TOOL_CLIPBOARD_WRITE => {
                let args: ClipboardWriteArgs = parse_arguments(request.arguments)?;
                if args.text.len() > MAX_CLIPBOARD_TEXT_BYTES {
                    return Err(McpError::invalid_params(
                        "clipboard text exceeds the 1 MiB bound",
                        None,
                    ));
                }
                Ok(DeviceCommand::ClipboardWrite {
                    context_id: args.context_id,
                    text: args.text,
                })
            }
            TOOL_GET_POINTER_POSITION => {
                let args: PointerPositionArgs = parse_arguments(request.arguments)?;
                Ok(DeviceCommand::PointerPosition {
                    context_id: Some(args.context_id),
                })
            }
            TOOL_MOVE_POINTER => {
                let args: MovePointerArgs = parse_arguments(request.arguments)?;
                Ok(DeviceCommand::MovePointer {
                    context_id: args.context_id,
                    x: args.x,
                    y: args.y,
                })
            }
            TOOL_SET_UI_VALUE => {
                let args: SetUiValueArgs = parse_arguments(request.arguments)?;
                validate_window_args(args.process_id, args.window_id)?;
                if args.element_ref.len() > 128 || args.value.len() > MAX_TYPE_TEXT_BYTES {
                    return Err(McpError::invalid_params("Invalid UI value arguments", None));
                }
                Ok(DeviceCommand::SetUiValue {
                    context_id: args.context_id,
                    process_id: args.process_id,
                    window_id: args.window_id,
                    element_ref: args.element_ref,
                    value: args.value,
                })
            }
            TOOL_CAPTURE_REGION => {
                let args: CaptureRegionArgs = parse_arguments(request.arguments)?;
                validate_window_args(args.process_id, args.window_id)?;
                if args.width == 0 || args.height == 0 {
                    return Err(McpError::invalid_params(
                        "Capture region must be positive",
                        None,
                    ));
                }
                Ok(DeviceCommand::CaptureRegion {
                    context_id: args.context_id,
                    process_id: args.process_id,
                    window_id: args.window_id,
                    bounds: UiRect {
                        x: args.x,
                        y: args.y,
                        width: args.width,
                        height: args.height,
                    },
                })
            }
            TOOL_EXPAND_INTERACTION_SCOPE => {
                let args: ExpandInteractionScopeArgs = parse_arguments(request.arguments)?;
                if args.reason.trim().is_empty() || args.reason.len() > 200 {
                    return Err(McpError::invalid_params(
                        "reason must contain 1..=200 UTF-8 bytes",
                        None,
                    ));
                }
                Ok(DeviceCommand::ExpandInteractionScope {
                    context_id: args.context_id,
                    reason: args.reason,
                })
            }
            _ => Err(McpError::invalid_params("Unknown V2 Hub tool", None)),
        })();
        let command = match command_result {
            Ok(command) => command,
            Err(error) => {
                settle_usage_best_effort(&usage, UsageSettlement::Zero, "invalid_arguments").await;
                return Err(error);
            }
        };
        let (command, interaction_binding) = match self
            .prepare_contextual_command(&auth.principal, command)
            .await
        {
            Ok(prepared) => prepared,
            Err(error) => {
                settle_usage_best_effort(
                    &usage,
                    UsageSettlement::Zero,
                    "invalid_interaction_context",
                )
                .await;
                return Err(error);
            }
        };
        let publicize_snapshot = matches!(command, DeviceCommand::InspectWindowContextual { .. });
        let expand_context_id = match &command {
            DeviceCommand::ExpandInteractionScope { context_id, .. } => Some(context_id.clone()),
            _ => None,
        };

        let public_operation_id = recoverable_process_call.then(|| operation_id.clone());
        let mut result = match self
            .execute_command(&auth.principal, operation_id, command, usage, &context)
            .await
        {
            Ok(result) => result,
            Err(error) => {
                let error = if let Some(operation_id) = public_operation_id.as_deref() {
                    mcp_error_with_operation_id(error, operation_id)
                } else {
                    error
                };
                return Ok(execution_error_response(error));
            }
        };
        if publicize_snapshot {
            let binding = interaction_binding.as_ref().ok_or_else(|| {
                McpError::internal_error("Contextual snapshot lost its interaction binding", None)
            })?;
            result = self.publicize_window_snapshot(binding, result).await?;
        }
        if let Some(context_id) = expand_context_id {
            let binding = interaction_binding.as_ref().ok_or_else(|| {
                McpError::internal_error("Scope expansion lost its interaction binding", None)
            })?;
            let id = InteractionContextId::parse(&context_id).map_err(|_| {
                McpError::invalid_request("Interaction context became invalid", None)
            })?;
            let now_ms = unix_time_ms()
                .map_err(|_| McpError::internal_error("System clock unavailable", None))?;
            self.interactions
                .lock()
                .await
                .contexts
                .expand_to_desktop_after_authorization(
                    &id,
                    &auth.principal,
                    self.hub.device_id(),
                    binding.device_generation,
                    binding.capability_revision,
                    now_ms,
                )
                .map_err(|_| {
                    McpError::invalid_request(
                        "Interaction context expired or changed during scope expansion",
                        None,
                    )
                })?;
        }
        match result {
            DeviceResult::Screenshot {
                data_base64,
                mime_type,
                width_pixels,
                height_pixels,
            } => {
                let metadata = json!({
                    "width_pixels": width_pixels,
                    "height_pixels": height_pixels,
                    "mime_type": mime_type,
                });
                Ok(CallToolResult::success(vec![
                    ContentBlock::image(data_base64, "image/png"),
                    ContentBlock::text(metadata.to_string()),
                ])
                .into())
            }
            DeviceResult::RegionCaptured { image } => {
                let metadata = json!({
                    "mime_type": image.mime_type,
                    "width_pixels": image.width_pixels,
                    "height_pixels": image.height_pixels,
                });
                Ok(CallToolResult::success(vec![
                    ContentBlock::image(image.data_base64, "image/jpeg"),
                    ContentBlock::text(metadata.to_string()),
                ])
                .into())
            }
            DeviceResult::WindowSnapshot {
                snapshot_ref,
                process_id,
                window_id,
                elements,
                elements_complete,
                screenshot,
            } => {
                let screenshot_metadata = screenshot.as_ref().map(|image| {
                    json!({
                        "mime_type": image.mime_type,
                        "width_pixels": image.width_pixels,
                        "height_pixels": image.height_pixels,
                    })
                });
                let metadata = json!({
                    "type": "window_snapshot",
                    "snapshot_ref": snapshot_ref,
                    "process_id": process_id,
                    "window_id": window_id,
                    "elements": elements,
                    "elements_complete": elements_complete,
                    "screenshot": screenshot_metadata,
                });
                let mut content = Vec::new();
                if let Some(image) = screenshot {
                    content.push(ContentBlock::image(image.data_base64, image.mime_type));
                }
                content.push(ContentBlock::text(metadata.to_string()));
                Ok(CallToolResult::success(content).into())
            }
            DeviceResult::UiStateVerification {
                status,
                stable,
                samples,
                predicates,
                screenshot,
            } => {
                let screenshot_metadata = screenshot.as_ref().map(|image| {
                    json!({
                        "mime_type": image.mime_type,
                        "width_pixels": image.width_pixels,
                        "height_pixels": image.height_pixels,
                    })
                });
                let metadata = json!({
                    "type": "ui_state_verification",
                    "status": status,
                    "stable": stable,
                    "samples": samples,
                    "predicates": predicates,
                    "screenshot": screenshot_metadata,
                });
                let mut content = Vec::new();
                if let Some(image) = screenshot {
                    content.push(ContentBlock::image(image.data_base64, image.mime_type));
                }
                content.push(ContentBlock::text(metadata.to_string()));
                Ok(CallToolResult::success(content).into())
            }
            other => {
                let mut value = serde_json::to_value(&other).map_err(|_| {
                    McpError::internal_error("Failed to serialize device result", None)
                })?;
                if let Some(operation_id) = public_operation_id
                    && let Value::Object(object) = &mut value
                {
                    object.insert("operation_id".into(), json!(operation_id));
                }
                Ok(CallToolResult::success(vec![ContentBlock::text(value.to_string())]).into())
            }
        }
    }
}

fn capability_is_live(
    advertisement: Option<&CapabilityAdvertisement>,
    capability: DeviceCapability,
) -> bool {
    advertisement.is_some_and(|advertisement| advertisement.supports(capability))
}

fn command_scoped_ui_element_ref_mut(command: &mut DeviceCommand) -> Option<&mut String> {
    match command {
        DeviceCommand::PointerClickAdvanced {
            target: PointerTarget::Element { element_ref, .. },
            ..
        }
        | DeviceCommand::TypeTextAdvanced {
            target: InputTarget::Element { element_ref, .. },
            ..
        }
        | DeviceCommand::KeyboardInput {
            target: InputTarget::Element { element_ref, .. },
            ..
        }
        | DeviceCommand::SetUiValue { element_ref, .. } => Some(element_ref),
        _ => None,
    }
}

fn command_interaction_context_id(command: &DeviceCommand) -> Option<&str> {
    match command {
        DeviceCommand::ScreenshotContextual { context_id }
        | DeviceCommand::InspectWindowContextual { context_id, .. }
        | DeviceCommand::VerifyUiStateContextual { context_id, .. }
        | DeviceCommand::MovePointer { context_id, .. }
        | DeviceCommand::SetUiValue { context_id, .. }
        | DeviceCommand::ExpandInteractionScope { context_id, .. } => Some(context_id.as_str()),
        DeviceCommand::PointerClickAdvanced { context_id, .. }
        | DeviceCommand::PointerDragAdvanced { context_id, .. }
        | DeviceCommand::TypeTextAdvanced { context_id, .. }
        | DeviceCommand::SetWindowFrame { context_id, .. }
        | DeviceCommand::InvokeMenu { context_id, .. }
        | DeviceCommand::KeyboardInput { context_id, .. }
        | DeviceCommand::Scroll { context_id, .. }
        | DeviceCommand::ClipboardRead { context_id, .. }
        | DeviceCommand::ClipboardWrite { context_id, .. }
        | DeviceCommand::PointerPosition { context_id }
        | DeviceCommand::CaptureRegion { context_id, .. } => context_id.as_deref(),
        _ => None,
    }
}

fn command_requires_desktop_scope(command: &DeviceCommand) -> bool {
    match command {
        DeviceCommand::ScreenshotContextual { .. }
        | DeviceCommand::MovePointer { .. }
        | DeviceCommand::PointerPosition {
            context_id: Some(_),
        } => true,
        DeviceCommand::PointerClickAdvanced { target, .. } => {
            matches!(target, PointerTarget::DesktopPhysical { .. })
        }
        DeviceCommand::PointerDragAdvanced { from, to, .. } => {
            matches!(from, PointerTarget::DesktopPhysical { .. })
                || matches!(to, PointerTarget::DesktopPhysical { .. })
        }
        DeviceCommand::TypeTextAdvanced { target, .. }
        | DeviceCommand::KeyboardInput { target, .. } => matches!(target, InputTarget::Desktop),
        DeviceCommand::Scroll { target, .. } => {
            matches!(target, ScrollTarget::DesktopPoint { .. })
        }
        _ => false,
    }
}

fn command_requires_window_scope(command: &DeviceCommand) -> bool {
    match command {
        DeviceCommand::InspectWindowContextual { .. }
        | DeviceCommand::VerifyUiStateContextual { .. }
        | DeviceCommand::SetUiValue { .. }
        | DeviceCommand::CaptureRegion {
            context_id: Some(_),
            ..
        } => true,
        DeviceCommand::PointerClickAdvanced {
            context_id: Some(_),
            target: PointerTarget::WindowPhysical { .. } | PointerTarget::Element { .. },
            ..
        } => true,
        DeviceCommand::PointerDragAdvanced {
            context_id: Some(_),
            from,
            to,
            ..
        } => {
            matches!(from, PointerTarget::WindowPhysical { .. })
                && matches!(to, PointerTarget::WindowPhysical { .. })
        }
        DeviceCommand::TypeTextAdvanced {
            context_id: Some(_),
            target,
            ..
        }
        | DeviceCommand::KeyboardInput {
            context_id: Some(_),
            target,
            ..
        } => !matches!(target, InputTarget::Desktop),
        DeviceCommand::SetWindowFrame {
            context_id: Some(_),
            ..
        }
        | DeviceCommand::InvokeMenu {
            context_id: Some(_),
            ..
        } => true,
        DeviceCommand::Scroll {
            context_id: Some(_),
            target,
            ..
        } => !matches!(target, ScrollTarget::DesktopPoint { .. }),
        _ => false,
    }
}

fn require_browser_window_scope(
    binding: InteractionContextBinding,
) -> Result<InteractionContextBinding, McpError> {
    if binding.scope != InteractionScope::WindowScoped {
        return Err(McpError::invalid_request(
            "Browser interaction is unavailable after desktop scope expansion; close the context and open a fresh one",
            None,
        ));
    }
    Ok(binding)
}

fn is_browser_tool(name: &str) -> bool {
    matches!(
        name,
        TOOL_BROWSER_PREPARE
            | TOOL_BROWSER_BIND
            | TOOL_BROWSER_INSPECT
            | TOOL_BROWSER_NAVIGATE
            | TOOL_BROWSER_CLICK
            | TOOL_BROWSER_TYPE
            | TOOL_BROWSER_DIALOG
            | TOOL_BROWSER_POINTER
            | TOOL_BROWSER_UPLOAD_FILE
            | TOOL_BROWSER_DOWNLOAD
    )
}

fn browser_contract_error_to_mcp(_: BrowserContractError) -> McpError {
    McpError::invalid_params("Invalid browser arguments", None)
}

fn browser_ref_error_to_mcp(_: BrowserRefError) -> McpError {
    McpError::invalid_request(
        "Browser ref is stale, invalid, or unavailable for this action",
        None,
    )
}

fn scoped_ref_error_to_mcp(_: ScopedRefError) -> McpError {
    McpError::invalid_request(
        "Scoped ref is stale, invalid, or unavailable for this action",
        None,
    )
}

fn scoped_ref_mint_error_to_mcp(_: ScopedRefError) -> McpError {
    McpError::internal_error("Scoped ref registry unavailable", None)
}

fn browser_ref_mint_error_to_mcp(_: BrowserRefError) -> McpError {
    McpError::internal_error("Browser ref registry unavailable", None)
}

fn browser_public_ref_internal_error() -> McpError {
    McpError::internal_error("Browser public ref binding unavailable", None)
}

fn browser_json_response(value: Value) -> Result<CallToolResponse, McpError> {
    Ok(CallToolResult::success(vec![ContentBlock::text(value.to_string())]).into())
}

fn publicize_browser_semantic_ref(
    backend: BrowserBackendSemanticRef,
    element_ref: String,
) -> BrowserSemanticRef {
    BrowserSemanticRef {
        element_ref,
        role: backend.role,
        name: backend.name,
        value: backend.value,
        states: backend.states,
        actions: backend.actions,
        frame: backend.frame,
        visibility: backend.visibility,
    }
}

fn requested_operation_id(
    tool_name: &str,
    arguments: Option<&JsonObject>,
) -> Result<Option<String>, McpError> {
    if !matches!(tool_name, TOOL_EXECUTE_PROCESS | TOOL_SHELL) {
        return Ok(None);
    }
    let Some(value) = arguments.and_then(|arguments| arguments.get("operation_id")) else {
        return Ok(None);
    };
    let operation_id = value
        .as_str()
        .ok_or_else(|| McpError::invalid_params("operation_id must be a string", None))?;
    validate_operation_id(operation_id)?;
    Ok(Some(operation_id.to_owned()))
}

fn validate_operation_id(operation_id: &str) -> Result<(), McpError> {
    let suffix = operation_id
        .strip_prefix("op_")
        .ok_or_else(|| McpError::invalid_params("Invalid operation_id", None))?;
    if suffix.len() != 32
        || !suffix
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(McpError::invalid_params("Invalid operation_id", None));
    }
    Ok(())
}

fn operation_not_found_error() -> McpError {
    McpError::invalid_request(
        "Operation is unknown or not available to this principal",
        Some(json!({"code": "operation_not_found"})),
    )
}

fn operation_lookup_error_to_mcp(error: HubCommandError) -> McpError {
    match error {
        HubCommandError::UnknownOperation | HubCommandError::Rejected => {
            operation_not_found_error()
        }
        other => hub_error_to_mcp(other),
    }
}

fn public_recovery_state(
    state: HubOperationState,
    result: Option<&RecoverableOperationResult>,
) -> &'static str {
    match state {
        HubOperationState::Queued
        | HubOperationState::ActiveNotDispatched
        | HubOperationState::Dispatched
        | HubOperationState::CancelRequested => "running",
        HubOperationState::Completed => "succeeded",
        HubOperationState::Cancelled => "cancelled",
        HubOperationState::Indeterminate => "indeterminate",
        HubOperationState::Failed
            if matches!(
                result,
                Some(RecoverableOperationResult::Process { output }
                    | RecoverableOperationResult::Shell { output }) if output.timed_out
            ) =>
        {
            "timed_out"
        }
        HubOperationState::Failed => "failed",
    }
}

fn recoverable_result_json(result: RecoverableOperationResult) -> Value {
    match result {
        RecoverableOperationResult::Process { output } => {
            json!({"type": "process", "output": output})
        }
        RecoverableOperationResult::Shell { output } => {
            json!({"type": "shell", "output": output})
        }
        RecoverableOperationResult::Error { code } => {
            json!({"type": "error", "code": code.safe_code()})
        }
    }
}

fn mcp_error_with_operation_id(mut error: McpError, operation_id: &str) -> McpError {
    let mut data = error.data.take().unwrap_or_else(|| json!({}));
    if let Value::Object(object) = &mut data {
        object.insert("operation_id".into(), json!(operation_id));
    } else {
        data = json!({"operation_id": operation_id});
    }
    error.data = Some(data);
    error
}

fn usage_settlement_for_error(
    dispatched: bool,
    read_only: bool,
    error: &HubCommandError,
) -> (UsageSettlement, &'static str) {
    if !dispatched {
        return (UsageSettlement::Zero, "pre_dispatch_rejected");
    }
    if read_only && matches!(error, HubCommandError::Remote(_)) {
        // A verified remote failure of a read-only operation is the intentionally
        // narrow current post-dispatch path where no state-changing effect can be
        // attributed to the business operation.
        return (UsageSettlement::Zero, "proven_no_effect");
    }
    // Any dispatched mutable-operation failure, timeout/disconnect, or
    // indeterminate state is charged fully. Accounting never authorizes replay.
    (UsageSettlement::Full, "dispatched_conservative")
}

async fn settle_usage_best_effort(
    usage: &UsageLease,
    settlement: UsageSettlement,
    outcome: &'static str,
) {
    if let Err(error) = usage.settle(settlement, outcome).await {
        // Settlement/reconciliation is intentionally separated from execution.
        // Never clear quarantine, retry a business operation, or hide a completed
        // result because the optional accounting sidecar is unavailable.
        warn!(
            event = "v2_usage_settlement_failed",
            operation_id = usage.operation_id(),
            outcome,
            error_code = error.safe_error_code(),
            "usage settlement failed; CUMG execution state remains authoritative"
        );
    }
}

fn usage_error_to_mcp(error: UsageError) -> McpError {
    match error {
        UsageError::Denied(_) => McpError::invalid_request("Usage quota denied", None),
        UsageError::Unavailable
        | UsageError::InvalidResponse
        | UsageError::InvalidConfiguration => {
            McpError::internal_error("Usage accounting is temporarily unavailable", None)
        }
    }
}

fn hub_error_to_mcp(error: HubCommandError) -> McpError {
    let (message, code, operation_id) = match error {
        HubCommandError::AgentOffline => ("Agent is offline", "agent_offline", None),
        HubCommandError::Busy => ("Device is busy", "busy", None),
        HubCommandError::DeviceIndeterminate { operation_id } => (
            "Device execution state is indeterminate",
            "device_indeterminate",
            Some(operation_id),
        ),
        HubCommandError::Indeterminate => (
            "Device execution state is indeterminate",
            "device_indeterminate",
            None,
        ),
        HubCommandError::CancelledBeforeDispatch => (
            "Operation was cancelled before dispatch",
            "cancelled_before_dispatch",
            None,
        ),
        HubCommandError::UsageUnavailable => (
            "Usage accounting is temporarily unavailable",
            "usage_unavailable",
            None,
        ),
        HubCommandError::GrantSigningUnavailable => {
            return McpError::internal_error(
                "Grant signing is temporarily unavailable",
                Some(json!({"code": "grant_signing_unavailable"})),
            );
        }
        HubCommandError::Remote(code) => {
            let message = if code.is_browser_refusal() {
                "Browser operation was refused"
            } else {
                "Device operation was rejected or could not be completed"
            };
            return McpError::invalid_request(message, Some(json!({"code": code.safe_code()})));
        }
        HubCommandError::SessionSuperseded => {
            ("Device session was superseded", "session_superseded", None)
        }
        HubCommandError::SessionClosed => ("Device session closed", "session_closed", None),
        HubCommandError::OperationReplay => {
            ("Operation replay was rejected", "operation_replay", None)
        }
        HubCommandError::UnknownOperation => ("Operation is unknown", "unknown_operation", None),
        HubCommandError::Rejected => (
            "Device operation was rejected or could not be completed",
            "rejected",
            None,
        ),
        HubCommandError::UnexpectedResult => (
            "Device operation returned an unexpected result",
            "unexpected_result",
            None,
        ),
    };
    let mut data = json!({"code": code});
    if let Some(operation_id) = operation_id {
        data["operation_id"] = json!(operation_id);
    }
    McpError::invalid_request(message, Some(data))
}

fn execution_error_response(error: McpError) -> CallToolResponse {
    let code = error
        .data
        .as_ref()
        .and_then(|value| value.get("code"))
        .and_then(Value::as_str)
        .unwrap_or("tool_call_cancelled");
    let mut payload = json!({
        "type": "tool_error",
        "code": code,
        "message": error.message,
        "retry_safe": false,
    });
    if let Some(operation_id) = error
        .data
        .as_ref()
        .and_then(|value| value.get("operation_id"))
        .and_then(Value::as_str)
    {
        payload["operation_id"] = json!(operation_id);
    }
    CallToolResult::error(vec![ContentBlock::text(payload.to_string())]).into()
}

fn tool_capability(name: &str) -> Option<DeviceCapability> {
    match name {
        TOOL_LIST_APPS => Some(DeviceCapability::ListApplications),
        TOOL_GET_SCREEN_SIZE => Some(DeviceCapability::ScreenGeometry),
        TOOL_SCREENSHOT => Some(DeviceCapability::Screenshot),
        TOOL_CLICK => Some(DeviceCapability::PointerClick),
        TOOL_DRAG => Some(DeviceCapability::PointerDrag),
        TOOL_TYPE_TEXT => Some(DeviceCapability::TypeText),
        TOOL_EXECUTE_PROCESS => Some(DeviceCapability::ExecuteProcess),
        TOOL_SHELL => Some(DeviceCapability::Shell),
        TOOL_READ_FILE => Some(DeviceCapability::ReadFile),
        TOOL_LIST_DIRECTORY => Some(DeviceCapability::ListDirectory),
        TOOL_LIST_WINDOWS => Some(DeviceCapability::ListWindows),
        TOOL_LAUNCH_APPLICATION => Some(DeviceCapability::LaunchApplication),
        TOOL_INSPECT_WINDOW => Some(DeviceCapability::InspectWindow),
        TOOL_VERIFY_UI_STATE => Some(DeviceCapability::VerifyUiState),
        TOOL_TERMINATE_APPLICATION => Some(DeviceCapability::TerminateApplication),
        TOOL_ACTIVATE_WINDOW => Some(DeviceCapability::ActivateWindow),
        TOOL_SET_WINDOW_FRAME => Some(DeviceCapability::SetWindowFrame),
        TOOL_INVOKE_MENU => Some(DeviceCapability::InvokeMenu),
        TOOL_KEYBOARD_INPUT => Some(DeviceCapability::KeyboardInput),
        TOOL_SCROLL => Some(DeviceCapability::Scroll),
        TOOL_CLIPBOARD_READ => Some(DeviceCapability::ClipboardRead),
        TOOL_CLIPBOARD_WRITE => Some(DeviceCapability::ClipboardWrite),
        TOOL_GET_POINTER_POSITION => Some(DeviceCapability::PointerPosition),
        TOOL_MOVE_POINTER => Some(DeviceCapability::MovePointer),
        TOOL_SET_UI_VALUE => Some(DeviceCapability::SetUiValue),
        TOOL_CAPTURE_REGION => Some(DeviceCapability::CaptureRegion),
        TOOL_EXPAND_INTERACTION_SCOPE => Some(DeviceCapability::DesktopScope),
        TOOL_BROWSER_PREPARE => Some(DeviceCapability::BrowserPrepare),
        TOOL_BROWSER_BIND | TOOL_BROWSER_INSPECT => Some(DeviceCapability::BrowserInspect),
        TOOL_BROWSER_NAVIGATE => Some(DeviceCapability::BrowserNavigate),
        TOOL_BROWSER_CLICK => Some(DeviceCapability::BrowserClick),
        TOOL_BROWSER_TYPE => Some(DeviceCapability::BrowserType),
        TOOL_BROWSER_DIALOG => Some(DeviceCapability::BrowserDialog),
        TOOL_BROWSER_POINTER => Some(DeviceCapability::BrowserPointer),
        TOOL_BROWSER_STAGE_UPLOAD_FILE | TOOL_BROWSER_UPLOAD_FILE => {
            Some(DeviceCapability::BrowserUploadFile)
        }
        TOOL_BROWSER_DOWNLOAD => Some(DeviceCapability::BrowserDownload),
        _ => None,
    }
}

fn all_tools() -> Vec<Tool> {
    vec![
        Tool::new(
            TOOL_OPEN_INTERACTION_CONTEXT,
            "Open bounded CUMG workflow state for stateful Computer Use. The opaque context id is not authorization.",
            object_schema(vec![], &[]),
        )
        .with_annotations(ToolAnnotations::new().read_only(false).idempotent(false)),
        Tool::new(
            TOOL_CLOSE_INTERACTION_CONTEXT,
            "Invalidate one interaction context and all CUMG-scoped refs owned by the authenticated principal/device.",
            object_schema(vec![("context_id", interaction_context_id_schema())], &["context_id"]),
        )
        .with_annotations(ToolAnnotations::new().destructive(false).idempotent(true)),
        Tool::new(
            TOOL_BROWSER_PREPARE,
            "Explicitly prepare a browser route inside the current InteractionContext. Existing-profile setup remains backend/operator-authorized; CUMG accepts no approval artifact.",
            object_schema(
                vec![
                    ("context_id", interaction_context_id_schema()),
                    ("process_id", positive_integer_schema()),
                    ("window_id", positive_integer_schema()),
                    ("allow_launch", boolean_schema()),
                    ("profile_mode", browser_profile_mode_schema()),
                    ("profile_name", browser_profile_name_schema()),
                ],
                &["context_id", "process_id", "allow_launch", "profile_mode"],
            ),
        )
        .with_annotations(ToolAnnotations::new().destructive(true).idempotent(false)),
        Tool::new(
            TOOL_BROWSER_BIND,
            "Bind one exact native browser window and mint CUMG target/tab refs. Heuristic bindings are refused and raw backend ids are never returned.",
            object_schema(
                vec![
                    ("context_id", interaction_context_id_schema()),
                    ("process_id", positive_integer_schema()),
                    ("window_id", positive_integer_schema()),
                ],
                &["context_id", "process_id", "window_id"],
            ),
        )
        .with_annotations(ToolAnnotations::new().read_only(true)),
        Tool::new(
            TOOL_BROWSER_INSPECT,
            "Read a fresh semantic snapshot for one exact CUMG browser tab. Fresh snapshots invalidate older page refs; continuation refs are single-use.",
            object_schema(
                vec![
                    ("context_id", interaction_context_id_schema()),
                    ("target_ref", scoped_ref_schema()),
                    ("tab_ref", scoped_ref_schema()),
                    ("scope_ref", scoped_ref_schema()),
                    ("query", browser_query_schema()),
                    ("continuation_ref", scoped_ref_schema()),
                    ("include_screenshot", boolean_schema()),
                ],
                &["context_id", "target_ref", "tab_ref", "include_screenshot"],
            ),
        )
        .with_annotations(ToolAnnotations::new().read_only(true)),
        Tool::new(
            TOOL_BROWSER_NAVIGATE,
            "Navigate one exact CUMG browser tab to an http(s) URL. Successful navigation invalidates page/dialog refs and requires a fresh snapshot.",
            object_schema(
                vec![
                    ("context_id", interaction_context_id_schema()),
                    ("target_ref", scoped_ref_schema()),
                    ("tab_ref", scoped_ref_schema()),
                    ("url", browser_url_schema()),
                ],
                &["context_id", "target_ref", "tab_ref", "url"],
            ),
        )
        .with_annotations(ToolAnnotations::new().destructive(true).idempotent(false)),
        Tool::new(
            TOOL_BROWSER_CLICK,
            "Click through an exact browser tab using either a current typed element ref or explicit trusted viewport coordinates. No automatic input-route or foreground fallback is performed.",
            object_schema(
                vec![
                    ("context_id", interaction_context_id_schema()),
                    ("target_ref", scoped_ref_schema()),
                    ("tab_ref", scoped_ref_schema()),
                    ("target", browser_click_target_schema()),
                    ("input_route", browser_input_route_schema()),
                ],
                &["context_id", "target_ref", "tab_ref", "target", "input_route"],
            ),
        )
        .with_annotations(ToolAnnotations::new().destructive(true).idempotent(false)),
        Tool::new(
            TOOL_BROWSER_TYPE,
            "Type bounded text through a current typed browser element ref. Trusted-input refusal is returned as a semantic refusal and never causes an automatic route switch.",
            object_schema(
                vec![
                    ("context_id", interaction_context_id_schema()),
                    ("target_ref", scoped_ref_schema()),
                    ("tab_ref", scoped_ref_schema()),
                    ("element_ref", scoped_ref_schema()),
                    ("text", browser_text_or_empty_schema()),
                    ("mode", browser_type_mode_schema()),
                    ("replace", boolean_schema()),
                ],
                &["context_id", "target_ref", "tab_ref", "element_ref", "text", "mode", "replace"],
            ),
        )
        .with_annotations(ToolAnnotations::new().destructive(true).idempotent(false)),
        Tool::new(
            TOOL_BROWSER_DIALOG,
            "Inspect or explicitly resolve a page-owned JavaScript dialog on one exact browser tab. Inspect mints an opaque CUMG dialog ref; background refusal never triggers automatic foreground retry.",
            object_schema(
                vec![
                    ("context_id", interaction_context_id_schema()),
                    ("target_ref", scoped_ref_schema()),
                    ("tab_ref", scoped_ref_schema()),
                    ("dialog_ref", scoped_ref_schema()),
                    ("action", browser_dialog_action_schema()),
                    ("prompt_text", browser_prompt_text_schema()),
                    ("delivery", browser_dialog_delivery_schema()),
                ],
                &["context_id", "target_ref", "tab_ref", "action", "delivery"],
            ),
        )
        .with_annotations(ToolAnnotations::new().destructive(true).idempotent(false)),
        Tool::new(
            TOOL_BROWSER_POINTER,
            "Perform a typed pointer action through a current semantic browser ref. Scroll accepts only refs with scroll/pointer authority; no desktop escalation or route fallback is performed.",
            object_schema(
                vec![
                    ("context_id", interaction_context_id_schema()),
                    ("target_ref", scoped_ref_schema()),
                    ("tab_ref", scoped_ref_schema()),
                    ("element_ref", scoped_ref_schema()),
                    ("action", browser_pointer_action_schema()),
                    ("destination_ref", scoped_ref_schema()),
                    ("delta_x", browser_scroll_delta_schema()),
                    ("delta_y", browser_scroll_delta_schema()),
                    ("input_route", browser_input_route_schema()),
                ],
                &["context_id", "target_ref", "tab_ref", "element_ref", "action", "delta_x", "delta_y", "input_route"],
            ),
        )
        .with_annotations(ToolAnnotations::new().destructive(true).idempotent(false)),
        Tool::new(
            TOOL_BROWSER_STAGE_UPLOAD_FILE,
            "Stage one bounded browser-upload payload on the Agent and return an opaque CUMG file ref. No local filesystem path is accepted or returned.",
            object_schema(
                vec![
                    ("context_id", interaction_context_id_schema()),
                    ("file_name", json!({"type":"string","minLength":1,"maxLength":MAX_BROWSER_UPLOAD_NAME_BYTES})),
                    ("data_base64", json!({"type":"string","minLength":1,"maxLength":MAX_BROWSER_UPLOAD_BASE64_BYTES})),
                ],
                &["context_id", "file_name", "data_base64"],
            ),
        )
        .with_annotations(ToolAnnotations::new().destructive(true).idempotent(false)),
        Tool::new(
            TOOL_BROWSER_UPLOAD_FILE,
            "Assign previously staged opaque CUMG file refs to an exact current file-input semantic ref. No backend ids or local paths are accepted.",
            object_schema(
                vec![
                    ("context_id", interaction_context_id_schema()),
                    ("target_ref", scoped_ref_schema()),
                    ("tab_ref", scoped_ref_schema()),
                    ("element_ref", scoped_ref_schema()),
                    ("file_refs", bounded_array_schema(scoped_ref_schema(), 1, MAX_BROWSER_UPLOAD_FILES as u64)),
                ],
                &["context_id", "target_ref", "tab_ref", "element_ref", "file_refs"],
            ),
        )
        .with_annotations(ToolAnnotations::new().destructive(true).idempotent(false)),
        Tool::new(
            TOOL_BROWSER_DOWNLOAD,
            "Trigger one exact browser download into Agent-private staging and return a bounded opaque CUMG result ref plus bytes. No host destination path is accepted or returned.",
            object_schema(
                vec![
                    ("context_id", interaction_context_id_schema()),
                    ("target_ref", scoped_ref_schema()),
                    ("tab_ref", scoped_ref_schema()),
                    ("element_ref", scoped_ref_schema()),
                    ("destination_name", json!({"type":"string","minLength":1,"maxLength":MAX_BROWSER_DOWNLOAD_NAME_BYTES})),
                    ("max_bytes", json!({"type":"integer","minimum":1,"maximum":MAX_BROWSER_DOWNLOAD_BYTES})),
                    ("overwrite", json!({"type":"boolean"})),
                ],
                &["context_id", "target_ref", "tab_ref", "element_ref", "destination_name", "max_bytes", "overwrite"],
            ),
        )
        .with_annotations(ToolAnnotations::new().destructive(true).idempotent(false)),
        Tool::new(
            TOOL_LIST_APPS,
            "List applications through the enrolled computer-use backend.",
            object_schema(vec![], &[]),
        )
        .with_annotations(ToolAnnotations::new().read_only(true)),
        Tool::new(
            TOOL_GET_SCREEN_SIZE,
            "Read desktop screen geometry through the enrolled computer-use backend.",
            object_schema(vec![], &[]),
        )
        .with_annotations(ToolAnnotations::new().read_only(true)),
        Tool::new(
            TOOL_SCREENSHOT,
            "Capture the enrolled device desktop as a bounded PNG image. Requires an explicitly desktop-scoped interaction context.",
            object_schema(
                vec![("context_id", interaction_context_id_schema())],
                &["context_id"],
            ),
        )
        .with_annotations(ToolAnnotations::new().read_only(true)),
        Tool::new(
            TOOL_CLICK,
            "Click desktop coordinates through the enrolled computer-use backend.",
            object_schema(
                vec![
                    ("context_id", interaction_context_id_schema()),
                    ("x", signed_integer_schema()),
                    ("y", signed_integer_schema()),
                    ("element_ref", scoped_ref_schema()),
                    ("action", ui_element_action_schema()),
                    ("button", pointer_button_schema()),
                    ("coordinate_space", pointer_coordinate_space_schema()),
                    ("process_id", positive_integer_schema()),
                    ("window_id", positive_integer_schema()),
                    ("click_count", bounded_positive_integer_schema(3)),
                    ("modifiers", keyboard_modifiers_schema()),
                    ("delivery", delivery_mode_schema()),
                ],
                &[],
            ),
        )
        .with_annotations(ToolAnnotations::new().destructive(true).idempotent(false)),
        Tool::new(
            TOOL_DRAG,
            "Drag the desktop pointer through the enrolled computer-use backend.",
            object_schema(
                vec![
                    ("context_id", interaction_context_id_schema()),
                    ("from_x", signed_integer_schema()),
                    ("from_y", signed_integer_schema()),
                    ("to_x", signed_integer_schema()),
                    ("to_y", signed_integer_schema()),
                    ("duration_ms", bounded_nonnegative_integer_schema(10_000)),
                    ("coordinate_space", pointer_coordinate_space_schema()),
                    ("process_id", positive_integer_schema()),
                    ("window_id", positive_integer_schema()),
                    ("button", pointer_button_schema()),
                    ("modifiers", keyboard_modifiers_schema()),
                    ("delivery", delivery_mode_schema()),
                    ("steps", bounded_positive_integer_schema(200)),
                ],
                &["from_x", "from_y", "to_x", "to_y", "duration_ms"],
            ),
        )
        .with_annotations(ToolAnnotations::new().destructive(true).idempotent(false)),
        Tool::new(
            TOOL_TYPE_TEXT,
            "Type text into the current foreground desktop application.",
            object_schema(
                vec![
                    ("context_id", interaction_context_id_schema()),
                    ("text", bounded_text_schema()),
                    ("target_kind", input_target_kind_schema()),
                    ("process_id", positive_integer_schema()),
                    ("window_id", positive_integer_schema()),
                    ("x", signed_integer_schema()),
                    ("y", signed_integer_schema()),
                    ("element_ref", scoped_ref_schema()),
                    ("delivery", delivery_mode_schema()),
                    ("delay_ms", bounded_nonnegative_integer_schema(200)),
                ],
                &["text"],
            ),
        )
        .with_annotations(ToolAnnotations::new().destructive(true).idempotent(false)),
        Tool::new(
            TOOL_GET_OPERATION,
            "Read the durable status/result of a prior process or shell operation without replaying it.",
            object_schema(vec![("operation_id", operation_id_schema())], &["operation_id"]),
        )
        .with_annotations(ToolAnnotations::new().read_only(true)),
        Tool::new(
            TOOL_EXECUTE_PROCESS,
            "Execute a bounded structured local process. Ordinary descendants remaining in the supervised process group/Job Object are cleaned when the operation ends; this is not a persistent service launcher. Supply operation_id before long-running or mutating work so a lost response can be recovered with get_operation; lookup never replays the process.",
            object_schema(
                vec![
                    ("operation_id", operation_id_schema()),
                    ("program", string_schema()),
                    ("args", array_schema(string_schema())),
                    ("cwd", string_schema()),
                    ("env", string_map_schema()),
                    ("timeout_ms", positive_integer_schema()),
                ],
                &["program", "cwd", "timeout_ms"],
            ),
        )
        .with_annotations(ToolAnnotations::new().destructive(true).idempotent(false)),
        Tool::new(
            TOOL_SHELL,
            "Execute a bounded free-form shell command. Ordinary descendants remaining in the supervised process group/Job Object are cleaned when the operation ends; nohup/backgrounding is not a supported persistence mechanism. Supply operation_id before long-running or mutating work so a lost response can be recovered with get_operation; lookup never replays the shell command.",
            object_schema(
                vec![
                    ("operation_id", operation_id_schema()),
                    ("command", string_schema()),
                    ("cwd", string_schema()),
                    ("env", string_map_schema()),
                    ("timeout_ms", positive_integer_schema()),
                ],
                &["command", "cwd", "timeout_ms"],
            ),
        )
        .with_annotations(ToolAnnotations::new().destructive(true).idempotent(false)),
        Tool::new(
            TOOL_READ_FILE,
            "Read a bounded file from an Agent-approved filesystem root.",
            object_schema(vec![("path", string_schema())], &["path"]),
        )
        .with_annotations(ToolAnnotations::new().read_only(true)),
        Tool::new(
            TOOL_LIST_DIRECTORY,
            "List a bounded directory from an Agent-approved filesystem root.",
            object_schema(vec![("path", string_schema())], &["path"]),
        )
        .with_annotations(ToolAnnotations::new().read_only(true)),
        Tool::new(
            TOOL_LIST_WINDOWS,
            "List top-level windows through the enrolled computer-use backend using a backend-neutral window model.",
            object_schema(
                vec![
                    ("process_id", positive_integer_schema()),
                    ("on_screen_only", boolean_schema()),
                ],
                &[],
            ),
        )
        .with_annotations(ToolAnnotations::new().read_only(true)),
        Tool::new(
            TOOL_LAUNCH_APPLICATION,
            "Launch an application through the enrolled computer-use backend using an opaque application identifier or display name.",
            object_schema(
                vec![
                    ("identifier", string_schema()),
                    ("name", string_schema()),
                    ("targets", array_schema(string_schema())),
                    ("new_instance", boolean_schema()),
                ],
                &[],
            ),
        )
        .with_annotations(ToolAnnotations::new().destructive(true).idempotent(false)),
        Tool::new(
            TOOL_INSPECT_WINDOW,
            "Inspect one exact top-level window as a bounded backend-neutral UI element snapshot, optionally with a window screenshot.",
            object_schema(
                vec![
                    ("context_id", interaction_context_id_schema()),
                    ("process_id", positive_integer_schema()),
                    ("window_id", positive_integer_schema()),
                    ("query", bounded_ui_query_schema()),
                    ("max_elements", bounded_positive_integer_schema(MAX_UI_ELEMENTS as u64)),
                    ("max_depth", bounded_positive_integer_schema(64)),
                    ("include_screenshot", boolean_schema()),
                ],
                &["process_id", "window_id"],
            ),
        )
        .with_annotations(ToolAnnotations::new().read_only(true)),
        Tool::new(
            TOOL_VERIFY_UI_STATE,
            "Verify bounded predicates against one exact window. Unknown never implies success.",
            object_schema(
                vec![
                    ("context_id", interaction_context_id_schema()),
                    ("process_id", positive_integer_schema()),
                    ("window_id", positive_integer_schema()),
                    ("expect", bounded_array_schema(ui_predicate_schema(), 1, MAX_UI_PREDICATES as u64)),
                    ("timeout_ms", bounded_nonnegative_integer_schema(10_000)),
                    ("stable_samples", bounded_positive_integer_schema(5)),
                    ("include_screenshot", boolean_schema()),
                ],
                &["process_id", "window_id", "expect"],
            ),
        )
        .with_annotations(ToolAnnotations::new().read_only(true)),
        Tool::new(
            TOOL_TERMINATE_APPLICATION,
            "Force-terminate one exact process. Unsaved application state may be lost.",
            object_schema(vec![("process_id", positive_integer_schema())], &["process_id"]),
        )
        .with_annotations(ToolAnnotations::new().destructive(true).idempotent(false)),
        Tool::new(
            TOOL_ACTIVATE_WINDOW,
            "Persistently bring an application or exact window to the foreground. This intentionally steals foreground.",
            object_schema(
                vec![
                    ("process_id", positive_integer_schema()),
                    ("window_id", positive_integer_schema()),
                ],
                &["process_id"],
            ),
        )
        .with_annotations(ToolAnnotations::new().destructive(true).idempotent(false)),
        Tool::new(
            TOOL_SET_WINDOW_FRAME,
            "Set and verify an exact top-level window frame in desktop logical coordinates.",
            object_schema(
                vec![
                    ("context_id", interaction_context_id_schema()),
                    ("process_id", positive_integer_schema()),
                    ("window_id", positive_integer_schema()),
                    ("x", signed_integer_schema()),
                    ("y", signed_integer_schema()),
                    ("width", positive_integer_schema()),
                    ("height", positive_integer_schema()),
                ],
                &["process_id", "window_id", "x", "y", "width", "height"],
            ),
        )
        .with_annotations(ToolAnnotations::new().destructive(true).idempotent(false)),
        Tool::new(
            TOOL_INVOKE_MENU,
            "Resolve and invoke an exact bounded application-menu path without pixel fallback.",
            object_schema(
                vec![
                    ("context_id", interaction_context_id_schema()),
                    ("process_id", positive_integer_schema()),
                    ("window_id", positive_integer_schema()),
                    ("path", bounded_array_schema(bounded_menu_segment_schema(), 1, MAX_MENU_PATH_SEGMENTS as u64)),
                ],
                &["process_id", "window_id", "path"],
            ),
        )
        .with_annotations(ToolAnnotations::new().destructive(true).idempotent(false)),
        Tool::new(
            TOOL_KEYBOARD_INPUT,
            "Send one bounded semantic key with optional modifiers using explicit background or foreground delivery.",
            object_schema(
                vec![
                    ("context_id", interaction_context_id_schema()),
                    ("key", keyboard_key_schema()),
                    ("modifiers", keyboard_modifiers_schema()),
                    ("target_kind", input_target_kind_schema()),
                    ("process_id", positive_integer_schema()),
                    ("window_id", positive_integer_schema()),
                    ("x", signed_integer_schema()),
                    ("y", signed_integer_schema()),
                    ("element_ref", scoped_ref_schema()),
                    ("delivery", delivery_mode_schema()),
                ],
                &["key"],
            ),
        )
        .with_annotations(ToolAnnotations::new().destructive(true).idempotent(false)),
        Tool::new(
            TOOL_SCROLL,
            "Scroll a focused window region, exact window point, or desktop point with explicit coordinate targeting.",
            object_schema(
                vec![
                    ("context_id", interaction_context_id_schema()),
                    ("direction", scroll_direction_schema()),
                    ("granularity", scroll_granularity_schema()),
                    ("amount", bounded_positive_integer_schema(50)),
                    ("target_kind", scroll_target_kind_schema()),
                    ("process_id", positive_integer_schema()),
                    ("window_id", positive_integer_schema()),
                    ("x", signed_integer_schema()),
                    ("y", signed_integer_schema()),
                    ("delivery", delivery_mode_schema()),
                ],
                &["direction"],
            ),
        )
        .with_annotations(ToolAnnotations::new().destructive(true).idempotent(false)),
        Tool::new(
            TOOL_CLIPBOARD_READ,
            "Read bounded clipboard type metadata and optionally privacy-sensitive plain text. Clipboard content is never telemetry.",
            object_schema(
                vec![
                    ("context_id", interaction_context_id_schema()),
                    ("include_text", boolean_schema()),
                ],
                &[],
            ),
        )
        .with_annotations(ToolAnnotations::new().read_only(true)),
        Tool::new(
            TOOL_CLIPBOARD_WRITE,
            "Replace the clipboard with bounded plain text. File/image clipboard writes require a future CUMG-issued file ref.",
            object_schema(
                vec![
                    ("context_id", interaction_context_id_schema()),
                    ("text", clipboard_text_schema()),
                ],
                &["text"],
            ),
        )
        .with_annotations(ToolAnnotations::new().destructive(true).idempotent(false)),
        Tool::new(
            TOOL_GET_POINTER_POSITION,
            "Read the current real pointer position. Requires an explicitly desktop-scoped interaction context.",
            object_schema(
                vec![("context_id", interaction_context_id_schema())],
                &["context_id"],
            ),
        )
        .with_annotations(ToolAnnotations::new().read_only(true)),
        Tool::new(
            TOOL_MOVE_POINTER,
            "Move the real OS pointer in desktop physical screenshot pixels.",
            object_schema(
                vec![
                    ("context_id", interaction_context_id_schema()),
                    ("x", signed_integer_schema()),
                    ("y", signed_integer_schema()),
                ],
                &["context_id", "x", "y"],
            ),
        )
        .with_annotations(ToolAnnotations::new().destructive(true).idempotent(false)),
        Tool::new(
            TOOL_SET_UI_VALUE,
            "Set a bounded value on a UI element ref minted by inspect_window in the same interaction context.",
            object_schema(
                vec![
                    ("context_id", interaction_context_id_schema()),
                    ("process_id", positive_integer_schema()),
                    ("window_id", positive_integer_schema()),
                    ("element_ref", scoped_ref_schema()),
                    ("value", bounded_text_or_empty_schema()),
                ],
                &["context_id", "process_id", "window_id", "element_ref", "value"],
            ),
        )
        .with_annotations(ToolAnnotations::new().destructive(true).idempotent(false)),
        Tool::new(
            TOOL_CAPTURE_REGION,
            "Capture a bounded window-local region without hidden zoom-coordinate state.",
            object_schema(
                vec![
                    ("context_id", interaction_context_id_schema()),
                    ("process_id", positive_integer_schema()),
                    ("window_id", positive_integer_schema()),
                    ("x", signed_integer_schema()),
                    ("y", signed_integer_schema()),
                    ("width", positive_integer_schema()),
                    ("height", positive_integer_schema()),
                ],
                &["process_id", "window_id", "x", "y", "width", "height"],
            ),
        )
        .with_annotations(ToolAnnotations::new().read_only(true)),
        Tool::new(
            TOOL_EXPAND_INTERACTION_SCOPE,
            "Explicitly and monotonically expand one authorized interaction context from window-scoped to desktop-scoped execution.",
            object_schema(
                vec![
                    ("context_id", interaction_context_id_schema()),
                    ("reason", bounded_reason_schema()),
                ],
                &["context_id", "reason"],
            ),
        )
        .with_annotations(ToolAnnotations::new().destructive(true).idempotent(true)),
    ]
}

fn object_schema(properties: Vec<(&str, Value)>, required: &[&str]) -> Arc<JsonObject> {
    let properties: JsonObject = properties
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value))
        .collect();
    let mut schema = JsonObject::new();
    schema.insert("type".into(), Value::String("object".into()));
    schema.insert("properties".into(), Value::Object(properties));
    schema.insert(
        "required".into(),
        Value::Array(
            required
                .iter()
                .map(|name| Value::String((*name).to_owned()))
                .collect(),
        ),
    );
    schema.insert("additionalProperties".into(), Value::Bool(false));
    Arc::new(schema)
}

fn operation_id_schema() -> Value {
    json!({
        "type": "string",
        "pattern": "^op_[0-9a-f]{32}$"
    })
}

fn interaction_context_id_schema() -> Value {
    json!({
        "type": "string",
        "pattern": "^ctx_[0-9a-f]{32}$"
    })
}

fn scoped_ref_schema() -> Value {
    json!({
        "type": "string",
        "pattern": "^ref_[0-9a-f]{32}$"
    })
}

fn browser_profile_mode_schema() -> Value {
    enum_string_schema(&["isolated_new", "isolated_named", "existing_profile"])
}

fn browser_profile_name_schema() -> Value {
    json!({ "type": "string", "minLength": 1, "maxLength": MAX_BROWSER_PROFILE_NAME_BYTES })
}

fn browser_query_schema() -> Value {
    json!({ "type": "string", "minLength": 1, "maxLength": MAX_BROWSER_QUERY_BYTES })
}

fn browser_url_schema() -> Value {
    json!({ "type": "string", "minLength": 1, "maxLength": MAX_BROWSER_URL_BYTES })
}

fn browser_text_or_empty_schema() -> Value {
    json!({ "type": "string", "maxLength": MAX_BROWSER_TEXT_BYTES })
}

fn browser_prompt_text_schema() -> Value {
    json!({ "type": "string", "maxLength": MAX_BROWSER_PROMPT_TEXT_BYTES })
}

fn browser_input_route_schema() -> Value {
    enum_string_schema(&["trusted", "dom_event"])
}

fn browser_type_mode_schema() -> Value {
    enum_string_schema(&["insert_text", "keystrokes"])
}

fn browser_dialog_action_schema() -> Value {
    enum_string_schema(&["inspect", "accept", "dismiss"])
}

fn browser_dialog_delivery_schema() -> Value {
    enum_string_schema(&["background", "foreground"])
}

fn browser_pointer_action_schema() -> Value {
    enum_string_schema(&["hover", "right_click", "double_click", "scroll", "drag"])
}

fn browser_scroll_delta_schema() -> Value {
    json!({
        "type": "integer",
        "minimum": -(MAX_BROWSER_SCROLL_DELTA_CSS_PX as i64),
        "maximum": MAX_BROWSER_SCROLL_DELTA_CSS_PX
    })
}

fn browser_click_target_schema() -> Value {
    json!({
        "oneOf": [
            {
                "type": "object",
                "properties": {
                    "kind": {"const": "element"},
                    "element_ref": scoped_ref_schema()
                },
                "required": ["kind", "element_ref"],
                "additionalProperties": false
            },
            {
                "type": "object",
                "properties": {
                    "kind": {"const": "viewport_css"},
                    "x": {"type": "integer"},
                    "y": {"type": "integer"}
                },
                "required": ["kind", "x", "y"],
                "additionalProperties": false
            }
        ]
    })
}

fn bounded_reason_schema() -> Value {
    json!({ "type": "string", "minLength": 1, "maxLength": 200 })
}

fn bounded_text_or_empty_schema() -> Value {
    json!({ "type": "string", "maxLength": MAX_TYPE_TEXT_BYTES })
}

fn string_schema() -> Value {
    json!({ "type": "string", "minLength": 1 })
}

fn enum_string_schema(values: &[&str]) -> Value {
    json!({ "type": "string", "enum": values })
}

fn pointer_button_schema() -> Value {
    enum_string_schema(&["left", "right", "middle"])
}

fn pointer_coordinate_space_schema() -> Value {
    enum_string_schema(&["desktop_physical", "window_physical"])
}

fn ui_element_action_schema() -> Value {
    enum_string_schema(&["press", "open", "show_menu", "pick", "confirm", "cancel"])
}

fn delivery_mode_schema() -> Value {
    enum_string_schema(&["background", "foreground"])
}

fn input_target_kind_schema() -> Value {
    enum_string_schema(&["desktop", "window", "window_point", "element"])
}

fn scroll_target_kind_schema() -> Value {
    enum_string_schema(&["window", "window_point", "desktop_point"])
}

fn keyboard_modifier_schema() -> Value {
    enum_string_schema(&["meta", "shift", "alt", "control", "function"])
}

fn keyboard_modifiers_schema() -> Value {
    bounded_array_schema(keyboard_modifier_schema(), 0, MAX_KEYBOARD_MODIFIERS as u64)
}

fn scroll_direction_schema() -> Value {
    enum_string_schema(&["up", "down", "left", "right"])
}

fn scroll_granularity_schema() -> Value {
    enum_string_schema(&["line", "page"])
}

fn keyboard_key_schema() -> Value {
    json!({ "type": "string", "minLength": 1, "maxLength": 16 })
}

fn bounded_menu_segment_schema() -> Value {
    json!({ "type": "string", "minLength": 1, "maxLength": MAX_MENU_SEGMENT_BYTES })
}

fn clipboard_text_schema() -> Value {
    json!({ "type": "string", "maxLength": MAX_CLIPBOARD_TEXT_BYTES })
}

fn array_schema(items: Value) -> Value {
    json!({ "type": "array", "items": items })
}

fn string_map_schema() -> Value {
    json!({ "type": "object", "additionalProperties": { "type": "string" } })
}

fn bounded_text_schema() -> Value {
    json!({ "type": "string", "minLength": 1, "maxLength": MAX_TYPE_TEXT_BYTES })
}

fn signed_integer_schema() -> Value {
    json!({ "type": "integer" })
}

fn positive_integer_schema() -> Value {
    json!({ "type": "integer", "minimum": 1 })
}

fn bounded_positive_integer_schema(maximum: u64) -> Value {
    json!({ "type": "integer", "minimum": 1, "maximum": maximum })
}

fn bounded_nonnegative_integer_schema(maximum: u64) -> Value {
    json!({ "type": "integer", "minimum": 0, "maximum": maximum })
}

fn boolean_schema() -> Value {
    json!({ "type": "boolean" })
}

fn bounded_ui_query_schema() -> Value {
    json!({ "type": "string", "minLength": 1, "maxLength": MAX_UI_QUERY_BYTES })
}

fn bounded_array_schema(items: Value, minimum: u64, maximum: u64) -> Value {
    json!({ "type": "array", "items": items, "minItems": minimum, "maxItems": maximum })
}

fn ui_role_schema() -> Value {
    json!({
        "type": "string",
        "enum": [
            "window", "button", "text", "text_field", "checkbox", "radio_button",
            "link", "menu", "menu_item", "toolbar", "tab", "list", "list_item",
            "table", "row", "cell", "group", "image", "slider"
        ]
    })
}

fn ui_selector_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "role": ui_role_schema(),
            "label_contains": bounded_ui_query_schema()
        },
        "additionalProperties": false
    })
}

fn ui_rect_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "x": {"type":"integer"},
            "y": {"type":"integer"},
            "width": {"type":"integer","minimum":1},
            "height": {"type":"integer","minimum":1}
        },
        "required": ["x","y","width","height"],
        "additionalProperties": false
    })
}

fn ui_predicate_schema() -> Value {
    json!({
        "oneOf": [
            {
                "type": "object",
                "properties": {"type":{"const":"window_exists"},"exists":{"type":"boolean"}},
                "required": ["type","exists"],
                "additionalProperties": false
            },
            {
                "type": "object",
                "properties": {"type":{"const":"window_bounds"},"bounds":ui_rect_schema(),"tolerance_px":{"type":"integer","minimum":0,"maximum":100}},
                "required": ["type","bounds","tolerance_px"],
                "additionalProperties": false
            },
            {
                "type": "object",
                "properties": {"type":{"const":"element_exists"},"selector":ui_selector_schema()},
                "required": ["type","selector"],
                "additionalProperties": false
            },
            {
                "type": "object",
                "properties": {
                    "type":{"const":"element_state"},
                    "selector":ui_selector_schema(),
                    "enabled":{"type":"boolean"},
                    "selected":{"type":"boolean"},
                    "value_equals":{"type":"string","maxLength":MAX_TYPE_TEXT_BYTES}
                },
                "required": ["type","selector"],
                "additionalProperties": false
            }
        ]
    })
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OperationIdArgs {
    operation_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EmptyArgs {}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ContextIdArgs {
    context_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ScreenshotArgs {
    context_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExpandInteractionScopeArgs {
    context_id: String,
    reason: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SetUiValueArgs {
    context_id: String,
    process_id: u32,
    window_id: u64,
    element_ref: String,
    value: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CaptureRegionArgs {
    context_id: Option<String>,
    process_id: u32,
    window_id: u64,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ListWindowsArgs {
    process_id: Option<u32>,
    #[serde(default)]
    on_screen_only: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LaunchApplicationArgs {
    identifier: Option<String>,
    name: Option<String>,
    #[serde(default)]
    targets: Vec<String>,
    #[serde(default)]
    new_instance: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct InspectWindowArgs {
    context_id: Option<String>,
    process_id: u32,
    window_id: u64,
    query: Option<String>,
    #[serde(default = "default_max_ui_elements")]
    max_elements: u32,
    #[serde(default = "default_max_ui_depth")]
    max_depth: u32,
    #[serde(default = "default_true")]
    include_screenshot: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct VerifyUiStateArgs {
    context_id: Option<String>,
    process_id: u32,
    window_id: u64,
    expect: Vec<UiPredicate>,
    #[serde(default = "default_verify_timeout_ms")]
    timeout_ms: u64,
    #[serde(default = "default_stable_samples")]
    stable_samples: u8,
    #[serde(default)]
    include_screenshot: bool,
}

fn default_max_ui_elements() -> u32 {
    500
}
fn default_max_ui_depth() -> u32 {
    25
}
fn default_verify_timeout_ms() -> u64 {
    5_000
}
fn default_stable_samples() -> u8 {
    2
}
fn default_true() -> bool {
    true
}

fn validate_window_args(process_id: u32, window_id: u64) -> Result<(), McpError> {
    if process_id == 0 || window_id == 0 {
        return Err(McpError::invalid_params(
            "process_id and window_id must be positive",
            None,
        ));
    }
    Ok(())
}

fn validate_launch_args(args: &LaunchApplicationArgs) -> Result<(), McpError> {
    let identifier = args
        .identifier
        .as_deref()
        .filter(|value| !value.trim().is_empty());
    let name = args
        .name
        .as_deref()
        .filter(|value| !value.trim().is_empty());
    if identifier.is_none() && name.is_none() {
        return Err(McpError::invalid_params(
            "identifier or name is required",
            None,
        ));
    }
    if identifier.is_some_and(|value| value.len() > 512)
        || name.is_some_and(|value| value.len() > 512)
        || args.targets.len() > 16
        || args
            .targets
            .iter()
            .any(|target| target.is_empty() || target.len() > 4096)
    {
        return Err(McpError::invalid_params(
            "Invalid application launch arguments",
            None,
        ));
    }
    Ok(())
}

fn validate_ui_predicates(predicates: &[UiPredicate]) -> Result<(), McpError> {
    if predicates.is_empty() || predicates.len() > MAX_UI_PREDICATES {
        return Err(McpError::invalid_params(
            "expect must contain 1..=8 predicates",
            None,
        ));
    }
    for predicate in predicates {
        match predicate {
            UiPredicate::WindowExists { .. } => {}
            UiPredicate::WindowBounds {
                bounds,
                tolerance_px,
            } => {
                if bounds.width == 0 || bounds.height == 0 || *tolerance_px > 100 {
                    return Err(McpError::invalid_params(
                        "Invalid window-bounds predicate",
                        None,
                    ));
                }
            }
            UiPredicate::ElementExists { selector } => validate_ui_selector(selector)?,
            UiPredicate::ElementState {
                selector,
                enabled,
                selected,
                value_equals,
            } => {
                validate_ui_selector(selector)?;
                if enabled.is_none() && selected.is_none() && value_equals.is_none() {
                    return Err(McpError::invalid_params(
                        "Element-state predicate requires a state",
                        None,
                    ));
                }
                if value_equals
                    .as_ref()
                    .is_some_and(|value| value.len() > MAX_TYPE_TEXT_BYTES)
                {
                    return Err(McpError::invalid_params(
                        "Element value predicate is too large",
                        None,
                    ));
                }
            }
        }
    }
    Ok(())
}

fn validate_ui_selector(selector: &crate::v2_m0::UiElementSelector) -> Result<(), McpError> {
    if selector.role.is_none() && selector.label_contains.is_none() {
        return Err(McpError::invalid_params(
            "UI selector cannot be empty",
            None,
        ));
    }
    if selector.role == Some(UiRole::Other)
        || selector
            .label_contains
            .as_ref()
            .is_some_and(|value| value.is_empty() || value.len() > MAX_UI_QUERY_BYTES)
    {
        return Err(McpError::invalid_params("Invalid UI selector", None));
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TypeTextArgs {
    context_id: Option<String>,
    text: String,
    target_kind: Option<String>,
    process_id: Option<u32>,
    window_id: Option<u64>,
    x: Option<i32>,
    y: Option<i32>,
    element_ref: Option<String>,
    delivery: Option<String>,
    delay_ms: Option<u16>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ClickArgs {
    context_id: Option<String>,
    x: Option<i32>,
    y: Option<i32>,
    element_ref: Option<String>,
    action: Option<String>,
    button: Option<String>,
    coordinate_space: Option<String>,
    process_id: Option<u32>,
    window_id: Option<u64>,
    click_count: Option<u8>,
    #[serde(default)]
    modifiers: Vec<String>,
    delivery: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DragArgs {
    context_id: Option<String>,
    from_x: i32,
    from_y: i32,
    to_x: i32,
    to_y: i32,
    duration_ms: u64,
    coordinate_space: Option<String>,
    process_id: Option<u32>,
    window_id: Option<u64>,
    button: Option<String>,
    #[serde(default)]
    modifiers: Vec<String>,
    delivery: Option<String>,
    steps: Option<u16>,
}

fn require_positive_process_id(process_id: u32) -> Result<(), McpError> {
    if process_id == 0 {
        Err(McpError::invalid_params(
            "process_id must be positive",
            None,
        ))
    } else {
        Ok(())
    }
}

fn parse_delivery_mode(value: Option<&str>) -> Result<InputDeliveryMode, McpError> {
    match value.unwrap_or("background") {
        "background" => Ok(InputDeliveryMode::Background),
        "foreground" => Ok(InputDeliveryMode::Foreground),
        _ => Err(McpError::invalid_params(
            "delivery must be background or foreground",
            None,
        )),
    }
}

fn parse_keyboard_modifiers(values: &[String]) -> Result<Vec<KeyboardModifier>, McpError> {
    if values.len() > MAX_KEYBOARD_MODIFIERS {
        return Err(McpError::invalid_params(
            "too many keyboard modifiers",
            None,
        ));
    }
    let mut output = Vec::with_capacity(values.len());
    for value in values {
        let modifier = match value.as_str() {
            "meta" => KeyboardModifier::Meta,
            "shift" => KeyboardModifier::Shift,
            "alt" => KeyboardModifier::Alt,
            "control" => KeyboardModifier::Control,
            "function" => KeyboardModifier::Function,
            _ => return Err(McpError::invalid_params("invalid keyboard modifier", None)),
        };
        if output.contains(&modifier) {
            return Err(McpError::invalid_params(
                "duplicate keyboard modifier",
                None,
            ));
        }
        output.push(modifier);
    }
    Ok(output)
}

fn validate_keyboard_key_input(key: &str) -> Result<(), McpError> {
    let named = [
        "return", "tab", "escape", "up", "down", "left", "right", "space", "delete", "home", "end",
        "pageup", "pagedown", "f1", "f2", "f3", "f4", "f5", "f6", "f7", "f8", "f9", "f10", "f11",
        "f12",
    ];
    let single_ascii = key.len() == 1 && key.as_bytes()[0].is_ascii_alphanumeric();
    if single_ascii || named.contains(&key) {
        Ok(())
    } else {
        Err(McpError::invalid_params("unsupported keyboard key", None))
    }
}

fn require_coordinate_pair(x: Option<i32>, y: Option<i32>) -> Result<(i32, i32), McpError> {
    match (x, y) {
        (Some(x), Some(y)) => Ok((x, y)),
        _ => Err(McpError::invalid_params(
            "coordinate click requires both x and y",
            None,
        )),
    }
}

fn parse_ui_element_action(value: &str) -> Result<UiElementAction, McpError> {
    match value {
        "press" => Ok(UiElementAction::Press),
        "open" => Ok(UiElementAction::Open),
        "show_menu" => Ok(UiElementAction::ShowMenu),
        "pick" => Ok(UiElementAction::Pick),
        "confirm" => Ok(UiElementAction::Confirm),
        "cancel" => Ok(UiElementAction::Cancel),
        _ => Err(McpError::invalid_params("invalid UI element action", None)),
    }
}

fn parse_click_target(
    context_id: Option<&str>,
    coordinate_space: Option<&str>,
    process_id: Option<u32>,
    window_id: Option<u64>,
    x: Option<i32>,
    y: Option<i32>,
    element_ref: Option<String>,
) -> Result<PointerTarget, McpError> {
    if let Some(element_ref) = element_ref {
        if context_id.is_none() {
            return Err(McpError::invalid_params(
                "element_ref click requires context_id",
                None,
            ));
        }
        if coordinate_space.is_some() || x.is_some() || y.is_some() {
            return Err(McpError::invalid_params(
                "element_ref click cannot include coordinate fields",
                None,
            ));
        }
        let process_id = process_id.ok_or_else(|| {
            McpError::invalid_params("element_ref click requires process_id", None)
        })?;
        let window_id = window_id.ok_or_else(|| {
            McpError::invalid_params("element_ref click requires window_id", None)
        })?;
        validate_window_args(process_id, window_id)?;
        return Ok(PointerTarget::Element {
            process_id,
            window_id,
            element_ref,
        });
    }
    let (x, y) = require_coordinate_pair(x, y)?;
    parse_pointer_target(coordinate_space, process_id, window_id, x, y)
}

fn validate_element_click_options(
    target: &PointerTarget,
    button: PointerButton,
    click_count: u8,
    action: Option<UiElementAction>,
    modifiers: &[String],
) -> Result<(), McpError> {
    if !matches!(target, PointerTarget::Element { .. }) {
        if action.is_some() {
            return Err(McpError::invalid_params(
                "action is valid only for element_ref clicks",
                None,
            ));
        }
        return Ok(());
    }
    if !(1..=2).contains(&click_count) {
        return Err(McpError::invalid_params(
            "element_ref click_count must be 1 or 2",
            None,
        ));
    }
    if click_count == 2
        && (button != PointerButton::Left || action.is_some() || !modifiers.is_empty())
    {
        return Err(McpError::invalid_params(
            "element_ref double click supports only an unmodified left press",
            None,
        ));
    }
    if action.is_some_and(|action| action != UiElementAction::Press)
        && (button != PointerButton::Left || !modifiers.is_empty())
    {
        return Err(McpError::invalid_params(
            "non-press element actions cannot combine with button overrides or modifiers",
            None,
        ));
    }
    Ok(())
}

fn parse_pointer_target(
    coordinate_space: Option<&str>,
    process_id: Option<u32>,
    window_id: Option<u64>,
    x: i32,
    y: i32,
) -> Result<PointerTarget, McpError> {
    match coordinate_space.unwrap_or_else(|| {
        if process_id.is_some() || window_id.is_some() {
            "window_physical"
        } else {
            "desktop_physical"
        }
    }) {
        "desktop_physical" => {
            if process_id.is_some() || window_id.is_some() {
                return Err(McpError::invalid_params(
                    "desktop_physical cannot include process_id or window_id",
                    None,
                ));
            }
            Ok(PointerTarget::DesktopPhysical { x, y })
        }
        "window_physical" => {
            let process_id = process_id.ok_or_else(|| {
                McpError::invalid_params("window_physical requires process_id", None)
            })?;
            let window_id = window_id.ok_or_else(|| {
                McpError::invalid_params("window_physical requires window_id", None)
            })?;
            validate_window_args(process_id, window_id)?;
            Ok(PointerTarget::WindowPhysical {
                process_id,
                window_id,
                x,
                y,
            })
        }
        _ => Err(McpError::invalid_params(
            "coordinate_space must be desktop_physical or window_physical",
            None,
        )),
    }
}

fn parse_input_target(
    target_kind: Option<&str>,
    process_id: Option<u32>,
    window_id: Option<u64>,
    x: Option<i32>,
    y: Option<i32>,
    element_ref: Option<String>,
) -> Result<InputTarget, McpError> {
    let inferred = target_kind.unwrap_or_else(|| {
        if element_ref.is_some() {
            "element"
        } else if x.is_some() || y.is_some() {
            "window_point"
        } else if process_id.is_some() || window_id.is_some() {
            "window"
        } else {
            "desktop"
        }
    });
    match inferred {
        "desktop" => {
            if process_id.is_some()
                || window_id.is_some()
                || x.is_some()
                || y.is_some()
                || element_ref.is_some()
            {
                return Err(McpError::invalid_params(
                    "desktop target cannot include process/window/point fields",
                    None,
                ));
            }
            Ok(InputTarget::Desktop)
        }
        "window" => {
            let process_id = process_id.ok_or_else(|| {
                McpError::invalid_params("window target requires process_id", None)
            })?;
            require_positive_process_id(process_id)?;
            if window_id == Some(0) || x.is_some() || y.is_some() || element_ref.is_some() {
                return Err(McpError::invalid_params("invalid window target", None));
            }
            Ok(InputTarget::Window {
                process_id,
                window_id,
            })
        }
        "window_point" => {
            let process_id = process_id.ok_or_else(|| {
                McpError::invalid_params("window_point requires process_id", None)
            })?;
            let window_id = window_id
                .ok_or_else(|| McpError::invalid_params("window_point requires window_id", None))?;
            let x = x.ok_or_else(|| McpError::invalid_params("window_point requires x", None))?;
            let y = y.ok_or_else(|| McpError::invalid_params("window_point requires y", None))?;
            if element_ref.is_some() {
                return Err(McpError::invalid_params(
                    "window_point cannot include element_ref",
                    None,
                ));
            }
            validate_window_args(process_id, window_id)?;
            Ok(InputTarget::WindowPoint {
                process_id,
                window_id,
                x,
                y,
            })
        }
        "element" => {
            let process_id = process_id.ok_or_else(|| {
                McpError::invalid_params("element target requires process_id", None)
            })?;
            let window_id = window_id.ok_or_else(|| {
                McpError::invalid_params("element target requires window_id", None)
            })?;
            let element_ref = element_ref.ok_or_else(|| {
                McpError::invalid_params("element target requires element_ref", None)
            })?;
            if x.is_some() || y.is_some() {
                return Err(McpError::invalid_params(
                    "element target cannot include coordinates",
                    None,
                ));
            }
            validate_window_args(process_id, window_id)?;
            Ok(InputTarget::Element {
                process_id,
                window_id,
                element_ref,
            })
        }
        _ => Err(McpError::invalid_params(
            "target_kind must be desktop, window, window_point, or element",
            None,
        )),
    }
}

fn parse_scroll_target(
    target_kind: Option<&str>,
    process_id: Option<u32>,
    window_id: Option<u64>,
    x: Option<i32>,
    y: Option<i32>,
) -> Result<ScrollTarget, McpError> {
    let inferred = target_kind.unwrap_or_else(|| {
        if process_id.is_some() && (x.is_some() || y.is_some()) {
            "window_point"
        } else if process_id.is_some() || window_id.is_some() {
            "window"
        } else if x.is_some() || y.is_some() {
            "desktop_point"
        } else {
            "window"
        }
    });
    match inferred {
        "window" => {
            let process_id = process_id.ok_or_else(|| {
                McpError::invalid_params("window scroll requires process_id", None)
            })?;
            require_positive_process_id(process_id)?;
            if window_id == Some(0) || x.is_some() || y.is_some() {
                return Err(McpError::invalid_params(
                    "invalid window scroll target",
                    None,
                ));
            }
            Ok(ScrollTarget::Window {
                process_id,
                window_id,
            })
        }
        "window_point" => {
            let process_id = process_id.ok_or_else(|| {
                McpError::invalid_params("window_point scroll requires process_id", None)
            })?;
            let window_id = window_id.ok_or_else(|| {
                McpError::invalid_params("window_point scroll requires window_id", None)
            })?;
            let x =
                x.ok_or_else(|| McpError::invalid_params("window_point scroll requires x", None))?;
            let y =
                y.ok_or_else(|| McpError::invalid_params("window_point scroll requires y", None))?;
            validate_window_args(process_id, window_id)?;
            Ok(ScrollTarget::WindowPoint {
                process_id,
                window_id,
                x,
                y,
            })
        }
        "desktop_point" => {
            if process_id.is_some() || window_id.is_some() {
                return Err(McpError::invalid_params(
                    "desktop_point cannot include process_id or window_id",
                    None,
                ));
            }
            let x =
                x.ok_or_else(|| McpError::invalid_params("desktop_point scroll requires x", None))?;
            let y =
                y.ok_or_else(|| McpError::invalid_params("desktop_point scroll requires y", None))?;
            Ok(ScrollTarget::DesktopPoint { x, y })
        }
        _ => Err(McpError::invalid_params(
            "target_kind must be window, window_point, or desktop_point",
            None,
        )),
    }
}

fn parse_scroll_direction(value: &str) -> Result<ScrollDirection, McpError> {
    match value {
        "up" => Ok(ScrollDirection::Up),
        "down" => Ok(ScrollDirection::Down),
        "left" => Ok(ScrollDirection::Left),
        "right" => Ok(ScrollDirection::Right),
        _ => Err(McpError::invalid_params("invalid scroll direction", None)),
    }
}

fn parse_scroll_granularity(value: Option<&str>) -> Result<ScrollGranularity, McpError> {
    match value.unwrap_or("line") {
        "line" => Ok(ScrollGranularity::Line),
        "page" => Ok(ScrollGranularity::Page),
        _ => Err(McpError::invalid_params("invalid scroll granularity", None)),
    }
}

fn validate_menu_path(path: &[String]) -> Result<(), McpError> {
    if path.is_empty()
        || path.len() > MAX_MENU_PATH_SEGMENTS
        || path
            .iter()
            .any(|segment| segment.trim().is_empty() || segment.len() > MAX_MENU_SEGMENT_BYTES)
    {
        return Err(McpError::invalid_params(
            "invalid application menu path",
            None,
        ));
    }
    Ok(())
}

fn parse_pointer_button(value: Option<&str>) -> Result<PointerButton, McpError> {
    match value.unwrap_or("left") {
        "left" => Ok(PointerButton::Left),
        "right" => Ok(PointerButton::Right),
        "middle" => Ok(PointerButton::Middle),
        _ => Err(McpError::invalid_params(
            "button must be left, right, or middle",
            None,
        )),
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProcessIdArgs {
    process_id: u32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ActivateWindowArgs {
    process_id: u32,
    window_id: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SetWindowFrameArgs {
    context_id: Option<String>,
    process_id: u32,
    window_id: u64,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct InvokeMenuArgs {
    context_id: Option<String>,
    process_id: u32,
    window_id: u64,
    path: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct KeyboardInputArgs {
    context_id: Option<String>,
    key: String,
    #[serde(default)]
    modifiers: Vec<String>,
    target_kind: Option<String>,
    process_id: Option<u32>,
    window_id: Option<u64>,
    x: Option<i32>,
    y: Option<i32>,
    element_ref: Option<String>,
    delivery: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ScrollArgs {
    context_id: Option<String>,
    direction: String,
    granularity: Option<String>,
    #[serde(default = "default_scroll_amount")]
    amount: u8,
    target_kind: Option<String>,
    process_id: Option<u32>,
    window_id: Option<u64>,
    x: Option<i32>,
    y: Option<i32>,
    delivery: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ClipboardReadArgs {
    context_id: Option<String>,
    #[serde(default)]
    include_text: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ClipboardWriteArgs {
    context_id: Option<String>,
    text: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PointerPositionArgs {
    context_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MovePointerArgs {
    context_id: String,
    x: i32,
    y: i32,
}

fn default_scroll_amount() -> u8 {
    3
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExecuteProcessArgs {
    #[serde(default)]
    operation_id: Option<String>,
    program: String,
    #[serde(default)]
    args: Vec<String>,
    cwd: String,
    #[serde(default)]
    env: BTreeMap<String, String>,
    timeout_ms: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ShellArgs {
    #[serde(default)]
    operation_id: Option<String>,
    command: String,
    cwd: String,
    #[serde(default)]
    env: BTreeMap<String, String>,
    timeout_ms: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PathArgs {
    path: String,
}

fn parse_arguments<T: DeserializeOwned>(arguments: Option<JsonObject>) -> Result<T, McpError> {
    serde_json::from_value(Value::Object(arguments.unwrap_or_default()))
        .map_err(|_| McpError::invalid_params("Tool arguments do not match the input schema", None))
}

fn env_map(env: BTreeMap<String, String>) -> Vec<ProcessEnvVar> {
    env.into_iter()
        .map(|(key, value)| ProcessEnvVar { key, value })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NorthboundConfigError {
    InvalidUrl,
    HttpsRequired,
    InvalidResourceUri,
    InvalidAuthorizationServerUri,
    InvalidTrustedProxyIssuerUri,
    InvalidTrustedProxyPrincipal,
    InvalidScope,
    DuplicateScope,
}

impl fmt::Display for NorthboundConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for NorthboundConfigError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone)]
    struct AuditCaptureWriter(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    struct AuditCaptureSink(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for AuditCaptureSink {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for AuditCaptureWriter {
        type Writer = AuditCaptureSink;

        fn make_writer(&'a self) -> Self::Writer {
            AuditCaptureSink(self.0.clone())
        }
    }

    #[test]
    fn caller_supplied_client_info_is_bounded_and_distinguishes_operations() {
        let bytes = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .without_time()
            .with_writer(AuditCaptureWriter(bytes.clone()))
            .finish();
        let alpha = Implementation::new("operator-cli", "1.2.3");
        let beta = Implementation::new("automation-worker", "9.8.7");
        tracing::subscriber::with_default(subscriber, || {
            log_northbound_client_initialized(&alpha);
            log_northbound_operation_requested(
                "op-alpha",
                "dev-audit",
                DeviceCapability::Shell,
                Some(alpha.clone()),
            );
            log_northbound_client_initialized(&beta);
            log_northbound_operation_requested(
                "op-beta",
                "dev-audit",
                DeviceCapability::Shell,
                Some(beta.clone()),
            );
        });
        let log = String::from_utf8(bytes.lock().unwrap().clone()).unwrap();
        for expected in [
            "v2_northbound_client_initialized",
            "v2_northbound_operation_requested",
            "operator-cli",
            "1.2.3",
            "automation-worker",
            "9.8.7",
            "op-alpha",
            "op-beta",
            "mcp_client_info_untrusted",
        ] {
            assert!(
                log.contains(expected),
                "missing client audit field {expected}"
            );
        }
        let unsafe_client = Implementation::new(format!("{}\nforged", "x".repeat(256)), "\r\n");
        let audit = NorthboundClientAudit::from_implementation(&unsafe_client);
        assert!(audit.name.len() <= MAX_AUDIT_CLIENT_NAME_BYTES);
        assert!(!audit.name.contains('\n'));
        assert_eq!(audit.version, "unknown");
    }

    #[test]
    fn protected_resource_metadata_uses_mcp_endpoint_path_insertion() {
        let config = NorthboundMcpConfig::new(
            "https://hub.example/public/mcp",
            "https://auth.example/tenant",
            vec!["mcp:use".into()],
        )
        .unwrap();
        assert_eq!(config.mcp_path(), "/public/mcp");
        assert_eq!(
            config.metadata_url(),
            "https://hub.example/.well-known/oauth-protected-resource/public/mcp"
        );
        assert_eq!(
            config.protected_resource_metadata(),
            json!({
                "resource": "https://hub.example/public/mcp",
                "authorization_servers": ["https://auth.example/tenant"],
                "scopes_supported": ["mcp:use"],
                "bearer_methods_supported": ["header"]
            })
        );
    }

    #[test]
    fn root_resource_uses_root_protected_resource_metadata_endpoint() {
        assert_eq!(
            protected_resource_metadata_path("/"),
            "/.well-known/oauth-protected-resource"
        );
    }

    #[test]
    fn config_requires_https_and_header_safe_explicit_scopes() {
        assert!(
            NorthboundMcpConfig::new(
                "http://hub.example/mcp",
                "https://auth.example",
                vec!["mcp:use".into()]
            )
            .is_err()
        );
        assert!(
            NorthboundMcpConfig::new(
                "https://hub.example/mcp",
                "https://auth.example",
                vec!["bad\"scope".into()]
            )
            .is_err()
        );
    }

    #[test]
    fn trusted_proxy_config_requires_https_and_nonempty_fixed_principal() {
        assert!(
            TrustedProxyConfig::new(
                "https://hub.example/mcp",
                "https://access.example",
                "single-principal",
            )
            .is_ok()
        );
        assert!(
            TrustedProxyConfig::new(
                "http://hub.example/mcp",
                "https://access.example",
                "single-principal",
            )
            .is_err()
        );
        assert!(
            TrustedProxyConfig::new("https://hub.example/mcp", "https://access.example", "",)
                .is_err()
        );
    }

    #[tokio::test]
    async fn trusted_proxy_uses_only_configured_principal_and_strips_identity_credentials() {
        let configured =
            AuthenticatedClientPrincipal::new("https://access.example", "fixed-user").unwrap();
        let app = Router::new()
            .route(
                "/mcp",
                get(|request: Request| async move {
                    let principal = request
                        .extensions()
                        .get::<NorthboundAuthContext>()
                        .map(|auth| auth.principal.clone());
                    Json(json!({
                        "issuer": principal.as_ref().map(|p| p.issuer.as_str()),
                        "subject": principal.as_ref().map(|p| p.subject.as_str()),
                        "authorization_visible": request.headers().contains_key(AUTHORIZATION),
                        "cf_jwt_visible": request.headers().contains_key("cf-access-jwt-assertion"),
                        "x_user_visible": request.headers().contains_key("x-user")
                    }))
                }),
            )
            .layer(middleware::from_fn_with_state(
                TrustedProxyAuthState {
                    principal: configured,
                    resource_url: Url::parse("https://hub.example/mcp").unwrap(),
                },
                trusted_proxy_guard,
            ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let response = Client::new()
            .get(format!("http://{address}/mcp"))
            .header(AUTHORIZATION, "Bearer caller-controlled")
            .header("Cf-Access-Jwt-Assertion", "proxy-credential")
            .header("X-User", "attacker-selected-user")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body: Value = response.json().await.unwrap();
        assert_eq!(body["issuer"], "https://access.example");
        assert_eq!(body["subject"], "fixed-user");
        assert_eq!(body["authorization_visible"], false);
        assert_eq!(body["cf_jwt_visible"], false);
        assert_eq!(body["x_user_visible"], false);
        task.abort();
    }

    #[test]
    fn bearer_parser_rejects_duplicates_and_malformed_values() {
        let mut headers = HeaderMap::new();
        assert_eq!(bearer_token(&headers).unwrap(), None);
        headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer abc"));
        assert_eq!(bearer_token(&headers).unwrap(), Some("abc"));
        headers.append(AUTHORIZATION, HeaderValue::from_static("Bearer def"));
        assert!(bearer_token(&headers).is_err());

        let mut malformed = HeaderMap::new();
        malformed.insert(AUTHORIZATION, HeaderValue::from_static("Basic abc"));
        assert!(bearer_token(&malformed).is_err());
    }

    #[test]
    fn query_bearer_tokens_are_detected() {
        assert!(query_contains_access_token(Some("x=1&access_token=secret")));
        assert!(!query_contains_access_token(Some("token=secret&x=1")));
    }

    #[test]
    fn audience_binding_accepts_only_same_canonical_resource() {
        assert!(canonical_resource_eq(
            "HTTPS://HUB.EXAMPLE/mcp",
            "https://hub.example/mcp"
        ));
        assert!(!canonical_resource_eq(
            "https://hub.example/other",
            "https://hub.example/mcp"
        ));
        assert!(!canonical_resource_eq(
            "https://other.example/mcp",
            "https://hub.example/mcp"
        ));
    }

    #[test]
    fn introspection_requires_active_subject_audience_and_optional_matching_issuer() {
        let verifier = OAuthIntrospectionVerifier::new(OAuthIntrospectionConfig::new(
            "https://auth.example",
            "https://hub.example/mcp",
            "https://auth.example/introspect",
            "hub",
            "secret",
        ))
        .unwrap();
        let verified = verifier
            .validate_response(IntrospectionResponse {
                active: true,
                sub: Some("user-1".into()),
                aud: Some(AudienceClaim::One("https://hub.example/mcp".into())),
                scope: Some("mcp:use extra".into()),
                exp: Some(u64::MAX),
                iss: Some("https://auth.example".into()),
            })
            .unwrap();
        assert_eq!(verified.principal.subject, "user-1");
        assert!(verified.scopes.contains("mcp:use"));

        assert!(matches!(
            verifier.validate_response(IntrospectionResponse {
                active: true,
                sub: Some("user-1".into()),
                aud: Some(AudienceClaim::One("https://other.example/mcp".into())),
                scope: Some("mcp:use".into()),
                exp: Some(u64::MAX),
                iss: None,
            }),
            Err(TokenVerificationError::InvalidToken)
        ));
    }

    #[test]
    fn policy_is_exact_principal_device_capability_and_rejects_wrong_device() {
        let doc = NorthboundPolicyDocument::from_json(
            r#"{"grants":[{"issuer":"https://auth.example","subject":"u1","device_id":"dev-a","capabilities":["read_file"]}]}"#,
        )
        .unwrap();
        let policy = doc.build_policy("https://auth.example", "dev-a").unwrap();
        let principal = AuthenticatedClientPrincipal::new("https://auth.example", "u1").unwrap();
        assert!(
            policy
                .authorize_device_capability(&principal, "dev-a", DeviceCapability::ReadFile)
                .is_ok()
        );
        assert!(
            policy
                .authorize_device_capability(&principal, "dev-a", DeviceCapability::Shell)
                .is_err()
        );

        let wrong = NorthboundPolicyDocument::from_json(
            r#"{"grants":[{"issuer":"https://auth.example","subject":"u1","device_id":"dev-b","capabilities":["read_file"]}]}"#,
        )
        .unwrap();
        assert!(matches!(
            wrong.build_policy("https://auth.example", "dev-a"),
            Err(NorthboundPolicyError::DeviceMismatch)
        ));
    }

    #[test]
    fn exact_policy_filters_mcp_tool_discovery() {
        let principal = AuthenticatedClientPrincipal::new("https://auth.example", "u1").unwrap();
        let mut policy = ClientAuthorizationPolicy::default();
        policy.allow_device_capability(&principal, "dev-a", DeviceCapability::ReadFile);
        let visible: Vec<_> = all_tools()
            .into_iter()
            .filter(|tool| {
                tool_capability(tool.name.as_ref()).is_some_and(|capability| {
                    policy
                        .authorize_device_capability(&principal, "dev-a", capability)
                        .is_ok()
                })
            })
            .map(|tool| tool.name.to_string())
            .collect();
        assert_eq!(visible, vec![TOOL_READ_FILE.to_owned()]);
        assert!(!visible.contains(&TOOL_SCREENSHOT.to_owned()));
        assert!(!visible.contains(&TOOL_TYPE_TEXT.to_owned()));
    }

    #[test]
    fn exact_screenshot_authorization_does_not_expose_type_text() {
        let principal = AuthenticatedClientPrincipal::new("https://auth.example", "u1").unwrap();
        let mut policy = ClientAuthorizationPolicy::default();
        policy.allow_device_capability(&principal, "dev-a", DeviceCapability::Screenshot);
        let visible: Vec<_> = all_tools()
            .into_iter()
            .filter(|tool| {
                tool_capability(tool.name.as_ref()).is_some_and(|capability| {
                    policy
                        .authorize_device_capability(&principal, "dev-a", capability)
                        .is_ok()
                })
            })
            .map(|tool| tool.name.to_string())
            .collect();
        assert_eq!(visible, vec![TOOL_SCREENSHOT.to_owned()]);
    }

    #[derive(Debug)]
    struct ExactTestAuthorizer {
        principal: AuthenticatedClientPrincipal,
        device_id: String,
        capability: DeviceCapability,
    }

    impl DeviceCapabilityAuthorizer for ExactTestAuthorizer {
        fn authorize_device_capability(
            &self,
            principal: &AuthenticatedClientPrincipal,
            device_id: &str,
            capability: DeviceCapability,
        ) -> Result<(), TrustError> {
            if principal == &self.principal
                && device_id == self.device_id
                && capability == self.capability
            {
                Ok(())
            } else {
                Err(TrustError::ClientDeviceCapabilityDenied)
            }
        }
    }

    #[test]
    fn replaceable_authorizer_keeps_exact_principal_device_capability_boundary() {
        let principal = AuthenticatedClientPrincipal::new("https://auth.example", "u1").unwrap();
        let authorizer = ExactTestAuthorizer {
            principal: principal.clone(),
            device_id: "dev-a".into(),
            capability: DeviceCapability::ReadFile,
        };
        assert!(
            authorizer
                .authorize_device_capability(&principal, "dev-a", DeviceCapability::ReadFile)
                .is_ok()
        );
        assert!(
            authorizer
                .authorize_device_capability(&principal, "dev-a", DeviceCapability::Shell)
                .is_err()
        );
        assert!(
            authorizer
                .authorize_device_capability(&principal, "dev-b", DeviceCapability::ReadFile)
                .is_err()
        );
        let other = AuthenticatedClientPrincipal::new("https://auth.example", "u2").unwrap();
        assert!(
            authorizer
                .authorize_device_capability(&other, "dev-a", DeviceCapability::ReadFile)
                .is_err()
        );
    }

    #[derive(Clone)]
    struct FakeVerifier {
        result: Result<VerifiedAccessToken, TokenVerificationError>,
    }

    #[async_trait]
    impl AccessTokenVerifier for FakeVerifier {
        async fn verify(
            &self,
            _token: &str,
        ) -> Result<VerifiedAccessToken, TokenVerificationError> {
            self.result.clone()
        }
    }

    fn test_auth_state(
        result: Result<VerifiedAccessToken, TokenVerificationError>,
    ) -> NorthboundAuthState {
        NorthboundAuthState {
            verifier: Arc::new(FakeVerifier { result }),
            config: Arc::new(
                NorthboundMcpConfig::new(
                    "https://hub.example/mcp",
                    "https://auth.example",
                    vec!["mcp:use".into()],
                )
                .unwrap(),
            ),
        }
    }

    async fn spawn_guarded_test_server(
        result: Result<VerifiedAccessToken, TokenVerificationError>,
    ) -> (String, tokio::task::JoinHandle<()>) {
        let app = Router::new()
            .route(
                "/mcp",
                get(|request: Request| async move {
                    let principal = request
                        .extensions()
                        .get::<NorthboundAuthContext>()
                        .map(|auth| auth.principal.subject.clone());
                    let bearer_visible = request.headers().contains_key(AUTHORIZATION);
                    Json(json!({
                        "principal": principal,
                        "bearer_visible_downstream": bearer_visible
                    }))
                }),
            )
            .layer(middleware::from_fn_with_state(
                test_auth_state(result),
                oauth_resource_guard,
            ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://{address}/mcp"), task)
    }

    fn verified(scopes: &[&str]) -> VerifiedAccessToken {
        VerifiedAccessToken {
            principal: AuthenticatedClientPrincipal::new("https://auth.example", "user-1").unwrap(),
            scopes: scopes.iter().map(|scope| (*scope).to_owned()).collect(),
        }
    }

    #[tokio::test]
    async fn oauth_guard_emits_standard_challenges_and_strips_bearer_before_mcp() {
        let client = Client::new();

        let (url, task) = spawn_guarded_test_server(Ok(verified(&["mcp:use"]))).await;
        let missing = client.get(&url).send().await.unwrap();
        assert_eq!(missing.status(), StatusCode::UNAUTHORIZED);
        let challenge = missing
            .headers()
            .get(WWW_AUTHENTICATE)
            .unwrap()
            .to_str()
            .unwrap();
        assert!(challenge.starts_with("Bearer"));
        assert!(challenge.contains(
            "resource_metadata=\"https://hub.example/.well-known/oauth-protected-resource/mcp\""
        ));
        assert!(challenge.contains("scope=\"mcp:use\""));

        let query = client
            .get(format!("{url}?access_token=must-not-be-used"))
            .send()
            .await
            .unwrap();
        assert_eq!(query.status(), StatusCode::BAD_REQUEST);

        let accepted = client
            .get(&url)
            .bearer_auth("northbound-secret-token")
            .send()
            .await
            .unwrap();
        assert_eq!(accepted.status(), StatusCode::OK);
        let body: Value = accepted.json().await.unwrap();
        assert_eq!(body["principal"], "user-1");
        assert_eq!(body["bearer_visible_downstream"], false);
        task.abort();

        let (url, task) = spawn_guarded_test_server(Ok(verified(&["other"]))).await;
        let insufficient = client.get(&url).bearer_auth("token").send().await.unwrap();
        assert_eq!(insufficient.status(), StatusCode::FORBIDDEN);
        assert!(
            insufficient
                .headers()
                .get(WWW_AUTHENTICATE)
                .unwrap()
                .to_str()
                .unwrap()
                .contains("error=\"insufficient_scope\"")
        );
        task.abort();

        let (url, task) =
            spawn_guarded_test_server(Err(TokenVerificationError::InvalidToken)).await;
        let invalid = client
            .get(&url)
            .bearer_auth("invalid")
            .send()
            .await
            .unwrap();
        assert_eq!(invalid.status(), StatusCode::UNAUTHORIZED);
        assert!(
            invalid
                .headers()
                .get(WWW_AUTHENTICATE)
                .unwrap()
                .to_str()
                .unwrap()
                .contains("error=\"invalid_token\"")
        );
        task.abort();
    }

    fn temp_state_dir(name: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "cumg-northbound-{name}-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        std::fs::create_dir_all(&path).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        path
    }

    #[tokio::test]
    async fn browser_publicizer_never_leaks_backend_target_tab_or_page_refs() {
        use crate::{
            v2_m0::{DeviceIdentity, GrantAuthority},
            v2_m0_transport::HubIdentity,
            v2_m1_hub::{HubProvisionedMaterial, HubServiceConfig, SingleDeviceHub},
        };

        let device_identity = DeviceIdentity::generate();
        let state_dir = temp_state_dir("browser-publicizer");
        let (hub, handle) = SingleDeviceHub::new(
            HubServiceConfig {
                state_dir: state_dir.clone(),
                heartbeat_timeout: Duration::from_secs(1),
                max_agent_session_lifetime: Duration::from_secs(60 * 60),
                agent_session_reauth_drain: Duration::from_secs(30),
                checkpoint_generation_rollover_bytes: 512 * 1024,
                max_queued_per_device: 1,
                max_agent_sessions: 2,
                max_agent_session_starts_per_minute: 30,
            },
            HubProvisionedMaterial {
                hub_identity: HubIdentity::generate(),
                grant_signer: GrantAuthority::generate().into(),
                device_verifier: device_identity.verifying_key(),
                device_rotation: None,
            },
        )
        .unwrap();
        let device_id = hub.device_id().to_owned();
        let principal =
            AuthenticatedClientPrincipal::new("https://auth.example", "browser-user").unwrap();
        let service = V2NorthboundMcp::new(handle, ClientAuthorizationPolicy::default());
        let binding = InteractionContextManager::new(InteractionContextLimits::default())
            .unwrap()
            .open(&principal, &device_id, 7, 11, 1)
            .unwrap();

        let bind_command = BrowserBackendCommand::Bind {
            context_id: binding.id.as_str().to_owned(),
            process_id: 42,
            window_id: 9,
        };
        let bind_response = service
            .publicize_browser_result(
                PreparedBrowserCall {
                    binding: binding.clone(),
                    command: bind_command,
                    public_target_ref: None,
                    public_tab_ref: None,
                    public_dialog_ref: None,
                },
                DeviceResult::Browser {
                    result: BrowserBackendResult::Bound {
                        backend_target_id: "RAW_TARGET_BIND_SECRET".into(),
                        process_id: 42,
                        window_id: 9,
                        tabs: vec![crate::v2_browser_runtime::BrowserBackendTab {
                            backend_tab_id: "RAW_TAB_BIND_SECRET".into(),
                            title: Some("Example".into()),
                            url: Some("https://example.com".into()),
                            active: Some(true),
                        }],
                    },
                },
            )
            .await
            .unwrap();
        let bind_wire = match bind_response {
            CallToolResponse::Complete(result) => serde_json::to_string(&result).unwrap(),
            other => panic!("unexpected browser response: {other:?}"),
        };
        assert!(!bind_wire.contains("RAW_TARGET_BIND_SECRET"));
        assert!(!bind_wire.contains("RAW_TAB_BIND_SECRET"));
        assert!(bind_wire.contains("ref_"));

        let (target_ref, tab_ref) = {
            let mut state = service.interactions.lock().await;
            let target_ref = state
                .browser_refs
                .mint_target(&binding, "RAW_TARGET_SNAPSHOT_SECRET")
                .unwrap();
            let tab_ref = state
                .browser_refs
                .mint_tab(&binding, &target_ref, "RAW_TAB_SNAPSHOT_SECRET")
                .unwrap();
            (target_ref, tab_ref)
        };
        let inspect_command = BrowserBackendCommand::Inspect {
            context_id: binding.id.as_str().to_owned(),
            backend_target_id: "RAW_TARGET_SNAPSHOT_SECRET".into(),
            backend_tab_id: "RAW_TAB_SNAPSHOT_SECRET".into(),
            backend_scope_ref: None,
            query: None,
            backend_continuation: None,
            include_screenshot: false,
        };
        let snapshot_response = service
            .publicize_browser_result(
                PreparedBrowserCall {
                    binding: binding.clone(),
                    command: inspect_command,
                    public_target_ref: Some(target_ref),
                    public_tab_ref: Some(tab_ref),
                    public_dialog_ref: None,
                },
                DeviceResult::Browser {
                    result: BrowserBackendResult::Snapshot {
                        backend_snapshot_id: "RAW_SNAPSHOT_SECRET".into(),
                        outline: "button Example".into(),
                        action_refs: vec![BrowserBackendSemanticRef {
                            backend_ref: "RAW_ACTION_REF_SECRET".into(),
                            role: "button".into(),
                            name: Some("Example".into()),
                            value: None,
                            states: vec![],
                            actions: vec![BrowserAction::Click],
                            frame: "main".into(),
                            visibility: "visible".into(),
                        }],
                        content_refs: vec![BrowserBackendSemanticRef {
                            backend_ref: "RAW_CONTENT_REF_SECRET".into(),
                            role: "text".into(),
                            name: Some("Content".into()),
                            value: None,
                            states: vec![],
                            actions: vec![],
                            frame: "main".into(),
                            visibility: "visible".into(),
                        }],
                        complete: false,
                        omitted: 1,
                        backend_continuation: Some("RAW_CONTINUATION_SECRET".into()),
                        screenshot: None,
                    },
                },
            )
            .await
            .unwrap();
        let snapshot_wire = match snapshot_response {
            CallToolResponse::Complete(result) => serde_json::to_string(&result).unwrap(),
            other => panic!("unexpected browser response: {other:?}"),
        };
        for secret in [
            "RAW_TARGET_SNAPSHOT_SECRET",
            "RAW_TAB_SNAPSHOT_SECRET",
            "RAW_SNAPSHOT_SECRET",
            "RAW_ACTION_REF_SECRET",
            "RAW_CONTENT_REF_SECRET",
            "RAW_CONTINUATION_SECRET",
        ] {
            assert!(!snapshot_wire.contains(secret), "leaked {secret}");
        }
        assert!(snapshot_wire.contains("ref_"));

        drop(service);
        drop(hub);
        let _ = std::fs::remove_dir_all(state_dir);
    }

    #[tokio::test]
    async fn operation_recovery_remains_discoverable_while_agent_is_offline() {
        use crate::{
            v2_m0::{DeviceIdentity, GrantAuthority},
            v2_m0_transport::HubIdentity,
            v2_m1_hub::{HubProvisionedMaterial, HubServiceConfig, SingleDeviceHub},
        };

        let device_identity = DeviceIdentity::generate();
        let state_dir = temp_state_dir("offline-recovery-discovery");
        let (hub, handle) = SingleDeviceHub::new(
            HubServiceConfig {
                state_dir: state_dir.clone(),
                heartbeat_timeout: Duration::from_secs(1),
                max_agent_session_lifetime: Duration::from_secs(60 * 60),
                agent_session_reauth_drain: Duration::from_secs(30),
                checkpoint_generation_rollover_bytes: 512 * 1024,
                max_queued_per_device: 1,
                max_agent_sessions: 2,
                max_agent_session_starts_per_minute: 30,
            },
            HubProvisionedMaterial {
                hub_identity: HubIdentity::generate(),
                grant_signer: GrantAuthority::generate().into(),
                device_verifier: device_identity.verifying_key(),
                device_rotation: None,
            },
        )
        .unwrap();
        let principal =
            AuthenticatedClientPrincipal::new("https://auth.example", "recovery-user").unwrap();
        let mut policy = ClientAuthorizationPolicy::default();
        policy.allow_device_capability(&principal, handle.device_id(), DeviceCapability::Shell);
        let service = V2NorthboundMcp::new(handle, policy);

        let names: Vec<_> = service
            .tools_for(&principal)
            .await
            .into_iter()
            .map(|tool| tool.name.to_string())
            .collect();
        assert_eq!(names, vec![TOOL_GET_OPERATION.to_owned()]);

        drop(service);
        drop(hub);
        let _ = std::fs::remove_dir_all(state_dir);
    }

    #[tokio::test]
    async fn authenticated_2026_mcp_request_exposes_no_device_tools_without_live_agent() {
        use crate::{
            v2_m0::{DeviceIdentity, GrantAuthority},
            v2_m0_transport::HubIdentity,
            v2_m1_hub::{HubProvisionedMaterial, HubServiceConfig, SingleDeviceHub},
        };

        let device_identity = DeviceIdentity::generate();
        let state_dir = temp_state_dir("mcp-http");
        let (hub, handle) = SingleDeviceHub::new(
            HubServiceConfig {
                state_dir: state_dir.clone(),
                heartbeat_timeout: Duration::from_secs(1),
                max_agent_session_lifetime: Duration::from_secs(60 * 60),
                agent_session_reauth_drain: Duration::from_secs(30),
                checkpoint_generation_rollover_bytes: 512 * 1024,
                max_queued_per_device: 1,
                max_agent_sessions: 2,
                max_agent_session_starts_per_minute: 30,
            },
            HubProvisionedMaterial {
                hub_identity: HubIdentity::generate(),
                grant_signer: GrantAuthority::generate().into(),
                device_verifier: device_identity.verifying_key(),
                device_rotation: None,
            },
        )
        .unwrap();
        let device_id = hub.device_id().to_owned();
        let principal =
            AuthenticatedClientPrincipal::new("https://auth.example", "user-1").unwrap();
        let mut policy = ClientAuthorizationPolicy::default();
        policy.allow_device_capability(&principal, &device_id, DeviceCapability::ReadFile);

        let config = NorthboundMcpConfig::new(
            "https://hub.example/mcp",
            "https://auth.example",
            vec!["mcp:use".into()],
        )
        .unwrap();
        let router = build_northbound_router(
            V2NorthboundMcp::new(handle, policy),
            config,
            Arc::new(FakeVerifier {
                result: Ok(verified(&["mcp:use"])),
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });

        let rejected = Client::new()
            .post(format!("http://{address}/mcp"))
            .bearer_auth("northbound-only-token")
            .header("Origin", "https://evil.example")
            .header("MCP-Protocol-Version", "2026-07-28")
            .header("Mcp-Method", "tools/list")
            .header("Accept", "application/json, text/event-stream")
            .json(&json!({
                "jsonrpc": "2.0",
                "id": 0,
                "method": "tools/list",
                "params": {
                    "_meta": {
                        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                        "io.modelcontextprotocol/clientCapabilities": {}
                    }
                }
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(rejected.status(), StatusCode::FORBIDDEN);

        let wrong_port = Client::new()
            .post(format!("http://{address}/mcp"))
            .bearer_auth("northbound-only-token")
            .header("Origin", "https://hub.example:8443")
            .header("MCP-Protocol-Version", "2026-07-28")
            .header("Mcp-Method", "tools/list")
            .header("Accept", "application/json, text/event-stream")
            .json(&json!({"jsonrpc":"2.0","id":0,"method":"tools/list","params":{}}))
            .send()
            .await
            .unwrap();
        assert_eq!(wrong_port.status(), StatusCode::FORBIDDEN);

        let response = Client::new()
            .post(format!("http://{address}/mcp"))
            .bearer_auth("northbound-only-token")
            .header("Origin", "https://hub.example")
            .header("MCP-Protocol-Version", "2026-07-28")
            .header("Mcp-Method", "tools/list")
            .header("Accept", "application/json, text/event-stream")
            .json(&json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/list",
                "params": {
                    "_meta": {
                        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                        "io.modelcontextprotocol/clientCapabilities": {}
                    }
                }
            }))
            .send()
            .await
            .unwrap();
        let status = response.status();
        let raw = response.text().await.unwrap();
        assert_eq!(status, StatusCode::OK, "unexpected MCP response: {raw}");
        let body: Value = serde_json::from_str(&raw)
            .unwrap_or_else(|error| panic!("non-JSON MCP response ({error}): {raw}"));
        let tools = body["result"]["tools"].as_array().unwrap();
        let names: Vec<_> = tools
            .iter()
            .filter_map(|tool| tool["name"].as_str())
            .collect();
        // Authentication and policy allow are necessary but not sufficient. This
        // fixture has no live Agent advertisement, so discovery must stay empty.
        assert!(names.is_empty());

        task.abort();
        drop(hub);
        let _ = std::fs::remove_dir_all(state_dir);
    }

    #[tokio::test]
    async fn trusted_proxy_mcp_ignores_caller_identity_but_requires_live_agent() {
        use crate::{
            v2_m0::{DeviceIdentity, GrantAuthority},
            v2_m0_transport::HubIdentity,
            v2_m1_hub::{HubProvisionedMaterial, HubServiceConfig, SingleDeviceHub},
        };

        let device_identity = DeviceIdentity::generate();
        let state_dir = temp_state_dir("trusted-proxy-mcp-http");
        let (hub, handle) = SingleDeviceHub::new(
            HubServiceConfig {
                state_dir: state_dir.clone(),
                heartbeat_timeout: Duration::from_secs(1),
                max_agent_session_lifetime: Duration::from_secs(60 * 60),
                agent_session_reauth_drain: Duration::from_secs(30),
                checkpoint_generation_rollover_bytes: 512 * 1024,
                max_queued_per_device: 1,
                max_agent_sessions: 2,
                max_agent_session_starts_per_minute: 30,
            },
            HubProvisionedMaterial {
                hub_identity: HubIdentity::generate(),
                grant_signer: GrantAuthority::generate().into(),
                device_verifier: device_identity.verifying_key(),
                device_rotation: None,
            },
        )
        .unwrap();
        let device_id = hub.device_id().to_owned();
        let proxy_config = TrustedProxyConfig::new(
            "https://hub.example/mcp",
            "https://access.example",
            "fixed-user",
        )
        .unwrap();
        let mut policy = ClientAuthorizationPolicy::default();
        policy.allow_device_capability(
            proxy_config.principal(),
            &device_id,
            DeviceCapability::ReadFile,
        );
        let router = build_trusted_proxy_router(V2NorthboundMcp::new(handle, policy), proxy_config);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });

        let rejected = Client::new()
            .post(format!("http://{address}/mcp"))
            .header("Origin", "https://evil.example")
            .header("X-User", "attacker-selected-user")
            .header("MCP-Protocol-Version", "2026-07-28")
            .header("Mcp-Method", "tools/list")
            .header("Accept", "application/json, text/event-stream")
            .json(&json!({
                "jsonrpc": "2.0",
                "id": 0,
                "method": "tools/list",
                "params": {
                    "_meta": {
                        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                        "io.modelcontextprotocol/clientCapabilities": {}
                    }
                }
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(rejected.status(), StatusCode::FORBIDDEN);

        let wrong_port = Client::new()
            .post(format!("http://{address}/mcp"))
            .header("Origin", "https://hub.example:8443")
            .header("X-User", "attacker-selected-user")
            .header("MCP-Protocol-Version", "2026-07-28")
            .header("Mcp-Method", "tools/list")
            .header("Accept", "application/json, text/event-stream")
            .json(&json!({"jsonrpc":"2.0","id":0,"method":"tools/list","params":{}}))
            .send()
            .await
            .unwrap();
        assert_eq!(wrong_port.status(), StatusCode::FORBIDDEN);

        let response = Client::new()
            .post(format!("http://{address}/mcp"))
            .header("Origin", "https://hub.example")
            .header("X-User", "attacker-selected-user")
            .header("MCP-Protocol-Version", "2026-07-28")
            .header("Mcp-Method", "tools/list")
            .header("Accept", "application/json, text/event-stream")
            .json(&json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/list",
                "params": {
                    "_meta": {
                        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                        "io.modelcontextprotocol/clientCapabilities": {}
                    }
                }
            }))
            .send()
            .await
            .unwrap();
        let status = response.status();
        let raw = response.text().await.unwrap();
        assert_eq!(status, StatusCode::OK, "unexpected MCP response: {raw}");
        let body: Value = serde_json::from_str(&raw)
            .unwrap_or_else(|error| panic!("non-JSON MCP response ({error}): {raw}"));
        let names: Vec<_> = body["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|tool| tool["name"].as_str())
            .collect();
        // The fixed trusted-proxy principal is still authoritative, but policy
        // alone does not manufacture a backend capability while the Agent is offline.
        assert!(names.is_empty());

        task.abort();
        drop(hub);
        let _ = std::fs::remove_dir_all(state_dir);
    }

    #[test]
    fn oauth_debug_representations_redact_secret_and_principal() {
        let secret = "INTROSPECTION_SECRET_DO_NOT_LOG";
        let config = OAuthIntrospectionConfig::new(
            "https://auth.example",
            "https://hub.example/mcp",
            "https://auth.example/introspect",
            "client-id",
            secret,
        );
        let rendered = format!("{config:?}");
        assert!(!rendered.contains(secret));
        assert!(rendered.contains("[REDACTED]"));

        let token = VerifiedAccessToken {
            principal: AuthenticatedClientPrincipal::new(
                "https://auth.example",
                "PRIVATE_SUBJECT_DO_NOT_LOG",
            )
            .unwrap(),
            scopes: ["mcp:use".to_owned()].into_iter().collect(),
        };
        let token_debug = format!("{token:?}");
        assert!(!token_debug.contains("PRIVATE_SUBJECT_DO_NOT_LOG"));
        assert!(token_debug.contains("[REDACTED]"));
    }

    #[test]
    fn live_backend_advertisement_narrows_semantic_discovery_fail_closed() {
        let advertisement = CapabilityAdvertisement {
            backend: "fixture".into(),
            backend_version: "1".into(),
            platform: "test".into(),
            capability_schema_version: crate::v2_m0::CAPABILITY_SCHEMA_VERSION,
            revision: 1,
            supported: vec![DeviceCapability::ListWindows],
        };
        assert!(capability_is_live(
            Some(&advertisement),
            DeviceCapability::ListWindows,
        ));
        assert!(!capability_is_live(
            Some(&advertisement),
            DeviceCapability::InspectWindow,
        ));
        // Discovery is the exact intersection of policy and a live backend
        // advertisement. An offline Agent exposes no semantic device tools.
        assert!(!capability_is_live(None, DeviceCapability::InspectWindow,));
    }

    #[test]
    fn northbound_exposes_typed_semantic_capabilities_without_generic_raw_tool() {
        let mappings = [
            (TOOL_LIST_APPS, DeviceCapability::ListApplications),
            (TOOL_GET_SCREEN_SIZE, DeviceCapability::ScreenGeometry),
            (TOOL_SCREENSHOT, DeviceCapability::Screenshot),
            (TOOL_CLICK, DeviceCapability::PointerClick),
            (TOOL_DRAG, DeviceCapability::PointerDrag),
            (TOOL_TYPE_TEXT, DeviceCapability::TypeText),
            (TOOL_EXECUTE_PROCESS, DeviceCapability::ExecuteProcess),
            (TOOL_SHELL, DeviceCapability::Shell),
            (TOOL_READ_FILE, DeviceCapability::ReadFile),
            (TOOL_LIST_DIRECTORY, DeviceCapability::ListDirectory),
            (TOOL_LIST_WINDOWS, DeviceCapability::ListWindows),
            (TOOL_LAUNCH_APPLICATION, DeviceCapability::LaunchApplication),
            (TOOL_INSPECT_WINDOW, DeviceCapability::InspectWindow),
            (TOOL_VERIFY_UI_STATE, DeviceCapability::VerifyUiState),
            (
                TOOL_TERMINATE_APPLICATION,
                DeviceCapability::TerminateApplication,
            ),
            (TOOL_ACTIVATE_WINDOW, DeviceCapability::ActivateWindow),
            (TOOL_SET_WINDOW_FRAME, DeviceCapability::SetWindowFrame),
            (TOOL_INVOKE_MENU, DeviceCapability::InvokeMenu),
            (TOOL_KEYBOARD_INPUT, DeviceCapability::KeyboardInput),
            (TOOL_SCROLL, DeviceCapability::Scroll),
            (TOOL_CLIPBOARD_READ, DeviceCapability::ClipboardRead),
            (TOOL_CLIPBOARD_WRITE, DeviceCapability::ClipboardWrite),
            (TOOL_GET_POINTER_POSITION, DeviceCapability::PointerPosition),
            (TOOL_MOVE_POINTER, DeviceCapability::MovePointer),
            (TOOL_SET_UI_VALUE, DeviceCapability::SetUiValue),
            (TOOL_CAPTURE_REGION, DeviceCapability::CaptureRegion),
            (
                TOOL_EXPAND_INTERACTION_SCOPE,
                DeviceCapability::DesktopScope,
            ),
            (TOOL_BROWSER_PREPARE, DeviceCapability::BrowserPrepare),
            (TOOL_BROWSER_BIND, DeviceCapability::BrowserInspect),
            (TOOL_BROWSER_INSPECT, DeviceCapability::BrowserInspect),
            (TOOL_BROWSER_NAVIGATE, DeviceCapability::BrowserNavigate),
            (TOOL_BROWSER_CLICK, DeviceCapability::BrowserClick),
            (TOOL_BROWSER_TYPE, DeviceCapability::BrowserType),
            (TOOL_BROWSER_DIALOG, DeviceCapability::BrowserDialog),
            (TOOL_BROWSER_POINTER, DeviceCapability::BrowserPointer),
            (
                TOOL_BROWSER_STAGE_UPLOAD_FILE,
                DeviceCapability::BrowserUploadFile,
            ),
            (
                TOOL_BROWSER_UPLOAD_FILE,
                DeviceCapability::BrowserUploadFile,
            ),
            (TOOL_BROWSER_DOWNLOAD, DeviceCapability::BrowserDownload),
        ];
        for (tool, capability) in mappings {
            assert_eq!(tool_capability(tool), Some(capability));
        }
        assert_eq!(tool_capability(TOOL_OPEN_INTERACTION_CONTEXT), None);
        assert_eq!(tool_capability(TOOL_CLOSE_INTERACTION_CONTEXT), None);
        assert_eq!(tool_capability(TOOL_GET_OPERATION), None);
        let names: Vec<_> = all_tools()
            .into_iter()
            .map(|tool| tool.name.to_string())
            .collect();
        assert_eq!(names.len(), mappings.len() + 3);
        assert!(names.contains(&TOOL_OPEN_INTERACTION_CONTEXT.to_owned()));
        assert!(names.contains(&TOOL_CLOSE_INTERACTION_CONTEXT.to_owned()));
        assert!(names.contains(&TOOL_GET_OPERATION.to_owned()));
        assert!(
            !names
                .iter()
                .any(|name| name == "raw_cua" || name == "call_tool")
        );
        assert!(!names.iter().any(|name| name == "browser_upload"));
        assert!(!names.iter().any(|name| name == "browser_download"));
        assert_eq!(tool_capability("browser_upload"), None);
        assert_eq!(tool_capability("browser_download"), None);
    }

    #[test]
    fn browser_core_rejects_desktop_scoped_contexts_without_implicit_downgrade() {
        let principal =
            AuthenticatedClientPrincipal::new("https://auth.example", "browser-user").unwrap();
        let mut binding = InteractionContextManager::new(InteractionContextLimits::default())
            .unwrap()
            .open(&principal, "dev-browser", 3, 4, 1)
            .unwrap();
        assert_eq!(binding.scope, InteractionScope::WindowScoped);
        assert!(require_browser_window_scope(binding.clone()).is_ok());
        binding.scope = InteractionScope::DesktopScoped;
        assert!(require_browser_window_scope(binding).is_err());
    }

    #[test]
    fn browser_tool_schemas_expose_only_cumg_semantics_and_safe_transfer_surface() {
        let tools = all_tools();
        let browser_names = [
            TOOL_BROWSER_PREPARE,
            TOOL_BROWSER_BIND,
            TOOL_BROWSER_INSPECT,
            TOOL_BROWSER_NAVIGATE,
            TOOL_BROWSER_CLICK,
            TOOL_BROWSER_TYPE,
            TOOL_BROWSER_DIALOG,
            TOOL_BROWSER_POINTER,
            TOOL_BROWSER_STAGE_UPLOAD_FILE,
            TOOL_BROWSER_UPLOAD_FILE,
            TOOL_BROWSER_DOWNLOAD,
        ];
        for name in browser_names {
            let tool = tools
                .iter()
                .find(|tool| tool.name.as_ref() == name)
                .unwrap_or_else(|| panic!("missing {name}"));
            let schema = serde_json::to_string(&tool.input_schema).unwrap();
            assert!(!schema.contains("target_id"));
            assert!(!schema.contains("tab_id"));
            assert!(!schema.contains("cdp"));
            assert!(!schema.contains("approval"));
            assert!(!schema.contains("bearer"));
            assert!(!schema.contains("proxy"));
        }
        assert!(
            tools
                .iter()
                .any(|tool| tool.name.as_ref() == TOOL_BROWSER_STAGE_UPLOAD_FILE)
        );
        assert!(
            tools
                .iter()
                .any(|tool| tool.name.as_ref() == TOOL_BROWSER_UPLOAD_FILE)
        );
        assert!(
            tools
                .iter()
                .any(|tool| tool.name.as_ref() == TOOL_BROWSER_DOWNLOAD)
        );
        assert!(
            !tools
                .iter()
                .any(|tool| tool.name.as_ref() == "browser_download")
        );
    }

    #[test]
    fn browser_dialog_inspect_cannot_smuggle_resolution_authority() {
        let base = json!({
            "context_id": "ctx_0123456789abcdef0123456789abcdef",
            "target_ref": "ref_0123456789abcdef0123456789abcdef",
            "tab_ref": "ref_1123456789abcdef0123456789abcdef",
            "action": "inspect",
            "delivery": "background"
        });
        let request: BrowserDialogRequest = serde_json::from_value(base.clone()).unwrap();
        assert!(request.validate().is_ok());

        let mut with_ref = base.clone();
        with_ref["dialog_ref"] = json!("ref_2123456789abcdef0123456789abcdef");
        let request: BrowserDialogRequest = serde_json::from_value(with_ref).unwrap();
        assert_eq!(
            request.validate(),
            Err(BrowserContractError::InvalidDialogAction)
        );

        let mut foreground = base;
        foreground["delivery"] = json!("foreground");
        let request: BrowserDialogRequest = serde_json::from_value(foreground).unwrap();
        assert_eq!(
            request.validate(),
            Err(BrowserContractError::InvalidDialogAction)
        );
    }

    #[test]
    fn operation_recovery_ids_and_states_are_closed_and_stable() {
        let valid = "op_0123456789abcdef0123456789abcdef";
        assert!(validate_operation_id(valid).is_ok());
        for invalid in [
            "",
            "op_0123",
            "OP_0123456789abcdef0123456789abcdef",
            "op_0123456789ABCDEF0123456789abcdef",
            "op_0123456789abcdef0123456789abcdeg",
        ] {
            assert!(
                validate_operation_id(invalid).is_err(),
                "accepted {invalid}"
            );
        }

        let output = crate::v2_m0::ProcessOutput {
            exit_code: None,
            stdout: String::new(),
            stderr: String::new(),
            stdout_truncated: false,
            stderr_truncated: false,
            timed_out: true,
            cancelled: false,
            duration_ms: 10,
        };
        assert_eq!(
            public_recovery_state(
                HubOperationState::Failed,
                Some(&RecoverableOperationResult::Shell { output })
            ),
            "timed_out"
        );
        assert_eq!(
            public_recovery_state(HubOperationState::Indeterminate, None),
            "indeterminate"
        );
        assert_eq!(
            public_recovery_state(HubOperationState::Dispatched, None),
            "running"
        );
    }

    #[test]
    fn process_and_shell_tool_schemas_expose_optional_recovery_ref() {
        let tools = all_tools();
        for name in [TOOL_EXECUTE_PROCESS, TOOL_SHELL, TOOL_GET_OPERATION] {
            let tool = tools
                .iter()
                .find(|tool| tool.name.as_ref() == name)
                .unwrap();
            let schema = serde_json::to_string(&tool.input_schema).unwrap();
            assert!(schema.contains("operation_id"));
            assert!(schema.contains("^op_[0-9a-f]{32}$"));
        }
        let recovery = tools
            .iter()
            .find(|tool| tool.name.as_ref() == TOOL_GET_OPERATION)
            .unwrap();
        assert_eq!(
            recovery.annotations.as_ref().and_then(|a| a.read_only_hint),
            Some(true)
        );
    }

    #[test]
    fn operational_failures_are_tool_results_not_protocol_errors() {
        let response =
            execution_error_response(hub_error_to_mcp(HubCommandError::DeviceIndeterminate {
                operation_id: "op_0123456789abcdef0123456789abcdef".into(),
            }));
        let serialized = match response {
            CallToolResponse::Complete(result) => {
                assert_eq!(result.is_error, Some(true));
                serde_json::to_string(&result).unwrap()
            }
            other => panic!("unexpected tool response: {other:?}"),
        };
        assert!(serialized.contains("device_indeterminate"));
        assert!(serialized.contains("retry_safe"));
        assert!(serialized.contains("op_0123456789abcdef0123456789abcdef"));
        assert!(!serialized.contains("ExceptionGroup"));

        let response = execution_error_response(hub_error_to_mcp(HubCommandError::Remote(
            crate::v2_m0::DeviceErrorCode::BrowserConsentRequired,
        )));
        let serialized = match response {
            CallToolResponse::Complete(result) => {
                assert_eq!(result.is_error, Some(true));
                serde_json::to_string(&result).unwrap()
            }
            other => panic!("unexpected tool response: {other:?}"),
        };
        assert!(serialized.contains("browser_consent_required"));
        assert!(!serialized.contains("provider"));
    }

    #[test]
    fn environment_policy_failures_are_stable_northbound_codes() {
        for (code, expected) in [
            (
                crate::v2_m0::DeviceErrorCode::EnvironmentKeyDenied,
                "environment_key_denied",
            ),
            (
                crate::v2_m0::DeviceErrorCode::InvalidEnvironment,
                "invalid_environment",
            ),
            (
                crate::v2_m0::DeviceErrorCode::TooManyEnvironmentEntries,
                "too_many_environment_entries",
            ),
        ] {
            let response =
                execution_error_response(hub_error_to_mcp(HubCommandError::Remote(code)));
            let serialized = match response {
                CallToolResponse::Complete(result) => {
                    assert_eq!(result.is_error, Some(true));
                    serde_json::to_string(&result).unwrap()
                }
                other => panic!("unexpected tool response: {other:?}"),
            };
            assert!(serialized.contains(expected));
            assert!(serialized.contains("retry_safe"));
            assert!(!serialized.contains("AWS_SECRET_ACCESS_KEY"));
            assert!(!serialized.contains("secret-value"));
        }

        let generic = serde_json::to_string(&hub_error_to_mcp(HubCommandError::Remote(
            crate::v2_m0::DeviceErrorCode::InternalFailure,
        )))
        .unwrap();
        assert!(generic.contains("internal_failure"));
        assert!(!generic.contains("process_"));
    }

    #[test]
    fn browser_refusal_mcp_error_carries_only_closed_safe_code() {
        let error = hub_error_to_mcp(HubCommandError::Remote(
            crate::v2_m0::DeviceErrorCode::BrowserInputTrustUnavailable,
        ));
        let serialized = serde_json::to_string(&error).unwrap();
        assert!(serialized.contains("browser_input_trust_unavailable"));
        assert!(!serialized.contains("provider"));
        assert!(!serialized.contains("CDP"));
        assert!(!serialized.contains("Chromium"));
    }

    #[test]
    fn pointer_button_and_drag_bounds_fail_closed() {
        assert_eq!(parse_pointer_button(None).unwrap(), PointerButton::Left);
        assert_eq!(
            parse_pointer_button(Some("right")).unwrap(),
            PointerButton::Right
        );
        assert!(parse_pointer_button(Some("primary")).is_err());
    }

    #[test]
    fn native_element_tool_schemas_expose_only_scoped_element_refs() {
        let tools = all_tools();
        let click = tools
            .iter()
            .find(|tool| tool.name.as_ref() == TOOL_CLICK)
            .expect("click tool");
        let click_schema = serde_json::to_value(&click.input_schema).unwrap();
        let click_properties = click_schema["properties"].as_object().unwrap();
        assert!(click_properties.contains_key("element_ref"));
        assert!(click_properties.contains_key("action"));
        let click_required = click_schema["required"].as_array().unwrap();
        assert!(!click_required.iter().any(|field| field == "x"));
        assert!(!click_required.iter().any(|field| field == "y"));
        let serialized = serde_json::to_string(&click_schema).unwrap();
        assert!(!serialized.contains("element_token"));
        assert!(!serialized.contains("element_index"));
        assert!(!serialized.contains("snapshot_id"));

        for name in [TOOL_TYPE_TEXT, TOOL_KEYBOARD_INPUT] {
            let tool = tools
                .iter()
                .find(|tool| tool.name.as_ref() == name)
                .unwrap_or_else(|| panic!("missing {name}"));
            let schema = serde_json::to_string(&tool.input_schema).unwrap();
            assert!(schema.contains("element_ref"));
            assert!(schema.contains("\"element\""));
            assert!(!schema.contains("element_token"));
            assert!(!schema.contains("snapshot_id"));
        }
    }

    #[test]
    fn scoped_native_element_ref_extraction_covers_all_element_consumers() {
        let public_ref = "ref_0123456789abcdef0123456789abcdef".to_owned();
        let context = Some("ctx_0123456789abcdef0123456789abcdef".to_owned());
        let mut commands = [
            DeviceCommand::PointerClickAdvanced {
                context_id: context.clone(),
                target: PointerTarget::Element {
                    process_id: 42,
                    window_id: 7,
                    element_ref: public_ref.clone(),
                },
                button: PointerButton::Left,
                click_count: 1,
                action: Some(UiElementAction::Press),
                modifiers: vec![],
                delivery: InputDeliveryMode::Background,
            },
            DeviceCommand::TypeTextAdvanced {
                context_id: context.clone(),
                text: "hello".into(),
                target: InputTarget::Element {
                    process_id: 42,
                    window_id: 7,
                    element_ref: public_ref.clone(),
                },
                delivery: InputDeliveryMode::Background,
                delay_ms: 30,
            },
            DeviceCommand::KeyboardInput {
                context_id: context,
                key: "return".into(),
                modifiers: vec![],
                target: InputTarget::Element {
                    process_id: 42,
                    window_id: 7,
                    element_ref: public_ref.clone(),
                },
                delivery: InputDeliveryMode::Background,
            },
            DeviceCommand::SetUiValue {
                context_id: "ctx_0123456789abcdef0123456789abcdef".into(),
                process_id: 42,
                window_id: 7,
                element_ref: public_ref.clone(),
                value: "value".into(),
            },
        ];
        for command in &mut commands {
            assert_eq!(
                command_scoped_ui_element_ref_mut(command).map(|value| value.as_str()),
                Some(public_ref.as_str())
            );
        }

        let mut coordinate = DeviceCommand::PointerClickAdvanced {
            context_id: Some("ctx_0123456789abcdef0123456789abcdef".into()),
            target: PointerTarget::WindowPhysical {
                process_id: 42,
                window_id: 7,
                x: 1,
                y: 2,
            },
            button: PointerButton::Left,
            click_count: 1,
            action: None,
            modifiers: vec![],
            delivery: InputDeliveryMode::Background,
        };
        assert!(command_scoped_ui_element_ref_mut(&mut coordinate).is_none());
    }

    #[test]
    fn native_element_targets_are_context_bound_and_coordinate_exclusive() {
        let public_ref = "ref_0123456789abcdef0123456789abcdef";
        let context = "ctx_0123456789abcdef0123456789abcdef";
        let target = parse_click_target(
            Some(context),
            None,
            Some(42),
            Some(7),
            None,
            None,
            Some(public_ref.into()),
        )
        .unwrap();
        assert_eq!(
            target,
            PointerTarget::Element {
                process_id: 42,
                window_id: 7,
                element_ref: public_ref.into(),
            }
        );
        assert!(
            parse_click_target(
                None,
                None,
                Some(42),
                Some(7),
                None,
                None,
                Some(public_ref.into()),
            )
            .is_err()
        );
        assert!(
            parse_click_target(
                Some(context),
                None,
                Some(42),
                Some(7),
                Some(1),
                Some(2),
                Some(public_ref.into()),
            )
            .is_err()
        );

        let input = parse_input_target(
            Some("element"),
            Some(42),
            Some(7),
            None,
            None,
            Some(public_ref.into()),
        )
        .unwrap();
        assert_eq!(
            input,
            InputTarget::Element {
                process_id: 42,
                window_id: 7,
                element_ref: public_ref.into(),
            }
        );
        assert!(
            parse_input_target(
                Some("element"),
                Some(42),
                Some(7),
                Some(1),
                Some(2),
                Some(public_ref.into()),
            )
            .is_err()
        );
    }

    #[test]
    fn native_element_action_options_fail_closed() {
        let target = PointerTarget::Element {
            process_id: 42,
            window_id: 7,
            element_ref: "ref_0123456789abcdef0123456789abcdef".into(),
        };
        assert!(
            validate_element_click_options(
                &target,
                PointerButton::Left,
                1,
                Some(UiElementAction::Open),
                &[],
            )
            .is_ok()
        );
        assert!(
            validate_element_click_options(&target, PointerButton::Right, 2, None, &[],).is_err()
        );
        assert!(
            validate_element_click_options(&target, PointerButton::Left, 3, None, &[],).is_err()
        );
    }

    #[test]
    fn type_text_arguments_fail_closed_before_dispatch() {
        assert!(
            parse_arguments::<TypeTextArgs>(Some(
                serde_json::json!({"text": "ok", "shell": "echo nope"})
                    .as_object()
                    .unwrap()
                    .clone(),
            ))
            .is_err()
        );
        assert!(serde_json::from_value::<TypeTextArgs>(serde_json::json!({"text": "ok"})).is_ok());
    }

    #[test]
    fn interaction_scope_classification_is_explicit_and_monotonic() {
        let context_id = Some("ctx_0123456789abcdef0123456789abcdef".into());
        let window_click = DeviceCommand::PointerClickAdvanced {
            context_id: context_id.clone(),
            target: PointerTarget::WindowPhysical {
                process_id: 1,
                window_id: 2,
                x: 3,
                y: 4,
            },
            button: PointerButton::Left,
            click_count: 1,
            action: None,
            modifiers: vec![],
            delivery: InputDeliveryMode::Background,
        };
        assert!(command_requires_window_scope(&window_click));
        assert!(!command_requires_desktop_scope(&window_click));

        let element_click = DeviceCommand::PointerClickAdvanced {
            context_id: context_id.clone(),
            target: PointerTarget::Element {
                process_id: 1,
                window_id: 2,
                element_ref: "ref_0123456789abcdef0123456789abcdef".into(),
            },
            button: PointerButton::Left,
            click_count: 1,
            action: Some(UiElementAction::Press),
            modifiers: vec![],
            delivery: InputDeliveryMode::Background,
        };
        assert!(command_requires_window_scope(&element_click));
        assert!(!command_requires_desktop_scope(&element_click));

        let desktop_click = DeviceCommand::PointerClickAdvanced {
            context_id,
            target: PointerTarget::DesktopPhysical { x: 3, y: 4 },
            button: PointerButton::Left,
            click_count: 1,
            action: None,
            modifiers: vec![],
            delivery: InputDeliveryMode::Foreground,
        };
        assert!(command_requires_desktop_scope(&desktop_click));
        assert!(!command_requires_window_scope(&desktop_click));

        let capture = DeviceCommand::CaptureRegion {
            context_id: Some("ctx_0123456789abcdef0123456789abcdef".into()),
            process_id: 1,
            window_id: 2,
            bounds: UiRect {
                x: 0,
                y: 0,
                width: 10,
                height: 10,
            },
        };
        assert!(command_requires_window_scope(&capture));
        assert!(!command_requires_desktop_scope(&capture));

        let clipboard = DeviceCommand::ClipboardRead {
            context_id: Some("ctx_0123456789abcdef0123456789abcdef".into()),
            include_text: false,
        };
        assert!(!command_requires_window_scope(&clipboard));
        assert!(!command_requires_desktop_scope(&clipboard));
    }

    #[test]
    fn screenshot_failure_is_read_only_but_type_text_is_mutating_for_accounting() {
        assert!(DeviceCommand::Screenshot.is_read_only());
        assert!(
            DeviceCommand::ListWindows {
                process_id: None,
                on_screen_only: true,
            }
            .is_read_only()
        );
        assert!(
            DeviceCommand::InspectWindow {
                process_id: 1,
                window_id: 1,
                query: None,
                max_elements: 10,
                max_depth: 5,
                include_screenshot: false,
            }
            .is_read_only()
        );
        assert!(
            !DeviceCommand::LaunchApplication {
                identifier: Some("app".into()),
                name: None,
                targets: vec![],
                new_instance: false,
            }
            .is_read_only()
        );
        assert!(!DeviceCommand::TypeText { text: "x".into() }.is_read_only());
        let remote = HubCommandError::Remote(crate::v2_m0::DeviceErrorCode::InternalFailure);
        assert_eq!(
            usage_settlement_for_error(true, DeviceCommand::Screenshot.is_read_only(), &remote),
            (UsageSettlement::Zero, "proven_no_effect")
        );
        assert_eq!(
            usage_settlement_for_error(
                true,
                DeviceCommand::TypeText { text: "x".into() }.is_read_only(),
                &remote,
            ),
            (UsageSettlement::Full, "dispatched_conservative")
        );
    }

    #[test]
    fn usage_outcome_mapping_is_conservative_after_dispatch() {
        let remote = HubCommandError::Remote(crate::v2_m0::DeviceErrorCode::InternalFailure);
        // Screenshot is read-only: a verified remote failure proves no business
        // side effect. TypeText is mutable: any dispatched failure is charged fully.
        assert_eq!(
            usage_settlement_for_error(false, false, &HubCommandError::Rejected),
            (UsageSettlement::Zero, "pre_dispatch_rejected")
        );
        assert_eq!(
            usage_settlement_for_error(true, true, &remote),
            (UsageSettlement::Zero, "proven_no_effect")
        );
        assert_eq!(
            usage_settlement_for_error(true, false, &remote),
            (UsageSettlement::Full, "dispatched_conservative")
        );
        assert_eq!(
            usage_settlement_for_error(true, false, &HubCommandError::Indeterminate),
            (UsageSettlement::Full, "dispatched_conservative")
        );
        assert_eq!(
            usage_settlement_for_error(true, false, &HubCommandError::SessionClosed),
            (UsageSettlement::Full, "dispatched_conservative")
        );
    }
}
