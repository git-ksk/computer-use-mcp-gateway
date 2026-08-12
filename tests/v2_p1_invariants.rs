use computer_use_mcp_gateway::{
    v2_execution_safety::{
        AuthoritativeOperationController, ExecutionEvidence, IndeterminateReason, OperationOwner,
    },
    v2_m0::DeviceCapability,
    v2_m0_execution::{
        AdmissionDecision, AdmissionLimits, ExecutionError, HubOperationState,
        IndeterminateResolution, OperationRef,
    },
};
use proptest::prelude::*;

fn op_on(device_id: &str, operation_id: &str, generation: u64) -> OperationRef {
    OperationRef {
        device_id: device_id.into(),
        device_generation: generation,
        operation_id: operation_id.into(),
    }
}

fn alice() -> OperationOwner {
    OperationOwner::new("https://issuer.example", "alice").unwrap()
}

fn bob() -> OperationOwner {
    OperationOwner::new("https://issuer.example", "bob").unwrap()
}

#[test]
fn device_a_quarantine_does_not_block_device_b_and_restores_independently() {
    let limits = AdmissionLimits {
        max_global_active: 2,
        max_queued_per_device: 2,
    };
    let mut ledger = AuthoritativeOperationController::new(limits).unwrap();

    assert!(matches!(
        ledger
            .prepare(
                op_on("desktop-a", "op-a", 11),
                alice(),
                DeviceCapability::PointerDrag,
                1,
            )
            .unwrap(),
        AdmissionDecision::StartNow(_)
    ));
    assert!(matches!(
        ledger
            .prepare(
                op_on("desktop-b", "op-b", 31),
                bob(),
                DeviceCapability::Shell,
                1,
            )
            .unwrap(),
        AdmissionDecision::StartNow(_)
    ));
    ledger.mark_dispatched("op-a", &alice(), 11, 2).unwrap();
    ledger.mark_dispatched("op-b", &bob(), 31, 2).unwrap();
    ledger
        .mark_indeterminate(
            "op-a",
            &alice(),
            11,
            IndeterminateReason::ConnectionLost,
            3,
        )
        .unwrap();

    assert!(matches!(
        ledger.prepare(
            op_on("desktop-a", "op-a-steal", 12),
            bob(),
            DeviceCapability::Shell,
            4,
        ),
        Err(ExecutionError::DeviceIndeterminate { operation_id }) if operation_id == "op-a"
    ));
    let (_, b_receipt) = ledger
        .finalize(
            "op-b",
            &bob(),
            31,
            HubOperationState::Completed,
            ExecutionEvidence::VerifiedAgentResult,
            4,
        )
        .unwrap();
    assert_eq!(b_receipt.operation.device_id, "desktop-b");
    assert_eq!(b_receipt.owner, bob());
    assert!(ledger.quarantine("desktop-b").is_none());

    let snapshot = ledger.snapshot_for_restart();
    let mut restored =
        AuthoritativeOperationController::restore_after_restart(limits, snapshot).unwrap();
    let quarantine = restored.quarantine("desktop-a").unwrap();
    assert_eq!(quarantine.operation_id, "op-a");
    assert_eq!(quarantine.device_generation, 11);
    assert_eq!(quarantine.owner, alice());
    assert_eq!(restored.state("op-b"), Some(HubOperationState::Completed));
    assert!(restored.quarantine("desktop-b").is_none());
    assert!(matches!(
        restored
            .prepare(
                op_on("desktop-b", "op-b-next", 32),
                bob(),
                DeviceCapability::Shell,
                5,
            )
            .unwrap(),
        AdmissionDecision::StartNow(_)
    ));
    assert!(matches!(
        restored.prepare(
            op_on("desktop-a", "op-a-next", 12),
            bob(),
            DeviceCapability::Shell,
            5,
        ),
        Err(ExecutionError::DeviceIndeterminate { .. })
    ));
}

#[test]
fn per_device_queue_cannot_bypass_quarantine_or_cancel_neighbor_work() {
    let limits = AdmissionLimits {
        max_global_active: 2,
        max_queued_per_device: 1,
    };
    let mut ledger = AuthoritativeOperationController::new(limits).unwrap();
    ledger
        .prepare(
            op_on("desktop-a", "op-a", 1),
            alice(),
            DeviceCapability::PointerClick,
            1,
        )
        .unwrap();
    assert!(matches!(
        ledger
            .prepare(
                op_on("desktop-a", "op-a-queued", 1),
                bob(),
                DeviceCapability::Shell,
                1,
            )
            .unwrap(),
        AdmissionDecision::Queued { position: 1 }
    ));
    ledger
        .prepare(
            op_on("desktop-b", "op-b", 7),
            bob(),
            DeviceCapability::Shell,
            1,
        )
        .unwrap();
    ledger.mark_dispatched("op-a", &alice(), 1, 2).unwrap();
    ledger.mark_dispatched("op-b", &bob(), 7, 2).unwrap();
    ledger
        .mark_indeterminate(
            "op-a",
            &alice(),
            1,
            IndeterminateReason::CancellationUnproven,
            3,
        )
        .unwrap();

    assert_eq!(
        ledger.state("op-a-queued"),
        Some(HubOperationState::Cancelled)
    );
    assert_eq!(
        ledger.receipt("op-a-queued").unwrap().evidence,
        ExecutionEvidence::CancelledBeforeDispatch
    );
    assert_eq!(ledger.state("op-b"), Some(HubOperationState::Dispatched));
    assert!(matches!(
        ledger.prepare(
            op_on("desktop-a", "op-a-bypass", 2),
            bob(),
            DeviceCapability::Shell,
            4,
        ),
        Err(ExecutionError::DeviceIndeterminate { .. })
    ));
    ledger
        .finalize(
            "op-b",
            &bob(),
            7,
            HubOperationState::Completed,
            ExecutionEvidence::VerifiedAgentResult,
            5,
        )
        .unwrap();
    assert_eq!(ledger.state("op-b"), Some(HubOperationState::Completed));
    assert_eq!(ledger.state("op-a"), Some(HubOperationState::Indeterminate));
}

