//! Standard MCP Authorization boundary for the V2 Hub.
//!
//! This module deliberately keeps OAuth access tokens northbound. The bearer
//! token is validated at the HTTP resource-server boundary and reduced to an
//! [`AuthenticatedClientPrincipal`] plus OAuth scopes. Only that principal is
//! passed into the local principal -> device -> exact `DeviceCapability`
//! policy. Southbound Hub/Agent messages continue to carry only typed commands
//! and short-lived exact capability grants.

use crate::{
    v2_m0::{
        DeviceCapability, DeviceCommand, DeviceResult, ProcessEnvVar, ProcessRequest, ShellRequest,
    },
    v2_m0_trust::{AuthenticatedClientPrincipal, ClientAuthorizationPolicy, TrustError},
    v2_m1_hub::{HubCommandError, HubHandle},
};
use async_trait::async_trait;
use axum::{
    Json, Router,
    extract::{Request, State},
    http::{
        HeaderMap, HeaderValue, StatusCode,
        header::{AUTHORIZATION, WWW_AUTHENTICATE},
        request::Parts,
    },
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
};
use reqwest::{Client, Url, redirect::Policy as RedirectPolicy};
use rmcp::{
    ErrorData as McpError, ServerHandler,
    model::{
        CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, JsonObject,
        ListToolsResult, PaginatedRequestParams, ProtocolVersion, ServerCapabilities, ServerInfo,
        Tool, ToolAnnotations,
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
use tracing::warn;

const DEFAULT_INTROSPECTION_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_BEARER_TOKEN_BYTES: usize = 16 * 1024;
const TOOL_EXECUTE_PROCESS: &str = "execute_process";
const TOOL_SHELL: &str = "shell";
const TOOL_READ_FILE: &str = "read_file";
const TOOL_LIST_DIRECTORY: &str = "list_directory";

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

#[derive(Debug, Clone)]
pub struct OAuthIntrospectionConfig {
    pub issuer: String,
    pub resource: String,
    pub endpoint: String,
    pub client_id: String,
    pub client_secret: String,
    pub timeout: Duration,
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

#[derive(Debug, Clone)]
pub struct VerifiedAccessToken {
    pub principal: AuthenticatedClientPrincipal,
    pub scopes: HashSet<String>,
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
    let http_config = StreamableHttpServerConfig::default()
        .with_legacy_session_mode(false)
        .with_json_response(true)
        .with_allowed_hosts(allowed_hosts)
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
            let challenge = bearer_challenge(&state.config, Some("invalid_token"));
            return oauth_error_response(
                StatusCode::UNAUTHORIZED,
                Some(challenge),
                "invalid_token",
                "Access token is invalid or expired",
            );
        }
        Err(TokenVerificationError::Unavailable | TokenVerificationError::InvalidConfiguration) => {
            warn!(
                event = "v2_northbound_auth_unavailable",
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

#[derive(Clone)]
pub struct V2NorthboundMcp {
    hub: HubHandle,
    policy: Arc<ClientAuthorizationPolicy>,
}

impl V2NorthboundMcp {
    pub fn new(hub: HubHandle, policy: ClientAuthorizationPolicy) -> Self {
        Self {
            hub,
            policy: Arc::new(policy),
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
        self.policy
            .authorize_device_capability(principal, self.hub.device_id(), capability)
            .map_err(|_| McpError::invalid_request("Device capability is not authorized", None))
    }

    fn tools_for(&self, principal: &AuthenticatedClientPrincipal) -> Vec<Tool> {
        all_tools()
            .into_iter()
            .filter(|tool| {
                tool_capability(tool.name.as_ref()).is_some_and(|capability| {
                    self.policy
                        .authorize_device_capability(principal, self.hub.device_id(), capability)
                        .is_ok()
                })
            })
            .collect()
    }

    async fn execute_command(
        &self,
        command: DeviceCommand,
        context: &RequestContext<RoleServer>,
    ) -> Result<DeviceResult, McpError> {
        let pending = self
            .hub
            .start_command(command)
            .await
            .map_err(hub_error_to_mcp)?;
        let operation_id = pending.operation_id.clone();
        let mut wait = Box::pin(pending.wait());
        tokio::select! {
            result = &mut wait => result.map(|result| result.result).map_err(hub_error_to_mcp),
            _ = context.ct.cancelled() => {
                let _ = self.hub.cancel(operation_id).await;
                Err(McpError::invalid_request("Tool call was cancelled", None))
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

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        let auth = Self::auth_context(&context)?;
        Ok(ListToolsResult {
            tools: self.tools_for(&auth.principal),
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
        let capability = tool_capability(request.name.as_ref())
            .ok_or_else(|| McpError::invalid_params("Unknown V2 Hub tool", None))?;
        self.authorize(&auth.principal, capability)?;

        let command = match request.name.as_ref() {
            TOOL_EXECUTE_PROCESS => {
                let args: ExecuteProcessArgs = parse_arguments(request.arguments)?;
                DeviceCommand::ExecuteProcess {
                    request: ProcessRequest {
                        program: args.program,
                        args: args.args,
                        cwd: args.cwd,
                        env: env_map(args.env),
                        timeout_ms: args.timeout_ms,
                    },
                }
            }
            TOOL_SHELL => {
                let args: ShellArgs = parse_arguments(request.arguments)?;
                DeviceCommand::Shell {
                    request: ShellRequest {
                        command: args.command,
                        cwd: args.cwd,
                        env: env_map(args.env),
                        timeout_ms: args.timeout_ms,
                    },
                }
            }
            TOOL_READ_FILE => {
                let args: PathArgs = parse_arguments(request.arguments)?;
                DeviceCommand::ReadFile { path: args.path }
            }
            TOOL_LIST_DIRECTORY => {
                let args: PathArgs = parse_arguments(request.arguments)?;
                DeviceCommand::ListDirectory { path: args.path }
            }
            _ => return Err(McpError::invalid_params("Unknown V2 Hub tool", None)),
        };

        let result = self.execute_command(command, &context).await?;
        let value = serde_json::to_string(&result)
            .map_err(|_| McpError::internal_error("Failed to serialize device result", None))?;
        Ok(CallToolResult::success(vec![ContentBlock::text(value)]).into())
    }
}

fn hub_error_to_mcp(error: HubCommandError) -> McpError {
    let message = match error {
        HubCommandError::AgentOffline => "Agent is offline",
        HubCommandError::Busy => "Device is busy",
        HubCommandError::DeviceIndeterminate { .. } | HubCommandError::Indeterminate => {
            "Device execution state is indeterminate"
        }
        HubCommandError::CancelledBeforeDispatch => "Operation was cancelled before dispatch",
        _ => "Device operation was rejected or could not be completed",
    };
    McpError::invalid_request(message, None)
}

fn tool_capability(name: &str) -> Option<DeviceCapability> {
    match name {
        TOOL_EXECUTE_PROCESS => Some(DeviceCapability::ExecuteProcess),
        TOOL_SHELL => Some(DeviceCapability::Shell),
        TOOL_READ_FILE => Some(DeviceCapability::ReadFile),
        TOOL_LIST_DIRECTORY => Some(DeviceCapability::ListDirectory),
        _ => None,
    }
}

fn all_tools() -> Vec<Tool> {
    vec![
        Tool::new(
            TOOL_EXECUTE_PROCESS,
            "Execute a structured local process on the enrolled device.",
            object_schema(
                vec![
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
            "Execute a free-form shell command on the enrolled device.",
            object_schema(
                vec![
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

fn string_schema() -> Value {
    json!({ "type": "string", "minLength": 1 })
}

fn array_schema(items: Value) -> Value {
    json!({ "type": "array", "items": items })
}

fn string_map_schema() -> Value {
    json!({ "type": "object", "additionalProperties": { "type": "string" } })
}

fn positive_integer_schema() -> Value {
    json!({ "type": "integer", "minimum": 1 })
}

#[derive(Debug, Deserialize)]
struct ExecuteProcessArgs {
    program: String,
    #[serde(default)]
    args: Vec<String>,
    cwd: String,
    #[serde(default)]
    env: BTreeMap<String, String>,
    timeout_ms: u64,
}

#[derive(Debug, Deserialize)]
struct ShellArgs {
    command: String,
    cwd: String,
    #[serde(default)]
    env: BTreeMap<String, String>,
    timeout_ms: u64,
}

#[derive(Debug, Deserialize)]
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
    async fn authenticated_2026_mcp_request_receives_only_exactly_authorized_tools() {
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
                max_queued_per_device: 1,
            },
            HubProvisionedMaterial {
                hub_identity: HubIdentity::generate(),
                grant_authority: GrantAuthority::generate(),
                device_verifier: device_identity.verifying_key(),
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

        let response = Client::new()
            .post(format!("http://{address}/mcp"))
            .bearer_auth("northbound-only-token")
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
        assert_eq!(names, vec![TOOL_READ_FILE]);

        task.abort();
        drop(hub);
        let _ = std::fs::remove_dir_all(state_dir);
    }
}
