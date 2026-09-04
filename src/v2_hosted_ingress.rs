//! Closed route classification for the future hosted one-port CUMG ingress.
//!
//! This module does not open a listener or authenticate a caller. It fixes the
//! protocol-routing boundary that a one-port HTTP/2/h2c listener must enforce
//! before dispatching to the existing Agent gRPC, northbound MCP, or hosted
//! Handoff routers. Unknown or ambiguous routes fail closed.

use axum::http::Method;
use std::fmt;

pub const AGENT_GRPC_OPEN_SESSION_PATH: &str = "/cumg.v2.AgentControl/OpenSession";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostedIngressSurface {
    AgentGrpc,
    NorthboundMcp,
    NorthboundMetadata,
    HostedHandoffContext,
    HostedHandoffControl,
    HostedHandoffMetadata,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostedIngressError {
    InvalidConfiguration,
    UnknownRoute,
    MethodNotAllowed,
    InvalidContentType,
}

impl HostedIngressError {
    pub const fn safe_code(self) -> &'static str {
        match self {
            Self::InvalidConfiguration => "hosted_ingress_invalid_configuration",
            Self::UnknownRoute => "hosted_ingress_unknown_route",
            Self::MethodNotAllowed => "hosted_ingress_method_not_allowed",
            Self::InvalidContentType => "hosted_ingress_invalid_content_type",
        }
    }
}

impl fmt::Display for HostedIngressError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.safe_code())
    }
}

impl std::error::Error for HostedIngressError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostedIngressClassifier {
    mcp_path: String,
    mcp_metadata_path: String,
    handoff_context_path: String,
    handoff_control_path: String,
    handoff_metadata_path: String,
}

impl HostedIngressClassifier {
    pub fn new(
        mcp_path: impl Into<String>,
        mcp_metadata_path: impl Into<String>,
        handoff_context_path: impl Into<String>,
        handoff_control_path: impl Into<String>,
        handoff_metadata_path: impl Into<String>,
    ) -> Result<Self, HostedIngressError> {
        let classifier = Self {
            mcp_path: mcp_path.into(),
            mcp_metadata_path: mcp_metadata_path.into(),
            handoff_context_path: handoff_context_path.into(),
            handoff_control_path: handoff_control_path.into(),
            handoff_metadata_path: handoff_metadata_path.into(),
        };
        classifier.validate()?;
        Ok(classifier)
    }

    fn validate(&self) -> Result<(), HostedIngressError> {
        let routes = [
            self.mcp_path.as_str(),
            self.mcp_metadata_path.as_str(),
            self.handoff_context_path.as_str(),
            self.handoff_control_path.as_str(),
            self.handoff_metadata_path.as_str(),
            AGENT_GRPC_OPEN_SESSION_PATH,
        ];
        if routes.iter().any(|path| !valid_exact_path(path)) {
            return Err(HostedIngressError::InvalidConfiguration);
        }
        for (index, left) in routes.iter().enumerate() {
            if routes[index + 1..].contains(left) {
                return Err(HostedIngressError::InvalidConfiguration);
            }
        }
        Ok(())
    }

    pub fn classify(
        &self,
        method: &Method,
        path: &str,
        content_type: Option<&str>,
    ) -> Result<HostedIngressSurface, HostedIngressError> {
        if path == AGENT_GRPC_OPEN_SESSION_PATH {
            if method != Method::POST {
                return Err(HostedIngressError::MethodNotAllowed);
            }
            if !content_type.is_some_and(is_native_grpc_content_type) {
                return Err(HostedIngressError::InvalidContentType);
            }
            return Ok(HostedIngressSurface::AgentGrpc);
        }
        if path == self.mcp_path {
            if !matches!(*method, Method::POST | Method::GET | Method::DELETE) {
                return Err(HostedIngressError::MethodNotAllowed);
            }
            return Ok(HostedIngressSurface::NorthboundMcp);
        }
        if path == self.mcp_metadata_path {
            return exact_get(method, HostedIngressSurface::NorthboundMetadata);
        }
        if path == self.handoff_context_path {
            return exact_json_post(
                method,
                content_type,
                HostedIngressSurface::HostedHandoffContext,
            );
        }
        if path == self.handoff_control_path {
            return exact_json_post(
                method,
                content_type,
                HostedIngressSurface::HostedHandoffControl,
            );
        }
        if path == self.handoff_metadata_path {
            return exact_get(method, HostedIngressSurface::HostedHandoffMetadata);
        }
        Err(HostedIngressError::UnknownRoute)
    }
}

fn exact_get(
    method: &Method,
    surface: HostedIngressSurface,
) -> Result<HostedIngressSurface, HostedIngressError> {
    if method == Method::GET {
        Ok(surface)
    } else {
        Err(HostedIngressError::MethodNotAllowed)
    }
}

fn exact_json_post(
    method: &Method,
    content_type: Option<&str>,
    surface: HostedIngressSurface,
) -> Result<HostedIngressSurface, HostedIngressError> {
    if method != Method::POST {
        return Err(HostedIngressError::MethodNotAllowed);
    }
    if !content_type.is_some_and(is_json_content_type) {
        return Err(HostedIngressError::InvalidContentType);
    }
    Ok(surface)
}

