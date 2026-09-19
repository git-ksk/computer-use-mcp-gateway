use anyhow::{Context, Result, bail};
#[path = "v2_recover/linux_fido2.rs"]
mod linux_fido2;
use linux_fido2::{
    LinuxFido2ProviderArgs, accept_current_state_linux_fido2, init_linux_fido2,
    resolve_linux_fido2, resume_mutations_linux_fido2,
};

use clap::{Parser, Subcommand, ValueEnum};
#[cfg(any(target_os = "macos", windows))]
use computer_use_mcp_gateway::v2_online_recovery::{
    RecoveryAuditAssessment, new_authorization, new_current_state_acceptance_authorization,
    new_mutation_resume_authorization, recovery_decision_name, store_authorization,
};
use computer_use_mcp_gateway::{
    v2_execution_safety::RetirementPolicy,
    v2_guided_recovery::{
        GuidedRecoveryDisposition, GuidedRecoveryPlan, GuidedRecoveryPostDisposition,
        classify_guided_recovery_post_status, compose_guided_recovery_plan,
        decision_name as guided_decision_name, revalidate_guided_current_state_acceptance,
        revalidate_guided_human_historical_selection, revalidate_guided_recovery_selection,
    },
    v2_incident_brief::{build_incident_brief_read_only, render_incident_brief_text},
    v2_m0_execution::IndeterminateResolution,
    v2_m1_keys::{load_secret_text, load_verifying_key},
    v2_maintenance::{ReconciliationSupportedDecision, inspect_quarantines_read_only},
    v2_online_recovery::{
        RecoveryChallenge, RecoveryDecision, RecoveryPhase, RecoveryResolved, load_challenge,
        load_recovery_resolved, verify_recovery_challenge, verify_recovery_resolved,
    },
    v2_operator_status::OperatorOverallStatus,
};
#[cfg(any(target_os = "macos", windows))]
use std::fs::OpenOptions;
use std::io::{IsTerminal as _, Write as _};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Parser)]
#[command(name = "cumg-v2-recover")]
#[command(about = "Local-user approval for V2 online quarantine recovery")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Create a new macOS Secure Enclave recovery key and export only its public key.
    InitKey {
        #[arg(long, env = "CUMG_V2_RECOVERY_KEY_FILE")]
        key_file: PathBuf,
        #[arg(long, env = "CUMG_V2_RECOVERY_HELPER")]
        secure_enclave_helper: PathBuf,
        #[arg(long)]
        public_key_out: PathBuf,
    },
    /// Export the public key for an already provisioned Secure Enclave sealed key.
    ExportPublic {
        #[arg(long, env = "CUMG_V2_RECOVERY_KEY_FILE")]
        key_file: PathBuf,
        #[arg(long, env = "CUMG_V2_RECOVERY_HELPER")]
        secure_enclave_helper: PathBuf,
        #[arg(long)]
        public_key_out: PathBuf,
    },
    /// Create a dedicated Windows Hello recovery credential and export only its public verifier.
    InitWindowsHello {
        #[arg(long)]
        verifier_out: PathBuf,
    },
    /// Approve an exact recovery decision with the provisioned Windows Hello credential.
    ResolveWindowsHello {
        #[arg(long, env = "CUMG_V2_STATE_DIR")]
        state_dir: PathBuf,
        #[arg(long, env = "CUMG_V2_HUB_PUBLIC_KEY_FILE")]
        hub_public_key_file: PathBuf,
        #[arg(long, env = "CUMG_V2_RECOVERY_WEBAUTHN_VERIFIER_FILE")]
        verifier_file: PathBuf,
        #[arg(long, value_enum)]
        decision: ResolutionDecisionArg,
        #[arg(long)]
        evidence: String,
        #[arg(long, default_value_t = 0, value_parser = clap::value_parser!(u64).range(0..=120))]
        wait_secs: u64,
    },
    /// Accept the current desktop state using Windows Hello user verification.
    AcceptCurrentStateWindowsHello {
        #[arg(long, env = "CUMG_V2_STATE_DIR")]
        state_dir: PathBuf,
        #[arg(long, env = "CUMG_V2_HUB_PUBLIC_KEY_FILE")]
        hub_public_key_file: PathBuf,
        #[arg(long, env = "CUMG_V2_RECOVERY_WEBAUTHN_VERIFIER_FILE")]
        verifier_file: PathBuf,
        #[arg(long)]
        evidence: String,
        #[arg(long, default_value_t = 0, value_parser = clap::value_parser!(u64).range(0..=120))]
        wait_secs: u64,
    },
    /// Create a dedicated Linux FIDO2 recovery credential with explicit user verification.
    InitLinuxFido2 {
        #[arg(long, env = "CUMG_V2_FIDO2_TOOL_DIR")]
        tool_dir: PathBuf,
        #[arg(long, env = "CUMG_V2_FIDO2_DEVICE")]
        device: PathBuf,
        #[arg(long, value_enum)]
        uv_mode: LinuxFido2UvModeArg,
        #[arg(long)]
        verifier_out: PathBuf,
    },
    /// Approve an exact recovery decision with a provisioned Linux FIDO2 credential.
    ResolveLinuxFido2 {
        #[arg(long, env = "CUMG_V2_STATE_DIR")]
        state_dir: PathBuf,
        #[arg(long, env = "CUMG_V2_HUB_PUBLIC_KEY_FILE")]
        hub_public_key_file: PathBuf,
        #[arg(long, env = "CUMG_V2_FIDO2_TOOL_DIR")]
        tool_dir: PathBuf,
        #[arg(long, env = "CUMG_V2_FIDO2_DEVICE")]
        device: PathBuf,
        #[arg(long, value_enum)]
        uv_mode: LinuxFido2UvModeArg,
        #[arg(long, env = "CUMG_V2_RECOVERY_WEBAUTHN_VERIFIER_FILE")]
        verifier_file: PathBuf,
        #[arg(long, value_enum)]
        decision: ResolutionDecisionArg,
        #[arg(long)]
        evidence: String,
        #[arg(long, default_value_t = 0, value_parser = clap::value_parser!(u64).range(0..=120))]
        wait_secs: u64,
    },
    /// Accept the current desktop state with a provisioned Linux FIDO2 credential.
    AcceptCurrentStateLinuxFido2 {
        #[arg(long, env = "CUMG_V2_STATE_DIR")]
        state_dir: PathBuf,
        #[arg(long, env = "CUMG_V2_HUB_PUBLIC_KEY_FILE")]
        hub_public_key_file: PathBuf,
        #[arg(long, env = "CUMG_V2_FIDO2_TOOL_DIR")]
        tool_dir: PathBuf,
        #[arg(long, env = "CUMG_V2_FIDO2_DEVICE")]
        device: PathBuf,
        #[arg(long, value_enum)]
        uv_mode: LinuxFido2UvModeArg,
        #[arg(long, env = "CUMG_V2_RECOVERY_WEBAUTHN_VERIFIER_FILE")]
        verifier_file: PathBuf,
        /// Reviewed continuation policy. PointerClick requires the dedicated policy and a later resume step.
        #[arg(long, value_enum, default_value = "transient-ui-interaction-v1")]
        policy: CurrentStatePolicyArg,
        #[arg(long)]
        evidence: String,
        #[arg(long, default_value_t = 0, value_parser = clap::value_parser!(u64).range(0..=120))]
        wait_secs: u64,
    },
    /// Re-arm fresh effectful admission with the provisioned Linux FIDO2 credential.
    ResumeMutationsLinuxFido2 {
        #[arg(long, env = "CUMG_V2_STATE_DIR")]
        state_dir: PathBuf,
        #[arg(long, env = "CUMG_V2_HUB_PUBLIC_KEY_FILE")]
        hub_public_key_file: PathBuf,
        #[arg(long, env = "CUMG_V2_FIDO2_TOOL_DIR")]
        tool_dir: PathBuf,
        #[arg(long, env = "CUMG_V2_FIDO2_DEVICE")]
        device: PathBuf,
        #[arg(long, value_enum)]
        uv_mode: LinuxFido2UvModeArg,
        #[arg(long, env = "CUMG_V2_RECOVERY_WEBAUTHN_VERIFIER_FILE")]
        verifier_file: PathBuf,
        #[arg(long)]
        evidence: String,
        #[arg(long, default_value_t = 0, value_parser = clap::value_parser!(u64).range(0..=120))]
        wait_secs: u64,
    },
    /// Guide a Human through authoritative quarantine review and durable recovery verification.
    Guide {
        #[arg(long)]
        hub_state_dir: PathBuf,
        #[arg(long, env = "CUMG_V2_STATE_DIR")]
        agent_state_dir: PathBuf,
        #[arg(long, env = "CUMG_V2_HUB_PUBLIC_KEY_FILE")]
        hub_public_key_file: PathBuf,
        #[arg(long, env = "CUMG_V2_RECOVERY_KEY_FILE")]
        key_file: Option<PathBuf>,
        #[arg(long, env = "CUMG_V2_RECOVERY_HELPER")]
        secure_enclave_helper: Option<PathBuf>,
        #[arg(long, env = "CUMG_MUTATION_AUTHORITY_DIR")]
        mutation_authority_dir: Option<PathBuf>,
        /// Optional bounded #233 diagnostics JSON. Observations never widen supported decisions.
        #[arg(long)]
        diagnostics_file: Option<PathBuf>,
        /// Root forwarded only to the existing v2_status post-recovery verification.
        #[arg(long, env = "CUMG_V2_INSTALL_ROOT")]
        install_root: Option<PathBuf>,
        /// Runtime root forwarded only to the existing v2_status post-recovery verification.
        #[arg(long, env = "CUMG_V2_RUN_ROOT")]
        run_root: Option<PathBuf>,
        /// Wait for the exact Hub-signed durable acknowledgement; guided recovery never uses zero.
        #[arg(long, default_value_t = 60, value_parser = clap::value_parser!(u64).range(1..=120))]
        wait_secs: u64,
        /// Emit a privacy-bounded read-only plan. JSON mode never prompts, signs, or publishes.
        #[arg(long)]
        json: bool,
    },
    /// Show the current Hub-signed recovery challenge without resolving it.
    Status {
        #[arg(long, env = "CUMG_V2_STATE_DIR")]
        state_dir: PathBuf,
        #[arg(long, env = "CUMG_V2_HUB_PUBLIC_KEY_FILE")]
        hub_public_key_file: PathBuf,
    },
    /// Sign an exact local-user resolution decision. Signing requires OS user presence on macOS.
    Resolve {
        #[arg(long, env = "CUMG_V2_STATE_DIR")]
        state_dir: PathBuf,
        #[arg(long, env = "CUMG_V2_HUB_PUBLIC_KEY_FILE")]
        hub_public_key_file: PathBuf,
        #[arg(long, env = "CUMG_V2_RECOVERY_KEY_FILE")]
        key_file: PathBuf,
        #[arg(long, env = "CUMG_V2_RECOVERY_HELPER")]
        secure_enclave_helper: PathBuf,
        #[arg(long, value_enum)]
        decision: ResolutionDecisionArg,
        /// Short metadata describing what the local user inspected. Do not include secrets or screenshots.
        #[arg(long)]
        evidence: String,
        /// Wait up to this many seconds for the exact signed Hub durable-completion acknowledgement.
        #[arg(long, default_value_t = 0, value_parser = clap::value_parser!(u64).range(0..=120))]
        wait_secs: u64,
    },
    /// Accept the current desktop state as the continuation point without claiming a historical outcome.
    AcceptCurrentState {
        #[arg(long, env = "CUMG_V2_STATE_DIR")]
        state_dir: PathBuf,
        #[arg(long, env = "CUMG_V2_HUB_PUBLIC_KEY_FILE")]
        hub_public_key_file: PathBuf,
        #[arg(long, env = "CUMG_V2_RECOVERY_KEY_FILE")]
        key_file: PathBuf,
        #[arg(long, env = "CUMG_V2_RECOVERY_HELPER")]
        secure_enclave_helper: PathBuf,
        /// Reviewed continuation policy. PointerClick requires its dedicated policy and a later resume-mutations approval.
        #[arg(long, value_enum, default_value = "transient-ui-interaction-v1")]
        policy: CurrentStatePolicyArg,
        /// Short metadata stating what the local user inspected. Never include a screenshot or secret.
        #[arg(long)]
        evidence: String,
        /// Wait up to this many seconds for the exact signed Hub durable-completion acknowledgement.
        #[arg(long, default_value_t = 0, value_parser = clap::value_parser!(u64).range(0..=120))]
        wait_secs: u64,
    },
    /// Re-arm fresh effectful admission after an acknowledged unknown PointerClick and generation rollover.
    ResumeMutations {
        #[arg(long, env = "CUMG_V2_STATE_DIR")]
        state_dir: PathBuf,
        #[arg(long, env = "CUMG_V2_HUB_PUBLIC_KEY_FILE")]
        hub_public_key_file: PathBuf,
        #[arg(long, env = "CUMG_V2_RECOVERY_KEY_FILE")]
        key_file: PathBuf,
        #[arg(long, env = "CUMG_V2_RECOVERY_HELPER")]
        secure_enclave_helper: PathBuf,
        /// Short metadata acknowledging that only fresh mutations will be admitted after this re-arm.
        #[arg(long)]
        evidence: String,
        #[arg(long, default_value_t = 0, value_parser = clap::value_parser!(u64).range(0..=120))]
        wait_secs: u64,
    },
    /// Verify the exact signed Hub durable-completion acknowledgement for a published request.
    Confirm {
        #[arg(long, env = "CUMG_V2_STATE_DIR")]
        state_dir: PathBuf,
        #[arg(long, env = "CUMG_V2_HUB_PUBLIC_KEY_FILE")]
        hub_public_key_file: PathBuf,
        #[arg(long)]
        request_id: String,
        #[arg(long)]
        device_id: String,
        #[arg(long)]
        operation_id: String,
        #[arg(long)]
        current_generation: u64,
        #[arg(long, value_enum)]
        decision: RecoveryDecisionArg,
        /// Required for current-state-accepted confirmation so the exact policy is signature-bound.
        #[arg(long, value_enum)]
        policy: Option<CurrentStatePolicyArg>,
        #[arg(long, default_value_t = 0, value_parser = clap::value_parser!(u64).range(0..=120))]
        wait_secs: u64,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum LinuxFido2UvModeArg {
    Pin,
    Builtin,
}

impl From<LinuxFido2UvModeArg>
    for computer_use_mcp_gateway::v2_linux_fido2_recovery::LinuxFido2UvMode
{
    fn from(value: LinuxFido2UvModeArg) -> Self {
        match value {
            LinuxFido2UvModeArg::Pin => Self::Pin,
            LinuxFido2UvModeArg::Builtin => Self::Builtin,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum ResolutionDecisionArg {
    ConfirmedCompleted,
    ConfirmedNotExecuted,
}

impl From<ResolutionDecisionArg> for IndeterminateResolution {
    fn from(value: ResolutionDecisionArg) -> Self {
        match value {
            ResolutionDecisionArg::ConfirmedCompleted => Self::ConfirmedCompleted,
            ResolutionDecisionArg::ConfirmedNotExecuted => Self::ConfirmedNotExecuted,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum RecoveryDecisionArg {
    ConfirmedCompleted,
    ConfirmedNotExecuted,
    CurrentStateAccepted,
    MutationsResumed,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum CurrentStatePolicyArg {
    TransientUiInteractionV1,
    AcknowledgedUnknownPointerClickV1,
}

impl From<CurrentStatePolicyArg> for RetirementPolicy {
    fn from(value: CurrentStatePolicyArg) -> Self {
        match value {
            CurrentStatePolicyArg::TransientUiInteractionV1 => {
                RetirementPolicy::TransientUiInteractionV1
            }
            CurrentStatePolicyArg::AcknowledgedUnknownPointerClickV1 => {
                RetirementPolicy::AcknowledgedUnknownPointerClickV1
            }
        }
    }
}

impl RecoveryDecisionArg {
    fn decision(self) -> RecoveryDecision {
        match self {
            Self::ConfirmedCompleted => RecoveryDecision::ConfirmedCompleted,
            Self::ConfirmedNotExecuted => RecoveryDecision::ConfirmedNotExecuted,
            Self::CurrentStateAccepted => RecoveryDecision::CurrentStateAccepted,
            Self::MutationsResumed => RecoveryDecision::MutationsResumed,
        }
    }

    fn phase(self) -> RecoveryPhase {
        match self {
            Self::MutationsResumed => RecoveryPhase::MutationResume,
            Self::ConfirmedCompleted | Self::ConfirmedNotExecuted | Self::CurrentStateAccepted => {
                RecoveryPhase::QuarantineResolution
            }
        }
    }
}

const MAX_GUIDED_DIAGNOSTICS_BYTES: u64 = 64 * 1024;
#[cfg(target_os = "macos")]
const GUIDED_RECOVERY_EVIDENCE: &str = "guided_recovery_authoritative_incident_review_v1";
#[cfg(target_os = "macos")]
const GUIDED_HUMAN_HISTORICAL_EVIDENCE: &str = "guided_recovery_human_historical_assertion_v1";
#[cfg(target_os = "macos")]
const GUIDED_CURRENT_STATE_EVIDENCE: &str = "guided_recovery_current_state_accepted_v1";
const GUIDED_MUTATION_RESUME_EVIDENCE: &str = "guided_recovery_mutation_resume_v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GuidedRecoverySelection {
    Authoritative(ReconciliationSupportedDecision),
    HumanHistorical(ReconciliationSupportedDecision),
    CurrentStateAccepted(RetirementPolicy),
}

impl GuidedRecoverySelection {
    #[cfg(target_os = "macos")]
    const fn authority_name(self) -> &'static str {
        match self {
            Self::Authoritative(_) => "authoritative_reconciliation",
            Self::HumanHistorical(_) => "human_historical_assertion",
            Self::CurrentStateAccepted(_) => "local_user_current_state_acceptance",
        }
    }

    #[cfg(target_os = "macos")]
    const fn selection_name(self) -> &'static str {
        match self {
            Self::Authoritative(ReconciliationSupportedDecision::ConfirmedCompleted) => {
                "confirmed_completed"
            }
            Self::Authoritative(ReconciliationSupportedDecision::ConfirmedNotExecuted) => {
                "confirmed_not_executed"
            }
            Self::HumanHistorical(ReconciliationSupportedDecision::ConfirmedCompleted) => {
                "human_confirmed_completed"
            }
            Self::HumanHistorical(ReconciliationSupportedDecision::ConfirmedNotExecuted) => {
                "human_confirmed_not_executed"
            }
            Self::CurrentStateAccepted(_) => "current_state_accepted_historical_outcome_unknown",
        }
    }
}

#[derive(Debug)]
struct GuidedRecoveryArgs {
    hub_state_dir: PathBuf,
    agent_state_dir: PathBuf,
    hub_public_key_file: PathBuf,
    key_file: Option<PathBuf>,
    secure_enclave_helper: Option<PathBuf>,
    mutation_authority_dir: Option<PathBuf>,
    diagnostics_file: Option<PathBuf>,
    install_root: Option<PathBuf>,
    run_root: Option<PathBuf>,
    wait_secs: u64,
    json: bool,
}

#[derive(Debug)]
struct GuidedRecoveryReview {
    brief: computer_use_mcp_gateway::v2_incident_brief::IncidentBrief,
    challenge: RecoveryChallenge,
    plan: GuidedRecoveryPlan,
}

#[derive(Debug)]
struct PostRecoveryStatus {
    overall: OperatorOverallStatus,
    primary_reason: String,
    quarantine: String,
    recovery_mode: String,
    handoff: String,
    mutation_authority: String,
    runtime: String,
}

#[derive(Debug, Clone)]
struct ExpectedRecoveryCompletion {
    phase: RecoveryPhase,
    request_id: String,
    device_id: String,
    operation_id: String,
    current_generation: u64,
    decision: RecoveryDecision,
    current_state_policy: Option<RetirementPolicy>,
}

fn now_ms() -> Result<u64> {
    Ok(u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .context("system clock is before Unix epoch")?
            .as_millis(),
    )
    .unwrap_or(u64::MAX))
}

fn verified_challenge(
    state_dir: &Path,
    hub_public_key_file: &Path,
) -> Result<computer_use_mcp_gateway::v2_online_recovery::RecoveryChallenge> {
    let challenge = load_challenge(state_dir)
        .context("failed to read recovery challenge")?
        .context("no active recovery challenge")?;
    let trusted_hub =
        load_verifying_key(hub_public_key_file).context("failed to load pinned Hub public key")?;
    verify_recovery_challenge(
        &challenge,
        &trusted_hub,
        &challenge.device_id,
        challenge.current_generation,
        now_ms()?,
    )
    .context("recovery challenge is stale or invalid")?;
    Ok(challenge)
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::InitKey {
            key_file,
            secure_enclave_helper,
            public_key_out,
        } => init_key(key_file, secure_enclave_helper, public_key_out),
        Command::ExportPublic {
            key_file,
            secure_enclave_helper,
            public_key_out,
        } => export_public(key_file, secure_enclave_helper, public_key_out),
        Command::InitWindowsHello { verifier_out } => init_windows_hello(verifier_out),
        Command::ResolveWindowsHello {
            state_dir,
            hub_public_key_file,
            verifier_file,
            decision,
            evidence,
            wait_secs,
        } => resolve_windows_hello(
            state_dir,
            hub_public_key_file,
            verifier_file,
            decision.into(),
            evidence,
            wait_secs,
        ),
        Command::AcceptCurrentStateWindowsHello {
            state_dir,
            hub_public_key_file,
            verifier_file,
            evidence,
            wait_secs,
        } => accept_current_state_windows_hello(
            state_dir,
            hub_public_key_file,
            verifier_file,
            evidence,
            wait_secs,
        ),
        Command::InitLinuxFido2 {
            tool_dir,
            device,
            uv_mode,
            verifier_out,
        } => init_linux_fido2(tool_dir, device, uv_mode.into(), verifier_out),
        Command::ResolveLinuxFido2 {
            state_dir,
            hub_public_key_file,
            tool_dir,
            device,
            uv_mode,
            verifier_file,
            decision,
            evidence,
            wait_secs,
        } => resolve_linux_fido2(
            state_dir,
            hub_public_key_file,
            LinuxFido2ProviderArgs {
                tool_dir,
                device,
                uv_mode: uv_mode.into(),
                verifier_file,
            },
            decision.into(),
            evidence,
            wait_secs,
        ),
        Command::AcceptCurrentStateLinuxFido2 {
            state_dir,
            hub_public_key_file,
            tool_dir,
            device,
            uv_mode,
            verifier_file,
            policy,
            evidence,
            wait_secs,
        } => accept_current_state_linux_fido2(
            state_dir,
            hub_public_key_file,
            LinuxFido2ProviderArgs {
                tool_dir,
                device,
                uv_mode: uv_mode.into(),
                verifier_file,
            },
            policy.into(),
            evidence,
            wait_secs,
        ),
        Command::ResumeMutationsLinuxFido2 {
            state_dir,
            hub_public_key_file,
            tool_dir,
            device,
            uv_mode,
            verifier_file,
            evidence,
            wait_secs,
        } => resume_mutations_linux_fido2(
            state_dir,
            hub_public_key_file,
            LinuxFido2ProviderArgs {
                tool_dir,
                device,
                uv_mode: uv_mode.into(),
                verifier_file,
            },
            evidence,
            wait_secs,
        ),
        Command::Guide {
            hub_state_dir,
            agent_state_dir,
            hub_public_key_file,
            key_file,
            secure_enclave_helper,
            mutation_authority_dir,
            diagnostics_file,
            install_root,
            run_root,
            wait_secs,
            json,
        } => guide(GuidedRecoveryArgs {
            hub_state_dir,
            agent_state_dir,
            hub_public_key_file,
            key_file,
            secure_enclave_helper,
            mutation_authority_dir,
            diagnostics_file,
            install_root,
            run_root,
            wait_secs,
            json,
        }),
        Command::Status {
            state_dir,
            hub_public_key_file,
        } => {
            let challenge = verified_challenge(&state_dir, &hub_public_key_file)?;
            println!(
                "recovery_phase={}",
                computer_use_mcp_gateway::v2_online_recovery::recovery_phase_name(challenge.phase)
            );
            println!("device_id={}", challenge.device_id);
            println!("operation_id={}", challenge.operation_id);
            println!("quarantine_generation={}", challenge.quarantine_generation);
            println!("current_generation={}", challenge.current_generation);
            println!("audit_assessment=inconclusive");
            println!("expires_at_ms={}", challenge.expires_at_ms);
            Ok(())
        }
        Command::Resolve {
            state_dir,
            hub_public_key_file,
            key_file,
            secure_enclave_helper,
            decision,
            evidence,
            wait_secs,
        } => resolve(
            state_dir,
            hub_public_key_file,
            key_file,
            secure_enclave_helper,
            decision.into(),
            evidence,
            wait_secs,
        ),
        Command::AcceptCurrentState {
            state_dir,
            hub_public_key_file,
            key_file,
            secure_enclave_helper,
            policy,
            evidence,
            wait_secs,
        } => accept_current_state(
            state_dir,
            hub_public_key_file,
            key_file,
            secure_enclave_helper,
            policy.into(),
            evidence,
            wait_secs,
        ),
        Command::ResumeMutations {
            state_dir,
            hub_public_key_file,
            key_file,
            secure_enclave_helper,
            evidence,
            wait_secs,
        } => resume_mutations(
            state_dir,
            hub_public_key_file,
            key_file,
            secure_enclave_helper,
            evidence,
            wait_secs,
        ),
        Command::Confirm {
            state_dir,
            hub_public_key_file,
            request_id,
            device_id,
            operation_id,
            current_generation,
            decision,
            policy,
            wait_secs,
        } => {
            let current_state_policy = match decision {
                RecoveryDecisionArg::CurrentStateAccepted => Some(
                    policy
                        .context("--policy is required when confirming current-state-accepted")?
                        .into(),
                ),
                RecoveryDecisionArg::ConfirmedCompleted
                | RecoveryDecisionArg::ConfirmedNotExecuted
                | RecoveryDecisionArg::MutationsResumed => {
                    if policy.is_some() {
                        anyhow::bail!("--policy is valid only with current-state-accepted");
                    }
                    None
                }
            };
            wait_for_completion(
                &state_dir,
                &hub_public_key_file,
                &ExpectedRecoveryCompletion {
                    phase: decision.phase(),
                    request_id,
                    device_id,
                    operation_id,
                    current_generation,
                    decision: decision.decision(),
                    current_state_policy,
                },
                wait_secs,
            )
        }
    }
}

fn load_guided_review(args: &GuidedRecoveryArgs) -> Result<GuidedRecoveryReview> {
    let before = verified_challenge(&args.agent_state_dir, &args.hub_public_key_file)?;
    let diagnostics = args
        .diagnostics_file
        .as_deref()
        .map(|path| {
            load_secret_text(path, MAX_GUIDED_DIAGNOSTICS_BYTES)
                .context("failed to load private bounded incident diagnostics")
        })
        .transpose()?;
    let brief = build_incident_brief_read_only(
        &args.hub_state_dir,
        &args.agent_state_dir,
        &before.operation_id,
        args.mutation_authority_dir.as_deref(),
        diagnostics.as_deref(),
    )
    .context("guided recovery incident inspection failed")?;
    let after = verified_challenge(&args.agent_state_dir, &args.hub_public_key_file)?;
    if before != after {
        anyhow::bail!("recovery state changed during inspection; re-run guided review");
    }
    let plan = compose_guided_recovery_plan(&brief, &after);
    Ok(GuidedRecoveryReview {
        brief,
        challenge: after,
        plan,
    })
}

fn prompt_authoritative_guided_decision(
    plan: &GuidedRecoveryPlan,
) -> Result<Option<GuidedRecoverySelection>> {
    let stdin = std::io::stdin();
    if !stdin.is_terminal() {
        anyhow::bail!(
            "authority-bearing guided recovery requires an interactive Human terminal; use --json for read-only planning"
        );
    }
    println!("\nHuman decision required");
    println!("  0) Keep quarantine / cancel");
    for (index, decision) in plan.supported_decisions.iter().enumerate() {
        println!("  {}) {}", index + 1, guided_decision_name(*decision));
    }
    loop {
        print!("Select a CUMG-supported decision: ");
        std::io::stdout().flush()?;
        let mut input = String::new();
        if stdin.read_line(&mut input)? == 0 {
            return Ok(None);
        }
        let input = input.trim();
        if matches!(input, "0" | "q" | "quit" | "cancel") {
            return Ok(None);
        }
        let Ok(index) = input.parse::<usize>() else {
            println!("Invalid selection; choose one listed number or 0 to keep quarantine.");
            continue;
        };
        if let Some(decision) = index
            .checked_sub(1)
            .and_then(|offset| plan.supported_decisions.get(offset))
        {
            return Ok(Some(GuidedRecoverySelection::Authoritative(*decision)));
        }
        println!("Unsupported selection; the authoritative decision set was not widened.");
    }
}

fn prompt_human_historical_assertion(
    plan: &GuidedRecoveryPlan,
) -> Result<Option<GuidedRecoverySelection>> {
    let stdin = std::io::stdin();
    if !stdin.is_terminal() {
        anyhow::bail!(
            "Human historical assertion requires an interactive Human terminal; use --json for read-only planning"
        );
    }
    if !plan.human_historical_assertion.available
        || plan.human_historical_assertion.automatic_selection_allowed
        || !plan.supported_decisions.is_empty()
    {
        anyhow::bail!("Human historical assertion is not available for this reviewed plan");
    }

    println!("\nCUMG cannot determine the historical outcome from authoritative evidence.");
    println!(
        "Do not guess. Choose a historical assertion only if you personally observed this exact operation."
    );
    println!("  0) Keep quarantine / cancel");
    println!("  1) I directly observed this exact operation complete");
    println!("  2) I directly observed this exact operation did not execute");
    if plan.current_state_acceptance.available {
        println!(
            "  3) Historical outcome remains unknown; accept the current desktop state without replay"
        );
    }
    loop {
        print!("Select a Human historical assertion: ");
        std::io::stdout().flush()?;
        let mut input = String::new();
        if stdin.read_line(&mut input)? == 0 {
            return Ok(None);
        }
        match input.trim() {
            "0" | "q" | "quit" | "cancel" => return Ok(None),
            "1" => {
                return Ok(Some(GuidedRecoverySelection::HumanHistorical(
                    ReconciliationSupportedDecision::ConfirmedCompleted,
                )));
            }
            "2" => {
                return Ok(Some(GuidedRecoverySelection::HumanHistorical(
                    ReconciliationSupportedDecision::ConfirmedNotExecuted,
                )));
            }
            "3" if plan.current_state_acceptance.available => {
                let policy = plan
                    .current_state_acceptance
                    .policy
                    .context("guided current-state policy is missing")?;
                return Ok(Some(GuidedRecoverySelection::CurrentStateAccepted(policy)));
            }
            _ => println!("Invalid selection; choose a listed option or 0 to keep quarantine."),
        }
    }
}

fn guide(args: GuidedRecoveryArgs) -> Result<()> {
    let current_challenge = verified_challenge(&args.agent_state_dir, &args.hub_public_key_file)?;
    if current_challenge.phase == RecoveryPhase::MutationResume {
        if args.json {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "schema_version": 1,
                    "phase": "mutation_resume",
                    "device_id": current_challenge.device_id,
                    "source_operation_id": current_challenge.operation_id,
                    "current_generation": current_challenge.current_generation,
                    "historical_execution_outcome": "indeterminate",
                    "old_operation_replayed": false,
                    "user_presence_required": true,
                    "next_action": "resume_mutations"
                }))?
            );
            return Ok(());
        }
        let stdin = std::io::stdin();
        if !stdin.is_terminal() {
            anyhow::bail!("mutation resume requires an interactive Human terminal");
        }
        println!("Mutation resume required");
        println!("  Historical PointerClick outcome remains unknown.");
        println!("  The old operation will not be replayed.");
        println!("  0) Keep fresh mutations fenced");
        println!("  1) Re-arm only fresh effectful operations in this new generation");
        print!("Select: ");
        std::io::stdout().flush()?;
        let mut input = String::new();
        if stdin.read_line(&mut input)? == 0 || input.trim() != "1" {
            println!("guided_outcome=mutation_resume_deferred");
            println!("fresh_mutations=fenced");
            return Ok(());
        }
        let key_file = args
            .key_file
            .clone()
            .context("--key-file is required for interactive guided recovery")?;
        let secure_enclave_helper = args
            .secure_enclave_helper
            .clone()
            .context("--secure-enclave-helper is required for interactive guided recovery")?;
        return resume_mutations(
            args.agent_state_dir.clone(),
            args.hub_public_key_file.clone(),
            key_file,
            secure_enclave_helper,
            GUIDED_MUTATION_RESUME_EVIDENCE.into(),
            args.wait_secs,
        );
    }
    let reviewed = load_guided_review(&args)?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&reviewed.plan)?);
        return Ok(());
    }

    println!("{}", render_incident_brief_text(&reviewed.brief));
    println!("\nGuided recovery");
    println!("  operation_id={}", reviewed.plan.operation.operation_id);
    println!("  device_id={}", reviewed.plan.operation.device_id);
    println!(
        "  original_generation={}",
        reviewed.plan.operation.original_generation
    );
    println!(
        "  current_generation={}",
        reviewed
            .plan
            .operation
            .current_generation
            .map_or_else(|| "unknown".to_owned(), |value| value.to_string())
    );
    println!(
        "  old_operation_replayed={}",
        reviewed.plan.old_operation_replayed
    );

    let selected = match reviewed.plan.disposition {
        GuidedRecoveryDisposition::KeepQuarantine => {
            if reviewed.plan.human_historical_assertion.available {
                prompt_human_historical_assertion(&reviewed.plan)?
            } else {
                None
            }
        }
        GuidedRecoveryDisposition::Reinspect => {
            println!("guided_outcome=re_review_required");
            println!("authorization=not_published");
            println!("durable_completion=not_verified");
            anyhow::bail!("exact recovery binding does not match the reviewed quarantine");
        }
        GuidedRecoveryDisposition::HumanSelectionRequired => {
            prompt_authoritative_guided_decision(&reviewed.plan)?
        }
    };

    let Some(selected) = selected else {
        println!("guided_outcome=keep_quarantine");
        println!("human_selection=unknown_or_cancelled");
        println!("authorization=not_published");
        println!("durable_completion=not_verified");
        return Ok(());
    };

    // Re-inspect the exact challenge and #233 brief after Human review. The
    // helper below samples the signed challenge both before and after the brief
    // read, so generation/fingerprint/nonce changes fail before signing.
    let fresh = load_guided_review(&args)?;
    let revalidation = match selected {
        GuidedRecoverySelection::Authoritative(decision) => {
            revalidate_guided_recovery_selection(&reviewed.plan, &fresh.plan, decision)
        }
        GuidedRecoverySelection::HumanHistorical(decision) => {
            revalidate_guided_human_historical_selection(&reviewed.plan, &fresh.plan, decision)
        }
        GuidedRecoverySelection::CurrentStateAccepted(policy) => {
            revalidate_guided_current_state_acceptance(&reviewed.plan, &fresh.plan, policy)
        }
    };
    if let Err(error) = revalidation {
        println!("guided_outcome=re_review_required");
        println!("authorization=not_published");
        println!("durable_completion=not_verified");
        anyhow::bail!("reviewed recovery state became stale before signing: {error:?}");
    }

    let key_file = args
        .key_file
        .as_deref()
        .context("--key-file is required for interactive guided recovery")?;
    let secure_enclave_helper = args
        .secure_enclave_helper
        .as_deref()
        .context("--secure-enclave-helper is required for interactive guided recovery")?;

    let expected = match publish_guided_authorization(
        &args.agent_state_dir,
        key_file,
        secure_enclave_helper,
        &fresh.challenge,
        selected,
    ) {
        Ok(expected) => expected,
        Err(error) => {
            println!("guided_outcome=authorization_not_completed");
            println!("authorization=not_published");
            println!("durable_completion=not_verified");
            println!("quarantine=retained");
            return Err(error);
        }
    };

    println!("request_id={}", expected.request_id);
    println!("operation_id={}", expected.operation_id);
    println!("authorization=published");
    let _resolved = match wait_for_completion_verified(
        &args.agent_state_dir,
        &args.hub_public_key_file,
        &expected,
        args.wait_secs,
    ) {
        Ok(resolved) => resolved,
        Err(error) => {
            println!("durable_completion=not_verified");
            println!("guided_outcome=durable_completion_incomplete");
            return Err(error);
        }
    };
    println!("durable_completion=verified");
    println!("old_operation_replayed=false");

    let exact_quarantine_cleared = exact_quarantine_cleared(&args.hub_state_dir, &expected)?;
    println!("exact_quarantine_cleared={exact_quarantine_cleared}");

    let post_status = match run_post_recovery_status(&args) {
        Ok(status) => status,
        Err(error) => {
            println!("post_recovery_status=unavailable");
            println!("recovery_outcome=durably_verified_post_verification_unavailable");
            return Err(error);
        }
    };
    println!(
        "post_recovery_status={}",
        operator_overall_name(post_status.overall)
    );
    println!("post_recovery_reason={}", post_status.primary_reason);
    println!("post_recovery_quarantine={}", post_status.quarantine);
    println!("post_recovery_recovery_mode={}", post_status.recovery_mode);
    println!("post_recovery_handoff={}", post_status.handoff);
    println!(
        "post_recovery_mutation_authority={}",
        post_status.mutation_authority
    );
    println!("post_recovery_runtime={}", post_status.runtime);

    if matches!(
        selected,
        GuidedRecoverySelection::CurrentStateAccepted(
            RetirementPolicy::AcknowledgedUnknownPointerClickV1
        )
    ) {
        if !exact_quarantine_cleared {
            anyhow::bail!("current-state acceptance was acknowledged but quarantine did not clear");
        }
        if post_status.recovery_mode != "mutation_resume_required" {
            anyhow::bail!(
                "PointerClick current-state acceptance did not enter the required mutation-resume barrier"
            );
        }
        println!("recovery_outcome=mutation_resume_required");
        println!("next_action=rerun_guide_or_resume_mutations");
        return Ok(());
    }

    let disposition = classify_guided_recovery_post_status(
        true,
        exact_quarantine_cleared,
        post_status.recovery_mode == "normal",
        post_status.overall,
    );
    match disposition {
        GuidedRecoveryPostDisposition::VerifiedHealthy => {
            println!("recovery_outcome=verified_healthy");
            Ok(())
        }
        GuidedRecoveryPostDisposition::VerifiedWithUnrelatedStatusProblem => {
            println!("recovery_outcome=verified_with_unrelated_status_problem");
            Ok(())
        }
        GuidedRecoveryPostDisposition::VerificationIncomplete => {
            println!("recovery_outcome=post_recovery_verification_incomplete");
            anyhow::bail!(
                "durable acknowledgement was verified but the exact quarantine did not clear as expected"
            )
        }
    }
}

