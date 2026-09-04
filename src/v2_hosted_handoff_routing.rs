//! Provider-blind hosted routing fences for Human Handoff.
//!
//! This module models only ephemeral route/session metadata. It deliberately has no
//! Agent/Human execution-authority state, media frames, Human input, ICE/SDP, TURN
//! credentials, or durable restore API. A new process therefore starts with no routes.

use rand::{RngCore, rngs::OsRng};
use std::{
    collections::HashMap,
    fmt,
    sync::Mutex,
    time::{Duration, Instant},
};

const MAX_ROUTE_ENTRIES: usize = 256;
const MAX_ROUTE_TTL: Duration = Duration::from_secs(15 * 60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostedHandoffRoutingError {
    InvalidConfiguration,
    InvalidBinding,
    RouteUnavailable,
    RouteFenceMismatch,
    ViewerFenceMismatch,
    TransportFenceMismatch,
    CapacityExceeded,
    GenerationExhausted,
}

impl HostedHandoffRoutingError {
    pub const fn safe_code(self) -> &'static str {
        match self {
            Self::InvalidConfiguration => "hosted_handoff_routing_invalid_configuration",
            Self::InvalidBinding => "hosted_handoff_routing_invalid_binding",
            Self::RouteUnavailable => "hosted_handoff_route_unavailable",
            Self::RouteFenceMismatch => "hosted_handoff_route_fence_mismatch",
            Self::ViewerFenceMismatch => "hosted_handoff_viewer_fence_mismatch",
            Self::TransportFenceMismatch => "hosted_handoff_transport_fence_mismatch",
            Self::CapacityExceeded => "hosted_handoff_routing_capacity_exceeded",
            Self::GenerationExhausted => "hosted_handoff_routing_generation_exhausted",
        }
    }
}

impl fmt::Display for HostedHandoffRoutingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.safe_code())
    }
}

impl std::error::Error for HostedHandoffRoutingError {}

#[derive(Clone, PartialEq, Eq)]
pub struct HostedHandoffRouteLease {
    pub route_id: String,
    pub agent_generation: u64,
    pub intervention_epoch: u64,
}

impl fmt::Debug for HostedHandoffRouteLease {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HostedHandoffRouteLease")
            .field("route_id_present", &true)
            .field("agent_generation", &self.agent_generation)
            .field("intervention_epoch", &self.intervention_epoch)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct HostedHandoffViewerLease {
    pub route_id: String,
    pub viewer_generation: u64,
}

impl fmt::Debug for HostedHandoffViewerLease {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HostedHandoffViewerLease")
            .field("route_id_present", &true)
            .field("viewer_generation", &self.viewer_generation)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct HostedHandoffTransportLease {
    pub route_id: String,
    pub viewer_generation: u64,
    pub transport_generation: u64,
}

impl fmt::Debug for HostedHandoffTransportLease {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HostedHandoffTransportLease")
            .field("route_id_present", &true)
            .field("viewer_generation", &self.viewer_generation)
            .field("transport_generation", &self.transport_generation)
            .finish()
    }
}

struct HostedRouteState {
    device_id: String,
    agent_generation: u64,
    intervention_id: String,
    intervention_epoch: u64,
    viewer_generation: u64,
    viewer_active: bool,
    transport_generation: u64,
    transport_active: bool,
    expires_at: Instant,
}

pub struct HostedHandoffRouteRegistry {
    routes: Mutex<HashMap<String, HostedRouteState>>,
    ttl: Duration,
    max_entries: usize,
}

impl HostedHandoffRouteRegistry {
    pub fn new(ttl: Duration, max_entries: usize) -> Result<Self, HostedHandoffRoutingError> {
        if ttl.is_zero()
            || ttl > MAX_ROUTE_TTL
            || max_entries == 0
            || max_entries > MAX_ROUTE_ENTRIES
        {
            return Err(HostedHandoffRoutingError::InvalidConfiguration);
        }
        Ok(Self {
            routes: Mutex::new(HashMap::new()),
            ttl,
            max_entries,
        })
    }

    pub fn create_route(
        &self,
        device_id: &str,
        agent_generation: u64,
        intervention_id: &str,
        intervention_epoch: u64,
    ) -> Result<HostedHandoffRouteLease, HostedHandoffRoutingError> {
        if !valid_bounded_id(device_id, 256)
            || agent_generation == 0
            || !valid_bounded_id(intervention_id, 256)
            || intervention_epoch == 0
        {
            return Err(HostedHandoffRoutingError::InvalidBinding);
        }
        let now = Instant::now();
        let mut routes = self
            .routes
            .lock()
            .expect("hosted Handoff route lock poisoned");
        prune_expired(&mut routes, now);
        if routes.len() >= self.max_entries {
            return Err(HostedHandoffRoutingError::CapacityExceeded);
        }
        let route_id = random_route_id();
        routes.insert(
            route_id.clone(),
            HostedRouteState {
                device_id: device_id.to_owned(),
                agent_generation,
                intervention_id: intervention_id.to_owned(),
                intervention_epoch,
                viewer_generation: 0,
                viewer_active: false,
                transport_generation: 0,
                transport_active: false,
                expires_at: now + self.ttl,
            },
        );
        Ok(HostedHandoffRouteLease {
            route_id,
            agent_generation,
            intervention_epoch,
        })
    }

