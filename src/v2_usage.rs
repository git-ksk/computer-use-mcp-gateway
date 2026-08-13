//! Optional usage-accounting seam for the V2 northbound runtime.
//!
//! CUMG remains the sole execution/replay/quarantine authority. Usage controllers
//! only admit accounting reservations, mark the cost-liability boundary, and
//! settle usage. No bearer token, tool payload, or CUMG safety state crosses this
//! boundary.

use async_trait::async_trait;
use reqwest::{Client, Url};
use serde::{Deserialize, Serialize};
use std::{
    fmt,
    net::IpAddr,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU8, Ordering},
    },
    time::Duration,
};

const RESERVED: u8 = 0;
const LIABLE: u8 = 1;
const DISPATCHED: u8 = 2;

#[derive(Clone, PartialEq, Eq)]
pub struct UsageOperation {
    pub operation_id: String,
    pub issuer: String,
    pub subject: String,
    pub tool: String,
}

#[derive(Clone, PartialEq, Eq)]
pub struct UsageReservation {
    pub operation: UsageOperation,
    /// Opaque sidecar-owned reservation identity. It is never used as CUMG
    /// execution authority.
    pub reservation_id: String,
}

impl fmt::Debug for UsageOperation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UsageOperation")
            .field("operation_id", &self.operation_id)
            .field("principal", &"[REDACTED]")
            .field("tool", &self.tool)
            .finish()
    }
}

