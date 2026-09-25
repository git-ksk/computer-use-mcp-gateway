use axum::{
    Router,
    routing::{get, post},
};
use computer_use_mcp_gateway::{
    v2_hosted_ingress::{
        AGENT_GRPC_OPEN_SESSION_PATH, HostedIngressClassifier, apply_hosted_ingress_classifier,
    },
    v2_m1_grpc::proto::{
        AgentFrame, HubFrame,
        agent_control_client::AgentControlClient,
        agent_control_server::{AgentControl, AgentControlServer},
    },
};
use std::{pin::Pin, time::Duration};
use tokio_stream::{Stream, wrappers::ReceiverStream};
use tonic::{Request, Response, Status, Streaming, service::Routes, transport::Endpoint};

#[derive(Clone)]
struct ProbeAgentService;

#[tonic::async_trait]
impl AgentControl for ProbeAgentService {
    type OpenSessionStream = Pin<Box<dyn Stream<Item = Result<HubFrame, Status>> + Send + 'static>>;

    async fn open_session(
        &self,
        _request: Request<Streaming<AgentFrame>>,
    ) -> Result<Response<Self::OpenSessionStream>, Status> {
        Err(Status::permission_denied("agent-surface-probe"))
    }
}

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

#[tokio::test]
async fn one_h2c_listener_carries_grpc_mcp_and_handoff_without_cross_surface_fallback() {
    let http = Router::new()
        .route("/mcp", post(|| async { "mcp-surface" }))
        .route(
            "/.well-known/oauth-protected-resource/mcp",
            get(|| async { "mcp-metadata" }),
        )
        .route(
            "/operator/v1/handoff/context",
            post(|| async { "handoff-context" }),
        )
        .route(
            "/operator/v1/handoff/control",
            post(|| async { "handoff-control" }),
        )
        .route(
            "/.well-known/oauth-protected-resource/operator/v1/handoff",
            get(|| async { "handoff-metadata" }),
        );
    let router = Routes::from(http)
        .add_service(AgentControlServer::new(ProbeAgentService))
        .into_axum_router();
    let router = apply_hosted_ingress_classifier(router, classifier());

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });

    let endpoint = Endpoint::from_shared(format!("http://{address}"))
        .unwrap()
        .connect_timeout(Duration::from_secs(3));
    let channel = endpoint.connect().await.unwrap();
    let mut grpc = AgentControlClient::new(channel);
    let (_tx, rx) = tokio::sync::mpsc::channel::<AgentFrame>(1);
    let error = grpc
        .open_session(ReceiverStream::new(rx))
        .await
        .unwrap_err();
    assert_eq!(error.code(), tonic::Code::PermissionDenied);
    assert_eq!(error.message(), "agent-surface-probe");

    let http2 = reqwest::Client::builder()
        .http2_prior_knowledge()
        .build()
        .unwrap();
    let base = format!("http://{address}");

    let response = http2
        .post(format!("{base}/mcp"))
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body("{}")
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success());
    assert_eq!(response.text().await.unwrap(), "mcp-surface");

    let response = http2
        .post(format!("{base}/mcp"))
        .header(reqwest::header::CONTENT_TYPE, "application/grpc")
        .body(Vec::<u8>::new())
        .send()
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        reqwest::StatusCode::UNSUPPORTED_MEDIA_TYPE
    );
    assert_eq!(
        response.text().await.unwrap(),
        "hosted_ingress_invalid_content_type"
    );

    let response = http2
        .post(format!("{base}/operator/v1/handoff/control"))
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body("{}")
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success());
    assert_eq!(response.text().await.unwrap(), "handoff-control");

    let response = http2
        .post(format!("{base}/operator/v1/handoff/control"))
        .header(reqwest::header::CONTENT_TYPE, "application/grpc")
        .body(Vec::<u8>::new())
        .send()
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        reqwest::StatusCode::UNSUPPORTED_MEDIA_TYPE
    );
    assert_eq!(
        response.text().await.unwrap(),
        "hosted_ingress_invalid_content_type"
    );

    let response = http2
        .post(format!("{base}{AGENT_GRPC_OPEN_SESSION_PATH}"))
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body("{}")
        .send()
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        reqwest::StatusCode::UNSUPPORTED_MEDIA_TYPE
    );

    let response = http2
        .post(format!("{base}/not-a-route"))
        .body("{}")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::NOT_FOUND);
    assert_eq!(
        response.text().await.unwrap(),
        "hosted_ingress_unknown_route"
    );

    server.abort();
}