    pub fn attach_viewer(
        &self,
        route: &HostedHandoffRouteLease,
        device_id: &str,
        intervention_id: &str,
    ) -> Result<HostedHandoffViewerLease, HostedHandoffRoutingError> {
        let now = Instant::now();
        let mut routes = self
            .routes
            .lock()
            .expect("hosted Handoff route lock poisoned");
        prune_expired(&mut routes, now);
        let state = routes
            .get_mut(&route.route_id)
            .ok_or(HostedHandoffRoutingError::RouteUnavailable)?;
        if state.device_id != device_id
            || state.agent_generation != route.agent_generation
            || state.intervention_id != intervention_id
            || state.intervention_epoch != route.intervention_epoch
        {
            return Err(HostedHandoffRoutingError::RouteFenceMismatch);
        }
        state.viewer_generation = next_generation(state.viewer_generation)?;
        state.viewer_active = true;
        state.transport_active = false;
        state.expires_at = now + self.ttl;
        Ok(HostedHandoffViewerLease {
            route_id: route.route_id.clone(),
            viewer_generation: state.viewer_generation,
        })
    }

    pub fn attach_transport(
        &self,
        viewer: &HostedHandoffViewerLease,
    ) -> Result<HostedHandoffTransportLease, HostedHandoffRoutingError> {
        let now = Instant::now();
        let mut routes = self
            .routes
            .lock()
            .expect("hosted Handoff route lock poisoned");
        prune_expired(&mut routes, now);
        let state = routes
            .get_mut(&viewer.route_id)
            .ok_or(HostedHandoffRoutingError::RouteUnavailable)?;
        if !state.viewer_active || state.viewer_generation != viewer.viewer_generation {
            return Err(HostedHandoffRoutingError::ViewerFenceMismatch);
        }
        state.transport_generation = next_generation(state.transport_generation)?;
        state.transport_active = true;
        state.expires_at = now + self.ttl;
        Ok(HostedHandoffTransportLease {
            route_id: viewer.route_id.clone(),
            viewer_generation: viewer.viewer_generation,
            transport_generation: state.transport_generation,
        })
    }

    pub fn validate_transport(
        &self,
        transport: &HostedHandoffTransportLease,
    ) -> Result<(), HostedHandoffRoutingError> {
        let now = Instant::now();
        let mut routes = self
            .routes
            .lock()
            .expect("hosted Handoff route lock poisoned");
        prune_expired(&mut routes, now);
        let state = routes
            .get(&transport.route_id)
            .ok_or(HostedHandoffRoutingError::RouteUnavailable)?;
        if !state.viewer_active || state.viewer_generation != transport.viewer_generation {
            return Err(HostedHandoffRoutingError::ViewerFenceMismatch);
        }
        if !state.transport_active || state.transport_generation != transport.transport_generation {
            return Err(HostedHandoffRoutingError::TransportFenceMismatch);
        }
        Ok(())
    }

    /// Mark route/transport loss without translating it into Human Done or Agent resume.
    pub fn detach_viewer(
        &self,
        viewer: &HostedHandoffViewerLease,
    ) -> Result<(), HostedHandoffRoutingError> {
        let mut routes = self
            .routes
            .lock()
            .expect("hosted Handoff route lock poisoned");
        let state = routes
            .get_mut(&viewer.route_id)
            .ok_or(HostedHandoffRoutingError::RouteUnavailable)?;
        if !state.viewer_active || state.viewer_generation != viewer.viewer_generation {
            return Err(HostedHandoffRoutingError::ViewerFenceMismatch);
        }
        state.viewer_active = false;
        state.transport_active = false;
        Ok(())
    }

    pub fn revoke_route(
        &self,
        route: &HostedHandoffRouteLease,
    ) -> Result<(), HostedHandoffRoutingError> {
        let mut routes = self
            .routes
            .lock()
            .expect("hosted Handoff route lock poisoned");
        let state = routes
            .get(&route.route_id)
            .ok_or(HostedHandoffRoutingError::RouteUnavailable)?;
        if state.agent_generation != route.agent_generation
            || state.intervention_epoch != route.intervention_epoch
        {
            return Err(HostedHandoffRoutingError::RouteFenceMismatch);
        }
        routes.remove(&route.route_id);
        Ok(())
    }

