use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand, ValueEnum};
use computer_use_mcp_gateway::{
    v2_m0_execution::IndeterminateResolution,
    v2_m1_keys::load_secret_text,
    v2_maintenance::{
        compare_quarantined_request_read_only, inspect_quarantines_read_only,
        resolve_indeterminate_offline,
    },
};
use std::path::PathBuf;

const MAX_AUDIT_FINGERPRINT_SECRET_BYTES: u64 = 4 * 1024;
const MAX_CANDIDATE_REQUEST_BYTES: u64 = 256 * 1024;
const MIN_AUDIT_FINGERPRINT_SECRET_BYTES: usize = 32;

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
    /// Inspect durable quarantine metadata without resolving or dispatching work.
    InspectQuarantine {
        #[arg(long, env = "CUMG_V2_HUB_STATE_DIR")]
        state_dir: PathBuf,
        /// Optionally restrict output to one stable device ID.
        #[arg(long)]
        device_id: Option<String>,
    },
    /// Compare one local candidate shell/process request to a quarantined request.
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
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum CandidateTool {
    #[value(name = "shell")]
    Shell,
    #[value(name = "execute_process")]
    ExecuteProcess,
}

impl CandidateTool {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Shell => "shell",
            Self::ExecuteProcess => "execute_process",
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum ResolutionDecision {
    #[value(name = "confirmed_completed")]
    ConfirmedCompleted,
    #[value(name = "confirmed_not_executed")]
    ConfirmedNotExecuted,
}

impl From<ResolutionDecision> for IndeterminateResolution {
    fn from(value: ResolutionDecision) -> Self {
        match value {
            ResolutionDecision::ConfirmedCompleted => Self::ConfirmedCompleted,
            ResolutionDecision::ConfirmedNotExecuted => Self::ConfirmedNotExecuted,
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
        Command::InspectQuarantine {
            state_dir,
            device_id,
        } => {
            let report = inspect_quarantines_read_only(&state_dir, device_id.as_deref())
                .context("read-only quarantine inspection failed")?;
            println!("{}", serde_json::to_string_pretty(&report)?);
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
