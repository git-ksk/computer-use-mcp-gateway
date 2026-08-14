//! Ephemeral V2 workflow state for stateful Computer Use backends.
//!
//! An interaction context is deliberately separate from transport sessions,
//! execution ownership, capability grants, and the durable safety ledger. It
//! binds backend workflow state to one authenticated principal, stable device,
//! Agent generation, and capability revision. Generation/revision drift fails
//! closed and must never resurrect a backend session or scoped handle.

use crate::v2_m0_trust::AuthenticatedClientPrincipal;
use rand::{RngCore, rngs::OsRng};
use std::collections::HashMap;
use std::fmt;

pub const DEFAULT_MAX_CONTEXTS_PER_OWNER: usize = 8;
pub const DEFAULT_CONTEXT_IDLE_TIMEOUT_MS: u64 = 15 * 60 * 1000;
pub const DEFAULT_CONTEXT_MAX_LIFETIME_MS: u64 = 2 * 60 * 60 * 1000;
pub const DEFAULT_MAX_REFS_PER_CONTEXT: usize = 2_048;
pub const MAX_BACKEND_REF_BYTES: usize = 4 * 1024;

#[derive(Clone, PartialEq, Eq, Hash)]
pub struct InteractionContextId(String);

impl InteractionContextId {
    pub fn parse(value: &str) -> Result<Self, InteractionContextError> {
        const PREFIX: &str = "ctx_";
        const RANDOM_HEX_LEN: usize = 32;
        let Some(hex) = value.strip_prefix(PREFIX) else {
            return Err(InteractionContextError::InvalidIdentifier);
        };
        if hex.len() != RANDOM_HEX_LEN
            || !hex
                .as_bytes()
                .iter()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            return Err(InteractionContextError::InvalidIdentifier);
        }
        Ok(Self(value.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for InteractionContextId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("InteractionContextId([redacted])")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InteractionScope {
    WindowScoped,
    DesktopScoped,
}

#[derive(Clone, PartialEq, Eq)]
pub struct InteractionContextBinding {
    pub id: InteractionContextId,
    pub issuer: String,
    pub subject: String,
    pub device_id: String,
    pub device_generation: u64,
    pub capability_revision: u64,
    pub scope: InteractionScope,
    pub created_at_ms: u64,
    pub last_used_at_ms: u64,
}

impl fmt::Debug for InteractionContextBinding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InteractionContextBinding")
            .field("id", &self.id)
            .field("principal", &"[redacted]")
            .field("device_id", &self.device_id)
            .field("device_generation", &self.device_generation)
            .field("capability_revision", &self.capability_revision)
            .field("scope", &self.scope)
            .field("created_at_ms", &self.created_at_ms)
            .field("last_used_at_ms", &self.last_used_at_ms)
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InteractionContextLimits {
    pub max_contexts_per_owner: usize,
    pub idle_timeout_ms: u64,
    pub max_lifetime_ms: u64,
}

impl Default for InteractionContextLimits {
    fn default() -> Self {
        Self {
            max_contexts_per_owner: DEFAULT_MAX_CONTEXTS_PER_OWNER,
            idle_timeout_ms: DEFAULT_CONTEXT_IDLE_TIMEOUT_MS,
            max_lifetime_ms: DEFAULT_CONTEXT_MAX_LIFETIME_MS,
        }
    }
}

impl InteractionContextLimits {
    pub fn validate(self) -> Result<Self, InteractionContextError> {
        if self.max_contexts_per_owner == 0
            || self.idle_timeout_ms == 0
            || self.max_lifetime_ms == 0
            || self.idle_timeout_ms > self.max_lifetime_ms
        {
            return Err(InteractionContextError::InvalidLimits);
        }
        Ok(self)
    }
}

pub struct InteractionContextManager {
    limits: InteractionContextLimits,
    contexts: HashMap<String, InteractionContextBinding>,
}

impl InteractionContextManager {
    pub fn new(limits: InteractionContextLimits) -> Result<Self, InteractionContextError> {
        Ok(Self {
            limits: limits.validate()?,
            contexts: HashMap::new(),
        })
    }

    pub fn open(
        &mut self,
        principal: &AuthenticatedClientPrincipal,
        device_id: &str,
        device_generation: u64,
        capability_revision: u64,
        now_ms: u64,
    ) -> Result<InteractionContextBinding, InteractionContextError> {
        if principal.issuer.trim().is_empty()
            || principal.subject.trim().is_empty()
            || device_id.trim().is_empty()
            || device_generation == 0
            || capability_revision == 0
        {
            return Err(InteractionContextError::InvalidBinding);
        }
        self.prune(now_ms);
        let owner_count = self
            .contexts
            .values()
            .filter(|context| {
                context.issuer == principal.issuer
                    && context.subject == principal.subject
                    && context.device_id == device_id
            })
            .count();
        if owner_count >= self.limits.max_contexts_per_owner {
            return Err(InteractionContextError::ContextLimitExceeded);
        }

        for _ in 0..4 {
            let id = random_id("ctx_");
            if self.contexts.contains_key(&id) {
                continue;
            }
            let context = InteractionContextBinding {
                id: InteractionContextId(id.clone()),
                issuer: principal.issuer.clone(),
                subject: principal.subject.clone(),
                device_id: device_id.to_owned(),
                device_generation,
                capability_revision,
                scope: InteractionScope::WindowScoped,
                created_at_ms: now_ms,
                last_used_at_ms: now_ms,
            };
            self.contexts.insert(id, context.clone());
            return Ok(context);
        }
        Err(InteractionContextError::IdentifierCollision)
    }

    pub fn validate_and_touch(
        &mut self,
        id: &InteractionContextId,
        principal: &AuthenticatedClientPrincipal,
        device_id: &str,
        device_generation: u64,
        capability_revision: u64,
        now_ms: u64,
    ) -> Result<InteractionContextBinding, InteractionContextError> {
        let key = id.as_str().to_owned();
        let Some(context) = self.contexts.get(&key) else {
            return Err(InteractionContextError::UnknownContext);
        };
        if is_expired(context, self.limits, now_ms) {
            self.contexts.remove(&key);
            return Err(InteractionContextError::Expired);
        }
        if context.issuer != principal.issuer || context.subject != principal.subject {
            return Err(InteractionContextError::PrincipalMismatch);
        }
        if context.device_id != device_id {
            return Err(InteractionContextError::DeviceMismatch);
        }
        if context.device_generation != device_generation {
            self.contexts.remove(&key);
            return Err(InteractionContextError::GenerationMismatch);
        }
        if context.capability_revision != capability_revision {
            self.contexts.remove(&key);
            return Err(InteractionContextError::CapabilityRevisionMismatch);
        }
        let context = self
            .contexts
            .get_mut(&key)
            .expect("context was checked immediately above");
        context.last_used_at_ms = now_ms;
        Ok(context.clone())
    }

    /// Monotonic scope expansion after the caller has separately passed the
    /// CUMG authorization/approval boundary for desktop-scoped execution.
    pub fn expand_to_desktop_after_authorization(
        &mut self,
        id: &InteractionContextId,
        principal: &AuthenticatedClientPrincipal,
        device_id: &str,
        device_generation: u64,
        capability_revision: u64,
        now_ms: u64,
    ) -> Result<InteractionContextBinding, InteractionContextError> {
        let validated = self.validate_and_touch(
            id,
            principal,
            device_id,
            device_generation,
            capability_revision,
            now_ms,
        )?;
        let context = self
            .contexts
            .get_mut(validated.id.as_str())
            .expect("validated context remains present");
        context.scope = InteractionScope::DesktopScoped;
        Ok(context.clone())
    }

    pub fn close(
        &mut self,
        id: &InteractionContextId,
        principal: &AuthenticatedClientPrincipal,
        device_id: &str,
    ) -> Result<(), InteractionContextError> {
        let Some(context) = self.contexts.get(id.as_str()) else {
            return Err(InteractionContextError::UnknownContext);
        };
        if context.issuer != principal.issuer || context.subject != principal.subject {
            return Err(InteractionContextError::PrincipalMismatch);
        }
        if context.device_id != device_id {
            return Err(InteractionContextError::DeviceMismatch);
        }
        self.contexts.remove(id.as_str());
        Ok(())
    }

    pub fn invalidate_device_generation(
        &mut self,
        device_id: &str,
        current_generation: u64,
    ) -> Vec<InteractionContextId> {
        let removed: Vec<_> = self
            .contexts
            .iter()
            .filter(|(_, context)| {
                context.device_id == device_id && context.device_generation != current_generation
            })
            .map(|(id, _)| InteractionContextId(id.clone()))
            .collect();
        self.contexts.retain(|_, context| {
            context.device_id != device_id || context.device_generation == current_generation
        });
        removed
    }

    pub fn invalidate_capability_revision(
        &mut self,
        device_id: &str,
        current_revision: u64,
    ) -> Vec<InteractionContextId> {
        let removed: Vec<_> = self
            .contexts
            .iter()
            .filter(|(_, context)| {
                context.device_id == device_id && context.capability_revision != current_revision
            })
            .map(|(id, _)| InteractionContextId(id.clone()))
            .collect();
        self.contexts.retain(|_, context| {
            context.device_id != device_id || context.capability_revision == current_revision
        });
        removed
    }

    pub fn prune(&mut self, now_ms: u64) -> Vec<InteractionContextId> {
        let limits = self.limits;
        let expired: Vec<_> = self
            .contexts
            .iter()
            .filter(|(_, context)| is_expired(context, limits, now_ms))
            .map(|(id, _)| InteractionContextId(id.clone()))
            .collect();
        for id in &expired {
            self.contexts.remove(id.as_str());
        }
        expired
    }

    pub fn len(&self) -> usize {
        self.contexts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.contexts.is_empty()
    }
}

fn is_expired(
    context: &InteractionContextBinding,
    limits: InteractionContextLimits,
    now_ms: u64,
) -> bool {
    now_ms.saturating_sub(context.created_at_ms) > limits.max_lifetime_ms
        || now_ms.saturating_sub(context.last_used_at_ms) > limits.idle_timeout_ms
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScopedRefKind {
    Snapshot,
    Element,
    BrowserTarget,
    BrowserTab,
    BrowserElement,
    UploadFile,
}

#[derive(Clone)]
struct ScopedBackendRef {
    public_ref: String,
    context_id: String,
    device_id: String,
    device_generation: u64,
    capability_revision: u64,
    kind: ScopedRefKind,
    backend_ref: String,
}

impl fmt::Debug for ScopedBackendRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ScopedBackendRef")
            .field("public_ref", &"[redacted]")
            .field("context_id", &"[redacted]")
            .field("device_id", &self.device_id)
            .field("device_generation", &self.device_generation)
            .field("capability_revision", &self.capability_revision)
            .field("kind", &self.kind)
            .field("backend_ref", &"[redacted]")
            .finish()
    }
}

pub struct ScopedBackendRefRegistry {
    max_refs_per_context: usize,
    refs: HashMap<String, ScopedBackendRef>,
}

impl ScopedBackendRefRegistry {
    pub fn new(max_refs_per_context: usize) -> Result<Self, ScopedRefError> {
        if max_refs_per_context == 0 {
            return Err(ScopedRefError::InvalidLimit);
        }
        Ok(Self {
            max_refs_per_context,
            refs: HashMap::new(),
        })
    }

    pub fn mint(
        &mut self,
        context: &InteractionContextBinding,
        kind: ScopedRefKind,
        backend_ref: &str,
    ) -> Result<String, ScopedRefError> {
        if backend_ref.is_empty() || backend_ref.len() > MAX_BACKEND_REF_BYTES {
            return Err(ScopedRefError::InvalidBackendRef);
        }
        let count = self
            .refs
            .values()
            .filter(|reference| reference.context_id == context.id.as_str())
            .count();
        if count >= self.max_refs_per_context {
            return Err(ScopedRefError::RefLimitExceeded);
        }
        for _ in 0..4 {
            let public_ref = random_id("ref_");
            if self.refs.contains_key(&public_ref) {
                continue;
            }
            self.refs.insert(
                public_ref.clone(),
                ScopedBackendRef {
                    public_ref: public_ref.clone(),
                    context_id: context.id.as_str().to_owned(),
                    device_id: context.device_id.clone(),
                    device_generation: context.device_generation,
                    capability_revision: context.capability_revision,
                    kind,
                    backend_ref: backend_ref.to_owned(),
                },
            );
            return Ok(public_ref);
        }
        Err(ScopedRefError::IdentifierCollision)
    }

    pub fn resolve(
        &self,
        public_ref: &str,
        context: &InteractionContextBinding,
        expected_kind: ScopedRefKind,
    ) -> Result<&str, ScopedRefError> {
        let reference = self
            .refs
            .get(public_ref)
            .ok_or(ScopedRefError::UnknownRef)?;
        if reference.public_ref != public_ref
            || reference.context_id != context.id.as_str()
            || reference.device_id != context.device_id
        {
            return Err(ScopedRefError::ContextMismatch);
        }
        if reference.device_generation != context.device_generation {
            return Err(ScopedRefError::GenerationMismatch);
        }
        if reference.capability_revision != context.capability_revision {
            return Err(ScopedRefError::CapabilityRevisionMismatch);
        }
        if reference.kind != expected_kind {
            return Err(ScopedRefError::KindMismatch);
        }
        Ok(reference.backend_ref.as_str())
    }

    pub fn invalidate_context(&mut self, context_id: &InteractionContextId) {
        self.refs
            .retain(|_, reference| reference.context_id != context_id.as_str());
    }

    pub fn invalidate_device_generation(&mut self, device_id: &str, current_generation: u64) {
        self.refs.retain(|_, reference| {
            reference.device_id != device_id || reference.device_generation == current_generation
        });
    }

    pub fn invalidate_capability_revision(&mut self, device_id: &str, current_revision: u64) {
        self.refs.retain(|_, reference| {
            reference.device_id != device_id || reference.capability_revision == current_revision
        });
    }

    pub fn len(&self) -> usize {
        self.refs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.refs.is_empty()
    }
}

fn random_id(prefix: &str) -> String {
    let mut random = [0_u8; 16];
    OsRng.fill_bytes(&mut random);
    let mut output = String::with_capacity(prefix.len() + random.len() * 2);
    output.push_str(prefix);
    for byte in random {
        use std::fmt::Write as _;
        let _ = write!(&mut output, "{byte:02x}");
    }
    output
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InteractionContextError {
    InvalidLimits,
    InvalidBinding,
    InvalidIdentifier,
    ContextLimitExceeded,
    IdentifierCollision,
    UnknownContext,
    Expired,
    PrincipalMismatch,
    DeviceMismatch,
    GenerationMismatch,
    CapabilityRevisionMismatch,
}

impl fmt::Display for InteractionContextError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for InteractionContextError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopedRefError {
    InvalidLimit,
    InvalidBackendRef,
    RefLimitExceeded,
    IdentifierCollision,
    UnknownRef,
    ContextMismatch,
    GenerationMismatch,
    CapabilityRevisionMismatch,
    KindMismatch,
}

impl fmt::Display for ScopedRefError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for ScopedRefError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn principal(subject: &str) -> AuthenticatedClientPrincipal {
        AuthenticatedClientPrincipal {
            issuer: "https://issuer.example".into(),
            subject: subject.into(),
        }
    }

    fn manager() -> InteractionContextManager {
        InteractionContextManager::new(InteractionContextLimits {
            max_contexts_per_owner: 2,
            idle_timeout_ms: 100,
            max_lifetime_ms: 1_000,
        })
        .unwrap()
    }

    #[test]
    fn opaque_context_identifier_parser_is_strict() {
        let valid = "ctx_0123456789abcdef0123456789abcdef";
        assert_eq!(InteractionContextId::parse(valid).unwrap().as_str(), valid);
        for invalid in [
            "",
            "ctx_",
            "CTX_0123456789abcdef0123456789abcdef",
            "ctx_0123456789ABCDEF0123456789abcdef",
            "ctx_0123456789abcdef0123456789abcdeg",
            "ctx_0123456789abcdef0123456789abcdef00",
        ] {
            assert_eq!(
                InteractionContextId::parse(invalid),
                Err(InteractionContextError::InvalidIdentifier)
            );
        }
    }

    #[test]
    fn context_is_exactly_bound_and_not_a_bearer_authorization_credential() {
        let alice = principal("alice");
        let bob = principal("bob");
        let mut contexts = manager();
        let context = contexts.open(&alice, "dev-a", 7, 3, 10).unwrap();

        assert_eq!(
            contexts.validate_and_touch(&context.id, &bob, "dev-a", 7, 3, 20),
            Err(InteractionContextError::PrincipalMismatch)
        );
        assert_eq!(
            contexts.validate_and_touch(&context.id, &alice, "dev-b", 7, 3, 20),
            Err(InteractionContextError::DeviceMismatch)
        );
        assert!(
            contexts
                .validate_and_touch(&context.id, &alice, "dev-a", 7, 3, 20)
                .is_ok()
        );
    }

    #[test]
    fn generation_or_capability_revision_drift_invalidates_context_fail_closed() {
        let alice = principal("alice");
        let mut contexts = manager();
        let generation_context = contexts.open(&alice, "dev-a", 7, 3, 0).unwrap();
        assert_eq!(
            contexts.validate_and_touch(&generation_context.id, &alice, "dev-a", 8, 3, 1),
            Err(InteractionContextError::GenerationMismatch)
        );
        assert_eq!(
            contexts.validate_and_touch(&generation_context.id, &alice, "dev-a", 7, 3, 2),
            Err(InteractionContextError::UnknownContext)
        );

        let revision_context = contexts.open(&alice, "dev-a", 8, 3, 3).unwrap();
        assert_eq!(
            contexts.validate_and_touch(&revision_context.id, &alice, "dev-a", 8, 4, 4),
            Err(InteractionContextError::CapabilityRevisionMismatch)
        );
        assert!(contexts.is_empty());
    }

    #[test]
    fn idle_absolute_expiry_and_per_owner_limits_are_bounded() {
        let alice = principal("alice");
        let mut contexts = manager();
        let first = contexts.open(&alice, "dev-a", 1, 1, 0).unwrap();
        let _second = contexts.open(&alice, "dev-a", 1, 1, 0).unwrap();
        assert_eq!(
            contexts.open(&alice, "dev-a", 1, 1, 0),
            Err(InteractionContextError::ContextLimitExceeded)
        );
        assert_eq!(
            contexts.validate_and_touch(&first.id, &alice, "dev-a", 1, 1, 101),
            Err(InteractionContextError::Expired)
        );

        let fresh = contexts.open(&alice, "dev-a", 1, 1, 200).unwrap();
        assert!(
            contexts
                .validate_and_touch(&fresh.id, &alice, "dev-a", 1, 1, 250)
                .is_ok()
        );
        assert_eq!(
            contexts.validate_and_touch(&fresh.id, &alice, "dev-a", 1, 1, 1_201),
            Err(InteractionContextError::Expired)
        );
    }

    #[test]
    fn desktop_scope_expansion_is_explicit_monotonic_and_context_local() {
        let alice = principal("alice");
        let mut contexts = manager();
        let first = contexts.open(&alice, "dev-a", 1, 1, 0).unwrap();
        let second = contexts.open(&alice, "dev-a", 1, 1, 0).unwrap();
        assert_eq!(first.scope, InteractionScope::WindowScoped);

        let expanded = contexts
            .expand_to_desktop_after_authorization(&first.id, &alice, "dev-a", 1, 1, 1)
            .unwrap();
        assert_eq!(expanded.scope, InteractionScope::DesktopScoped);
        assert_eq!(
            contexts
                .validate_and_touch(&first.id, &alice, "dev-a", 1, 1, 2)
                .unwrap()
                .scope,
            InteractionScope::DesktopScoped
        );
        assert_eq!(
            contexts
                .validate_and_touch(&second.id, &alice, "dev-a", 1, 1, 2)
                .unwrap()
                .scope,
            InteractionScope::WindowScoped
        );
    }

    #[test]
    fn scoped_backend_refs_cannot_cross_context_generation_revision_or_kind() {
        let alice = principal("alice");
        let mut contexts = manager();
        let first = contexts.open(&alice, "dev-a", 4, 9, 0).unwrap();
        let second = contexts.open(&alice, "dev-a", 4, 9, 0).unwrap();
        let mut refs = ScopedBackendRefRegistry::new(4).unwrap();
        let public_ref = refs
            .mint(&first, ScopedRefKind::Element, "backend-element-secret")
            .unwrap();

        assert_eq!(
            refs.resolve(&public_ref, &second, ScopedRefKind::Element),
            Err(ScopedRefError::ContextMismatch)
        );
        assert_eq!(
            refs.resolve(&public_ref, &first, ScopedRefKind::Snapshot),
            Err(ScopedRefError::KindMismatch)
        );

        let mut wrong_generation = first.clone();
        wrong_generation.device_generation += 1;
        assert_eq!(
            refs.resolve(&public_ref, &wrong_generation, ScopedRefKind::Element),
            Err(ScopedRefError::GenerationMismatch)
        );
        let mut wrong_revision = first.clone();
        wrong_revision.capability_revision += 1;
        assert_eq!(
            refs.resolve(&public_ref, &wrong_revision, ScopedRefKind::Element),
            Err(ScopedRefError::CapabilityRevisionMismatch)
        );
        assert_eq!(
            refs.resolve(&public_ref, &first, ScopedRefKind::Element),
            Ok("backend-element-secret")
        );
    }

    #[test]
    fn prune_reports_expired_context_ids_for_scoped_ref_cleanup() {
        let alice = principal("alice");
        let mut contexts = manager();
        let context = contexts.open(&alice, "dev-a", 4, 9, 0).unwrap();
        let expired = contexts.prune(101);
        assert_eq!(expired, vec![context.id]);
        assert!(contexts.is_empty());
    }

    #[test]
    fn generation_invalidation_drops_contexts_and_refs_without_replay_or_rebinding() {
        let alice = principal("alice");
        let mut contexts = manager();
        let context = contexts.open(&alice, "dev-a", 4, 9, 0).unwrap();
        let mut refs = ScopedBackendRefRegistry::new(4).unwrap();
        let _ = refs
            .mint(&context, ScopedRefKind::BrowserTab, "backend-tab")
            .unwrap();
        contexts.invalidate_device_generation("dev-a", 5);
        refs.invalidate_device_generation("dev-a", 5);
        assert!(contexts.is_empty());
        assert!(refs.is_empty());
    }

    #[test]
    fn repeated_context_close_and_ref_cleanup_do_not_accumulate_registry_state() {
        let alice = principal("alice");
        let mut contexts = manager();
        let mut refs = ScopedBackendRefRegistry::new(8).unwrap();

        for index in 0..1_000_u64 {
            let now_ms = index.saturating_mul(2);
            let context = contexts.open(&alice, "dev-a", 4, 9, now_ms).unwrap();
            for ref_index in 0..8 {
                refs.mint(
                    &context,
                    ScopedRefKind::Element,
                    &format!("backend-element-{index}-{ref_index}"),
                )
                .unwrap();
            }
            assert_eq!(refs.len(), 8);
            contexts.close(&context.id, &alice, "dev-a").unwrap();
            refs.invalidate_context(&context.id);
            assert!(contexts.is_empty());
            assert!(refs.is_empty());
        }
    }

    #[test]
    fn debug_output_redacts_context_principal_and_backend_ref_material() {
        let alice = principal("alice-secret-subject");
        let mut contexts = manager();
        let context = contexts.open(&alice, "dev-a", 4, 9, 0).unwrap();
        let context_debug = format!("{context:?}");
        assert!(!context_debug.contains(context.id.as_str()));
        assert!(!context_debug.contains("alice-secret-subject"));

        let mut refs = ScopedBackendRefRegistry::new(4).unwrap();
        let public_ref = refs
            .mint(&context, ScopedRefKind::Element, "backend-secret-ref")
            .unwrap();
        let ref_debug = format!("{:?}", refs.refs.get(&public_ref).unwrap());
        assert!(!ref_debug.contains(&public_ref));
        assert!(!ref_debug.contains("backend-secret-ref"));
    }
}
