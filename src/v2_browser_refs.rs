//! Snapshot-scoped browser capability references for V2.
//!
//! Browser references are shorter-lived than desktop references. A newer
//! semantic snapshot or a navigation invalidates page refs for exactly one tab.
//! Provider target/tab/ref values stay southbound and remain fenced by the
//! owning `InteractionContextBinding`.

use crate::v2_browser::BrowserAction;
use crate::v2_interaction_context::InteractionContextBinding;
use rand::{RngCore, rngs::OsRng};
use std::collections::HashMap;
use std::fmt;

pub const DEFAULT_MAX_BROWSER_REFS_PER_CONTEXT: usize = 4_096;
pub const MAX_BROWSER_BACKEND_REF_BYTES: usize = 4 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BrowserRefKind {
    Target,
    Tab,
    Snapshot,
    ActionElement,
    ContentElement,
    Continuation,
    Dialog,
}

impl BrowserRefKind {
    fn snapshot_bound(self) -> bool {
        matches!(
            self,
            Self::Snapshot
                | Self::ActionElement
                | Self::ContentElement
                | Self::Continuation
                | Self::Dialog
        )
    }
}

#[derive(Clone)]
struct BrowserBackendRef {
    public_ref: String,
    context_id: String,
    device_id: String,
    device_generation: u64,
    capability_revision: u64,
    kind: BrowserRefKind,
    backend_ref: String,
    target_ref: Option<String>,
    tab_ref: Option<String>,
    snapshot_ref: Option<String>,
    actions: Vec<BrowserAction>,
    single_use: bool,
}

