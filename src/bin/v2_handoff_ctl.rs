#![cfg_attr(not(unix), allow(dead_code))]

#[cfg(unix)]
use anyhow::{Result, bail};
#[cfg(unix)]
use clap::{Parser, Subcommand};
#[cfg(unix)]
use computer_use_mcp_gateway::v2_handoff_control::{
    LocalHandoffControlRequest, exchange_unix_handoff_control,
};
#[cfg(unix)]
use std::path::PathBuf;

#[cfg(unix)]
#[derive(Debug, Parser)]
#[command(name = "v2_handoff_ctl")]
#[command(about = "Local operator control for the Agent-owned Handoff coordinator")]
struct Args {
    #[arg(long, env = "CUMG_V2_HANDOFF_CONTROL_SOCKET")]
    socket: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[cfg(unix)]
#[derive(Debug, Subcommand)]
enum Command {
    /// Inspect bounded Handoff lifecycle state and the current Human takeover locator, if any.
    Status,
    /// Begin Human handoff for the last fresh exact Window admitted by CUMG.
    Begin,
    /// Reissue a non-expired signed recovery checkpoint against a fresh CUMG Window observation.
    RecoverReissue,
    /// Rebind recovery after context expiry/generation rollover using explicit prior-owner proof.
    RecoverRebind {
        #[arg(long)]
        prior_context_id: String,
        #[arg(long)]
        prior_generation: Option<u64>,
        #[arg(long)]
        prior_capability_revision: Option<u64>,
    },
    /// Rebind a live intervention after a fresh exact-surface observation on a newer Agent generation.
    RebindLive,
    /// Explicitly discard only an expired recovery tombstone; never replay or mark the old action successful.
    AbandonExpiredRecovery {
        #[arg(long)]
        expected_epoch: u64,
    },
    /// Arm explicit Agent resume after fresh CUMG verification reached ready_to_resume.
    RequestResume,
    /// Cancel only while Human authority has not yet been claimed.
    CancelBeforeHuman,
}

#[cfg(unix)]
impl From<Command> for LocalHandoffControlRequest {
    fn from(value: Command) -> Self {
        match value {
            Command::Status => Self::Status,
            Command::Begin => Self::Begin,
            Command::RecoverReissue => Self::RecoverReissue,
            Command::RecoverRebind {
                prior_context_id,
                prior_generation,
                prior_capability_revision,
            } => Self::RecoverRebind {
                prior_context_id,
                prior_generation,
                prior_capability_revision,
            },
            Command::RebindLive => Self::RebindLive,
            Command::AbandonExpiredRecovery { expected_epoch } => {
                Self::AbandonExpiredRecovery { expected_epoch }
            }
            Command::RequestResume => Self::RequestResume,
            Command::CancelBeforeHuman => Self::CancelBeforeHuman,
        }
    }
}

#[cfg(unix)]
fn main() -> Result<()> {
    let args = Args::parse();
    let response = exchange_unix_handoff_control(&args.socket, &args.command.into())?;
    println!("{}", serde_json::to_string_pretty(&response)?);
    if !response.ok {
        bail!(
            "handoff control rejected: {}",
            response.error_code.as_deref().unwrap_or("unknown")
        );
    }
    Ok(())
}

#[cfg(not(unix))]
fn main() {
    eprintln!("v2_handoff_ctl is supported only on Unix hosts");
    std::process::exit(2);
}
