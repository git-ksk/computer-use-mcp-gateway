//! Hub-owned opaque references for Agent-local managed jobs.
//!
//! Agent locators are private transport details. Northbound callers receive only
//! random Hub-minted references bound to the authenticated owner and exact
//! device session fence.

use crate::v2_execution_safety::OperationOwner;
use rand::{RngCore, rngs::OsRng};
use std::collections::HashMap;
use std::fmt::Write as _;

pub const DEFAULT_MAX_MANAGED_JOB_REFS: usize = 128;
pub const DEFAULT_MAX_MANAGED_JOB_REFS_PER_OWNER: usize = 32;

#[derive(Debug, Clone)]
pub struct HubManagedJobRefLimits {
    pub max_refs: usize,
    pub max_refs_per_owner: usize,
}

impl Default for HubManagedJobRefLimits {
    fn default() -> Self {
        Self {
            max_refs: DEFAULT_MAX_MANAGED_JOB_REFS,
            max_refs_per_owner: DEFAULT_MAX_MANAGED_JOB_REFS_PER_OWNER,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedManagedJobRef {
    pub agent_locator: String,
    pub source_operation_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HubManagedJobRefError {
    InvalidLimits,
    InvalidBinding,
    Unavailable,
    RefLimitExceeded,
    OwnerRefLimitExceeded,
    IdentifierCollision,
}

#[derive(Debug, Clone)]
struct ManagedJobRefEntry {
    owner: OperationOwner,
    device_id: String,
    generation: u64,
    capability_revision: u64,
    agent_locator: Option<String>,
    source_operation_id: String,
    expires_at_ms: u64,
}

pub struct HubManagedJobRefRegistry {
    limits: HubManagedJobRefLimits,
    refs: HashMap<String, ManagedJobRefEntry>,
}

impl HubManagedJobRefRegistry {
    pub fn new(limits: HubManagedJobRefLimits) -> Result<Self, HubManagedJobRefError> {
        if limits.max_refs == 0 || limits.max_refs_per_owner == 0 {
            return Err(HubManagedJobRefError::InvalidLimits);
        }
        Ok(Self {
            limits,
            refs: HashMap::new(),
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn reserve(
        &mut self,
        owner: OperationOwner,
        device_id: &str,
        generation: u64,
        capability_revision: u64,
        source_operation_id: &str,
        expires_at_ms: u64,
        now_ms: u64,
    ) -> Result<String, HubManagedJobRefError> {
        self.prune(now_ms);
        if device_id.trim().is_empty()
            || source_operation_id.trim().is_empty()
            || expires_at_ms <= now_ms
        {
            return Err(HubManagedJobRefError::InvalidBinding);
        }
        if self.refs.len() >= self.limits.max_refs {
            return Err(HubManagedJobRefError::RefLimitExceeded);
        }
        let owner_count = self
            .refs
            .values()
            .filter(|entry| entry.owner == owner)
            .count();
        if owner_count >= self.limits.max_refs_per_owner {
            return Err(HubManagedJobRefError::OwnerRefLimitExceeded);
        }
        for _ in 0..8 {
            let public_ref = random_job_ref();
            if self.refs.contains_key(&public_ref) {
                continue;
            }
            self.refs.insert(
                public_ref.clone(),
                ManagedJobRefEntry {
                    owner,
                    device_id: device_id.to_owned(),
                    generation,
                    capability_revision,
                    agent_locator: None,
                    source_operation_id: source_operation_id.to_owned(),
                    expires_at_ms,
                },
            );
            return Ok(public_ref);
        }
        Err(HubManagedJobRefError::IdentifierCollision)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn bind(
        &mut self,
        public_ref: &str,
        owner: &OperationOwner,
        device_id: &str,
        generation: u64,
        capability_revision: u64,
        source_operation_id: &str,
        agent_locator: &str,
        now_ms: u64,
    ) -> Result<(), HubManagedJobRefError> {
        self.prune(now_ms);
        if agent_locator.trim().is_empty() {
            return Err(HubManagedJobRefError::InvalidBinding);
        }
        let entry = self
            .refs
            .get_mut(public_ref)
            .ok_or(HubManagedJobRefError::Unavailable)?;
        if &entry.owner != owner
            || entry.device_id != device_id
            || entry.generation != generation
            || entry.capability_revision != capability_revision
            || entry.source_operation_id != source_operation_id
            || entry.agent_locator.is_some()
        {
            return Err(HubManagedJobRefError::Unavailable);
        }
        entry.agent_locator = Some(agent_locator.to_owned());
        Ok(())
    }

    pub fn release_unbound(
        &mut self,
        public_ref: &str,
        owner: &OperationOwner,
        device_id: &str,
        generation: u64,
        capability_revision: u64,
    ) {
        let remove = self.refs.get(public_ref).is_some_and(|entry| {
            &entry.owner == owner
                && entry.device_id == device_id
                && entry.generation == generation
                && entry.capability_revision == capability_revision
                && entry.agent_locator.is_none()
        });
        if remove {
            self.refs.remove(public_ref);
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn mint(
        &mut self,
        owner: OperationOwner,
        device_id: &str,
        generation: u64,
        capability_revision: u64,
        source_operation_id: &str,
        agent_locator: &str,
        expires_at_ms: u64,
        now_ms: u64,
    ) -> Result<String, HubManagedJobRefError> {
        let public_ref = self.reserve(
            owner.clone(),
            device_id,
            generation,
            capability_revision,
            source_operation_id,
            expires_at_ms,
            now_ms,
        )?;
        if let Err(error) = self.bind(
            &public_ref,
            &owner,
            device_id,
            generation,
            capability_revision,
            source_operation_id,
            agent_locator,
            now_ms,
        ) {
            self.release_unbound(
                &public_ref,
                &owner,
                device_id,
                generation,
                capability_revision,
            );
            return Err(error);
        }
        Ok(public_ref)
    }

    pub fn resolve_owned(
        &mut self,
        public_ref: &str,
        owner: &OperationOwner,
        device_id: &str,
        generation: u64,
        capability_revision: u64,
        now_ms: u64,
    ) -> Result<ResolvedManagedJobRef, HubManagedJobRefError> {
        self.prune(now_ms);
        let entry = self
            .refs
            .get(public_ref)
            .ok_or(HubManagedJobRefError::Unavailable)?;
        if &entry.owner != owner
            || entry.device_id != device_id
            || entry.generation != generation
            || entry.capability_revision != capability_revision
        {
            return Err(HubManagedJobRefError::Unavailable);
        }
        let agent_locator = entry
            .agent_locator
            .clone()
            .ok_or(HubManagedJobRefError::Unavailable)?;
        Ok(ResolvedManagedJobRef {
            agent_locator,
            source_operation_id: entry.source_operation_id.clone(),
        })
    }

    fn prune(&mut self, now_ms: u64) {
        self.refs.retain(|_, entry| entry.expires_at_ms > now_ms);
    }
}

fn random_job_ref() -> String {
    let mut bytes = [0_u8; 24];
    OsRng.fill_bytes(&mut bytes);
    let mut value = String::with_capacity(4 + bytes.len() * 2);
    value.push_str("job_");
    for byte in bytes {
        let _ = write!(&mut value, "{byte:02x}");
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owner(subject: &str) -> OperationOwner {
        OperationOwner {
            issuer: "https://issuer.example".into(),
            subject: subject.into(),
        }
    }

    #[test]
    fn opaque_ref_enforces_owner_and_session_fence_without_distinguishing_mismatch() {
        let mut refs = HubManagedJobRefRegistry::new(HubManagedJobRefLimits::default()).unwrap();
        let public_ref = refs
            .mint(
                owner("a"),
                "dev-a",
                7,
                9,
                "op-1",
                "agent-private",
                10_000,
                1_000,
            )
            .unwrap();

        let resolved = refs
            .resolve_owned(&public_ref, &owner("a"), "dev-a", 7, 9, 1_001)
            .unwrap();
        assert_eq!(resolved.agent_locator, "agent-private");
        assert_eq!(resolved.source_operation_id, "op-1");

        for result in [
            refs.resolve_owned(&public_ref, &owner("b"), "dev-a", 7, 9, 1_001),
            refs.resolve_owned(&public_ref, &owner("a"), "dev-a", 8, 9, 1_001),
            refs.resolve_owned(&public_ref, &owner("a"), "dev-a", 7, 10, 1_001),
            refs.resolve_owned("job_missing", &owner("a"), "dev-a", 7, 9, 1_001),
        ] {
            assert_eq!(result, Err(HubManagedJobRefError::Unavailable));
        }
    }

    #[test]
    fn expired_ref_is_unavailable() {
        let mut refs = HubManagedJobRefRegistry::new(HubManagedJobRefLimits::default()).unwrap();
        let public_ref = refs
            .mint(
                owner("a"),
                "dev-a",
                7,
                9,
                "op-1",
                "agent-private",
                2_000,
                1_000,
            )
            .unwrap();
        assert_eq!(
            refs.resolve_owned(&public_ref, &owner("a"), "dev-a", 7, 9, 2_000),
            Err(HubManagedJobRefError::Unavailable)
        );
    }

    #[test]
    fn reserved_ref_is_unavailable_until_bound() {
        let mut refs = HubManagedJobRefRegistry::new(HubManagedJobRefLimits::default()).unwrap();
        let public_ref = refs
            .reserve(owner("a"), "dev-a", 7, 9, "op-1", 10_000, 1_000)
            .unwrap();
        assert_eq!(
            refs.resolve_owned(&public_ref, &owner("a"), "dev-a", 7, 9, 1_001),
            Err(HubManagedJobRefError::Unavailable)
        );
        refs.bind(
            &public_ref,
            &owner("a"),
            "dev-a",
            7,
            9,
            "op-1",
            "agent-private",
            1_001,
        )
        .unwrap();
        assert_eq!(
            refs.resolve_owned(&public_ref, &owner("a"), "dev-a", 7, 9, 1_002)
                .unwrap()
                .agent_locator,
            "agent-private"
        );
    }
}
