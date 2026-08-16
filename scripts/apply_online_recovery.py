from pathlib import Path


def replace_once(path: str, old: str, new: str) -> None:
    p = Path(path)
    text = p.read_text()
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"{path}: expected exactly one match, got {count}: {old[:120]!r}")
    p.write_text(text.replace(old, new, 1))

# Wire protocol: add additive recovery messages and advance the application schema.
replace_once(
    "src/v2_m0_transport.rs",
    "use crate::v2_m0::{\n    CapabilityAdvertisement, CommandEnvelope, CommandResultEnvelope, ControlError, DeviceIdentity,\n    DeviceRegistry, GrantToken,\n};\n",
    "use crate::v2_m0::{\n    CapabilityAdvertisement, CommandEnvelope, CommandResultEnvelope, ControlError, DeviceIdentity,\n    DeviceRegistry, GrantToken,\n};\nuse crate::v2_online_recovery::{RecoveryAuthorization, RecoveryChallenge, RecoveryResolved};\n",
)
replace_once(
    "src/v2_m0_transport.rs",
    "pub const HUB_AGENT_SCHEMA_VERSION: u16 = 1;",
    "pub const HUB_AGENT_SCHEMA_VERSION: u16 = 2;",
)
replace_once(
    "src/v2_m0_transport.rs",
    "    BackendSessionEnded(RemoteBackendSessionEnded),\n    Heartbeat(AgentHeartbeat),\n}",
    "    BackendSessionEnded(RemoteBackendSessionEnded),\n    RecoveryAuthorization(RecoveryAuthorization),\n    Heartbeat(AgentHeartbeat),\n}",
)
replace_once(
    "src/v2_m0_transport.rs",
    "    BackendSessionEnd(RemoteBackendSessionEnd),\n    HeartbeatAck(HubHeartbeatAck),\n}",
    "    BackendSessionEnd(RemoteBackendSessionEnd),\n    RecoveryChallenge(RecoveryChallenge),\n    RecoveryResolved(RecoveryResolved),\n    HeartbeatAck(HubHeartbeatAck),\n}",
)
replace_once(
    "src/v2_m0_transport.rs",
    "            Self::BackendSessionEnded(_) => \"backend_session_ended\",\n            Self::Heartbeat(_) => \"heartbeat\",",
    "            Self::BackendSessionEnded(_) => \"backend_session_ended\",\n            Self::RecoveryAuthorization(_) => \"recovery_authorization\",\n            Self::Heartbeat(_) => \"heartbeat\",",
)
replace_once(
    "src/v2_m0_transport.rs",
    "            Self::BackendSessionEnd(_) => \"backend_session_end\",\n            Self::HeartbeatAck(_) => \"heartbeat_ack\",",
    "            Self::BackendSessionEnd(_) => \"backend_session_end\",\n            Self::RecoveryChallenge(_) => \"recovery_challenge\",\n            Self::RecoveryResolved(_) => \"recovery_resolved\",\n            Self::HeartbeatAck(_) => \"heartbeat_ack\",",
)