fn is_native_grpc_content_type(value: &str) -> bool {
    let media_type = value.split(';').next().unwrap_or("").trim();
    media_type.eq_ignore_ascii_case("application/grpc")
        || media_type
            .get(..17)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("application/grpc+"))
}

fn is_json_content_type(value: &str) -> bool {
    value
        .split(';')
        .next()
        .is_some_and(|media_type| media_type.trim().eq_ignore_ascii_case("application/json"))
}

fn valid_exact_path(path: &str) -> bool {
    path.starts_with('/')
        && path.len() > 1
        && path.len() <= 512
        && !path.contains('?')
        && !path.contains('#')
        && !path.contains("//")
        && !path.bytes().any(|byte| byte.is_ascii_control())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn classifier() -> HostedIngressClassifier {
        HostedIngressClassifier::new(
            "/mcp",
            "/.well-known/oauth-protected-resource/mcp",
            "/operator/v1/handoff/context",
            "/operator/v1/handoff/control",
            "/.well-known/oauth-protected-resource/operator/v1/handoff",
        )
        .unwrap()
    }

    #[test]
    fn exact_agent_grpc_route_requires_post_and_native_grpc_content_type() {
        let classifier = classifier();
        assert_eq!(
            classifier.classify(
                &Method::POST,
                AGENT_GRPC_OPEN_SESSION_PATH,
                Some("application/grpc+proto")
            ),
            Ok(HostedIngressSurface::AgentGrpc)
        );
        assert_eq!(
            classifier.classify(
                &Method::GET,
                AGENT_GRPC_OPEN_SESSION_PATH,
                Some("application/grpc")
            ),
            Err(HostedIngressError::MethodNotAllowed)
        );
        assert_eq!(
            classifier.classify(
                &Method::POST,
                AGENT_GRPC_OPEN_SESSION_PATH,
                Some("application/grpc-web")
            ),
            Err(HostedIngressError::InvalidContentType)
        );
    }

    #[test]
    fn mcp_route_is_exact_and_never_matches_handoff_or_grpc() {
        let classifier = classifier();
        for method in [Method::POST, Method::GET, Method::DELETE] {
            assert_eq!(
                classifier.classify(&method, "/mcp", None),
                Ok(HostedIngressSurface::NorthboundMcp)
            );
        }
        assert_eq!(
            classifier.classify(&Method::POST, "/mcp/extra", None),
            Err(HostedIngressError::UnknownRoute)
        );
    }

    #[test]
    fn hosted_handoff_routes_require_exact_json_post() {
        let classifier = classifier();
        assert_eq!(
            classifier.classify(
                &Method::POST,
                "/operator/v1/handoff/context",
                Some("application/json; charset=utf-8")
            ),
            Ok(HostedIngressSurface::HostedHandoffContext)
        );
        assert_eq!(
            classifier.classify(
                &Method::POST,
                "/operator/v1/handoff/control",
                Some("application/json")
            ),
            Ok(HostedIngressSurface::HostedHandoffControl)
        );
        assert_eq!(
            classifier.classify(
                &Method::POST,
                "/operator/v1/handoff/control",
                Some("application/grpc")
            ),
            Err(HostedIngressError::InvalidContentType)
        );
    }

    #[test]
    fn metadata_routes_are_get_only_and_distinct() {
        let classifier = classifier();
        assert_eq!(
            classifier.classify(
                &Method::GET,
                "/.well-known/oauth-protected-resource/mcp",
                None
            ),
            Ok(HostedIngressSurface::NorthboundMetadata)
        );
        assert_eq!(
            classifier.classify(
                &Method::GET,
                "/.well-known/oauth-protected-resource/operator/v1/handoff",
                None
            ),
            Ok(HostedIngressSurface::HostedHandoffMetadata)
        );
        assert_eq!(
            classifier.classify(
                &Method::POST,
                "/.well-known/oauth-protected-resource/operator/v1/handoff",
                Some("application/json")
            ),
            Err(HostedIngressError::MethodNotAllowed)
        );
    }

    #[test]
    fn unknown_and_near_match_routes_fail_closed() {
        let classifier = classifier();
        for path in [
            "/",
            "/healthz",
            "/cumg.v2.AgentControl",
            "/cumg.v2.AgentControl/OpenSession/",
            "/operator/v1/handoff",
            "/operator/v1/handoff/control/",
            "/.well-known/oauth-protected-resource",
        ] {
            assert_eq!(
                classifier.classify(&Method::POST, path, Some("application/json")),
                Err(HostedIngressError::UnknownRoute),
                "unexpectedly classified {path}"
            );
        }
    }

    #[test]
    fn duplicate_or_ambiguous_configured_paths_are_rejected() {
        assert_eq!(
            HostedIngressClassifier::new(
                "/mcp",
                "/mcp",
                "/operator/v1/handoff/context",
                "/operator/v1/handoff/control",
                "/.well-known/oauth-protected-resource/operator/v1/handoff",
            ),
            Err(HostedIngressError::InvalidConfiguration)
        );
        assert_eq!(
            HostedIngressClassifier::new(
                AGENT_GRPC_OPEN_SESSION_PATH,
                "/.well-known/oauth-protected-resource/mcp",
                "/operator/v1/handoff/context",
                "/operator/v1/handoff/control",
                "/.well-known/oauth-protected-resource/operator/v1/handoff",
            ),
            Err(HostedIngressError::InvalidConfiguration)
        );
    }
}