#[cfg(target_os = "macos")]
fn publish_guided_authorization(
    state_dir: &Path,
    key_file: &Path,
    secure_enclave_helper: &Path,
    challenge: &RecoveryChallenge,
    selected: GuidedRecoverySelection,
) -> Result<ExpectedRecoveryCompletion> {
    use computer_use_mcp_gateway::v2_online_recovery::macos::MacRecoveryKey;
    let authorization = match selected {
        GuidedRecoverySelection::Authoritative(decision) => {
            let (assessment, evidence) = match decision {
                ReconciliationSupportedDecision::ConfirmedCompleted => {
                    (RecoveryAuditAssessment::Completed, GUIDED_RECOVERY_EVIDENCE)
                }
                ReconciliationSupportedDecision::ConfirmedNotExecuted => (
                    RecoveryAuditAssessment::NotExecuted,
                    GUIDED_RECOVERY_EVIDENCE,
                ),
            };
            let resolution = match decision {
                ReconciliationSupportedDecision::ConfirmedCompleted => {
                    IndeterminateResolution::ConfirmedCompleted
                }
                ReconciliationSupportedDecision::ConfirmedNotExecuted => {
                    IndeterminateResolution::ConfirmedNotExecuted
                }
            };
            new_authorization(challenge, assessment, resolution, evidence)
                .context("failed to construct exact guided recovery authorization")?
        }
        GuidedRecoverySelection::HumanHistorical(decision) => {
            let resolution = match decision {
                ReconciliationSupportedDecision::ConfirmedCompleted => {
                    IndeterminateResolution::ConfirmedCompleted
                }
                ReconciliationSupportedDecision::ConfirmedNotExecuted => {
                    IndeterminateResolution::ConfirmedNotExecuted
                }
            };
            new_authorization(
                challenge,
                RecoveryAuditAssessment::Inconclusive,
                resolution,
                GUIDED_HUMAN_HISTORICAL_EVIDENCE,
            )
            .context("failed to construct Human historical authorization")?
        }
        GuidedRecoverySelection::CurrentStateAccepted(policy) => {
            new_current_state_acceptance_authorization(
                challenge,
                policy,
                GUIDED_CURRENT_STATE_EVIDENCE,
            )
            .context("failed to construct current-state acceptance authorization")?
        }
    };
    println!("human_selection={}", selected.selection_name());
    println!("decision_authority={}", selected.authority_name());
    println!(
        "audit_assessment={}",
        match authorization.audit_assessment {
            RecoveryAuditAssessment::Completed => "completed",
            RecoveryAuditAssessment::NotExecuted => "not_executed",
            RecoveryAuditAssessment::Inconclusive => "inconclusive",
        }
    );
    println!("user_presence=required");
    let key = MacRecoveryKey::load(secure_enclave_helper, key_file)
        .context("recovery key is not provisioned")?;
    let authorization = key
        .sign_authorization(authorization)
        .context("OS user-presence approval was not completed")?;
    store_authorization(state_dir, &authorization)
        .context("failed to publish recovery authorization to Agent")?;
    Ok(ExpectedRecoveryCompletion {
        phase: authorization.phase,
        request_id: authorization.request_id,
        device_id: authorization.device_id,
        operation_id: authorization.operation_id,
        current_generation: authorization.current_generation,
        decision: authorization.decision,
        current_state_policy: authorization.current_state_policy,
    })
}

