//! Small in-process overload guards that complement, but never replace, the
//! deployment reverse proxy/firewall and Hub operation admission controller.

use axum::response::IntoResponse as _;
use std::{
    collections::{HashMap, VecDeque},
    net::{IpAddr, SocketAddr},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use subtle::ConstantTimeEq as _;
use tracing::Instrument as _;

#[derive(Debug)]
pub struct SlidingWindowRateLimit {
    max_events: usize,
    window: Duration,
    events: Mutex<VecDeque<Instant>>,
}

impl SlidingWindowRateLimit {
    pub fn new(max_events: usize, window: Duration) -> Result<Self, LimitConfigError> {
        if max_events == 0 || window.is_zero() {
            return Err(LimitConfigError);
        }
        Ok(Self {
            max_events,
            window,
            events: Mutex::new(VecDeque::with_capacity(max_events)),
        })
    }

    pub fn try_acquire(&self) -> bool {
        let now = Instant::now();
        let mut events = self.events.lock().expect("rate limiter mutex poisoned");
        while events
            .front()
            .is_some_and(|seen| now.duration_since(*seen) >= self.window)
        {
            events.pop_front();
        }
        if events.len() >= self.max_events {
            return false;
        }
        events.push_back(now);
        true
    }
}

#[derive(Debug, Clone, Copy)]
pub struct LimitConfigError;

impl std::fmt::Display for LimitConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "rate limit must have a non-zero event count and window")
    }
}
impl std::error::Error for LimitConfigError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sliding_window_sheds_without_waiting() {
        let limit = SlidingWindowRateLimit::new(2, Duration::from_secs(60)).unwrap();
        assert!(limit.try_acquire());
        assert!(limit.try_acquire());
        assert!(!limit.try_acquire());
    }

    #[tokio::test]
    async fn http_concurrency_limit_sheds_without_waiting() {
        let guard = HttpOverloadGuard::new(1, 10).unwrap();
        let permit = guard.slots.clone().try_acquire_owned().unwrap();
        assert!(guard.slots.clone().try_acquire_owned().is_err());
        drop(permit);
        assert!(guard.slots.clone().try_acquire_owned().is_ok());
    }

    #[test]
    fn trusted_proxy_guard_requires_strong_exact_header_credential() {
        let secret = "a".repeat(32);
        let guard = TrustedProxyLoopbackGuard::new(&secret, 2, 10).unwrap();
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            TRUSTED_PROXY_TOKEN_HEADER,
            axum::http::HeaderValue::from_str(&secret).unwrap(),
        );
        assert!(guard.authenticate_headers(&headers));
        headers.append(
            TRUSTED_PROXY_TOKEN_HEADER,
            axum::http::HeaderValue::from_static("duplicate"),
        );
        assert!(!guard.authenticate_headers(&headers));
        assert!(TrustedProxyLoopbackGuard::new("short", 2, 10).is_err());
    }

    #[test]
    fn trusted_proxy_peer_limits_are_distinct_and_leave_global_headroom() {
        let guard = TrustedProxyLoopbackGuard::new("b".repeat(32), 1, 2).unwrap();
        let peer: IpAddr = "127.0.0.1".parse().unwrap();
        let first = guard.try_acquire_peer(peer).unwrap();
        assert!(matches!(
            guard.try_acquire_peer(peer),
            Err(PeerLimitRejection::Concurrency)
        ));
        drop(first);
        assert!(matches!(
            guard.try_acquire_peer(peer),
            Err(PeerLimitRejection::Rate)
        ));
    }

    #[tokio::test]
    async fn trusted_proxy_rejections_do_not_consume_global_rate_budget() {
        use axum::{Router, routing::get};

        let secret = "d".repeat(32);
        let peer_guard = TrustedProxyLoopbackGuard::new(&secret, 2, 10).unwrap();
        let global_guard = HttpOverloadGuard::new(2, 1).unwrap();
        let app = Router::new()
            .route("/", get(|| async { axum::http::StatusCode::OK }))
            .layer(axum::middleware::from_fn_with_state(
                global_guard,
                enforce_http_limits,
            ))
            .layer(axum::middleware::from_fn_with_state(
                peer_guard,
                enforce_trusted_proxy_loopback,
            ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .unwrap();
        });
        let client = reqwest::Client::new();
        let url = format!("http://{address}/");

        for _ in 0..3 {
            let rejected = client.get(&url).send().await.unwrap();
            assert_eq!(rejected.status(), axum::http::StatusCode::FORBIDDEN);
        }

        let accepted = client
            .get(&url)
            .header(TRUSTED_PROXY_TOKEN_HEADER, &secret)
            .send()
            .await
            .unwrap();
        assert_eq!(accepted.status(), axum::http::StatusCode::OK);
        task.abort();
    }

    #[tokio::test]
    async fn trusted_proxy_http_gate_rejects_local_bypass_and_caps_peer_before_global() {
        use axum::{Router, routing::get};

        let secret = "c".repeat(32);
        let peer_guard = TrustedProxyLoopbackGuard::new(&secret, 1, 10).unwrap();
        let global_guard = HttpOverloadGuard::new(2, 20).unwrap();
        let app = Router::new()
            .route(
                "/",
                get(|request: axum::extract::Request| async move {
                    assert!(request.headers().get(TRUSTED_PROXY_TOKEN_HEADER).is_none());
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    axum::http::StatusCode::OK
                }),
            )
            .layer(axum::middleware::from_fn_with_state(
                global_guard,
                enforce_http_limits,
            ))
            .layer(axum::middleware::from_fn_with_state(
                peer_guard,
                enforce_trusted_proxy_loopback,
            ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .unwrap();
        });
        let client = reqwest::Client::new();
        let url = format!("http://{address}/");

        let rejected = client.get(&url).send().await.unwrap();
        assert_eq!(rejected.status(), axum::http::StatusCode::FORBIDDEN);

        let first = client
            .get(&url)
            .header(TRUSTED_PROXY_TOKEN_HEADER, &secret)
            .send();
        tokio::time::sleep(Duration::from_millis(20)).await;
        let second = client
            .get(&url)
            .header(TRUSTED_PROXY_TOKEN_HEADER, &secret)
            .send();
        let (first, second) = tokio::join!(first, second);
        assert_eq!(first.unwrap().status(), axum::http::StatusCode::OK);
        assert_eq!(
            second.unwrap().status(),
            axum::http::StatusCode::SERVICE_UNAVAILABLE
        );
        task.abort();
    }
}