impl fmt::Debug for UsageReservation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UsageReservation")
            .field("operation", &self.operation)
            .field("reservation_id", &self.reservation_id)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UsageAdmission {
    Allowed(UsageReservation),
    Denied { reason: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageSettlement {
    Zero,
    Full,
}

impl UsageSettlement {
    fn actual_units(self) -> u64 {
        match self {
            Self::Zero => 0,
            Self::Full => 1,
        }
    }
}

#[async_trait]
pub trait UsageController: Send + Sync {
    async fn reserve(&self, operation: &UsageOperation) -> Result<UsageAdmission, UsageError>;

    async fn mark_liable(&self, reservation: &UsageReservation) -> Result<(), UsageError>;

    async fn settle(
        &self,
        reservation: &UsageReservation,
        settlement: UsageSettlement,
        outcome: &'static str,
    ) -> Result<(), UsageError>;
}

#[derive(Debug, Default)]
pub struct NoopUsageController;

#[async_trait]
impl UsageController for NoopUsageController {
    async fn reserve(&self, operation: &UsageOperation) -> Result<UsageAdmission, UsageError> {
        Ok(UsageAdmission::Allowed(UsageReservation {
            operation: operation.clone(),
            reservation_id: operation.operation_id.clone(),
        }))
    }

    async fn mark_liable(&self, _reservation: &UsageReservation) -> Result<(), UsageError> {
        Ok(())
    }

    async fn settle(
        &self,
        _reservation: &UsageReservation,
        _settlement: UsageSettlement,
        _outcome: &'static str,
    ) -> Result<(), UsageError> {
        Ok(())
    }
}

#[derive(Clone)]
pub struct UsageManager {
    controller: Arc<dyn UsageController>,
}

impl Default for UsageManager {
    fn default() -> Self {
        Self::noop()
    }
}

impl UsageManager {
    pub fn noop() -> Self {
        Self {
            controller: Arc::new(NoopUsageController),
        }
    }

    pub fn new(controller: Arc<dyn UsageController>) -> Self {
        Self { controller }
    }

    pub async fn reserve(&self, operation: UsageOperation) -> Result<UsageLease, UsageError> {
        match self.controller.reserve(&operation).await? {
            UsageAdmission::Allowed(reservation) => Ok(UsageLease {
                inner: Arc::new(UsageLeaseInner {
                    controller: self.controller.clone(),
                    reservation,
                    phase: AtomicU8::new(RESERVED),
                    settled: AtomicBool::new(false),
                }),
            }),
            UsageAdmission::Denied { reason } => Err(UsageError::Denied(reason)),
        }
    }
}

struct UsageLeaseInner {
    controller: Arc<dyn UsageController>,
    reservation: UsageReservation,
    phase: AtomicU8,
    settled: AtomicBool,
}

#[derive(Clone)]
pub struct UsageLease {
    inner: Arc<UsageLeaseInner>,
}

impl fmt::Debug for UsageLease {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UsageLease")
            .field("operation_id", &self.operation_id())
            .field("phase", &self.inner.phase.load(Ordering::Acquire))
            .field("settled", &self.inner.settled.load(Ordering::Acquire))
            .finish()
    }
}

impl UsageLease {
    pub fn operation_id(&self) -> &str {
        &self.inner.reservation.operation.operation_id
    }

    /// Called by the Hub immediately before its persisted/network dispatch
    /// boundary. Failure is fail-closed: the Agent must not receive the command.
    pub async fn mark_liable(&self) -> Result<(), UsageError> {
        if self.inner.phase.load(Ordering::Acquire) >= LIABLE {
            return Ok(());
        }
        self.inner
            .controller
            .mark_liable(&self.inner.reservation)
            .await?;
        self.inner.phase.store(LIABLE, Ordering::Release);
        Ok(())
    }

    /// Called only after the command has been accepted by the Hub -> Agent
    /// outbound transport. From this point an effect is possible.
    pub fn mark_dispatched(&self) {
        debug_assert!(self.inner.phase.load(Ordering::Acquire) >= LIABLE);
        self.inner.phase.store(DISPATCHED, Ordering::Release);
    }

    pub fn was_dispatched(&self) -> bool {
        self.inner.phase.load(Ordering::Acquire) >= DISPATCHED
    }

    pub async fn settle(
        &self,
        settlement: UsageSettlement,
        outcome: &'static str,
    ) -> Result<(), UsageError> {
        if self.inner.settled.load(Ordering::Acquire) {
            return Ok(());
        }
        self.inner
            .controller
            .settle(&self.inner.reservation, settlement, outcome)
            .await?;
        self.inner.settled.store(true, Ordering::Release);
        Ok(())
    }
}

#[derive(Clone)]
pub struct McpUsageController {
    client: Client,
    base_url: Url,
}

impl McpUsageController {
    pub fn new(endpoint: &str, timeout: Duration) -> Result<Self, UsageError> {
        if timeout.is_zero() {
            return Err(UsageError::InvalidConfiguration);
        }
        let base_url = Url::parse(endpoint).map_err(|_| UsageError::InvalidConfiguration)?;
        let host = base_url
            .host_str()
            .map(|host| {
                host.strip_prefix('[')
                    .and_then(|value| value.strip_suffix(']'))
                    .unwrap_or(host)
            })
            .and_then(|host| host.parse::<IpAddr>().ok())
            .ok_or(UsageError::InvalidConfiguration)?;
        if base_url.scheme() != "http"
            || !host.is_loopback()
            || !base_url.username().is_empty()
            || base_url.password().is_some()
            || base_url.query().is_some()
            || base_url.fragment().is_some()
            || !matches!(base_url.path(), "" | "/")
        {
            return Err(UsageError::InvalidConfiguration);
        }
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(timeout)
            .build()
            .map_err(|_| UsageError::InvalidConfiguration)?;
        Ok(Self { client, base_url })
    }

    fn url(&self, path: &'static str) -> Result<Url, UsageError> {
        self.base_url
            .join(path)
            .map_err(|_| UsageError::InvalidConfiguration)
    }

    async fn post<T: Serialize, R: for<'de> Deserialize<'de>>(
        &self,
        path: &'static str,
        body: &T,
    ) -> Result<R, UsageError> {
        let response = self
            .client
            .post(self.url(path)?)
            .json(body)
            .send()
            .await
            .map_err(|_| UsageError::Unavailable)?;
        if !response.status().is_success() {
            return Err(UsageError::Unavailable);
        }
        response
            .json::<R>()
            .await
            .map_err(|_| UsageError::InvalidResponse)
    }
}

#[derive(Serialize)]
struct ReserveRequest<'a> {
    #[serde(rename = "operationId")]
    operation_id: &'a str,
    principal: PrincipalRequest<'a>,
    tool: &'a str,
}

#[derive(Serialize)]
struct PrincipalRequest<'a> {
    issuer: &'a str,
    subject: &'a str,
}

#[derive(Deserialize)]
struct ReserveResponse {
    allowed: bool,
    #[serde(rename = "reservationId")]
    reservation_id: Option<String>,
    reason: Option<String>,
}