#[cfg(not(target_os = "macos"))]
fn publish_guided_authorization(
    _state_dir: &Path,
    _key_file: &Path,
    _secure_enclave_helper: &Path,
    _challenge: &RecoveryChallenge,
    _selected: GuidedRecoverySelection,
) -> Result<ExpectedRecoveryCompletion> {
    bail!("local user-presence recovery approval is supported only on macOS")
}

#[cfg(windows)]
fn init_windows_hello(verifier_out: PathBuf) -> Result<()> {
    use computer_use_mcp_gateway::v2_online_recovery::windows::WindowsHelloRecovery;
    if std::fs::symlink_metadata(&verifier_out).is_ok() {
        anyhow::bail!("refusing to overwrite {}", verifier_out.display());
    }
    let (_credential, verifier) = WindowsHelloRecovery::create_new()
        .context("Windows Hello recovery credential provisioning was not completed")?;
    let encoded = serde_json::to_vec_pretty(&verifier)?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&verifier_out)
        .with_context(|| format!("refusing to overwrite {}", verifier_out.display()))?;
    file.write_all(&encoded)?;
    file.sync_all()?;
    println!("windows_hello_recovery=provisioned");
    println!("user_verification=required");
    println!("recovery_verifier={}", verifier_out.display());
    println!("hub_filename=recovery-webauthn-verifier.json");
    Ok(())
}

