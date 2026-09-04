//! Hosted HTTP resource boundary for Human Handoff operator control.
//!
//! This is intentionally a separate OAuth resource from northbound MCP. It exposes only a
//! closed context-issuance endpoint and a closed lifecycle-control endpoint; it never registers
//! MCP tools. Bearer credentials terminate at this HTTP boundary and are reduced to an
//! authenticated principal before the transport-neutral hosted Handoff service is called.

use crate::{
    v2_hosted_handoff_control::{
        HostedHandoffContextRequest, HostedHandoffControlApi, HostedHandoffControlRequest,
        HostedHandoffControlResponse,
    },
    v2_m0_trust::AuthenticatedClientPrincipal,
    v2_m1_northbound::{AccessTokenVerifier, TokenVerificationError},
};
use axum::{
    Extension, Json, Router,
    extract::{DefaultBodyLimit, Request, State},
    http::{
        HeaderMap, HeaderValue, StatusCode,
        header::{AUTHORIZATION, ORIGIN, WWW_AUTHENTICATE},
    },
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use reqwest::Url;
use serde_json::{Value, json};
use std::{collections::HashSet, fmt, sync::Arc};

const MAX_HOSTED_HANDOFF_BODY_BYTES: usize = 8 * 1024;
const MAX_BEARER_TOKEN_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostedHandoffHttpConfigError {
    InvalidUrl,
    HttpsRequired,
    InvalidResourceUri,
    InvalidAuthorizationServerUri,
    InvalidScope,
    DuplicateScope,
}

impl fmt::Display for HostedHandoffHttpConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for HostedHandoffHttpConfigError {}

#[derive(Clone)]
pub struct HostedHandoffHttpConfig {
    resource: String,
    authorization_server: String,
    resource_url: Url,
    metadata_url: Url,
    context_path: String,
    control_path: String,
    required_scopes: Vec<String>,
}

impl fmt::Debug for HostedHandoffHttpConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HostedHandoffHttpConfig")
            .field("resource", &self.resource)
            .field("authorization_server", &self.authorization_server)
            .field("required_scope_count", &self.required_scopes.len())
            .finish()
    }
}

impl HostedHandoffHttpConfig {
    pub fn new(
        resource: impl Into<String>,
        authorization_server: impl Into<String>,
        required_scopes: Vec<String>,
    ) -> Result<Self, HostedHandoffHttpConfigError> {
        let resource = resource.into();
        let authorization_server = authorization_server.into();
        let resource_url = validate_https_url(&resource)?;
        if resource_url.query().is_some()
            || resource_url.fragment().is_some()
            || resource_url.path() == "/"
            || resource_url.path().ends_with('/')
        {
            return Err(HostedHandoffHttpConfigError::InvalidResourceUri);
        }
        let authorization_url = validate_https_url(&authorization_server)
            .map_err(|_| HostedHandoffHttpConfigError::InvalidAuthorizationServerUri)?;
        if authorization_url.query().is_some() || authorization_url.fragment().is_some() {
            return Err(HostedHandoffHttpConfigError::InvalidAuthorizationServerUri);
        }
        if required_scopes.is_empty() || required_scopes.iter().any(|scope| !valid_scope(scope)) {
            return Err(HostedHandoffHttpConfigError::InvalidScope);
        }
        let mut seen = HashSet::new();
        if required_scopes
            .iter()
            .any(|scope| !seen.insert(scope.clone()))
        {
            return Err(HostedHandoffHttpConfigError::DuplicateScope);
        }

        let base_path = resource_url.path();
        let context_path = format!("{base_path}/context");
        let control_path = format!("{base_path}/control");
        let metadata_path = format!(
            "/.well-known/oauth-protected-resource/{}",
            base_path.trim_start_matches('/')
        );
        let mut metadata_url = resource_url.clone();
        metadata_url.set_path(&metadata_path);
        metadata_url.set_query(None);
        metadata_url.set_fragment(None);

        Ok(Self {
            resource,
            authorization_server,
            resource_url,
            metadata_url,
            context_path,
            control_path,
            required_scopes,
        })
    }