    #[cfg(test)]
    fn route_count(&self) -> usize {
        self.routes.lock().unwrap().len()
    }
}

fn prune_expired(routes: &mut HashMap<String, HostedRouteState>, now: Instant) {
    routes.retain(|_, route| route.expires_at > now);
}

fn next_generation(current: u64) -> Result<u64, HostedHandoffRoutingError> {
    current
        .checked_add(1)
        .ok_or(HostedHandoffRoutingError::GenerationExhausted)
}

fn valid_bounded_id(value: &str, max: usize) -> bool {
    !value.trim().is_empty()
        && value.len() <= max
        && !value.bytes().any(|byte| byte.is_ascii_control())
}

fn random_route_id() -> String {
    let mut bytes = [0_u8; 16];
    OsRng.fill_bytes(&mut bytes);
    let mut value = String::with_capacity(36);
    value.push_str("hroute_");
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(value, "{byte:02x}");
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry() -> HostedHandoffRouteRegistry {
        HostedHandoffRouteRegistry::new(Duration::from_secs(60), 8).unwrap()
    }

    #[test]
    fn route_metadata_has_no_authority_or_human_payload_surface() {
        let registry = registry();
        let route = registry
            .create_route("device-a", 7, "intervention-a", 9)
            .unwrap();
        let viewer = registry
            .attach_viewer(&route, "device-a", "intervention-a")
            .unwrap();
        let transport = registry.attach_transport(&viewer).unwrap();
        assert!(registry.validate_transport(&transport).is_ok());
        let debug = format!("{route:?} {viewer:?} {transport:?}");
        assert!(!debug.contains("intervention-a"));
        assert!(!debug.contains(&route.route_id));
    }

    #[test]
    fn stale_agent_intervention_viewer_and_transport_generations_fail_closed() {
        let registry = registry();
        let route = registry
            .create_route("device-a", 7, "intervention-a", 9)
            .unwrap();
        let mut stale_route = route.clone();
        stale_route.agent_generation = 8;
        assert_eq!(
            registry.attach_viewer(&stale_route, "device-a", "intervention-a"),
            Err(HostedHandoffRoutingError::RouteFenceMismatch)
        );

        let viewer1 = registry
            .attach_viewer(&route, "device-a", "intervention-a")
            .unwrap();
        let transport1 = registry.attach_transport(&viewer1).unwrap();
        let transport2 = registry.attach_transport(&viewer1).unwrap();
        assert_eq!(
            registry.validate_transport(&transport1),
            Err(HostedHandoffRoutingError::TransportFenceMismatch)
        );
        assert!(registry.validate_transport(&transport2).is_ok());

        let viewer2 = registry
            .attach_viewer(&route, "device-a", "intervention-a")
            .unwrap();
        assert_eq!(
            registry.attach_transport(&viewer1),
            Err(HostedHandoffRoutingError::ViewerFenceMismatch)
        );
        assert!(registry.attach_transport(&viewer2).is_ok());
    }

    #[test]
    fn viewer_loss_never_becomes_done_and_requires_fresh_viewer_generation() {
        let registry = registry();
        let route = registry
            .create_route("device-a", 7, "intervention-a", 9)
            .unwrap();
        let viewer1 = registry
            .attach_viewer(&route, "device-a", "intervention-a")
            .unwrap();
        let transport1 = registry.attach_transport(&viewer1).unwrap();
        registry.detach_viewer(&viewer1).unwrap();
        assert_eq!(
            registry.validate_transport(&transport1),
            Err(HostedHandoffRoutingError::ViewerFenceMismatch)
        );
        let viewer2 = registry
            .attach_viewer(&route, "device-a", "intervention-a")
            .unwrap();
        assert!(viewer2.viewer_generation > viewer1.viewer_generation);
    }

    #[test]
    fn process_restart_has_no_route_restore_authority() {
        let first = registry();
        let route = first
            .create_route("device-a", 7, "intervention-a", 9)
            .unwrap();
        assert_eq!(first.route_count(), 1);

        let restarted = registry();
        assert_eq!(restarted.route_count(), 0);
        assert_eq!(
            restarted.attach_viewer(&route, "device-a", "intervention-a"),
            Err(HostedHandoffRoutingError::RouteUnavailable)
        );
    }

    #[test]
    fn route_revoke_makes_every_surviving_viewer_transport_stale() {
        let registry = registry();
        let route = registry
            .create_route("device-a", 7, "intervention-a", 9)
            .unwrap();
        let viewer = registry
            .attach_viewer(&route, "device-a", "intervention-a")
            .unwrap();
        let transport = registry.attach_transport(&viewer).unwrap();
        registry.revoke_route(&route).unwrap();
        assert_eq!(
            registry.validate_transport(&transport),
            Err(HostedHandoffRoutingError::RouteUnavailable)
        );
    }
}
