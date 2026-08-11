//! V2-M1 single secure remote Agent runtime semantics.

use crate::v2_m0::{CommandEnvelope, ControlError, DeviceSession, validate_command_session};
use crate::v2_m0_transport::AgentHeartbeat;
use std::fmt;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReconnectPolicy {
    pub initial_delay: Duration,
    pub max_delay: Duration,
    pub max_attempts: u32,
}

impl ReconnectPolicy {
    pub fn validate(self) -> Result<Self, M1Error> {
        if self.initial_delay.is_zero()
            || self.max_delay < self.initial_delay
            || self.max_attempts == 0
        {
            return Err(M1Error::InvalidReconnectPolicy);
        }
        Ok(self)
    }

    pub fn delay_for_attempt(self, attempt: u32) -> Result<Duration, M1Error> {
        let policy = self.validate()?;
        if attempt >= policy.max_attempts {
            return Err(M1Error::ReconnectExhausted);
        }
        let multiplier = 1_u128 << attempt.min(63);
        let initial_ms = policy.initial_delay.as_millis();
        let max_ms = policy.max_delay.as_millis();
        let delay_ms = initial_ms.saturating_mul(multiplier).min(max_ms);
        Ok(Duration::from_millis(
            u64::try_from(delay_ms).unwrap_or(u64::MAX),
        ))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeartbeatTracker {
    device_id: String,
    device_generation: u64,
    timeout_ms: u64,
    last_sequence: Option<u64>,
    last_seen_ms: u64,
}

impl HeartbeatTracker {
    pub fn new(
        device_id: impl Into<String>,
        device_generation: u64,
        connected_at_ms: u64,
        timeout_ms: u64,
    ) -> Result<Self, M1Error> {
        if timeout_ms == 0 {
            return Err(M1Error::InvalidHeartbeatTimeout);
        }
        Ok(Self {
            device_id: device_id.into(),
            device_generation,
            timeout_ms,
            last_sequence: None,
            last_seen_ms: connected_at_ms,
        })
    }

    pub fn observe(
        &mut self,
        heartbeat: &AgentHeartbeat,
        received_at_ms: u64,
    ) -> Result<(), M1Error> {
        if heartbeat.device_id != self.device_id {
            return Err(M1Error::HeartbeatDeviceMismatch);
        }
        if heartbeat.device_generation != self.device_generation {
            return Err(M1Error::HeartbeatGenerationMismatch);
        }
        if self
            .last_sequence
            .is_some_and(|sequence| heartbeat.sequence <= sequence)
        {
            return Err(M1Error::HeartbeatReplay);
        }
        if received_at_ms < self.last_seen_ms {
            return Err(M1Error::NonMonotonicHeartbeatTime);
        }
        self.last_sequence = Some(heartbeat.sequence);
        self.last_seen_ms = received_at_ms;
        Ok(())
    }

    pub fn is_timed_out(&self, now_ms: u64) -> bool {
        now_ms.saturating_sub(self.last_seen_ms) >= self.timeout_ms
    }

    pub fn last_sequence(&self) -> Option<u64> {
        self.last_sequence
    }
}

#[derive(Debug, Clone)]
pub struct SingleDeviceRouter {
    device_id: String,
    session: Option<DeviceSession>,
}

impl SingleDeviceRouter {
    pub fn new(device_id: impl Into<String>) -> Result<Self, M1Error> {
        let device_id = device_id.into();
        if device_id.trim().is_empty() {
            return Err(M1Error::InvalidDeviceId);
        }
        Ok(Self {
            device_id,
            session: None,
        })
    }

    pub fn connect(&mut self, session: DeviceSession) -> Result<(), M1Error> {
        if session.device_id != self.device_id {
            return Err(M1Error::WrongDevice);
        }
        self.session = Some(session);
        Ok(())
    }

    pub fn disconnect(&mut self, generation: u64) {
        if self
            .session
            .as_ref()
            .is_some_and(|session| session.generation == generation)
        {
            self.session = None;
        }
    }

    pub fn route(&self, command: &CommandEnvelope) -> Result<&DeviceSession, M1Error> {
        let session = self.session.as_ref().ok_or(M1Error::DeviceOffline)?;
        validate_command_session(command, session).map_err(M1Error::Control)?;
        Ok(session)
    }

    pub fn session(&self) -> Option<&DeviceSession> {
        self.session.as_ref()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum M1Error {
    InvalidReconnectPolicy,
    ReconnectExhausted,
    InvalidHeartbeatTimeout,
    HeartbeatDeviceMismatch,
    HeartbeatGenerationMismatch,
    HeartbeatReplay,
    NonMonotonicHeartbeatTime,
    InvalidDeviceId,
    WrongDevice,
    DeviceOffline,
    Control(ControlError),
}

impl fmt::Display for M1Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for M1Error {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v2_m0::{
        CAPABILITY_SCHEMA_VERSION, CONTROL_SCHEMA_VERSION, CapabilityAdvertisement,
        DeviceCapability, DeviceCommand,
    };

    fn session(generation: u64) -> DeviceSession {
        DeviceSession {
            device_id: "dev-a".into(),
            generation,
            capabilities: CapabilityAdvertisement {
                backend: "fixture".into(),
                backend_version: "1".into(),
                platform: "fixture".into(),
                capability_schema_version: CAPABILITY_SCHEMA_VERSION,
                revision: 3,
                supported: vec![DeviceCapability::ListApplications],
            },
        }
    }

    fn command(generation: u64) -> CommandEnvelope {
        CommandEnvelope {
            schema_version: CONTROL_SCHEMA_VERSION,
            device_id: "dev-a".into(),
            device_generation: generation,
            capability_revision: 3,
            operation_id: "op-1".into(),
            command: DeviceCommand::ListApplications,
        }
    }

    #[test]
    fn reconnect_backoff_is_exponential_bounded_and_finite() {
        let policy = ReconnectPolicy {
            initial_delay: Duration::from_millis(250),
            max_delay: Duration::from_secs(4),
            max_attempts: 8,
        };
        let delays: Vec<_> = (0..8)
            .map(|attempt| policy.delay_for_attempt(attempt).unwrap())
            .collect();
        assert_eq!(delays[0], Duration::from_millis(250));
        assert_eq!(delays[1], Duration::from_millis(500));
        assert_eq!(delays[4], Duration::from_secs(4));
        assert_eq!(delays[7], Duration::from_secs(4));
        assert_eq!(
            policy.delay_for_attempt(8),
            Err(M1Error::ReconnectExhausted)
        );
    }

    #[test]
    fn heartbeat_replay_generation_mismatch_and_timeout_fail_closed() {
        let mut tracker = HeartbeatTracker::new("dev-a", 7, 1_000, 5_000).unwrap();
        let heartbeat = AgentHeartbeat {
            schema_version: 1,
            device_id: "dev-a".into(),
            device_generation: 7,
            sequence: 1,
            signature: vec![],
        };
        tracker.observe(&heartbeat, 2_000).unwrap();
        assert_eq!(tracker.last_sequence(), Some(1));
        assert_eq!(
            tracker.observe(&heartbeat, 2_001),
            Err(M1Error::HeartbeatReplay)
        );
        let mut wrong_generation = heartbeat.clone();
        wrong_generation.sequence = 2;
        wrong_generation.device_generation = 8;
        assert_eq!(
            tracker.observe(&wrong_generation, 2_002),
            Err(M1Error::HeartbeatGenerationMismatch)
        );
        assert!(!tracker.is_timed_out(6_999));
        assert!(tracker.is_timed_out(7_000));
    }

    #[test]
    fn single_device_router_rejects_offline_wrong_and_stale_routes() {
        let mut router = SingleDeviceRouter::new("dev-a").unwrap();
        assert_eq!(router.route(&command(1)), Err(M1Error::DeviceOffline));
        let mut wrong = session(1);
        wrong.device_id = "dev-b".into();
        assert_eq!(router.connect(wrong), Err(M1Error::WrongDevice));
        router.connect(session(2)).unwrap();
        assert!(router.route(&command(2)).is_ok());
        assert!(matches!(
            router.route(&command(1)),
            Err(M1Error::Control(ControlError::StaleDeviceGeneration { .. }))
        ));
        router.disconnect(1);
        assert!(router.session().is_some());
        router.disconnect(2);
        assert_eq!(router.route(&command(2)), Err(M1Error::DeviceOffline));
    }
}