# Agent: verify/store Hub-signed challenges, poll the private handoff for a local-user
# authorization, relay it, and only clear the handoff after a Hub-signed result.
replace_once(
    "src/v2_m1_agent.rs",
    "use crate::v2_observability::SafeErrorCode;\n",
    "use crate::v2_observability::SafeErrorCode;\nuse crate::v2_online_recovery::{\n    RecoveryError, clear_authorization, clear_recovery_handoff, load_authorization,\n    load_challenge, store_challenge, validate_authorization_against_challenge,\n    verify_recovery_challenge, verify_recovery_resolved,\n};\n",
)
replace_once(
    "src/v2_m1_agent.rs",
    "        self.browser_download_staging\n            .cleanup_all()\n            .map_err(AgentServiceError::BrowserDownloadStaging)?;\n        let trusted_clock = TrustedSessionClock::new(accepted.hub_time_ms);",
    "        self.browser_download_staging\n            .cleanup_all()\n            .map_err(AgentServiceError::BrowserDownloadStaging)?;\n        // Recovery challenges are generation-bound. Never carry an authorization\n        // across an authenticated reconnect; a fresh Hub challenge is required.\n        clear_recovery_handoff(&self.config.state_dir)\n            .map_err(AgentServiceError::OnlineRecovery)?;\n        let trusted_clock = TrustedSessionClock::new(accepted.hub_time_ms);",
)
replace_once(
    "src/v2_m1_agent.rs",
    "        let mut active: Option<ActiveOperation> = None;\n        let mut pending_backend_session_ends = VecDeque::<RemoteBackendSessionEnd>::new();\n\n        let session_result = async {",
    "        let mut active: Option<ActiveOperation> = None;\n        let mut pending_backend_session_ends = VecDeque::<RemoteBackendSessionEnd>::new();\n        let mut recovery_poll = tokio::time::interval(Duration::from_millis(250));\n        recovery_poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);\n        let mut pending_recovery_request: Option<String> = None;\n\n        let session_result = async {",
)
replace_once(
    "src/v2_m1_agent.rs",
    "                message = inbound.message() => {\n",
    "                _ = recovery_poll.tick() => {\n                    if pending_recovery_request.is_none() {\n                        if let Some(authorization) = load_authorization(&self.config.state_dir)\n                            .map_err(AgentServiceError::OnlineRecovery)?\n                        {\n                            let challenge = match load_challenge(&self.config.state_dir)\n                                .map_err(AgentServiceError::OnlineRecovery)?\n                            {\n                                Some(challenge) => challenge,\n                                None => {\n                                    clear_authorization(&self.config.state_dir)\n                                        .map_err(AgentServiceError::OnlineRecovery)?;\n                                    tracing::warn!(\n                                        event = \"v2_recovery_authorization_rejected\",\n                                        device_id = %session.device_id,\n                                        generation = session.generation,\n                                        outcome = \"rejected\",\n                                        error_code = \"recovery_challenge_missing\",\n                                        \"local recovery authorization had no current Hub challenge\"\n                                    );\n                                    continue;\n                                }\n                            };\n                            if let Err(error) = validate_authorization_against_challenge(\n                                &challenge,\n                                &authorization,\n                                trusted_clock.now_ms(),\n                            ) {\n                                clear_authorization(&self.config.state_dir)\n                                    .map_err(AgentServiceError::OnlineRecovery)?;\n                                tracing::warn!(\n                                    event = \"v2_recovery_authorization_rejected\",\n                                    device_id = %session.device_id,\n                                    generation = session.generation,\n                                    outcome = \"rejected\",\n                                    error_code = error.safe_code(),\n                                    \"stale or mismatched local recovery authorization rejected before relay\"\n                                );\n                                continue;\n                            }\n                            let request_id = authorization.request_id.clone();\n                            send_agent(\n                                &outbound_tx,\n                                AgentToHub::RecoveryAuthorization(authorization),\n                            )\n                            .await?;\n                            pending_recovery_request = Some(request_id);\n                        }\n                    }\n                }\n                message = inbound.message() => {\n",
)
replace_once(
    "src/v2_m1_agent.rs",
    "                        HubToAgent::BackendSessionEnd(remote) => {",
    "                        HubToAgent::RecoveryChallenge(recovery) => {\n                            verify_recovery_challenge(\n                                &recovery,\n                                &self.trusted_hub.verifier(),\n                                &session.device_id,\n                                session.generation,\n                                trusted_clock.now_ms(),\n                            )\n                            .map_err(AgentServiceError::OnlineRecovery)?;\n                            clear_authorization(&self.config.state_dir)\n                                .map_err(AgentServiceError::OnlineRecovery)?;\n                            store_challenge(&self.config.state_dir, &recovery)\n                                .map_err(AgentServiceError::OnlineRecovery)?;\n                            pending_recovery_request = None;\n                            tracing::warn!(\n                                event = \"v2_recovery_challenge_received\",\n                                operation_id = %recovery.operation_id,\n                                device_id = %session.device_id,\n                                generation = session.generation,\n                                quarantine_generation = recovery.quarantine_generation,\n                                outcome = \"local_user_action_required\",\n                                \"Hub-signed online recovery challenge published for local user inspection\"\n                            );\n                        }\n                        HubToAgent::RecoveryResolved(resolved) => {\n                            let expected_request_id = pending_recovery_request\n                                .as_deref()\n                                .ok_or(AgentServiceError::RecoveryResultMismatch)?;\n                            verify_recovery_resolved(\n                                &resolved,\n                                &self.trusted_hub.verifier(),\n                                expected_request_id,\n                                &session.device_id,\n                                session.generation,\n                            )\n                            .map_err(AgentServiceError::OnlineRecovery)?;\n                            clear_recovery_handoff(&self.config.state_dir)\n                                .map_err(AgentServiceError::OnlineRecovery)?;\n                            pending_recovery_request = None;\n                            tracing::info!(\n                                event = \"v2_recovery_resolved\",\n                                operation_id = %resolved.operation_id,\n                                device_id = %session.device_id,\n                                generation = session.generation,\n                                outcome = \"resolved\",\n                                \"Hub durably resolved quarantine after local user authorization\"\n                            );\n                        }\n                        HubToAgent::BackendSessionEnd(remote) => {",
)
replace_once(
    "src/v2_m1_agent.rs",
    "    Persistence(PersistenceError),\n    UnexpectedMessage {",
    "    Persistence(PersistenceError),\n    OnlineRecovery(RecoveryError),\n    UnexpectedMessage {",
)
replace_once(
    "src/v2_m1_agent.rs",
    "    HeartbeatAckMismatch,\n    CancellationMismatch,",
    "    HeartbeatAckMismatch,\n    RecoveryResultMismatch,\n    CancellationMismatch,",
)
replace_once(
    "src/v2_m1_agent.rs",
    "            Self::Persistence(error) => error.safe_error_code(),\n            Self::UnexpectedMessage { .. } => \"unexpected_message\",\n            Self::HeartbeatAckMismatch => \"heartbeat_ack_mismatch\",",
    "            Self::Persistence(error) => error.safe_error_code(),\n            Self::OnlineRecovery(error) => error.safe_code(),\n            Self::UnexpectedMessage { .. } => \"unexpected_message\",\n            Self::HeartbeatAckMismatch => \"heartbeat_ack_mismatch\",\n            Self::RecoveryResultMismatch => \"recovery_result_mismatch\",",
)

