use computer_use_mcp_gateway::{
    v2_execution_safety::{
        AuthoritativeOperationController, ExecutionEvidence, IndeterminateReason, OperationOwner,
    },
    v2_m0::DeviceCapability,
    v2_m0_execution::{
        AdmissionDecision, AdmissionLimits, AgentExecutionGate, HubOperationState, OperationRef,
    },
    v2_m1_persistence::MAX_CHECKPOINT_BYTES,
};

const TOTAL_OPERATIONS: usize = 10_000;
const OPERATIONS_PER_GENERATION: usize = 100;
const WARMUP_OPERATIONS: usize = 2_000;
const RSS_GROWTH_ALLOWANCE_BYTES: u64 = 32 * 1024 * 1024;

fn owner() -> OperationOwner {
    OperationOwner::new("https://issuer.example", "resource-soak").unwrap()
}

fn operation(device_id: &str, operation_id: String, generation: u64) -> OperationRef {
    OperationRef {
        device_id: device_id.into(),
        device_generation: generation,
        operation_id,
    }
}

#[cfg(target_os = "linux")]
fn current_rss_bytes() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|line| line.starts_with("VmRSS:"))?;
    let kib = line.split_whitespace().nth(1)?.parse::<u64>().ok()?;
    kib.checked_mul(1024)
}

#[cfg(not(target_os = "linux"))]
fn current_rss_bytes() -> Option<u64> {
    None
}

#[test]
fn ten_thousand_operations_with_reconnect_churn_keep_v2_state_and_rss_bounded() {
    assert_eq!(TOTAL_OPERATIONS % OPERATIONS_PER_GENERATION, 0);
    assert!(WARMUP_OPERATIONS < TOTAL_OPERATIONS);

    let limits = AdmissionLimits {
        max_global_active: 2,
        max_queued_per_device: 2,
    };
    let owner = owner();
    let mut hub = AuthoritativeOperationController::new(limits).unwrap();
    let mut agent = AgentExecutionGate::default();

    // Keep one unresolved ambiguous operation alive for the entire soak. Terminal
    // replay state may be compacted on generation advance, but unresolved
    // ambiguity must never be forgotten merely to stay within a memory budget.
    let quarantine_operation = operation("desktop-quarantine", "quarantine-anchor".into(), 1);
    assert!(matches!(
        hub.prepare(
            quarantine_operation.clone(),
            owner.clone(),
            DeviceCapability::PointerDrag,
            1,
        )
        .unwrap(),
        AdmissionDecision::StartNow(_)
    ));
    hub.mark_dispatched(&quarantine_operation.operation_id, &owner, 1, 2)
        .unwrap();
    hub.mark_indeterminate(
        &quarantine_operation.operation_id,
        &owner,
        1,
        IndeterminateReason::CancellationUnproven,
        3,
    )
    .unwrap();

    let generation_count = TOTAL_OPERATIONS / OPERATIONS_PER_GENERATION;
    let mut warmup_rss = None;

    for generation_index in 0..generation_count {
        let generation = u64::try_from(generation_index + 1).unwrap();
        agent.prepare_generation(generation).unwrap();

        for offset in 0..OPERATIONS_PER_GENERATION {
            let index = generation_index * OPERATIONS_PER_GENERATION + offset;
            let operation = operation("desktop-a", format!("resource-soak-{index:05}"), generation);
            let now_ms = u64::try_from(index).unwrap().saturating_mul(4) + 10;

            assert!(matches!(
                hub.prepare(
                    operation.clone(),
                    owner.clone(),
                    DeviceCapability::Shell,
                    now_ms,
                )
                .unwrap(),
                AdmissionDecision::StartNow(_)
            ));
            agent.begin(operation.clone()).unwrap();
            hub.mark_dispatched(&operation.operation_id, &owner, generation, now_ms + 1)
                .unwrap();
            agent.finish(&operation.operation_id).unwrap();
            hub.finalize(
                &operation.operation_id,
                &owner,
                generation,
                HubOperationState::Completed,
                ExecutionEvidence::VerifiedAgentResult,
                now_ms + 2,
            )
            .unwrap();

            if index + 1 == WARMUP_OPERATIONS {
                warmup_rss = current_rss_bytes();
            }
        }

        // Exercise the exact durable state that restart/reconnect relies on.
        // Each checkpoint must remain comfortably below the persistence ceiling
        // rather than growing with the lifetime operation count.
        let hub_snapshot = hub.snapshot_for_restart();
        assert!(hub_snapshot.operations.len() <= OPERATIONS_PER_GENERATION + 1);
        assert_eq!(hub_snapshot.quarantines.len(), 1);
        assert_eq!(
            hub_snapshot.quarantines[0].operation_id,
            quarantine_operation.operation_id
        );
        let serialized_hub = serde_json::to_vec(&hub_snapshot).unwrap();
        assert!(u64::try_from(serialized_hub.len()).unwrap() < MAX_CHECKPOINT_BYTES);

        let agent_snapshot = agent.snapshot_for_restart();
        assert!(agent_snapshot.terminal_operation_ids.len() <= OPERATIONS_PER_GENERATION);
        let serialized_agent = serde_json::to_vec(&agent_snapshot).unwrap();
        assert!(u64::try_from(serialized_agent.len()).unwrap() < MAX_CHECKPOINT_BYTES);

        // Simulate process restart and the authenticated reconnect generation
        // advance. Production calls the same Hub compaction hook when a new
        // Agent session is accepted.
        hub =
            AuthoritativeOperationController::restore_after_restart(limits, hub_snapshot).unwrap();
        agent = AgentExecutionGate::restore_after_restart(agent_snapshot).unwrap();

        let next_generation = generation + 1;
        let removed = hub
            .prune_terminal_before_generation("desktop-a", next_generation)
            .unwrap();
        assert_eq!(removed, OPERATIONS_PER_GENERATION);
        agent.prepare_generation(next_generation).unwrap();
        assert!(
            agent
                .snapshot_for_restart()
                .terminal_operation_ids
                .is_empty()
        );

        assert_eq!(
            hub.state(&quarantine_operation.operation_id),
            Some(HubOperationState::Indeterminate)
        );
        assert_eq!(
            hub.quarantine("desktop-quarantine")
                .map(|quarantine| quarantine.operation_id.as_str()),
            Some(quarantine_operation.operation_id.as_str())
        );
    }

    let final_snapshot = hub.snapshot_for_restart();
    assert_eq!(final_snapshot.operations.len(), 1);
    assert_eq!(
        final_snapshot.operations[0].operation.operation_id,
        quarantine_operation.operation_id
    );

    // Linux CI supplies an actual process RSS guard in addition to the logical
    // cardinality/checkpoint bounds above. Warm the allocator first so this
    // catches sustained growth rather than ordinary one-time runtime setup.
    if let (Some(warmup), Some(final_rss)) = (warmup_rss, current_rss_bytes()) {
        assert!(
            final_rss <= warmup.saturating_add(RSS_GROWTH_ALLOWANCE_BYTES),
            "V2 resource soak RSS did not plateau after warmup: warmup={warmup} final={final_rss} allowance={RSS_GROWTH_ALLOWANCE_BYTES}"
        );
    }
}