impl fmt::Debug for BrowserBackendRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BrowserBackendRef")
            .field("public_ref", &"[redacted]")
            .field("context_id", &"[redacted]")
            .field("device_id", &self.device_id)
            .field("device_generation", &self.device_generation)
            .field("capability_revision", &self.capability_revision)
            .field("kind", &self.kind)
            .field("backend_ref", &"[redacted]")
            .field(
                "target_ref",
                &self.target_ref.as_ref().map(|_| "[redacted]"),
            )
            .field("tab_ref", &self.tab_ref.as_ref().map(|_| "[redacted]"))
            .field(
                "snapshot_ref",
                &self.snapshot_ref.as_ref().map(|_| "[redacted]"),
            )
            .field("actions", &self.actions)
            .field("single_use", &self.single_use)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedBrowserTargetTab {
    pub backend_target: String,
    pub backend_tab: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedBrowserPageRef {
    pub backend_ref: String,
    pub kind: BrowserRefKind,
}

pub struct BrowserRefRegistry {
    max_refs_per_context: usize,
    refs: HashMap<String, BrowserBackendRef>,
}

impl BrowserRefRegistry {
    pub fn new(max_refs_per_context: usize) -> Result<Self, BrowserRefError> {
        if max_refs_per_context == 0 {
            return Err(BrowserRefError::InvalidLimit);
        }
        Ok(Self {
            max_refs_per_context,
            refs: HashMap::new(),
        })
    }

    pub fn mint_target(
        &mut self,
        context: &InteractionContextBinding,
        backend_target: &str,
    ) -> Result<String, BrowserRefError> {
        self.mint(
            context,
            BrowserRefKind::Target,
            backend_target,
            None,
            None,
            None,
            &[],
            false,
        )
    }

    pub fn mint_tab(
        &mut self,
        context: &InteractionContextBinding,
        target_ref: &str,
        backend_tab: &str,
    ) -> Result<String, BrowserRefError> {
        self.require_kind(target_ref, context, BrowserRefKind::Target)?;
        self.mint(
            context,
            BrowserRefKind::Tab,
            backend_tab,
            Some(target_ref),
            None,
            None,
            &[],
            false,
        )
    }

    /// Start a fresh semantic snapshot for one exact target/tab. Existing page
    /// refs for that tab die first; target and tab capabilities survive.
    pub fn begin_snapshot(
        &mut self,
        context: &InteractionContextBinding,
        target_ref: &str,
        tab_ref: &str,
        backend_snapshot: &str,
    ) -> Result<String, BrowserRefError> {
        self.resolve_target_tab(context, target_ref, tab_ref)?;
        self.invalidate_snapshot_bound_for_tab(context, target_ref, tab_ref);
        self.mint(
            context,
            BrowserRefKind::Snapshot,
            backend_snapshot,
            Some(target_ref),
            Some(tab_ref),
            None,
            &[],
            false,
        )
    }

    pub fn mint_action_element(
        &mut self,
        context: &InteractionContextBinding,
        target_ref: &str,
        tab_ref: &str,
        snapshot_ref: &str,
        backend_element: &str,
        actions: &[BrowserAction],
    ) -> Result<String, BrowserRefError> {
        self.require_snapshot(context, target_ref, tab_ref, snapshot_ref)?;
        if actions.is_empty() {
            return Err(BrowserRefError::ActionUnavailable);
        }
        let mut exact = actions.to_vec();
        exact.sort_by_key(|action| *action as u8);
        exact.dedup();
        self.mint(
            context,
            BrowserRefKind::ActionElement,
            backend_element,
            Some(target_ref),
            Some(tab_ref),
            Some(snapshot_ref),
            &exact,
            false,
        )
    }

    pub fn mint_content_element(
        &mut self,
        context: &InteractionContextBinding,
        target_ref: &str,
        tab_ref: &str,
        snapshot_ref: &str,
        backend_element: &str,
    ) -> Result<String, BrowserRefError> {
        self.require_snapshot(context, target_ref, tab_ref, snapshot_ref)?;
        self.mint(
            context,
            BrowserRefKind::ContentElement,
            backend_element,
            Some(target_ref),
            Some(tab_ref),
            Some(snapshot_ref),
            &[],
            false,
        )
    }

    pub fn mint_continuation(
        &mut self,
        context: &InteractionContextBinding,
        target_ref: &str,
        tab_ref: &str,
        snapshot_ref: &str,
        backend_continuation: &str,
    ) -> Result<String, BrowserRefError> {
        self.require_snapshot(context, target_ref, tab_ref, snapshot_ref)?;
        self.mint(
            context,
            BrowserRefKind::Continuation,
            backend_continuation,
            Some(target_ref),
            Some(tab_ref),
            Some(snapshot_ref),
            &[],
            true,
        )
    }

    pub fn mint_dialog(
        &mut self,
        context: &InteractionContextBinding,
        target_ref: &str,
        tab_ref: &str,
        snapshot_ref: &str,
        backend_dialog: &str,
    ) -> Result<String, BrowserRefError> {
        self.require_snapshot(context, target_ref, tab_ref, snapshot_ref)?;
        self.mint(
            context,
            BrowserRefKind::Dialog,
            backend_dialog,
            Some(target_ref),
            Some(tab_ref),
            Some(snapshot_ref),
            &[],
            false,
        )
    }

    pub fn resolve_target_tab(
        &self,
        context: &InteractionContextBinding,
        target_ref: &str,
        tab_ref: &str,
    ) -> Result<ResolvedBrowserTargetTab, BrowserRefError> {
        let target = self.require_kind(target_ref, context, BrowserRefKind::Target)?;
        let tab = self.require_kind(tab_ref, context, BrowserRefKind::Tab)?;
        if tab.target_ref.as_deref() != Some(target_ref) {
            return Err(BrowserRefError::RelationMismatch);
        }
        Ok(ResolvedBrowserTargetTab {
            backend_target: target.backend_ref.clone(),
            backend_tab: tab.backend_ref.clone(),
        })
    }

    pub fn resolve_action(
        &self,
        context: &InteractionContextBinding,
        target_ref: &str,
        tab_ref: &str,
        element_ref: &str,
        required: BrowserAction,
    ) -> Result<ResolvedBrowserPageRef, BrowserRefError> {
        let resolved = self.resolve_page_ref(
            context,
            target_ref,
            tab_ref,
            element_ref,
            &[BrowserRefKind::ActionElement],
        )?;
        let reference = self.require_owned(element_ref, context)?;
        if !reference.actions.contains(&required) {
            return Err(BrowserRefError::ActionUnavailable);
        }
        Ok(resolved)
    }

    pub fn resolve_scope_ref(
        &self,
        context: &InteractionContextBinding,
        target_ref: &str,
        tab_ref: &str,
        scope_ref: &str,
    ) -> Result<ResolvedBrowserPageRef, BrowserRefError> {
        self.resolve_page_ref(
            context,
            target_ref,
            tab_ref,
            scope_ref,
            &[
                BrowserRefKind::ActionElement,
                BrowserRefKind::ContentElement,
            ],
        )
    }

    pub fn resolve_dialog(
        &self,
        context: &InteractionContextBinding,
        target_ref: &str,
        tab_ref: &str,
        dialog_ref: &str,
    ) -> Result<ResolvedBrowserPageRef, BrowserRefError> {
        self.resolve_page_ref(
            context,
            target_ref,
            tab_ref,
            dialog_ref,
            &[BrowserRefKind::Dialog],
        )
    }

    /// Continuations are opaque single-use capabilities.
    pub fn consume_continuation(
        &mut self,
        context: &InteractionContextBinding,
        target_ref: &str,
        tab_ref: &str,
        continuation_ref: &str,
    ) -> Result<String, BrowserRefError> {
        let resolved = self.resolve_page_ref(
            context,
            target_ref,
            tab_ref,
            continuation_ref,
            &[BrowserRefKind::Continuation],
        )?;
        let reference = self
            .refs
            .get(continuation_ref)
            .ok_or(BrowserRefError::UnknownRef)?;
        if !reference.single_use {
            return Err(BrowserRefError::KindMismatch);
        }
        self.refs.remove(continuation_ref);
        Ok(resolved.backend_ref)
    }

    /// Navigation changes document identity. A caller must inspect again before
    /// another ref-targeted action on this tab.
    pub fn invalidate_tab_document(
        &mut self,
        context: &InteractionContextBinding,
        target_ref: &str,
        tab_ref: &str,
    ) -> Result<(), BrowserRefError> {
        self.resolve_target_tab(context, target_ref, tab_ref)?;
        self.invalidate_snapshot_bound_for_tab(context, target_ref, tab_ref);
        Ok(())
    }

    pub fn invalidate_context(&mut self, context_id: &str) {
        self.refs
            .retain(|_, reference| reference.context_id != context_id);
    }

    pub fn invalidate_device_generation(&mut self, device_id: &str, generation: u64) {
        self.refs.retain(|_, reference| {
            reference.device_id != device_id || reference.device_generation == generation
        });
    }

    pub fn invalidate_capability_revision(&mut self, device_id: &str, revision: u64) {
        self.refs.retain(|_, reference| {
            reference.device_id != device_id || reference.capability_revision == revision
        });
    }

    pub fn len(&self) -> usize {
        self.refs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.refs.is_empty()
    }

    fn resolve_page_ref(
        &self,
        context: &InteractionContextBinding,
        target_ref: &str,
        tab_ref: &str,
        public_ref: &str,
        allowed: &[BrowserRefKind],
    ) -> Result<ResolvedBrowserPageRef, BrowserRefError> {
        self.resolve_target_tab(context, target_ref, tab_ref)?;
        let reference = self.require_owned(public_ref, context)?;
        if !allowed.contains(&reference.kind) {
            return Err(BrowserRefError::KindMismatch);
        }
        if reference.target_ref.as_deref() != Some(target_ref)
            || reference.tab_ref.as_deref() != Some(tab_ref)
        {
            return Err(BrowserRefError::RelationMismatch);
        }
        let snapshot_ref = reference
            .snapshot_ref
            .as_deref()
            .ok_or(BrowserRefError::RelationMismatch)?;
        self.require_snapshot(context, target_ref, tab_ref, snapshot_ref)?;
        Ok(ResolvedBrowserPageRef {
            backend_ref: reference.backend_ref.clone(),
            kind: reference.kind,
        })
    }

    fn require_snapshot(
        &self,
        context: &InteractionContextBinding,
        target_ref: &str,
        tab_ref: &str,
        snapshot_ref: &str,
    ) -> Result<(), BrowserRefError> {
        self.resolve_target_tab(context, target_ref, tab_ref)?;
        let snapshot = self.require_kind(snapshot_ref, context, BrowserRefKind::Snapshot)?;
        if snapshot.target_ref.as_deref() != Some(target_ref)
            || snapshot.tab_ref.as_deref() != Some(tab_ref)
        {
            return Err(BrowserRefError::RelationMismatch);
        }
        Ok(())
    }

    fn require_kind(
        &self,
        public_ref: &str,
        context: &InteractionContextBinding,
        expected: BrowserRefKind,
    ) -> Result<&BrowserBackendRef, BrowserRefError> {
        let reference = self.require_owned(public_ref, context)?;
        if reference.kind != expected {
            return Err(BrowserRefError::KindMismatch);
        }
        Ok(reference)
    }

    fn require_owned(
        &self,
        public_ref: &str,
        context: &InteractionContextBinding,
    ) -> Result<&BrowserBackendRef, BrowserRefError> {
        let reference = self
            .refs
            .get(public_ref)
            .ok_or(BrowserRefError::UnknownRef)?;
        if reference.public_ref != public_ref
            || reference.context_id != context.id.as_str()
            || reference.device_id != context.device_id
        {
            return Err(BrowserRefError::ContextMismatch);
        }
        if reference.device_generation != context.device_generation {
            return Err(BrowserRefError::GenerationMismatch);
        }
        if reference.capability_revision != context.capability_revision {
            return Err(BrowserRefError::CapabilityRevisionMismatch);
        }
        Ok(reference)
    }

    #[allow(clippy::too_many_arguments)]
    fn mint(
        &mut self,
        context: &InteractionContextBinding,
        kind: BrowserRefKind,
        backend_ref: &str,
        target_ref: Option<&str>,
        tab_ref: Option<&str>,
        snapshot_ref: Option<&str>,
        actions: &[BrowserAction],
        single_use: bool,
    ) -> Result<String, BrowserRefError> {
        validate_backend_ref(backend_ref)?;
        let count = self
            .refs
            .values()
            .filter(|reference| reference.context_id == context.id.as_str())
            .count();
        if count >= self.max_refs_per_context {
            return Err(BrowserRefError::RefLimitExceeded);
        }
        for _ in 0..4 {
            let public_ref = random_ref();
            if self.refs.contains_key(&public_ref) {
                continue;
            }
            self.refs.insert(
                public_ref.clone(),
                BrowserBackendRef {
                    public_ref: public_ref.clone(),
                    context_id: context.id.as_str().to_owned(),
                    device_id: context.device_id.clone(),
                    device_generation: context.device_generation,
                    capability_revision: context.capability_revision,
                    kind,
                    backend_ref: backend_ref.to_owned(),
                    target_ref: target_ref.map(str::to_owned),
                    tab_ref: tab_ref.map(str::to_owned),
                    snapshot_ref: snapshot_ref.map(str::to_owned),
                    actions: actions.to_vec(),
                    single_use,
                },
            );
            return Ok(public_ref);
        }
        Err(BrowserRefError::IdentifierCollision)
    }

    fn invalidate_snapshot_bound_for_tab(
        &mut self,
        context: &InteractionContextBinding,
        target_ref: &str,
        tab_ref: &str,
    ) {
        self.refs.retain(|_, reference| {
            !(reference.context_id == context.id.as_str()
                && reference.device_id == context.device_id
                && reference.device_generation == context.device_generation
                && reference.capability_revision == context.capability_revision
                && reference.target_ref.as_deref() == Some(target_ref)
                && reference.tab_ref.as_deref() == Some(tab_ref)
                && reference.kind.snapshot_bound())
        });
    }
}

impl Default for BrowserRefRegistry {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_BROWSER_REFS_PER_CONTEXT).expect("static browser ref limit is valid")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowserRefError {
    InvalidLimit,
    InvalidBackendRef,
    RefLimitExceeded,
    IdentifierCollision,
    UnknownRef,
    ContextMismatch,
    GenerationMismatch,
    CapabilityRevisionMismatch,
    KindMismatch,
    RelationMismatch,
    ActionUnavailable,
}

impl fmt::Display for BrowserRefError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for BrowserRefError {}

fn validate_backend_ref(value: &str) -> Result<(), BrowserRefError> {
    if value.is_empty()
        || value.len() > MAX_BROWSER_BACKEND_REF_BYTES
        || value.chars().any(char::is_control)
    {
        return Err(BrowserRefError::InvalidBackendRef);
    }
    Ok(())
}

fn random_ref() -> String {
    let mut random = [0_u8; 16];
    OsRng.fill_bytes(&mut random);
    let mut output = String::with_capacity(36);
    output.push_str("ref_");
    for byte in random {
        use std::fmt::Write as _;
        let _ = write!(&mut output, "{byte:02x}");
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v2_interaction_context::{InteractionContextLimits, InteractionContextManager};
    use crate::v2_m0_trust::AuthenticatedClientPrincipal;

    fn context(generation: u64, revision: u64) -> InteractionContextBinding {
        let principal = AuthenticatedClientPrincipal {
            issuer: "https://issuer.example".into(),
            subject: "alice".into(),
        };
        InteractionContextManager::new(InteractionContextLimits::default())
            .unwrap()
            .open(&principal, "dev-a", generation, revision, 1)
            .unwrap()
    }

    fn bound_tab(
        refs: &mut BrowserRefRegistry,
        context: &InteractionContextBinding,
    ) -> (String, String) {
        let target = refs.mint_target(context, "backend-target-secret").unwrap();
        let tab = refs
            .mint_tab(context, &target, "backend-tab-secret")
            .unwrap();
        (target, tab)
    }

    #[test]
    fn target_and_tab_are_context_generation_revision_and_relation_bound() {
        let first = context(4, 9);
        let second = context(4, 9);
        let mut refs = BrowserRefRegistry::default();
        let (target, tab) = bound_tab(&mut refs, &first);
        assert_eq!(
            refs.resolve_target_tab(&second, &target, &tab),
            Err(BrowserRefError::ContextMismatch)
        );
        let mut wrong_generation = first.clone();
        wrong_generation.device_generation += 1;
        assert_eq!(
            refs.resolve_target_tab(&wrong_generation, &target, &tab),
            Err(BrowserRefError::GenerationMismatch)
        );
        let mut wrong_revision = first.clone();
        wrong_revision.capability_revision += 1;
        assert_eq!(
            refs.resolve_target_tab(&wrong_revision, &target, &tab),
            Err(BrowserRefError::CapabilityRevisionMismatch)
        );
    }

    #[test]
    fn newer_snapshot_invalidates_old_refs_and_exact_actions_are_enforced() {
        let context = context(4, 9);
        let mut refs = BrowserRefRegistry::default();
        let (target, tab) = bound_tab(&mut refs, &context);
        let first_snapshot = refs
            .begin_snapshot(&context, &target, &tab, "snapshot-one")
            .unwrap();
        let action = refs
            .mint_action_element(
                &context,
                &target,
                &tab,
                &first_snapshot,
                "element-one",
                &[BrowserAction::Click, BrowserAction::Pointer],
            )
            .unwrap();
        assert!(
            refs.resolve_action(&context, &target, &tab, &action, BrowserAction::Click)
                .is_ok()
        );
        assert_eq!(
            refs.resolve_action(&context, &target, &tab, &action, BrowserAction::Type),
            Err(BrowserRefError::ActionUnavailable)
        );

        let _second_snapshot = refs
            .begin_snapshot(&context, &target, &tab, "snapshot-two")
            .unwrap();
        assert_eq!(
            refs.resolve_action(&context, &target, &tab, &action, BrowserAction::Click),
            Err(BrowserRefError::UnknownRef)
        );
    }

    #[test]
    fn content_ref_is_read_scope_not_action_authority() {
        let context = context(4, 9);
        let mut refs = BrowserRefRegistry::default();
        let (target, tab) = bound_tab(&mut refs, &context);
        let snapshot = refs
            .begin_snapshot(&context, &target, &tab, "snapshot")
            .unwrap();
        let content = refs
            .mint_content_element(&context, &target, &tab, &snapshot, "content")
            .unwrap();
        assert_eq!(
            refs.resolve_action(&context, &target, &tab, &content, BrowserAction::Click,),
            Err(BrowserRefError::KindMismatch)
        );
        assert!(
            refs.resolve_scope_ref(&context, &target, &tab, &content)
                .is_ok()
        );
    }

    #[test]
    fn navigation_invalidates_only_one_tabs_document_refs() {
        let context = context(4, 9);
        let mut refs = BrowserRefRegistry::default();
        let target = refs.mint_target(&context, "target").unwrap();
        let first_tab = refs.mint_tab(&context, &target, "tab-one").unwrap();
        let second_tab = refs.mint_tab(&context, &target, "tab-two").unwrap();
        let first_snapshot = refs
            .begin_snapshot(&context, &target, &first_tab, "snapshot-one")
            .unwrap();
        let first_element = refs
            .mint_action_element(
                &context,
                &target,
                &first_tab,
                &first_snapshot,
                "element-one",
                &[BrowserAction::Click],
            )
            .unwrap();
        let second_snapshot = refs
            .begin_snapshot(&context, &target, &second_tab, "snapshot-two")
            .unwrap();
        let second_element = refs
            .mint_action_element(
                &context,
                &target,
                &second_tab,
                &second_snapshot,
                "element-two",
                &[BrowserAction::Click],
            )
            .unwrap();

        refs.invalidate_tab_document(&context, &target, &first_tab)
            .unwrap();
        assert_eq!(
            refs.resolve_action(
                &context,
                &target,
                &first_tab,
                &first_element,
                BrowserAction::Click,
            ),
            Err(BrowserRefError::UnknownRef)
        );
        assert!(
            refs.resolve_action(
                &context,
                &target,
                &second_tab,
                &second_element,
                BrowserAction::Click,
            )
            .is_ok()
        );
    }

    #[test]
    fn continuation_is_single_use() {
        let context = context(4, 9);
        let mut refs = BrowserRefRegistry::default();
        let (target, tab) = bound_tab(&mut refs, &context);
        let snapshot = refs
            .begin_snapshot(&context, &target, &tab, "snapshot")
            .unwrap();
        let continuation = refs
            .mint_continuation(&context, &target, &tab, &snapshot, "backend-continuation")
            .unwrap();
        assert_eq!(
            refs.consume_continuation(&context, &target, &tab, &continuation),
            Ok("backend-continuation".into())
        );
        assert_eq!(
            refs.consume_continuation(&context, &target, &tab, &continuation),
            Err(BrowserRefError::UnknownRef)
        );
    }

    #[test]
    fn generation_revision_and_context_cleanup_drop_browser_refs() {
        let context = context(4, 9);
        let mut refs = BrowserRefRegistry::default();
        let _ = bound_tab(&mut refs, &context);
        refs.invalidate_device_generation("dev-a", 5);
        assert!(refs.is_empty());

        let context = context(5, 9);
        let _ = bound_tab(&mut refs, &context);
        refs.invalidate_capability_revision("dev-a", 10);
        assert!(refs.is_empty());

        let context = context(5, 10);
        let _ = bound_tab(&mut refs, &context);
        refs.invalidate_context(context.id.as_str());
        assert!(refs.is_empty());
    }

    #[test]
    fn debug_redacts_public_backend_and_context_refs() {
        let context = context(4, 9);
        let mut refs = BrowserRefRegistry::default();
        let target = refs
            .mint_target(&context, "backend-target-super-secret")
            .unwrap();
        let debug = format!("{:?}", refs.refs.get(&target).unwrap());
        assert!(!debug.contains(&target));
        assert!(!debug.contains("backend-target-super-secret"));
        assert!(!debug.contains(context.id.as_str()));
    }
}