# Hub: load the separately provisioned P-256 public key, issue fresh challenges,
# verify local-user authorizations, persist resolution before acknowledging it,
# and keep replay/idempotency state outside the durable execution authority.
replace_once(
    "src/v2_m1_hub.rs",
    "use crate::v2_observability::SafeErrorCode;\n",
    "use crate::v2_observability::SafeErrorCode;\nuse crate::v2_online_recovery::{\n    RecoveryAuditAssessment, RecoveryAuthorization, RecoveryChallenge, RecoveryError,\n    RecoveryResolved, RecoveryVerifier, build_recovery_challenge, build_recovery_resolved,\n    quarantine_fingerprint,\n};\n",
)
replace_once(
    "src/v2_m1_hub.rs",
    "#[derive(Clone)]\nstruct LiveSession {\n    generation: u64,\n    command_tx: mpsc::Sender<HubRequest>,\n    supersede: watch::Sender<bool>,\n}\n\nstruct HubInner {",
    "#[derive(Clone)]\nstruct LiveSession {\n    generation: u64,\n    command_tx: mpsc::Sender<HubRequest>,\n    supersede: watch::Sender<bool>,\n}\n\n#[derive(Default)]\nstruct RecoveryRuntimeState {\n    pending: Option<RecoveryChallenge>,\n    last_resolved: Option<RecoveryResolved>,\n}\n\nstruct HubInner {",
)
replace_once(
    "src/v2_m1_hub.rs",
    "    session_slots: Arc<Semaphore>,\n    session_rate: crate::v2_limits::SlidingWindowRateLimit,\n}",
    "    session_slots: Arc<Semaphore>,\n    session_rate: crate::v2_limits::SlidingWindowRateLimit,\n    recovery_verifier: Option<RecoveryVerifier>,\n    recovery_runtime: Mutex<RecoveryRuntimeState>,\n}",
)
replace_once(
    "src/v2_m1_hub.rs",
    "        let checkpoint = CheckpointStore::new(config.state_dir.clone(), \"hub\")\n            .map_err(HubServiceError::Persistence)?;\n\n        let mut identity_registry = DeviceRegistry::default();",
    "        let checkpoint = CheckpointStore::new(config.state_dir.clone(), \"hub\")\n            .map_err(HubServiceError::Persistence)?;\n        let recovery_verifier = RecoveryVerifier::load_optional(&config.state_dir)\n            .map_err(HubServiceError::OnlineRecovery)?;\n\n        let mut identity_registry = DeviceRegistry::default();",
)
replace_once(
    "src/v2_m1_hub.rs",
    "            session_slots,\n            session_rate,\n        });",
    "            session_slots,\n            session_rate,\n            recovery_verifier,\n            recovery_runtime: Mutex::new(RecoveryRuntimeState::default()),\n        });",
)
replace_once(
    "src/v2_m1_hub.rs",
    "        if let Some(prior) = prior {\n            tracing::info!(\n                event = \"v2_agent_session_superseded\",",
    "        if let Some(prior) = prior {\n            tracing::info!(\n                event = \"v2_agent_session_superseded\",",
)
# Send a fresh recovery challenge after the authenticated generation is installed.
replace_once(
    "src/v2_m1_hub.rs",
    "            let _ = prior.supersede.send(true);\n        }\n\n        let result = self\n            .run_session_loop(",
    "            let _ = prior.supersede.send(true);\n        }\n\n        self.maybe_send_recovery_challenge(&outbound, session.generation, &session_clock)\n            .await?;\n\n        let result = self\n            .run_session_loop(",
)
# Pass outbound/session clock into cancellation handling so cancellation ambiguity can
# publish a challenge without forcing a reconnect.
replace_once(
    "src/v2_m1_hub.rs",
    "                            self.handle_cancellation_ack(\n                                ack,\n                                &hello,\n                                &challenge,\n                                generation,\n                                &mut pending,",
    "                            self.handle_cancellation_ack(\n                                ack,\n                                &outbound,\n                                &hello,\n                                &challenge,\n                                generation,\n                                session_clock,\n                                &mut pending,",
)
replace_once(
    "src/v2_m1_hub.rs",
    "                        AgentToHub::CancellationAck(ack) => {\n                            self.handle_cancellation_ack(",
    "                        AgentToHub::RecoveryAuthorization(authorization) => {\n                            self.handle_recovery_authorization(\n                                authorization,\n                                &outbound,\n                                generation,\n                                session_clock,\n                            )\n                            .await?;\n                        }\n                        AgentToHub::CancellationAck(ack) => {\n                            self.handle_cancellation_ack(",
)
# Backend-reported ambiguity: publish challenge after durable quarantine.
replace_once(
    "src/v2_m1_hub.rs",
    "                \"backend returned no proof of completion after a mutating dispatch; device quarantined\"\n            );\n            return Ok(());",
    "                \"backend returned no proof of completion after a mutating dispatch; device quarantined\"\n            );\n            self.maybe_send_recovery_challenge(outbound, generation, session_clock)\n                .await?;\n            return Ok(());",
)
# Cancellation handler signature and post-quarantine challenge.
replace_once(
    "src/v2_m1_hub.rs",
    "    async fn handle_cancellation_ack(\n        &self,\n        ack: RemoteCancellationAck,\n        hello: &AgentHello,\n        challenge: &HubChallenge,\n        generation: u64,\n        pending: &mut HashMap<String, PendingOperation>,",
    "    async fn handle_cancellation_ack(\n        &self,\n        ack: RemoteCancellationAck,\n        outbound: &mpsc::Sender<Result<HubFrame, Status>>,\n        hello: &AgentHello,\n        challenge: &HubChallenge,\n        generation: u64,\n        session_clock: &TrustedSessionClock,\n        pending: &mut HashMap<String, PendingOperation>,",
)
replace_once(
    "src/v2_m1_hub.rs",
    "                \"backend cancellation was propagated but side-effect interruption is unproven; device quarantined\"\n            );\n        }\n        if let Some(waiter) = cancel_waiters.remove(&ack.operation_id) {",
    "                \"backend cancellation was propagated but side-effect interruption is unproven; device quarantined\"\n            );\n            self.maybe_send_recovery_challenge(outbound, generation, session_clock)\n                .await?;\n        }\n        if let Some(waiter) = cancel_waiters.remove(&ack.operation_id) {",
)
# Insert Hub online-recovery helpers before ensure_current_generation.
replace_once(
    "src/v2_m1_hub.rs",
    "    async fn ensure_current_generation(&self, generation: u64) -> Result<(), HubServiceError> {",
    r'''    async fn maybe_send_recovery_challenge(
        &self,
        outbound: &mpsc::Sender<Result<HubFrame, Status>>,
        generation: u64,
        session_clock: &TrustedSessionClock,
    ) -> Result<(), HubServiceError> {
        if self.inner.recovery_verifier.is_none() {
            return Ok(());
        }
        let quarantine = {
            let persistent = self.inner.persistent.lock().await;
            persistent
                .execution
                .quarantine(&self.inner.device_id)
                .cloned()
        };
        let Some(quarantine) = quarantine else {
            return Ok(());
        };
        let now_ms = session_clock.now_ms();
        let fingerprint = quarantine_fingerprint(&quarantine);
        {
            let runtime = self.inner.recovery_runtime.lock().await;
            if runtime.pending.as_ref().is_some_and(|pending| {
                pending.current_generation == generation
                    && pending.quarantine_fingerprint == fingerprint
                    && now_ms <= pending.expires_at_ms
            }) {
                return Ok(());
            }
        }
        let challenge = build_recovery_challenge(
            &self.inner.material.hub_identity,
            &quarantine,
            generation,
            now_ms,
        )
        .map_err(HubServiceError::OnlineRecovery)?;
        {
            let mut runtime = self.inner.recovery_runtime.lock().await;
            runtime.pending = Some(challenge.clone());
            runtime.last_resolved = None;
        }
        send_hub(outbound, HubToAgent::RecoveryChallenge(challenge.clone())).await?;
        tracing::warn!(
            event = "v2_recovery_challenge_issued",
            operation_id = %challenge.operation_id,
            device_id = %challenge.device_id,
            generation,
            quarantine_generation = challenge.quarantine_generation,
            outcome = "local_user_action_required",
            "online recovery challenge issued for quarantined desktop"
        );
        Ok(())
    }

    async fn handle_recovery_authorization(
        &self,
        authorization: RecoveryAuthorization,
        outbound: &mpsc::Sender<Result<HubFrame, Status>>,
        generation: u64,
        session_clock: &TrustedSessionClock,
    ) -> Result<(), HubServiceError> {
        let verifier = self
            .inner
            .recovery_verifier
            .clone()
            .ok_or(HubServiceError::OnlineRecovery(RecoveryError::KeyUnavailable))?;

        let (pending, duplicate_ack) = {
            let runtime = self.inner.recovery_runtime.lock().await;
            let duplicate = runtime
                .last_resolved
                .as_ref()
                .filter(|resolved| resolved.request_id == authorization.request_id)
                .cloned();
            (runtime.pending.clone(), duplicate)
        };
        if let Some(ack) = duplicate_ack {
            send_hub(outbound, HubToAgent::RecoveryResolved(ack)).await?;
            return Ok(());
        }
        let pending = pending
            .ok_or(HubServiceError::OnlineRecovery(RecoveryError::ChallengeMismatch))?;
        verifier
            .verify_authorization(&pending, &authorization, session_clock.now_ms())
            .map_err(HubServiceError::OnlineRecovery)?;
        if authorization.current_generation != generation {
            return Err(HubServiceError::StaleSession);
        }

        let resolved_at_ms = session_clock.now_ms();
        {
            let mut persistent = self.inner.persistent.lock().await;
            let quarantine = persistent
                .execution
                .quarantine(&self.inner.device_id)
                .cloned()
                .ok_or(HubServiceError::OnlineRecovery(RecoveryError::ChallengeMismatch))?;
            if quarantine.operation_id != authorization.operation_id
                || quarantine.device_generation != authorization.quarantine_generation
                || quarantine_fingerprint(&quarantine) != authorization.quarantine_fingerprint
            {
                return Err(HubServiceError::OnlineRecovery(RecoveryError::ChallengeMismatch));
            }
            let rollback = persistent.execution.snapshot_for_restart();
            let resolver = OperationOwner::new("cumg://local-user-recovery", verifier.key_id())?;
            persistent.execution.resolve_indeterminate(
                &authorization.operation_id,
                resolver,
                authorization.decision.clone(),
                authorization.evidence.clone(),
                resolved_at_ms,
            )?;
            if let Err(error) = persist_locked(&self.inner, &persistent) {
                persistent.execution = AuthoritativeOperationController::restore_after_restart(
                    self.inner.config.admission_limits(),
                    rollback,
                )?;
                return Err(error);
            }
        }

        let ack = build_recovery_resolved(
            &self.inner.material.hub_identity,
            &authorization,
            resolved_at_ms,
        )
        .map_err(HubServiceError::OnlineRecovery)?;
        {
            let mut runtime = self.inner.recovery_runtime.lock().await;
            runtime.pending = None;
            runtime.last_resolved = Some(ack.clone());
        }
        crate::v2_observability::quarantine_resolved();
        tracing::info!(
            event = "v2_quarantine_resolved_online",
            operation_id = %authorization.operation_id,
            device_id = %authorization.device_id,
            generation,
            quarantine_generation = authorization.quarantine_generation,
            recovery_key_id = %verifier.key_id(),
            audit_assessment = match authorization.audit_assessment {
                RecoveryAuditAssessment::Completed => "completed",
                RecoveryAuditAssessment::NotExecuted => "not_executed",
                RecoveryAuditAssessment::Inconclusive => "inconclusive",
            },
            outcome = crate::v2_observability::resolution_name(&authorization.decision),
            "local-user-authorized online recovery durably cleared desktop quarantine"
        );
        send_hub(outbound, HubToAgent::RecoveryResolved(ack)).await
    }

    async fn ensure_current_generation(&self, generation: u64) -> Result<(), HubServiceError> {''',
)
replace_once(
    "src/v2_m1_hub.rs",
    "    Persistence(PersistenceError),\n    StateDirectoryLock(StateDirectoryLockError),",
    "    Persistence(PersistenceError),\n    OnlineRecovery(RecoveryError),\n    StateDirectoryLock(StateDirectoryLockError),",
)
replace_once(
    "src/v2_m1_hub.rs",
    "            Self::Persistence(error) => error.safe_error_code(),\n            Self::StateDirectoryLock(StateDirectoryLockError::Busy) => \"state_directory_busy\",",
    "            Self::Persistence(error) => error.safe_error_code(),\n            Self::OnlineRecovery(error) => error.safe_code(),\n            Self::StateDirectoryLock(StateDirectoryLockError::Busy) => \"state_directory_busy\",",
)

print("online recovery integration patch applied")