#[derive(Serialize)]
struct ReservationRequest<'a> {
    #[serde(rename = "reservationId")]
    reservation_id: &'a str,
}

#[derive(Serialize)]
struct SettlementRequest<'a> {
    #[serde(rename = "reservationId")]
    reservation_id: &'a str,
    #[serde(rename = "actualUnits")]
    actual_units: u64,
    outcome: &'a str,
}

#[derive(Deserialize)]
struct OkResponse {
    ok: bool,
}

#[async_trait]
impl UsageController for McpUsageController {
    async fn reserve(&self, operation: &UsageOperation) -> Result<UsageAdmission, UsageError> {
        let response: ReserveResponse = self
            .post(
                "v1/reserve",
                &ReserveRequest {
                    operation_id: &operation.operation_id,
                    principal: PrincipalRequest {
                        issuer: &operation.issuer,
                        subject: &operation.subject,
                    },
                    tool: &operation.tool,
                },
            )
            .await?;
        if response.allowed {
            let reservation_id = response
                .reservation_id
                .filter(|value| !value.is_empty())
                .ok_or(UsageError::InvalidResponse)?;
            Ok(UsageAdmission::Allowed(UsageReservation {
                operation: operation.clone(),
                reservation_id,
            }))
        } else {
            Ok(UsageAdmission::Denied {
                reason: response.reason.unwrap_or_else(|| "usage_denied".into()),
            })
        }
    }

    async fn mark_liable(&self, reservation: &UsageReservation) -> Result<(), UsageError> {
        let response: OkResponse = self
            .post(
                "v1/mark-liable",
                &ReservationRequest {
                    reservation_id: &reservation.reservation_id,
                },
            )
            .await?;
        if response.ok {
            Ok(())
        } else {
            Err(UsageError::InvalidResponse)
        }
    }

    async fn settle(
        &self,
        reservation: &UsageReservation,
        settlement: UsageSettlement,
        outcome: &'static str,
    ) -> Result<(), UsageError> {
        let response: OkResponse = self
            .post(
                "v1/settle",
                &SettlementRequest {
                    reservation_id: &reservation.reservation_id,
                    actual_units: settlement.actual_units(),
                    outcome,
                },
            )
            .await?;
        if response.ok {
            Ok(())
        } else {
            Err(UsageError::InvalidResponse)
        }
    }
}

pub enum UsageError {
    Denied(String),
    Unavailable,
    InvalidResponse,
    InvalidConfiguration,
}

impl UsageError {
    pub fn safe_error_code(&self) -> &'static str {
        match self {
            Self::Denied(_) => "usage_denied",
            Self::Unavailable => "usage_unavailable",
            Self::InvalidResponse => "usage_invalid_response",
            Self::InvalidConfiguration => "usage_invalid_configuration",
        }
    }
}

impl fmt::Debug for UsageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.safe_error_code())
    }
}

impl fmt::Display for UsageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.safe_error_code())
    }
}

impl std::error::Error for UsageError {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    #[test]
    fn mcp_usage_endpoint_must_be_literal_loopback_http() {
        assert!(McpUsageController::new("http://127.0.0.1:8787", Duration::from_secs(1)).is_ok());
        assert!(McpUsageController::new("http://[::1]:8787", Duration::from_secs(1)).is_ok());
        assert!(McpUsageController::new("http://localhost:8787", Duration::from_secs(1)).is_err());
        assert!(McpUsageController::new("https://127.0.0.1:8787", Duration::from_secs(1)).is_err());
        assert!(McpUsageController::new("http://0.0.0.0:8787", Duration::from_secs(1)).is_err());
    }

    #[derive(Default)]
    struct RecordingController {
        fail_mark: AtomicBool,
        deny: AtomicBool,
        events: StdMutex<Vec<String>>,
    }

    #[async_trait]
    impl UsageController for RecordingController {
        async fn reserve(&self, operation: &UsageOperation) -> Result<UsageAdmission, UsageError> {
            self.events
                .lock()
                .unwrap()
                .push(format!("reserve:{}", operation.operation_id));
            if self.deny.load(Ordering::Acquire) {
                return Ok(UsageAdmission::Denied {
                    reason: "quota_exceeded".into(),
                });
            }
            Ok(UsageAdmission::Allowed(UsageReservation {
                operation: operation.clone(),
                reservation_id: format!("reservation:{}", operation.operation_id),
            }))
        }