#[cfg(not(windows))]
fn init_windows_hello(_verifier_out: PathBuf) -> Result<()> {
    bail!("Windows Hello recovery provisioning is supported only on Windows")
}

#[cfg(windows)]
fn load_windows_hello(
    verifier_file: &Path,
) -> Result<computer_use_mcp_gateway::v2_online_recovery::windows::WindowsHelloRecovery> {
    use computer_use_mcp_gateway::v2_online_recovery::windows::WindowsHelloRecovery;
    let metadata = std::fs::symlink_metadata(verifier_file)
        .context("failed to inspect Windows Hello recovery verifier")?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() == 0
        || metadata.len() > 4096
    {
        anyhow::bail!("Windows Hello recovery verifier is unsafe or invalid");
    }
    let bytes =
        std::fs::read(verifier_file).context("failed to read Windows Hello recovery verifier")?;
    WindowsHelloRecovery::from_verifier_document(&bytes)
        .context("Windows Hello recovery verifier is invalid")
}

#[cfg(windows)]
fn resolve_windows_hello(
    state_dir: PathBuf,
    hub_public_key_file: PathBuf,
    verifier_file: PathBuf,
    decision: IndeterminateResolution,
    evidence: String,
    wait_secs: u64,
) -> Result<()> {
    let challenge = verified_challenge(&state_dir, &hub_public_key_file)?;
    let authorization = new_authorization(
        &challenge,
        RecoveryAuditAssessment::Inconclusive,
        decision,
        evidence,
    )
    .context("invalid recovery decision")?;
    println!("approving_windows_hello_recovery");
    println!("device_id={}", authorization.device_id);
    println!("operation_id={}", authorization.operation_id);
    println!("current_generation={}", authorization.current_generation);
    println!(
        "decision={}",
        recovery_decision_name(authorization.decision)
    );
    println!("windows_hello_user_verification=required");
    let credential = load_windows_hello(&verifier_file)?;
    let authorization = credential
        .sign_authorization(authorization)
        .context("Windows Hello user verification was not completed")?;
    store_authorization(&state_dir, &authorization)
        .context("failed to publish Windows Hello recovery authorization")?;
    finish_published_authorization(&state_dir, &hub_public_key_file, &authorization, wait_secs)
}

