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
    v2_execution_safety::OperationOwner,
    v2_m0::{
        DeviceCapability, DeviceCommand, DeviceResult, MAX_TYPE_TEXT_BYTES, PointerButton,
        ProcessEnvVar, ProcessRequest, ShellRequest,
    },
    v2_m0_trust::{AuthenticatedClientPrincipal, ClientAuthorizationPolicy, TrustError},
    v2_m1_hub::{HubCommandError, HubHandle},
    v2_usage::{UsageError, UsageLease, UsageManager, UsageOperation, UsageSettlement},
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
const TOOL_LIST_APPS: &str = "list_apps";
const TOOL_GET_SCREEN_SIZE: &str = "get_screen_size";
const TOOL_SCREENSHOT: &str = "screenshot";
const TOOL_CLICK: &str = "click";
const TOOL_DRAG: &str = "drag";
const TOOL_TYPE_TEXT: &str = "type_text";
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
    let state = TrustedProxyAuthState {
        principal: config.principal.clone(),
    };
    Router::new()
        .nest_service(config.mcp_path(), service)
        .layer(middleware::from_fn_with_state(state, trusted_proxy_guard))
}

async fn trusted_proxy_guard(
    State(state): State<TrustedProxyAuthState>,
    mut request: Request,
    next: Next,
) -> Response {
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

#[derive(Clone)]
pub struct V2NorthboundMcp {
    hub: HubHandle,
    authorizer: Arc<dyn DeviceCapabilityAuthorizer>,
    usage: UsageManager,
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

    fn tools_for(&self, principal: &AuthenticatedClientPrincipal) -> Vec<Tool> {
        all_tools()
            .into_iter()
            .filter(|tool| {
                tool_capability(tool.name.as_ref()).is_some_and(|capability| {
                    self.authorizer
                        .authorize_device_capability(principal, self.hub.device_id(), capability)
                        .is_ok()
                })
            })
            .collect()
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
        let read_only = command_is_read_only(&command);
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
        tokio::select! {
            result = &mut wait => {
                match result {
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
        let operation_id = self.hub.new_operation_id();
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

        let command_result: Result<DeviceCommand, McpError> = (|| match request.name.as_ref() {
            TOOL_LIST_APPS => Ok(DeviceCommand::ListApplications),
            TOOL_GET_SCREEN_SIZE => Ok(DeviceCommand::ScreenGeometry),
            TOOL_SCREENSHOT => Ok(DeviceCommand::Screenshot),
            TOOL_CLICK => {
                let args: ClickArgs = parse_arguments(request.arguments)?;
                Ok(DeviceCommand::PointerClick {
                    x: args.x,
                    y: args.y,
                    button: parse_pointer_button(args.button.as_deref())?,
                })
            }
            TOOL_DRAG => {
                let args: DragArgs = parse_arguments(request.arguments)?;
                if args.duration_ms == 0 || args.duration_ms > 10_000 {
                    return Err(McpError::invalid_params(
                        "duration_ms must be within 1..=10000",
                        None,
                    ));
                }
                Ok(DeviceCommand::PointerDrag {
                    from_x: args.from_x,
                    from_y: args.from_y,
                    to_x: args.to_x,
                    to_y: args.to_y,
                    duration_ms: args.duration_ms,
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
                Ok(DeviceCommand::TypeText { text: args.text })
            }
            TOOL_EXECUTE_PROCESS => {
                let args: ExecuteProcessArgs = parse_arguments(request.arguments)?;
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
            _ => Err(McpError::invalid_params("Unknown V2 Hub tool", None)),
        })();
        let command = match command_result {
            Ok(command) => command,
            Err(error) => {
                settle_usage_best_effort(&usage, UsageSettlement::Zero, "invalid_arguments").await;
                return Err(error);
            }
        };

        let result = self
            .execute_command(&auth.principal, operation_id, command, usage, &context)
            .await?;
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
            other => {
                let value = serde_json::to_string(&other).map_err(|_| {
                    McpError::internal_error("Failed to serialize device result", None)
                })?;
                Ok(CallToolResult::success(vec![ContentBlock::text(value)]).into())
            }
        }
    }
}

fn command_is_read_only(command: &DeviceCommand) -> bool {
    matches!(
        command,
        DeviceCommand::ListApplications
            | DeviceCommand::ScreenGeometry
            | DeviceCommand::Screenshot
            | DeviceCommand::ReadFile { .. }
            | DeviceCommand::ListDirectory { .. }
    )
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
    let message = match error {
        HubCommandError::AgentOffline => "Agent is offline",
        HubCommandError::Busy => "Device is busy",
        HubCommandError::DeviceIndeterminate { .. } | HubCommandError::Indeterminate => {
            "Device execution state is indeterminate"
        }
        HubCommandError::CancelledBeforeDispatch => "Operation was cancelled before dispatch",
        HubCommandError::UsageUnavailable => "Usage accounting is temporarily unavailable",
        _ => "Device operation was rejected or could not be completed",
    };
    McpError::invalid_request(message, None)
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
        _ => None,
    }
}

fn all_tools() -> Vec<Tool> {
    vec![
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
            "Capture the enrolled device primary display as a bounded PNG image.",
            object_schema(vec![], &[]),
        )
        .with_annotations(ToolAnnotations::new().read_only(true)),
        Tool::new(
            TOOL_CLICK,
            "Click desktop coordinates through the enrolled computer-use backend.",
            object_schema(
                vec![
                    ("x", signed_integer_schema()),
                    ("y", signed_integer_schema()),
                    ("button", string_schema()),
                ],
                &["x", "y"],
            ),
        )
        .with_annotations(ToolAnnotations::new().destructive(true).idempotent(false)),
        Tool::new(
            TOOL_DRAG,
            "Drag the desktop pointer through the enrolled computer-use backend.",
            object_schema(
                vec![
                    ("from_x", signed_integer_schema()),
                    ("from_y", signed_integer_schema()),
                    ("to_x", signed_integer_schema()),
                    ("to_y", signed_integer_schema()),
                    ("duration_ms", positive_integer_schema()),
                ],
                &["from_x", "from_y", "to_x", "to_y", "duration_ms"],
            ),
        )
        .with_annotations(ToolAnnotations::new().destructive(true).idempotent(false)),
        Tool::new(
            TOOL_TYPE_TEXT,
            "Type text into the current foreground desktop application.",
            object_schema(vec![("text", bounded_text_schema())], &["text"]),
        )
        .with_annotations(ToolAnnotations::new().destructive(true).idempotent(false)),
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

fn bounded_text_schema() -> Value {
    json!({ "type": "string", "minLength": 1, "maxLength": MAX_TYPE_TEXT_BYTES })
}

fn signed_integer_schema() -> Value {
    json!({ "type": "integer" })
}

fn positive_integer_schema() -> Value {
    json!({ "type": "integer", "minimum": 1 })
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TypeTextArgs {
    text: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ClickArgs {
    x: i32,
    y: i32,
    button: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DragArgs {
    from_x: i32,
    from_y: i32,
    to_x: i32,
    to_y: i32,
    duration_ms: u64,
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
#[serde(deny_unknown_fields)]
struct ShellArgs {
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
                max_agent_sessions: 2,
                max_agent_session_starts_per_minute: 30,
            },
            HubProvisionedMaterial {
                hub_identity: HubIdentity::generate(),
                grant_authority: GrantAuthority::generate(),
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

    #[tokio::test]
    async fn trusted_proxy_mcp_ignores_caller_identity_and_keeps_exact_policy() {
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
                max_queued_per_device: 1,
                max_agent_sessions: 2,
                max_agent_session_starts_per_minute: 30,
            },
            HubProvisionedMaterial {
                hub_identity: HubIdentity::generate(),
                grant_authority: GrantAuthority::generate(),
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

        let response = Client::new()
            .post(format!("http://{address}/mcp"))
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
        assert_eq!(names, vec![TOOL_READ_FILE]);

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
    fn northbound_exposes_existing_exact_cua_capabilities_without_generic_raw_tool() {
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
        ];
        for (tool, capability) in mappings {
            assert_eq!(tool_capability(tool), Some(capability));
        }
        let names: Vec<_> = all_tools()
            .into_iter()
            .map(|tool| tool.name.to_string())
            .collect();
        assert_eq!(names.len(), mappings.len());
        assert!(
            !names
                .iter()
                .any(|name| name == "raw_cua" || name == "call_tool")
        );
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
    fn screenshot_failure_is_read_only_but_type_text_is_mutating_for_accounting() {
        assert!(command_is_read_only(&DeviceCommand::Screenshot));
        assert!(!command_is_read_only(&DeviceCommand::TypeText {
            text: "x".into(),
        }));
        let remote = HubCommandError::Remote(crate::v2_m0::DeviceErrorCode::InternalFailure);
        assert_eq!(
            usage_settlement_for_error(
                true,
                command_is_read_only(&DeviceCommand::Screenshot),
                &remote
            ),
            (UsageSettlement::Zero, "proven_no_effect")
        );
        assert_eq!(
            usage_settlement_for_error(
                true,
                command_is_read_only(&DeviceCommand::TypeText { text: "x".into() }),
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
