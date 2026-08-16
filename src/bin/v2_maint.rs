use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use computer_use_mcp_gateway::{
    v2_m0_execution::IndeterminateResolution, v2_maintenance::resolve_indeterminate_offline,
};
use std::path::PathBuf;

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
    }
    Ok(())
}