    pub fn resource(&self) -> &str {
        &self.resource
    }

    pub fn context_path(&self) -> &str {
        &self.context_path
    }

    pub fn control_path(&self) -> &str {
        &self.control_path
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

fn validate_https_url(value: &str) -> Result<Url, HostedHandoffHttpConfigError> {
    let url = Url::parse(value).map_err(|_| HostedHandoffHttpConfigError::InvalidUrl)?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(HostedHandoffHttpConfigError::HttpsRequired);
    }
    Ok(url)
}

fn valid_scope(scope: &str) -> bool {
    !scope.is_empty()
        && scope
            .as_bytes()
            .iter()
            .all(|byte| matches!(*byte, 0x21 | 0x23..=0x5b | 0x5d..=0x7e))
}

#[derive(Clone)]
struct HostedHandoffHandlerState {
    api: Arc<dyn HostedHandoffControlApi>,
}

#[derive(Clone)]
struct HostedHandoffAuthState {
    verifier: Arc<dyn AccessTokenVerifier>,
    config: Arc<HostedHandoffHttpConfig>,
}

#[derive(Clone)]
struct HostedHandoffAuthContext {
    principal: AuthenticatedClientPrincipal,
}

pub fn build_hosted_handoff_router(
    api: Arc<dyn HostedHandoffControlApi>,
    config: HostedHandoffHttpConfig,
    verifier: Arc<dyn AccessTokenVerifier>,
) -> Router {
    let config = Arc::new(config);
    let handler_state = HostedHandoffHandlerState { api };
    let auth_state = HostedHandoffAuthState {
        verifier,
        config: config.clone(),
    };
    let protected = Router::new()
        .route(config.context_path(), post(issue_context))
        .route(config.control_path(), post(execute_control))
        .with_state(handler_state)
        .layer(DefaultBodyLimit::max(MAX_HOSTED_HANDOFF_BODY_BYTES))
        .layer(middleware::from_fn_with_state(
            auth_state,
            hosted_handoff_auth_guard,
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

async fn issue_context(
    State(state): State<HostedHandoffHandlerState>,
    Extension(auth): Extension<HostedHandoffAuthContext>,
    Json(request): Json<HostedHandoffContextRequest>,
) -> Response {
    let response = state.api.issue_context(&auth.principal, request).await;
    let status = hosted_response_status(response.ok, response.error_code.as_deref());
    (status, Json(response)).into_response()
}

async fn execute_control(
    State(state): State<HostedHandoffHandlerState>,
    Extension(auth): Extension<HostedHandoffAuthContext>,
    Json(request): Json<HostedHandoffControlRequest>,
) -> Response {
    let response = state.api.execute(&auth.principal, request).await;
    let status = hosted_response_status(response.ok, response.error_code.as_deref());
    (status, Json(response)).into_response()
}

fn hosted_response_status(ok: bool, error_code: Option<&str>) -> StatusCode {
    if ok {
        return StatusCode::OK;
    }
    match error_code {
        Some("hosted_handoff_request_invalid") => StatusCode::BAD_REQUEST,
        Some("hosted_handoff_unauthorized") => StatusCode::FORBIDDEN,
        Some("handoff_runtime_unavailable") => StatusCode::SERVICE_UNAVAILABLE,
        Some("handoff_control_unsupported") => StatusCode::NOT_IMPLEMENTED,
        Some("hosted_handoff_invalid_configuration") => StatusCode::INTERNAL_SERVER_ERROR,
        _ => StatusCode::CONFLICT,
    }
}

async fn hosted_handoff_auth_guard(
    State(state): State<HostedHandoffAuthState>,
    mut request: Request,
    next: Next,
) -> Response {
    if let Err(status) = validate_exact_origin(request.headers(), &state.config.resource_url) {
        return status.into_response();
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
            return oauth_error_response(
                StatusCode::UNAUTHORIZED,
                Some(bearer_challenge(&state.config, None)),
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
            return oauth_error_response(
                StatusCode::UNAUTHORIZED,
                Some(bearer_challenge(&state.config, Some("invalid_token"))),
                "invalid_token",
                "Access token is invalid or expired",
            );
        }
        Err(TokenVerificationError::Unavailable | TokenVerificationError::InvalidConfiguration) => {
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
        return oauth_error_response(
            StatusCode::FORBIDDEN,
            Some(bearer_challenge(&state.config, Some("insufficient_scope"))),
            "insufficient_scope",
            "Access token does not contain the required Handoff operator scope",
        );
    }

    request.headers_mut().remove(AUTHORIZATION);
    request.extensions_mut().insert(HostedHandoffAuthContext {
        principal: verified.principal,
    });
    next.run(request).await
}

fn validate_exact_origin(headers: &HeaderMap, resource_url: &Url) -> Result<(), StatusCode> {
    let mut values = headers.get_all(ORIGIN).iter();
    let Some(value) = values.next() else {
        return Ok(());
    };
    if values.next().is_some() {
        return Err(StatusCode::BAD_REQUEST);
    }
    let raw = value.to_str().map_err(|_| StatusCode::BAD_REQUEST)?;
    let origin = Url::parse(raw).map_err(|_| StatusCode::BAD_REQUEST)?;
    if !origin.username().is_empty()
        || origin.password().is_some()
        || origin.path() != "/"
        || origin.query().is_some()
        || origin.fragment().is_some()
    {
        return Err(StatusCode::BAD_REQUEST);
    }
    if origin.origin() != resource_url.origin() {
        return Err(StatusCode::FORBIDDEN);
    }
    Ok(())
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

fn bearer_challenge(config: &HostedHandoffHttpConfig, error: Option<&str>) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        v2_hosted_handoff_control::{HostedHandoffAction, HostedHandoffContextResponse},
        v2_m1_northbound::VerifiedAccessToken,
    };
    use async_trait::async_trait;
    use std::sync::Mutex;

    #[derive(Clone)]
    struct StaticVerifier {
        scopes: HashSet<String>,
    }

    #[async_trait]
    impl AccessTokenVerifier for StaticVerifier {
        async fn verify(&self, token: &str) -> Result<VerifiedAccessToken, TokenVerificationError> {
            if token != "good-token" {
                return Err(TokenVerificationError::InvalidToken);
            }
            Ok(VerifiedAccessToken {
                principal: AuthenticatedClientPrincipal::new(
                    "https://operator.example",
                    "operator-1",
                )
                .unwrap(),
                scopes: self.scopes.clone(),
            })
        }
    }

    #[derive(Default)]
    struct FakeApi {
        seen: Mutex<Vec<(String, HostedHandoffAction)>>,
    }

    #[async_trait]
    impl HostedHandoffControlApi for FakeApi {
        async fn issue_context(
            &self,
            principal: &AuthenticatedClientPrincipal,
            request: HostedHandoffContextRequest,
        ) -> HostedHandoffContextResponse {
            self.seen
                .lock()
                .unwrap()
                .push((principal.subject.clone(), request.action));
            HostedHandoffContextResponse {
                ok: true,
                context_handle: Some("hctx_0123456789abcdef0123456789abcdef".to_owned()),
                error_code: None,
            }
        }

        async fn execute(
            &self,
            principal: &AuthenticatedClientPrincipal,
            request: HostedHandoffControlRequest,
        ) -> HostedHandoffControlResponse {
            self.seen
                .lock()
                .unwrap()
                .push((principal.subject.clone(), request.action));
            HostedHandoffControlResponse {
                ok: true,
                status: None,
                error_code: None,
            }
        }
    }

    fn config() -> HostedHandoffHttpConfig {
        HostedHandoffHttpConfig::new(
            "https://control.example/operator/v1/handoff",
            "https://operator.example",
            vec!["cumg.handoff.control".to_owned()],
        )
        .unwrap()
    }

    async fn spawn_router(scopes: &[&str]) -> (String, Arc<FakeApi>, tokio::task::JoinHandle<()>) {
        let api = Arc::new(FakeApi::default());
        let verifier = Arc::new(StaticVerifier {
            scopes: scopes.iter().map(|value| (*value).to_owned()).collect(),
        });
        let router = build_hosted_handoff_router(api.clone(), config(), verifier);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        (format!("http://{address}"), api, task)
    }

    #[test]
    fn hosted_http_config_is_a_distinct_https_resource() {
        let config = config();
        assert_eq!(config.context_path(), "/operator/v1/handoff/context");
        assert_eq!(config.control_path(), "/operator/v1/handoff/control");
        assert_ne!(config.context_path(), "/mcp");
        assert_eq!(
            HostedHandoffHttpConfig::new(
                "http://control.example/operator/v1/handoff",
                "https://operator.example",
                vec!["cumg.handoff.control".to_owned()],
            )
            .unwrap_err(),
            HostedHandoffHttpConfigError::HttpsRequired
        );
    }

    #[tokio::test]
    async fn hosted_http_requires_bearer_scope_and_never_exposes_mcp_routes() {
        let (base, api, task) = spawn_router(&["cumg.handoff.control"]).await;
        let client = reqwest::Client::new();
        let missing = client
            .post(format!("{base}/operator/v1/handoff/context"))
            .json(&json!({"action":"begin"}))
            .send()
            .await
            .unwrap();
        assert_eq!(missing.status(), reqwest::StatusCode::UNAUTHORIZED);

        let context = client
            .post(format!("{base}/operator/v1/handoff/context"))
            .bearer_auth("good-token")
            .json(&json!({"action":"begin"}))
            .send()
            .await
            .unwrap();
        assert_eq!(context.status(), reqwest::StatusCode::OK);
        let body: Value = context.json().await.unwrap();
        assert_eq!(
            body["context_handle"],
            "hctx_0123456789abcdef0123456789abcdef"
        );

        let control = client
            .post(format!("{base}/operator/v1/handoff/control"))
            .bearer_auth("good-token")
            .json(&json!({
                "action":"begin",
                "context_handle":"hctx_0123456789abcdef0123456789abcdef"
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(control.status(), reqwest::StatusCode::OK);

        assert_eq!(
            client
                .post(format!("{base}/mcp"))
                .bearer_auth("good-token")
                .send()
                .await
                .unwrap()
                .status(),
            reqwest::StatusCode::NOT_FOUND
        );
        assert_eq!(api.seen.lock().unwrap().len(), 2);
        task.abort();
    }

    #[tokio::test]
    async fn hosted_http_fails_closed_for_scope_origin_and_query_token() {
        let (base, _, task) = spawn_router(&[]).await;
        let client = reqwest::Client::new();
        let scope = client
            .post(format!("{base}/operator/v1/handoff/context"))
            .bearer_auth("good-token")
            .json(&json!({"action":"begin"}))
            .send()
            .await
            .unwrap();
        assert_eq!(scope.status(), reqwest::StatusCode::FORBIDDEN);
        task.abort();

        let (base, _, task) = spawn_router(&["cumg.handoff.control"]).await;
        let origin = client
            .post(format!("{base}/operator/v1/handoff/context"))
            .header("origin", "https://evil.example")
            .bearer_auth("good-token")
            .json(&json!({"action":"begin"}))
            .send()
            .await
            .unwrap();
        assert_eq!(origin.status(), reqwest::StatusCode::FORBIDDEN);

        let query = client
            .post(format!(
                "{base}/operator/v1/handoff/context?access_token=good-token"
            ))
            .bearer_auth("good-token")
            .json(&json!({"action":"begin"}))
            .send()
            .await
            .unwrap();
        assert_eq!(query.status(), reqwest::StatusCode::BAD_REQUEST);
        task.abort();
    }
}