#[cfg(not(windows))]
fn resolve_windows_hello(
    _state_dir: PathBuf,
    _hub_public_key_file: PathBuf,
    _verifier_file: PathBuf,
    _decision: IndeterminateResolution,
    _evidence: String,
    _wait_secs: u64,
) -> Result<()> {
    bail!("Windows Hello recovery approval is supported only on Windows")
}

#[cfg(windows)]
fn accept_current_state_windows_hello(
    state_dir: PathBuf,
    hub_public_key_file: PathBuf,
    verifier_file: PathBuf,
    evidence: String,
    wait_secs: u64,
) -> Result<()> {
    let challenge = verified_challenge(&state_dir, &hub_public_key_file)?;
    let authorization = new_current_state_acceptance_authorization(
        &challenge,
        RetirementPolicy::TransientUiInteractionV1,
        evidence,
    )
    .context("current-state acceptance is not valid for this recovery schema")?;
    println!("accepting_current_state_windows_hello");
    println!("device_id={}", authorization.device_id);
    println!("operation_id={}", authorization.operation_id);
    println!("historical_execution_outcome=indeterminate");
    println!("operator_observation=current_state_accepted");
    println!("old_operation_replayed=false");
    println!("windows_hello_user_verification=required");
    let credential = load_windows_hello(&verifier_file)?;
    let authorization = credential
        .sign_authorization(authorization)
        .context("Windows Hello user verification was not completed")?;
    store_authorization(&state_dir, &authorization)
        .context("failed to publish Windows Hello current-state authorization")?;
    finish_published_authorization(&state_dir, &hub_public_key_file, &authorization, wait_secs)
}