#[test]
fn late_duplicate_or_cross_device_settlement_cannot_mutate_neighbor() {
    let limits = AdmissionLimits {
        max_global_active: 2,
        max_queued_per_device: 2,
    };
    let mut ledger = AuthoritativeOperationController::new(limits).unwrap();
    ledger
        .prepare(
            op_on("desktop-a", "op-a", 17),
            alice(),
            DeviceCapability::PointerDrag,
            1,
        )
        .unwrap();
    ledger
        .prepare(
            op_on("desktop-b", "op-b", 41),
            bob(),
            DeviceCapability::Shell,
            1,
        )
        .unwrap();
    ledger.mark_dispatched("op-a", &alice(), 17, 2).unwrap();
    ledger.mark_dispatched("op-b", &bob(), 41, 2).unwrap();

    assert_eq!(
        ledger.finalize(
            "op-a",
            &bob(),
            17,
            HubOperationState::Completed,
            ExecutionEvidence::VerifiedAgentResult,
            3,
        ),
        Err(ExecutionError::OwnershipFenceMismatch)
    );
    assert_eq!(ledger.state("op-b"), Some(HubOperationState::Dispatched));

    ledger
        .mark_indeterminate(
            "op-a",
            &alice(),
            17,
            IndeterminateReason::ConnectionLost,
            4,
        )
        .unwrap();
    assert_eq!(
        ledger.finalize(
            "op-a",
            &alice(),
            17,
            HubOperationState::Completed,
            ExecutionEvidence::VerifiedAgentResult,
            5,
        ),
        Err(ExecutionError::InvalidTransition)
    );
    assert_eq!(ledger.state("op-b"), Some(HubOperationState::Dispatched));

    ledger
        .finalize(
            "op-b",
            &bob(),
            41,
            HubOperationState::Completed,
            ExecutionEvidence::VerifiedAgentResult,
            6,
        )
        .unwrap();
    ledger
        .resolve_indeterminate(
            "op-a",
            alice(),
            IndeterminateResolution::ConfirmedCompleted,
            "cross-device invariant fixture reconciled A",
            7,
        )
        .unwrap();
    assert_eq!(ledger.state("op-b"), Some(HubOperationState::Completed));
    assert_eq!(
        ledger.prepare(
            op_on("desktop-a", "op-a", 18),
            alice(),
            DeviceCapability::PointerDrag,
            8,
        ),
        Err(ExecutionError::OperationReplay)
    );
}

proptest! {
    #[test]
    fn stale_a_generation_or_principal_never_mutates_b(
        stale_generation in 1_u64..50_u64,
        use_competing_owner in any::<bool>(),
        b_generation in 51_u64..100_u64,
    ) {
        prop_assume!(stale_generation != 17 || use_competing_owner);
        let limits = AdmissionLimits {
            max_global_active: 2,
            max_queued_per_device: 2,
        };
        let mut ledger = AuthoritativeOperationController::new(limits).unwrap();
        ledger.prepare(
            op_on("desktop-a", "op-a-prop", 17),
            alice(),
            DeviceCapability::PointerClick,
            1,
        ).unwrap();
        ledger.prepare(
            op_on("desktop-b", "op-b-prop", b_generation),
            bob(),
            DeviceCapability::Shell,
            1,
        ).unwrap();
        ledger.mark_dispatched("op-a-prop", &alice(), 17, 2).unwrap();
        ledger.mark_dispatched("op-b-prop", &bob(), b_generation, 2).unwrap();

        let stale_owner = if use_competing_owner { bob() } else { alice() };
        let result = ledger.finalize(
            "op-a-prop",
            &stale_owner,
            stale_generation,
            HubOperationState::Completed,
            ExecutionEvidence::VerifiedAgentResult,
            3,
        );
        prop_assert_eq!(result, Err(ExecutionError::OwnershipFenceMismatch));
        prop_assert_eq!(ledger.state("op-a-prop"), Some(HubOperationState::Dispatched));
        prop_assert_eq!(ledger.state("op-b-prop"), Some(HubOperationState::Dispatched));

        ledger.finalize(
            "op-b-prop",
            &bob(),
            b_generation,
            HubOperationState::Completed,
            ExecutionEvidence::VerifiedAgentResult,
            4,
        ).unwrap();
        prop_assert_eq!(ledger.state("op-b-prop"), Some(HubOperationState::Completed));
        prop_assert_eq!(ledger.state("op-a-prop"), Some(HubOperationState::Dispatched));
    }
}
