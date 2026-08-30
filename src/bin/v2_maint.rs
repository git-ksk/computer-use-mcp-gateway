use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand, ValueEnum};
use computer_use_mcp_gateway::{
    mutation_authority::{
        MutationAuthorityRole, initialize_mutation_authority, inspect_mutation_authority,
        switch_mutation_authority_guarded,
    },
    v2_execution_safety::RetirementPolicy,
    v2_handoff_control::{LocalHandoffControlRequest, exchange_unix_handoff_control},
    v2_m0_execution::IndeterminateResolution,
    v2_m1_keys::load_secret_text,
    v2_maintenance::{
        audit_reconciliation_read_only, compare_quarantined_request_read_only,
        inspect_auto_resolutions_read_only, inspect_quarantines_read_only,
        resolve_indeterminate_offline, retire_indeterminate_offline,
    },
};
use std::path::PathBuf;

const MAX_AUDIT_FINGERPRINT_SECRET_BYTES: u64 = 4 * 1024;
const MAX_CANDIDATE_REQUEST_BYTES: u64 = 256 * 1024;
const MIN_AUDIT_FINGERPRINT_SECRET_BYTES: usize = 32;

fn handoff_is_idle(socket: &std::path::Path) -> bool {
    match exchange_unix_handoff_control(socket, &LocalHandoffControlRequest::Status) {
        Ok(response) if response.ok => response.status.is_some_and(|status| {
            status.active.is_none()
                && !status.recovery_required
                && !status.resume_requested
                && !status.faulted
        }),
        _ => false,
    }
}