#[cfg(not(windows))]
fn accept_current_state_windows_hello(
    _state_dir: PathBuf,
    _hub_public_key_file: PathBuf,
    _verifier_file: PathBuf,
    _evidence: String,
    _wait_secs: u64,
) -> Result<()> {
    bail!("Windows Hello current-state acceptance is supported only on Windows")
}

#[cfg(windows)]
fn finish_published_authorization(
    state_dir: &Path,
    hub_public_key_file: &Path,
    authorization: &computer_use_mcp_gateway::v2_online_recovery::RecoveryAuthorization,
    wait_secs: u64,
) -> Result<()> {
    println!("request_id={}", authorization.request_id);
    println!("authorization=published");
    if wait_secs > 0 {
        wait_for_completion(
            state_dir,
            hub_public_key_file,
            &ExpectedRecoveryCompletion {
                request_id: authorization.request_id.clone(),
                device_id: authorization.device_id.clone(),
                operation_id: authorization.operation_id.clone(),
                current_generation: authorization.current_generation,
                decision: authorization.decision,
                current_state_policy: authorization.current_state_policy,
            },
            wait_secs,
        )?;
    } else {
        println!("durable_completion=not_checked");
    }
    Ok(())
}

fn exact_quarantine_cleared(
    hub_state_dir: &Path,
    expected: &ExpectedRecoveryCompletion,
) -> Result<bool> {
    let report = inspect_quarantines_read_only(hub_state_dir, Some(&expected.device_id))
        .context("post-recovery exact quarantine inspection failed")?;
    Ok(!report
        .quarantines
        .iter()
        .any(|item| item.blocking_operation_id == expected.operation_id))
}

fn run_post_recovery_status(args: &GuidedRecoveryArgs) -> Result<PostRecoveryStatus> {
    let current = std::env::current_exe().context("failed to resolve v2_recover executable")?;
    let status_exe = current.with_file_name("v2_status");
    let mut command = std::process::Command::new(status_exe);
    command
        .arg("--hub-state-dir")
        .arg(&args.hub_state_dir)
        .arg("--agent-state-dir")
        .arg(&args.agent_state_dir)
        .arg("--json");
    if let Some(path) = args.install_root.as_deref() {
        command.arg("--install-root").arg(path);
    }
    if let Some(path) = args.run_root.as_deref() {
        command.arg("--run-root").arg(path);
    }
    if let Some(path) = args.mutation_authority_dir.as_deref() {
        command.arg("--mutation-authority-dir").arg(path);
    }
    let output = command
        .output()
        .context("failed to run existing v2_status post-recovery verification")?;
    if output.stdout.len() > 256 * 1024 {
        anyhow::bail!("v2_status output exceeded the bounded guided-recovery limit");
    }
    let value: serde_json::Value = serde_json::from_slice(&output.stdout)
        .context("v2_status did not return its privacy-bounded JSON contract")?;
    let string_at = |path: &[&str]| -> Result<String> {
        let mut current = &value;
        for key in path {
            current = current
                .get(*key)
                .with_context(|| format!("v2_status JSON missing {}", path.join(".")))?;
        }
        current
            .as_str()
            .map(ToOwned::to_owned)
            .with_context(|| format!("v2_status JSON field {} is not text", path.join(".")))
    };
    let overall_text = string_at(&["overall"])?;
    let overall = parse_operator_overall(&overall_text)
        .with_context(|| format!("unsupported v2_status overall value {overall_text}"))?;
    Ok(PostRecoveryStatus {
        overall,
        primary_reason: string_at(&["primary_reason"])?,
        quarantine: string_at(&["recovery", "quarantine"])?,
        recovery_mode: string_at(&["recovery", "recovery_mode"])?,
        handoff: string_at(&["handoff", "status"])?,
        mutation_authority: string_at(&["mutation_authority", "status"])?,
        runtime: string_at(&["runtime", "verification"])?,
    })
}

