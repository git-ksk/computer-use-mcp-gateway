//! Small in-process overload guards that complement, but never replace, the
//! deployment reverse proxy/firewall and Hub operation admission controller.

use axum::response::IntoResponse as _;
use std::{
    collections::VecDeque,
    sync::Mutex,
    time::{Duration, Instant},
};
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
        url.path = request.uri().path(),
    );
    async move {
        if !guard.rate.try_acquire() {
            crate::v2_observability::increment_counter(
                "cumg.v2.northbound_request_rejected",
                &[opentelemetry::KeyValue::new("reason", "rate_limit")],
            );
            return axum::http::StatusCode::TOO_MANY_REQUESTS.into_response();
        }
        let Ok(permit) = guard.slots.clone().try_acquire_owned() else {
            crate::v2_observability::increment_counter(
                "cumg.v2.northbound_request_rejected",
                &[opentelemetry::KeyValue::new("reason", "concurrency_limit")],
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