        async fn mark_liable(&self, reservation: &UsageReservation) -> Result<(), UsageError> {
            self.events
                .lock()
                .unwrap()
                .push(format!("liable:{}", reservation.operation.operation_id));
            if self.fail_mark.load(Ordering::Acquire) {
                return Err(UsageError::Unavailable);
            }
            Ok(())
        }

        async fn settle(
            &self,
            reservation: &UsageReservation,
            settlement: UsageSettlement,
            outcome: &'static str,
        ) -> Result<(), UsageError> {
            self.events.lock().unwrap().push(format!(
                "settle:{}:{}:{outcome}",
                reservation.operation.operation_id,
                settlement.actual_units()
            ));
            Ok(())
        }
    }

    fn operation(id: &str) -> UsageOperation {
        UsageOperation {
            operation_id: id.into(),
            issuer: "https://issuer.example".into(),
            subject: "alice".into(),
            tool: "click".into(),
        }
    }

    #[tokio::test]
    async fn noop_controller_preserves_v2_execution_path() {
        let lease = UsageManager::noop()
            .reserve(operation("noop-1"))
            .await
            .unwrap();
        lease.mark_liable().await.unwrap();
        assert!(!lease.was_dispatched());
        lease.mark_dispatched();
        assert!(lease.was_dispatched());
        lease
            .settle(UsageSettlement::Full, "completed")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn denied_reservation_fails_before_any_liability_transition() {
        let controller = Arc::new(RecordingController::default());
        controller.deny.store(true, Ordering::Release);
        let manager = UsageManager::new(controller.clone());
        assert!(matches!(
            manager.reserve(operation("deny-1")).await,
            Err(UsageError::Denied(_))
        ));
        assert_eq!(
            controller.events.lock().unwrap().as_slice(),
            ["reserve:deny-1"]
        );
    }

    #[tokio::test]
    async fn mark_liable_failure_never_marks_operation_as_dispatched() {
        let controller = Arc::new(RecordingController::default());
        controller.fail_mark.store(true, Ordering::Release);
        let lease = UsageManager::new(controller.clone())
            .reserve(operation("mark-fail-1"))
            .await
            .unwrap();
        assert!(matches!(
            lease.mark_liable().await,
            Err(UsageError::Unavailable)
        ));
        assert!(!lease.was_dispatched());
        lease
            .settle(UsageSettlement::Zero, "pre_dispatch_rejected")
            .await
            .unwrap();
        assert_eq!(
            controller.events.lock().unwrap().as_slice(),
            [
                "reserve:mark-fail-1",
                "liable:mark-fail-1",
                "settle:mark-fail-1:0:pre_dispatch_rejected"
            ]
        );
    }

    #[tokio::test]
    async fn one_logical_operation_maps_to_zero_or_one_unit_only() {
        let controller = Arc::new(RecordingController::default());
        let manager = UsageManager::new(controller.clone());
        let zero = manager.reserve(operation("zero-1")).await.unwrap();
        zero.settle(UsageSettlement::Zero, "authorization_denied")
            .await
            .unwrap();
        let full = manager.reserve(operation("full-1")).await.unwrap();
        full.mark_liable().await.unwrap();
        full.mark_dispatched();
        full.settle(UsageSettlement::Full, "completed")
            .await
            .unwrap();
        let events = controller.events.lock().unwrap();
        assert!(events.contains(&"settle:zero-1:0:authorization_denied".into()));
        assert!(events.contains(&"settle:full-1:1:completed".into()));
    }

    #[test]
    fn usage_debug_redacts_verified_principal_identity() {
        let operation = UsageOperation {
            operation_id: "op-debug".into(),
            issuer: "https://PRIVATE_ISSUER_DO_NOT_LOG".into(),
            subject: "PRIVATE_SUBJECT_DO_NOT_LOG".into(),
            tool: "click".into(),
        };
        let rendered = format!("{operation:?}");
        assert!(!rendered.contains("PRIVATE_ISSUER_DO_NOT_LOG"));
        assert!(!rendered.contains("PRIVATE_SUBJECT_DO_NOT_LOG"));
        assert!(rendered.contains("[REDACTED]"));
    }
}
