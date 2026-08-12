use anyhow::{Context, Result};
use computer_use_mcp_gateway::{
    v2_execution_safety::{
        AuthoritativeOperationController, ExecutionEvidence, IndeterminateReason, OperationOwner,
    },
    v2_m0::{DeviceCapability, DeviceCommand, ShellRequest},
    v2_m0_execution::{
        AdmissionDecision, AdmissionLimits, CancellationDecision, ExecutionError,
        HubOperationState, IndeterminateResolution, OperationRef,
    },
    v2_m1_backend::{BackendExecutionOutcome, CuaMcpAdapter},
    v2_reference_backend::{
        DeterministicReferenceExecutor, ReferenceCancellationContract, ReferenceExecutionOutcome,
    },
};
use std::time::Duration;
use tokio::sync::watch;

fn owner() -> OperationOwner {
    OperationOwner::new("https://issuer.example", "alice").unwrap()
}

fn competing_owner() -> OperationOwner {
    OperationOwner::new("https://issuer.example", "bob").unwrap()
}

fn controller() -> AuthoritativeOperationController {
    AuthoritativeOperationController::new(AdmissionLimits {
        max_global_active: 1,
        max_queued_per_device: 1,
    })
    .unwrap()
}

fn prepare_cancelled(
    ledger: &mut AuthoritativeOperationController,
    command: &DeviceCommand,
) -> Result<()> {
    let operation = OperationRef {
        device_id: "desktop-a".into(),
        device_generation: 9,
        operation_id: "op-shared".into(),
    };
    assert!(matches!(
        ledger.prepare(operation, owner(), command.capability(), 1)?,
        AdmissionDecision::StartNow(_)
    ));
    ledger.mark_dispatched("op-shared", &owner(), 9, 2)?;
    assert!(matches!(
        ledger.request_cancel("op-shared", &owner(), 9, 3)?,
        CancellationDecision::SendCancellation(_)
    ));
    Ok(())
}

fn settle_ambiguous(ledger: &mut AuthoritativeOperationController) -> Result<()> {
    ledger.mark_indeterminate(
        "op-shared",
        &owner(),
        9,
        IndeterminateReason::CancellationUnproven,
        4,
    )?;
    let quarantine = ledger
        .quarantine("desktop-a")
        .context("ambiguous backend result did not quarantine device")?;
    assert_eq!(quarantine.operation_id, "op-shared");
    assert_eq!(quarantine.device_generation, 9);
    assert_eq!(quarantine.owner, owner());
    assert!(matches!(
        ledger.prepare(
            OperationRef {
                device_id: "desktop-a".into(),
                device_generation: 10,
                operation_id: "op-competing".into(),
            },
            competing_owner(),
            DeviceCapability::Shell,
            5,
        ),
        Err(ExecutionError::DeviceIndeterminate { .. })
    ));
    ledger.resolve_indeterminate(
        "op-shared",
        owner(),
        IndeterminateResolution::ConfirmedCompleted,
        "backend portability conformance reconciled the external effect",
        6,
    )?;
    assert!(ledger.quarantine("desktop-a").is_none());
    assert_eq!(
        ledger.prepare(
            OperationRef {
                device_id: "desktop-a".into(),
                device_generation: 10,
                operation_id: "op-shared".into(),
            },
            owner(),
            DeviceCapability::Shell,
            7,
        ),
        Err(ExecutionError::OperationReplay)
    );
    Ok(())
}

fn reference_command() -> DeviceCommand {
    DeviceCommand::Shell {
        request: ShellRequest {
            command: "reference-effect".into(),
            cwd: "/reference".into(),
            env: vec![],
            timeout_ms: 1_000,
        },
    }
}

#[tokio::test]
async fn reference_backend_proven_not_started_is_terminal_without_quarantine() -> Result<()> {
    let command = reference_command();
    let mut ledger = controller();
    prepare_cancelled(&mut ledger, &command)?;

    let executor = DeterministicReferenceExecutor::new(
        Duration::from_millis(30),
        Duration::from_millis(30),
        ReferenceCancellationContract::UnprovenAfterCommit,
    )?;
    let (cancel_tx, cancel_rx) = watch::channel(false);
    cancel_tx.send(true)?;
    assert_eq!(
        executor.execute(&command, cancel_rx).await?,
        ReferenceExecutionOutcome::ProvenNotStarted
    );
    assert_eq!(executor.committed_effects(), 0);

    let (_, receipt) = ledger.finalize(
        "op-shared",
        &owner(),
        9,
        HubOperationState::Cancelled,
        ExecutionEvidence::ProvenProcessTermination,
        4,
    )?;
    assert_eq!(receipt.operation.operation_id, "op-shared");
    assert_eq!(receipt.owner, owner());
    assert_eq!(receipt.operation.device_generation, 9);
    assert_eq!(receipt.evidence, ExecutionEvidence::ProvenProcessTermination);
    assert!(ledger.quarantine("desktop-a").is_none());
    Ok(())
}