pub const TRUSTED_PROXY_TOKEN_HEADER: &str = "x-cumg-trusted-proxy-token";
const MIN_TRUSTED_PROXY_SECRET_BYTES: usize = 32;
const MAX_TRUSTED_PROXY_SECRET_BYTES: usize = 256;

#[derive(Debug)]
struct PeerHttpLimit {
    slots: Arc<tokio::sync::Semaphore>,
    rate: SlidingWindowRateLimit,
}

#[derive(Clone)]
pub struct TrustedProxyLoopbackGuard {
    secret: Arc<[u8]>,
    max_peer_concurrency: usize,
    max_peer_requests_per_minute: usize,
    peers: Arc<Mutex<HashMap<IpAddr, Arc<PeerHttpLimit>>>>,
}

#[derive(Debug)]
enum PeerLimitRejection {
    Rate,
    Concurrency,
}

impl TrustedProxyLoopbackGuard {
    pub fn new(
        secret: impl AsRef<str>,
        max_peer_concurrency: usize,
        max_peer_requests_per_minute: usize,
    ) -> Result<Self, LimitConfigError> {
        let secret = secret.as_ref().as_bytes();
        if secret.len() < MIN_TRUSTED_PROXY_SECRET_BYTES
            || secret.len() > MAX_TRUSTED_PROXY_SECRET_BYTES
            || !secret.iter().all(|byte| (0x21..=0x7e).contains(byte))
            || max_peer_concurrency == 0
            || max_peer_requests_per_minute == 0
        {
            return Err(LimitConfigError);
        }
        Ok(Self {
            secret: Arc::from(secret),
            max_peer_concurrency,
            max_peer_requests_per_minute,
            peers: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    fn authenticate(&self, supplied: &[u8]) -> bool {
        bool::from(self.secret.as_ref().ct_eq(supplied))
    }

    fn authenticate_headers(&self, headers: &axum::http::HeaderMap) -> bool {
        let mut values = headers.get_all(TRUSTED_PROXY_TOKEN_HEADER).iter();
        let supplied = values.next().map(|value| value.as_bytes());
        supplied.is_some_and(|value| values.next().is_none() && self.authenticate(value))
    }

    fn peer_limit(&self, peer: IpAddr) -> Arc<PeerHttpLimit> {
        let mut peers = self.peers.lock().expect("peer limiter mutex poisoned");
        peers
            .entry(peer)
            .or_insert_with(|| {
                Arc::new(PeerHttpLimit {
                    slots: Arc::new(tokio::sync::Semaphore::new(self.max_peer_concurrency)),
                    rate: SlidingWindowRateLimit::new(
                        self.max_peer_requests_per_minute,
                        Duration::from_secs(60),
                    )
                    .expect("validated peer request limit"),
                })
            })
            .clone()
    }

    fn try_acquire_peer(
        &self,
        peer: IpAddr,
    ) -> Result<tokio::sync::OwnedSemaphorePermit, PeerLimitRejection> {
        let peer_limit = self.peer_limit(peer);
        if !peer_limit.rate.try_acquire() {
            return Err(PeerLimitRejection::Rate);
        }
        peer_limit
            .slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| PeerLimitRejection::Concurrency)
    }
}

pub async fn enforce_trusted_proxy_loopback(
    axum::extract::State(guard): axum::extract::State<TrustedProxyLoopbackGuard>,
    mut request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let peer = request
        .extensions()
        .get::<axum::extract::ConnectInfo<SocketAddr>>()
        .map(|info| info.0);
    let Some(peer) = peer.filter(|peer| peer.ip().is_loopback()) else {
        crate::v2_observability::northbound_request_rejected(
            crate::v2_observability::RequestRejectReason::LocalTrustGate,
        );
        tracing::warn!(
            event = "v2_trusted_proxy_request_rejected",
            outcome = "rejected",
            error_code = "local_trust_gate",
            "trusted-proxy request lacked a verified loopback peer"
        );
        return axum::http::StatusCode::FORBIDDEN.into_response();
    };

    let valid = guard.authenticate_headers(request.headers());
    request.headers_mut().remove(TRUSTED_PROXY_TOKEN_HEADER);
    if !valid {
        crate::v2_observability::northbound_request_rejected(
            crate::v2_observability::RequestRejectReason::LocalTrustGate,
        );
        tracing::warn!(
            event = "v2_trusted_proxy_request_rejected",
            outcome = "rejected",
            error_code = "local_trust_gate",
            "trusted-proxy loopback credential rejected"
        );
        return axum::http::StatusCode::FORBIDDEN.into_response();
    }

    let permit = match guard.try_acquire_peer(peer.ip()) {
        Ok(permit) => permit,
        Err(PeerLimitRejection::Rate) => {
            crate::v2_observability::northbound_request_rejected(
                crate::v2_observability::RequestRejectReason::PeerRateLimit,
            );
            tracing::warn!(
                event = "v2_trusted_proxy_request_rejected",
                outcome = "rejected",
                error_code = "peer_rate_limit",
                "trusted-proxy peer request rate limit exceeded"
            );
            return axum::http::StatusCode::TOO_MANY_REQUESTS.into_response();
        }
        Err(PeerLimitRejection::Concurrency) => {
            crate::v2_observability::northbound_request_rejected(
                crate::v2_observability::RequestRejectReason::PeerConcurrencyLimit,
            );
            tracing::warn!(
                event = "v2_trusted_proxy_request_rejected",
                outcome = "rejected",
                error_code = "peer_concurrency_limit",
                "trusted-proxy peer concurrency limit exceeded"
            );
            return axum::http::StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
    };
    let response = next.run(request).await;
    drop(permit);
    response
}

#[derive(Clone)]
pub struct HttpOverloadGuard {
    slots: std::sync::Arc<tokio::sync::Semaphore>,
    rate: std::sync::Arc<SlidingWindowRateLimit>,
}

impl HttpOverloadGuard {
    pub fn new(
        max_concurrency: usize,
        max_requests_per_minute: usize,
    ) -> Result<Self, LimitConfigError> {
        if max_concurrency == 0 {
            return Err(LimitConfigError);
        }
        Ok(Self {
            slots: std::sync::Arc::new(tokio::sync::Semaphore::new(max_concurrency)),
            rate: std::sync::Arc::new(SlidingWindowRateLimit::new(
                max_requests_per_minute,
                Duration::from_secs(60),
            )?),
        })
    }
}

pub async fn enforce_http_limits(
    axum::extract::State(guard): axum::extract::State<HttpOverloadGuard>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let span = tracing::info_span!(
        "v2_northbound_http_request",
        http.request.method = %request.method(),
    );
    async move {
        if !guard.rate.try_acquire() {
            crate::v2_observability::northbound_request_rejected(
                crate::v2_observability::RequestRejectReason::RateLimit,
            );
            tracing::warn!(
                event = "v2_northbound_request_rejected",
                outcome = "rejected",
                error_code = "rate_limit",
                "northbound request rate limit exceeded"
            );
            return axum::http::StatusCode::TOO_MANY_REQUESTS.into_response();
        }
        let Ok(permit) = guard.slots.clone().try_acquire_owned() else {
            crate::v2_observability::northbound_request_rejected(
                crate::v2_observability::RequestRejectReason::ConcurrencyLimit,
            );
            tracing::warn!(
                event = "v2_northbound_request_rejected",
                outcome = "rejected",
                error_code = "concurrency_limit",
                "northbound request concurrency limit exceeded"
            );
            return axum::http::StatusCode::SERVICE_UNAVAILABLE.into_response();
        };
        let response = next.run(request).await;
        drop(permit);
        response
    }
    .instrument(span)
    .await
}