#[derive(Debug, Parser)]
#[command(name = "v2_maint")]
#[command(about = "Offline operator maintenance for durable V2 Hub state")]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Resolve one durable indeterminate operation while the Hub is stopped.
    Resolve {
        #[arg(long, env = "CUMG_V2_HUB_STATE_DIR")]
        state_dir: PathBuf,
        #[arg(long)]
        operation_id: String,
        #[arg(long, value_enum)]
        decision: ResolutionDecision,
        /// Audit metadata only; never include commands, results, desktop content, or secrets.
        #[arg(long)]
        evidence: String,
    },
    /// Retire one policy-eligible unknowable indeterminate operation while the Hub is stopped.
    /// This clears quarantine without asserting success/non-execution and never replays the old work.
    RetireIndeterminate {
        #[arg(long, env = "CUMG_V2_HUB_STATE_DIR")]
        state_dir: PathBuf,
        #[arg(long)]
        operation_id: String,
        /// Explicitly pin the reviewed retirement policy; future policies are never selected implicitly.
        #[arg(long, value_enum)]
        policy: RetirementPolicyArg,
        /// Bounded audit rationale only; never include commands, results, desktop content, URLs, or secrets.
        #[arg(long)]
        reason: String,
    },
    /// Inspect durable quarantine metadata without resolving or dispatching work.
    InspectQuarantine {
        #[arg(long, env = "CUMG_V2_HUB_STATE_DIR")]
        state_dir: PathBuf,
        /// Optionally restrict output to one stable device ID.
        #[arg(long)]
        device_id: Option<String>,
    },
    /// Inspect bounded self-reconciliation history without exposing raw requests or results.
    InspectReconciliationHistory {
        #[arg(long, env = "CUMG_V2_HUB_STATE_DIR")]
        state_dir: PathBuf,
        /// Optionally restrict output to one stable device ID.
        #[arg(long)]
        device_id: Option<String>,
    },
    /// Audit one quarantine against durable Hub and Agent reconciliation evidence.
    /// This is inspection-only and never resolves, retries, replays, signs, or dispatches work.
    AuditReconciliation {
        #[arg(long, env = "CUMG_V2_HUB_STATE_DIR")]
        state_dir: PathBuf,
        #[arg(long, env = "CUMG_V2_AGENT_STATE_DIR")]
        agent_state_dir: PathBuf,
        #[arg(long)]
        operation_id: String,
    },
    /// Compare one local candidate shell/process/text-input request to a quarantined request.
    /// Output is only same_request, different_request, or unavailable; request content
    /// and the keyed fingerprint are never printed.
    CompareQuarantineRequest {
        #[arg(long, env = "CUMG_V2_HUB_STATE_DIR")]
        state_dir: PathBuf,
        #[arg(long)]
        operation_id: String,
        #[arg(long, value_enum)]
        tool: CandidateTool,
        /// Private JSON file containing the candidate tool arguments.
        #[arg(long)]
        request_file: PathBuf,
        #[arg(long, env = "CUMG_V2_AUDIT_FINGERPRINT_SECRET_FILE")]
        fingerprint_secret_file: PathBuf,
    },
    /// Initialize the private shared V1/V2 mutation-authority state.
    MutationAuthorityInit {
        #[arg(long, env = "CUMG_MUTATION_AUTHORITY_DIR")]
        authority_dir: PathBuf,
        #[arg(long, value_enum)]
        owner: MutationAuthorityRoleArg,
    },
    /// Inspect the shared mutation owner without changing authority.
    MutationAuthorityStatus {
        #[arg(long, env = "CUMG_MUTATION_AUTHORITY_DIR")]
        authority_dir: PathBuf,
    },
    /// CAS-switch the shared mutation owner after proving V2 has no quarantine and Handoff is idle.
    MutationAuthoritySwitch {
        #[arg(long, env = "CUMG_MUTATION_AUTHORITY_DIR")]
        authority_dir: PathBuf,
        #[arg(long, value_enum)]
        from: MutationAuthorityRoleArg,
        #[arg(long, value_enum)]
        to: MutationAuthorityRoleArg,
        #[arg(long, env = "CUMG_V2_HUB_STATE_DIR")]
        hub_state_dir: PathBuf,
        #[arg(long, env = "CUMG_V2_HANDOFF_CONTROL_SOCKET")]
        handoff_control_socket: PathBuf,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum MutationAuthorityRoleArg {
    #[value(name = "v1")]
    V1,
    #[value(name = "v2")]
    V2,
}

impl From<MutationAuthorityRoleArg> for MutationAuthorityRole {
    fn from(value: MutationAuthorityRoleArg) -> Self {
        match value {
            MutationAuthorityRoleArg::V1 => Self::V1,
            MutationAuthorityRoleArg::V2 => Self::V2,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum CandidateTool {
    #[value(name = "shell")]
    Shell,
    #[value(name = "execute_process")]
    ExecuteProcess,
    #[value(name = "type_text")]
    TypeText,
}

impl CandidateTool {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Shell => "shell",
            Self::ExecuteProcess => "execute_process",
            Self::TypeText => "type_text",
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum RetirementPolicyArg {
    #[value(name = "transient_ui_interaction_v1")]
    TransientUiInteractionV1,
}

impl From<RetirementPolicyArg> for RetirementPolicy {
    fn from(value: RetirementPolicyArg) -> Self {
        match value {
            RetirementPolicyArg::TransientUiInteractionV1 => Self::TransientUiInteractionV1,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum ResolutionDecision {
    #[value(name = "confirmed_completed")]
    Completed,
    #[value(name = "confirmed_not_executed")]
    NotExecuted,
    #[value(name = "confirmed_effect_applied_uncommitted")]
    EffectAppliedUncommitted,
}

impl From<ResolutionDecision> for IndeterminateResolution {
    fn from(value: ResolutionDecision) -> Self {
        match value {
            ResolutionDecision::Completed => Self::ConfirmedCompleted,
            ResolutionDecision::NotExecuted => Self::ConfirmedNotExecuted,
            ResolutionDecision::EffectAppliedUncommitted => Self::ConfirmedEffectAppliedUncommitted,
        }
    }
}

fn main() -> Result<()> {
    let _observability = computer_use_mcp_gateway::v2_observability::init("cumg-v2-maint")?;
    let args = Args::parse();
    match args.command {
        Command::Resolve {
            state_dir,
            operation_id,
            decision,
            evidence,
        } => {
            let result =
                resolve_indeterminate_offline(&state_dir, &operation_id, decision.into(), evidence)
                    .context("offline quarantine resolution failed")?;
            computer_use_mcp_gateway::v2_observability::quarantine_resolved();
            tracing::info!(
                event = "v2_quarantine_resolved",
                operation_id = %result.receipt.operation.operation_id,
                device_id = %result.receipt.operation.device_id,
                generation = result.receipt.operation.device_generation,
                capability = computer_use_mcp_gateway::v2_observability::capability_name(result.receipt.capability),
                outcome = computer_use_mcp_gateway::v2_observability::resolution_name(&result.resolution.decision),
                resolver = "local_maintenance_operator",
                "indeterminate operation explicitly resolved offline; quarantine cleared"
            );
            println!(
                "resolved operation={} device={} generation={} terminal_state={:?}",
                result.receipt.operation.operation_id,
                result.receipt.operation.device_id,
                result.receipt.operation.device_generation,
                result.receipt.terminal_state,
            );
        }
        Command::RetireIndeterminate {
            state_dir,
            operation_id,
            policy,
            reason,
        } => {
            let result =
                retire_indeterminate_offline(&state_dir, &operation_id, policy.into(), reason)
                    .context("offline indeterminate retirement failed")?;
            tracing::info!(
                event = "v2_quarantine_retired",
                operation_id = %result.retirement.operation.operation_id,
                device_id = %result.retirement.operation.device_id,
                generation = result.retirement.operation.device_generation,
                authorized_generation = result.retirement.authorized_device_generation,
                capability = computer_use_mcp_gateway::v2_observability::capability_name(result.retirement.capability),
                outcome = "indeterminate",
                disposition = "retired",
                resolver = "local_maintenance_operator",
                replayed = false,
                "indeterminate operation retired offline without replay; quarantine cleared"
            );
            println!(
                "retired operation={} device={} generation={} authorized_generation={} outcome=indeterminate replayed=false",
                result.retirement.operation.operation_id,
                result.retirement.operation.device_id,
                result.retirement.operation.device_generation,
                result.retirement.authorized_device_generation,
            );
        }
        Command::InspectQuarantine {
            state_dir,
            device_id,
        } => {
            let report = inspect_quarantines_read_only(&state_dir, device_id.as_deref())
                .context("read-only quarantine inspection failed")?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        Command::InspectReconciliationHistory {
            state_dir,
            device_id,
        } => {
            let report = inspect_auto_resolutions_read_only(&state_dir, device_id.as_deref())
                .context("read-only reconciliation history inspection failed")?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        Command::AuditReconciliation {
            state_dir,
            agent_state_dir,
            operation_id,
        } => {
            let report =
                audit_reconciliation_read_only(&state_dir, &agent_state_dir, &operation_id)
                    .context("read-only reconciliation readiness audit failed")?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        Command::MutationAuthorityInit {
            authority_dir,
            owner,
        } => {
            let status = initialize_mutation_authority(&authority_dir, owner.into())
                .context("shared mutation authority initialization failed")?;
            println!("{}", serde_json::to_string_pretty(&status)?);
        }
        Command::MutationAuthorityStatus { authority_dir } => {
            let status = inspect_mutation_authority(&authority_dir)
                .context("shared mutation authority inspection failed")?;
            println!("{}", serde_json::to_string_pretty(&status)?);
        }
        Command::MutationAuthoritySwitch {
            authority_dir,
            from,
            to,
            hub_state_dir,
            handoff_control_socket,
        } => {
            let quarantine = inspect_quarantines_read_only(&hub_state_dir, None)
                .context("V2 quarantine preflight for mutation authority switch failed")?;
            ensure!(
                quarantine.quarantines.is_empty(),
                "mutation authority switch refused while V2 quarantine is non-empty"
            );
            let status =
                switch_mutation_authority_guarded(&authority_dir, from.into(), to.into(), || {
                    handoff_is_idle(&handoff_control_socket)
                })
                .context(
                    "shared mutation authority switch failed or Handoff was not provably idle",
                )?;
            println!("{}", serde_json::to_string_pretty(&status)?);
        }
        Command::CompareQuarantineRequest {
            state_dir,
            operation_id,
            tool,
            request_file,
            fingerprint_secret_file,
        } => {
            let request_text = load_secret_text(&request_file, MAX_CANDIDATE_REQUEST_BYTES)
                .context("failed to load private candidate request file")?;
            let candidate_request = serde_json::from_str(&request_text)
                .context("candidate request file must contain one JSON object")?;
            let fingerprint_secret =
                load_secret_text(&fingerprint_secret_file, MAX_AUDIT_FINGERPRINT_SECRET_BYTES)
                    .context("failed to load audit fingerprint secret")?;
            ensure!(
                fingerprint_secret.len() >= MIN_AUDIT_FINGERPRINT_SECRET_BYTES,
                "audit fingerprint secret must contain at least 32 bytes"
            );
            let comparison = compare_quarantined_request_read_only(
                &state_dir,
                &operation_id,
                tool.as_str(),
                candidate_request,
                fingerprint_secret.as_bytes(),
            )
            .context("read-only quarantined-request comparison failed")?;
            println!("{}", serde_json::to_string(&comparison)?);
        }
    }
    Ok(())
}