#[tokio::test]
async fn reference_backend_clean_termination_is_terminal_but_unproven_is_indeterminate() -> Result<()>
{
    for contract in [
        ReferenceCancellationContract::ProvenCleanTermination,
        ReferenceCancellationContract::UnprovenAfterCommit,
    ] {
        let command = reference_command();
        let mut ledger = controller();
        prepare_cancelled(&mut ledger, &command)?;
        let executor = DeterministicReferenceExecutor::new(
            Duration::from_millis(10),
            Duration::from_millis(150),
            contract,
        )?;
        let (cancel_tx, cancel_rx) = watch::channel(false);
        let worker = {
            let executor = executor.clone();
            let command = command.clone();
            tokio::spawn(async move { executor.execute(&command, cancel_rx).await })
        };
        tokio::time::sleep(Duration::from_millis(40)).await;
        cancel_tx.send(true)?;
        let outcome = worker.await??;
        assert_eq!(executor.committed_effects(), 1);

        match outcome {
            ReferenceExecutionOutcome::ProvenCleanTermination { .. } => {
                let (_, receipt) = ledger.finalize(
                    "op-shared",
                    &owner(),
                    9,
                    HubOperationState::Cancelled,
                    ExecutionEvidence::ProvenProcessTermination,
                    4,
                )?;
                assert_eq!(receipt.owner, owner());
                assert_eq!(receipt.operation.device_generation, 9);
                assert!(ledger.quarantine("desktop-a").is_none());
            }
            ReferenceExecutionOutcome::Indeterminate { .. } => {
                settle_ambiguous(&mut ledger)?;
            }
            other => panic!("unexpected reference outcome: {other:?}"),
        }
    }
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn cua_and_reference_unknown_outcomes_use_the_same_authoritative_core_semantics() -> Result<()> {
    let fixture = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("scripts/mock_cua_mcp_backend.py");
    let cua = CuaMcpAdapter::new(
        "python3",
        vec![fixture.to_string_lossy().into_owned()],
        "fixture",
        "test",
        1,
        Duration::from_secs(3),
        Duration::from_secs(5),
        2,
        Duration::from_millis(20),
    );
    cua.connect().await?;
    let cua_command = DeviceCommand::PointerDrag {
        from_x: 10,
        from_y: 10,
        to_x: 20,
        to_y: 20,
        duration_ms: 1_000,
    };
    let (cancel_tx, cancel_rx) = watch::channel(false);
    let cua_worker = {
        let cua = cua.clone();
        let command = cua_command.clone();
        tokio::spawn(async move { cua.execute(&command, cancel_rx).await })
    };
    tokio::time::sleep(Duration::from_millis(80)).await;
    cancel_tx.send(true)?;
    assert_eq!(
        cua_worker.await??,
        BackendExecutionOutcome::CancellationPropagatedIndeterminate
    );
    cua.shutdown().await?;

    let mut cua_ledger = controller();
    prepare_cancelled(&mut cua_ledger, &cua_command)?;
    settle_ambiguous(&mut cua_ledger)?;

    let reference_command = reference_command();
    let reference = DeterministicReferenceExecutor::new(
        Duration::from_millis(10),
        Duration::from_millis(150),
        ReferenceCancellationContract::UnprovenAfterCommit,
    )?;
    let (reference_cancel_tx, reference_cancel_rx) = watch::channel(false);
    let reference_worker = {
        let reference = reference.clone();
        let command = reference_command.clone();
        tokio::spawn(async move { reference.execute(&command, reference_cancel_rx).await })
    };
    tokio::time::sleep(Duration::from_millis(40)).await;
    reference_cancel_tx.send(true)?;
    assert!(matches!(
        reference_worker.await??,
        ReferenceExecutionOutcome::Indeterminate {
            effect_may_have_happened: true
        }
    ));

    let mut reference_ledger = controller();
    prepare_cancelled(&mut reference_ledger, &reference_command)?;
    settle_ambiguous(&mut reference_ledger)?;

    assert_eq!(
        cua_ledger.state("op-shared"),
        reference_ledger.state("op-shared")
    );
    assert_eq!(
        cua_ledger.receipt("op-shared").map(|receipt| &receipt.owner),
        reference_ledger
            .receipt("op-shared")
            .map(|receipt| &receipt.owner)
    );
    assert_eq!(
        cua_ledger.resolutions().len(),
        reference_ledger.resolutions().len()
    );
    Ok(())
}