fn parse_operator_overall(value: &str) -> Option<OperatorOverallStatus> {
    match value {
        "healthy" => Some(OperatorOverallStatus::Healthy),
        "degraded" => Some(OperatorOverallStatus::Degraded),
        "action_required" => Some(OperatorOverallStatus::ActionRequired),
        "unavailable" => Some(OperatorOverallStatus::Unavailable),
        "unknown" => Some(OperatorOverallStatus::Unknown),
        _ => None,
    }
}

const fn operator_overall_name(value: OperatorOverallStatus) -> &'static str {
    match value {
        OperatorOverallStatus::Healthy => "healthy",
        OperatorOverallStatus::Degraded => "degraded",
        OperatorOverallStatus::ActionRequired => "action_required",
        OperatorOverallStatus::Unavailable => "unavailable",
        OperatorOverallStatus::Unknown => "unknown",
    }
}

#[cfg(target_os = "macos")]
fn write_public_key(public_key_out: &Path, public_key: &[u8; 65]) -> Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options
        .open(public_key_out)
        .with_context(|| format!("refusing to overwrite {}", public_key_out.display()))?;
    file.write_all(public_key)?;
    file.sync_all()?;
    println!("recovery_public_key={}", public_key_out.display());
    Ok(())
}

#[cfg(target_os = "macos")]
fn init_key(
    key_file: PathBuf,
    secure_enclave_helper: PathBuf,
    public_key_out: PathBuf,
) -> Result<()> {
    use computer_use_mcp_gateway::v2_online_recovery::macos::MacRecoveryKey;
    if std::fs::symlink_metadata(&public_key_out).is_ok() {
        anyhow::bail!("refusing to overwrite {}", public_key_out.display());
    }
    let key = MacRecoveryKey::create_new(&secure_enclave_helper, &key_file)
        .context("failed to create Secure Enclave recovery key")?;
    let public_key = key
        .public_key_bytes()
        .context("failed to export recovery public key")?;
    write_public_key(&public_key_out, &public_key)?;
    println!("recovery_key_file={}", key_file.display());
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn init_key(
    _key_file: PathBuf,
    _secure_enclave_helper: PathBuf,
    _public_key_out: PathBuf,
) -> Result<()> {
    bail!("Secure Enclave recovery approval is supported only on macOS")
}

#[cfg(target_os = "macos")]
fn export_public(
    key_file: PathBuf,
    secure_enclave_helper: PathBuf,
    public_key_out: PathBuf,
) -> Result<()> {
    use computer_use_mcp_gateway::v2_online_recovery::macos::MacRecoveryKey;
    let key = MacRecoveryKey::load(&secure_enclave_helper, &key_file)
        .context("recovery key is not provisioned")?;
    let public_key = key
        .public_key_bytes()
        .context("failed to export recovery public key")?;
    write_public_key(&public_key_out, &public_key)
}

#[cfg(not(target_os = "macos"))]
fn export_public(
    _key_file: PathBuf,
    _secure_enclave_helper: PathBuf,
    _public_key_out: PathBuf,
) -> Result<()> {
    bail!("Secure Enclave recovery approval is supported only on macOS")
}

#[cfg(target_os = "macos")]
fn resolve(
    state_dir: PathBuf,
    hub_public_key_file: PathBuf,
    key_file: PathBuf,
    secure_enclave_helper: PathBuf,
    decision: IndeterminateResolution,
    evidence: String,
    wait_secs: u64,
) -> Result<()> {
    use computer_use_mcp_gateway::v2_online_recovery::macos::MacRecoveryKey;
    let challenge = verified_challenge(&state_dir, &hub_public_key_file)?;
    // The current privacy-preserving checkpoint deliberately does not retain raw
    // GUI command/result payloads, so generic post-hoc automatic audit is
    // conservative: the local user must inspect the desktop and choose the
    // resolution explicitly.
    let authorization = new_authorization(
        &challenge,
        RecoveryAuditAssessment::Inconclusive,
        decision,
        evidence,
    )
    .context("invalid recovery decision")?;
    println!("approving_local_recovery");
    println!("device_id={}", authorization.device_id);
    println!("operation_id={}", authorization.operation_id);
    println!(
        "quarantine_generation={}",
        authorization.quarantine_generation
    );
    println!("current_generation={}", authorization.current_generation);
    println!("audit_assessment=inconclusive");
    println!(
        "decision={}",
        recovery_decision_name(authorization.decision)
    );
    let key = MacRecoveryKey::load(&secure_enclave_helper, &key_file)
        .context("recovery key is not provisioned")?;
    let authorization = key
        .sign_authorization(authorization)
        .context("OS user-presence approval was not completed")?;
    store_authorization(&state_dir, &authorization)
        .context("failed to publish recovery authorization to Agent")?;
    println!("request_id={}", authorization.request_id);
    println!("operation_id={}", authorization.operation_id);
    println!("authorization=published");
    if wait_secs > 0 {
        wait_for_completion(
            &state_dir,
            &hub_public_key_file,
            &ExpectedRecoveryCompletion {
                phase: authorization.phase,
                request_id: authorization.request_id.clone(),
                device_id: authorization.device_id.clone(),
                operation_id: authorization.operation_id.clone(),
                current_generation: authorization.current_generation,
                decision: authorization.decision,
                current_state_policy: authorization.current_state_policy,
            },
            wait_secs,
        )?;
    } else {
        println!("durable_completion=not_checked");
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn accept_current_state(
    state_dir: PathBuf,
    hub_public_key_file: PathBuf,
    key_file: PathBuf,
    secure_enclave_helper: PathBuf,
    policy: RetirementPolicy,
    evidence: String,
    wait_secs: u64,
) -> Result<()> {
    use computer_use_mcp_gateway::v2_online_recovery::macos::MacRecoveryKey;
    let challenge = verified_challenge(&state_dir, &hub_public_key_file)?;
    let authorization = new_current_state_acceptance_authorization(&challenge, policy, evidence)
        .context("current-state acceptance is not valid for this recovery schema")?;
    println!("accepting_current_state");
    println!("device_id={}", authorization.device_id);
    println!("operation_id={}", authorization.operation_id);
    println!(
        "quarantine_generation={}",
        authorization.quarantine_generation
    );
    println!("current_generation={}", authorization.current_generation);
    println!("historical_execution_outcome=indeterminate");
    println!("operator_observation=current_state_accepted");
    println!("operational_disposition=current_state_accepted");
    println!(
        "retirement_policy={}",
        match policy {
            RetirementPolicy::TransientUiInteractionV1 => "transient_ui_interaction_v1",
            RetirementPolicy::AcknowledgedUnknownPointerClickV1 => {
                "acknowledged_unknown_pointer_click_v1"
            }
        }
    );
    println!("old_operation_replayed=false");
    if policy == RetirementPolicy::AcknowledgedUnknownPointerClickV1 {
        println!("mutation_resume_required=true");
        println!(
            "statement=I understand the prior click may or may not have executed and may already have caused side effects. I accept the current device state without asserting either historical outcome; fresh mutations remain fenced until a separate local-user resume."
        );
    } else {
        println!(
            "statement=I inspected the current screen. This state is acceptable to continue from."
        );
    }
    println!("user_presence=required");
    let key = MacRecoveryKey::load(&secure_enclave_helper, &key_file)
        .context("recovery key is not provisioned")?;
    let authorization = key
        .sign_authorization(authorization)
        .context("OS user-presence approval was not completed")?;
    store_authorization(&state_dir, &authorization)
        .context("failed to publish current-state acceptance authorization to Agent")?;
    println!("request_id={}", authorization.request_id);
    println!("authorization=published");
    if wait_secs > 0 {
        wait_for_completion(
            &state_dir,
            &hub_public_key_file,
            &ExpectedRecoveryCompletion {
                phase: authorization.phase,
                request_id: authorization.request_id.clone(),
                device_id: authorization.device_id.clone(),
                operation_id: authorization.operation_id.clone(),
                current_generation: authorization.current_generation,
                decision: authorization.decision,
                current_state_policy: authorization.current_state_policy,
            },
            wait_secs,
        )?;
    } else {
        println!("durable_completion=not_checked");
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn resume_mutations(
    state_dir: PathBuf,
    hub_public_key_file: PathBuf,
    key_file: PathBuf,
    secure_enclave_helper: PathBuf,
    evidence: String,
    wait_secs: u64,
) -> Result<()> {
    use computer_use_mcp_gateway::v2_online_recovery::macos::MacRecoveryKey;
    let challenge = verified_challenge(&state_dir, &hub_public_key_file)?;
    if challenge.phase != RecoveryPhase::MutationResume {
        anyhow::bail!("current recovery challenge is not a mutation-resume challenge");
    }
    let authorization = new_mutation_resume_authorization(&challenge, evidence)
        .context("mutation resume is not valid for this recovery challenge")?;
    println!("resuming_mutations");
    println!("device_id={}", authorization.device_id);
    println!("source_operation_id={}", authorization.operation_id);
    println!("current_generation={}", authorization.current_generation);
    println!("historical_execution_outcome=indeterminate");
    println!("old_operation_replayed=false");
    println!("stale_workflow_resumed=false");
    println!(
        "statement=I explicitly re-arm only fresh effectful operations from this new device generation. The prior click remains historically unknown and is not replayed."
    );
    println!("user_presence=required");
    let key = MacRecoveryKey::load(&secure_enclave_helper, &key_file)
        .context("recovery key is not provisioned")?;
    let authorization = key
        .sign_authorization(authorization)
        .context("OS user-presence approval was not completed")?;
    store_authorization(&state_dir, &authorization)
        .context("failed to publish mutation-resume authorization to Agent")?;
    println!("request_id={}", authorization.request_id);
    println!("authorization=published");
    if wait_secs > 0 {
        wait_for_completion(
            &state_dir,
            &hub_public_key_file,
            &ExpectedRecoveryCompletion {
                phase: authorization.phase,
                request_id: authorization.request_id.clone(),
                device_id: authorization.device_id.clone(),
                operation_id: authorization.operation_id.clone(),
                current_generation: authorization.current_generation,
                decision: authorization.decision,
                current_state_policy: authorization.current_state_policy,
            },
            wait_secs,
        )?;
    } else {
        println!("durable_completion=not_checked");
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn resume_mutations(
    _state_dir: PathBuf,
    _hub_public_key_file: PathBuf,
    _key_file: PathBuf,
    _secure_enclave_helper: PathBuf,
    _evidence: String,
    _wait_secs: u64,
) -> Result<()> {
    bail!("local user-presence mutation resume is supported only on macOS")
}

#[cfg(not(target_os = "macos"))]
fn accept_current_state(
    _state_dir: PathBuf,
    _hub_public_key_file: PathBuf,
    _key_file: PathBuf,
    _secure_enclave_helper: PathBuf,
    _policy: RetirementPolicy,
    _evidence: String,
    _wait_secs: u64,
) -> Result<()> {
    bail!("local user-presence current-state acceptance is supported only on macOS")
}

#[cfg(not(target_os = "macos"))]
fn resolve(
    _state_dir: PathBuf,
    _hub_public_key_file: PathBuf,
    _key_file: PathBuf,
    _secure_enclave_helper: PathBuf,
    _decision: IndeterminateResolution,
    _evidence: String,
    _wait_secs: u64,
) -> Result<()> {
    bail!("local user-presence recovery approval is supported only on macOS")
}

fn wait_for_completion(
    state_dir: &Path,
    hub_public_key_file: &Path,
    expected: &ExpectedRecoveryCompletion,
    wait_secs: u64,
) -> Result<()> {
    let _resolved =
        wait_for_completion_verified(state_dir, hub_public_key_file, expected, wait_secs)?;
    println!("request_id={}", expected.request_id);
    println!("operation_id={}", expected.operation_id);
    println!("durable_completion=verified");
    println!("old_operation_replayed=false");
    Ok(())
}

fn recovery_completion_matches_expected(
    resolved: &RecoveryResolved,
    trusted_hub: &ed25519_dalek::VerifyingKey,
    expected: &ExpectedRecoveryCompletion,
) -> Result<bool> {
    // A previously completed recovery may legitimately remain on disk while the
    // next phase is being authorized. Verify that receipt as authentic first,
    // but do not confuse a valid stale receipt with the exact completion we are
    // waiting for.
    verify_recovery_resolved(
        resolved,
        trusted_hub,
        &resolved.request_id,
        &resolved.device_id,
        resolved.current_generation,
    )
    .context("durable recovery acknowledgement has an invalid Hub signature")?;

    Ok(resolved.phase == expected.phase
        && resolved.request_id == expected.request_id
        && resolved.device_id == expected.device_id
        && resolved.operation_id == expected.operation_id
        && resolved.current_generation == expected.current_generation
        && resolved.decision == expected.decision
        && resolved.current_state_policy == expected.current_state_policy)
}

fn wait_for_completion_verified(
    state_dir: &Path,
    hub_public_key_file: &Path,
    expected: &ExpectedRecoveryCompletion,
    wait_secs: u64,
) -> Result<RecoveryResolved> {
    let trusted_hub =
        load_verifying_key(hub_public_key_file).context("failed to load pinned Hub public key")?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(wait_secs);
    let mut saw_other_valid_ack = false;
    loop {
        if let Some(resolved) = load_recovery_resolved(state_dir)
            .context("failed to read durable recovery acknowledgement")?
        {
            if recovery_completion_matches_expected(&resolved, &trusted_hub, expected)? {
                return Ok(resolved);
            }
            saw_other_valid_ack = true;
        }
        if wait_secs == 0 || std::time::Instant::now() >= deadline {
            if saw_other_valid_ack {
                anyhow::bail!(
                    "durable Hub recovery completion is not yet verified; latest acknowledgement belongs to a different recovery request or phase"
                );
            }
            anyhow::bail!("durable Hub recovery completion is not yet verified");
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
}

#[cfg(test)]
mod completion_wait_tests {
    use super::*;
    use computer_use_mcp_gateway::{
        v2_m0_transport::HubIdentity,
        v2_m1_keys::write_new_verifying_key,
        v2_online_recovery::{
            ONLINE_RECOVERY_SCHEMA_VERSION, RecoveryAuditAssessment, RecoveryAuthorization,
            build_recovery_resolved, store_recovery_resolved,
        },
    };

    fn temp_dir(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "cumg-v2-recover-{name}-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        std::fs::create_dir_all(&path).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        path
    }

    fn authorization(
        phase: RecoveryPhase,
        request_id: &str,
        generation: u64,
        decision: RecoveryDecision,
        policy: Option<RetirementPolicy>,
    ) -> RecoveryAuthorization {
        RecoveryAuthorization {
            schema_version: ONLINE_RECOVERY_SCHEMA_VERSION,
            phase,
            request_id: request_id.to_owned(),
            device_id: "dev_completion_wait".into(),
            operation_id: "op_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
            quarantine_generation: 7,
            current_generation: generation,
            quarantine_fingerprint: [7; 32],
            challenge_nonce: [9; 32],
            challenge_expires_at_ms: u64::MAX,
            audit_assessment: RecoveryAuditAssessment::Inconclusive,
            decision,
            current_state_policy: policy,
            evidence: "test".into(),
            signature: vec![],
        }
    }

    #[test]
    fn completion_wait_ignores_valid_stale_stage_one_receipt_until_stage_two_arrives() {
        let root = temp_dir("stale-stage-one");
        let state_dir = root.join("state");
        std::fs::create_dir_all(&state_dir).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&state_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let hub = HubIdentity::generate();
        let hub_public = root.join("hub.pub");
        write_new_verifying_key(&hub_public, &hub.verifier()).unwrap();

        let stale_authorization = authorization(
            RecoveryPhase::QuarantineResolution,
            "rec_11111111111111111111111111111111",
            1140,
            RecoveryDecision::CurrentStateAccepted,
            Some(RetirementPolicy::AcknowledgedUnknownPointerClickV1),
        );
        let stale = build_recovery_resolved(&hub, &stale_authorization, 10).unwrap();
        store_recovery_resolved(&state_dir, &stale).unwrap();

        let expected_authorization = authorization(
            RecoveryPhase::MutationResume,
            "rec_22222222222222222222222222222222",
            1141,
            RecoveryDecision::MutationsResumed,
            Some(RetirementPolicy::AcknowledgedUnknownPointerClickV1),
        );
        let expected = ExpectedRecoveryCompletion {
            phase: expected_authorization.phase,
            request_id: expected_authorization.request_id.clone(),
            device_id: expected_authorization.device_id.clone(),
            operation_id: expected_authorization.operation_id.clone(),
            current_generation: expected_authorization.current_generation,
            decision: expected_authorization.decision,
            current_state_policy: expected_authorization.current_state_policy,
        };
        let matching = build_recovery_resolved(&hub, &expected_authorization, 20).unwrap();
        let writer_state = state_dir.clone();
        let writer = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(100));
            store_recovery_resolved(&writer_state, &matching).unwrap();
        });

        let resolved = wait_for_completion_verified(&state_dir, &hub_public, &expected, 2).unwrap();
        writer.join().unwrap();
        assert_eq!(resolved.request_id, expected.request_id);
        assert_eq!(resolved.phase, RecoveryPhase::MutationResume);

        let _ = std::fs::remove_dir_all(root);
    }
}
