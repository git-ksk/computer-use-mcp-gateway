#[cfg(not(target_os = "macos"))]
use anyhow::bail;
use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use computer_use_mcp_gateway::{
    v2_m0_execution::IndeterminateResolution,
    v2_m1_keys::load_verifying_key,
    v2_online_recovery::{
        DEFAULT_MACOS_RECOVERY_KEY_LABEL, RecoveryAuditAssessment, load_challenge,
        new_authorization, store_authorization, verify_recovery_challenge,
    },
};
use std::fs::OpenOptions;
use std::io::Write as _;
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
    /// Create/load the macOS Secure Enclave recovery key and export only its public key.
    InitKey {
        #[arg(long, default_value = DEFAULT_MACOS_RECOVERY_KEY_LABEL)]
        key_label: String,
        #[arg(long)]
        public_key_out: PathBuf,
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
        #[arg(long, default_value = DEFAULT_MACOS_RECOVERY_KEY_LABEL)]
        key_label: String,
        #[arg(long, value_enum)]
        decision: DecisionArg,
        /// Short metadata describing what the local user inspected. Do not include secrets or screenshots.
        #[arg(long)]
        evidence: String,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum DecisionArg {
    ConfirmedCompleted,
    ConfirmedNotExecuted,
}

impl From<DecisionArg> for IndeterminateResolution {
    fn from(value: DecisionArg) -> Self {
        match value {
            DecisionArg::ConfirmedCompleted => Self::ConfirmedCompleted,
            DecisionArg::ConfirmedNotExecuted => Self::ConfirmedNotExecuted,
        }
    }
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
            key_label,
            public_key_out,
        } => init_key(key_label, public_key_out),
        Command::Status {
            state_dir,
            hub_public_key_file,
        } => {
            let challenge = verified_challenge(&state_dir, &hub_public_key_file)?;
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
            key_label,
            decision,
            evidence,
        } => resolve(
            state_dir,
            hub_public_key_file,
            key_label,
            decision.into(),
            evidence,
        ),
    }
}

#[cfg(target_os = "macos")]
fn init_key(key_label: String, public_key_out: PathBuf) -> Result<()> {
    use computer_use_mcp_gateway::v2_online_recovery::macos::MacRecoveryKey;
    let key = MacRecoveryKey::create_new(&key_label)
        .context("failed to create/load Secure Enclave recovery key")?;
    let public_key = key
        .public_key_bytes()
        .context("failed to export recovery public key")?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options
        .open(&public_key_out)
        .with_context(|| format!("refusing to overwrite {}", public_key_out.display()))?;
    file.write_all(&public_key)?;
    file.sync_all()?;
    println!("recovery_public_key={}", public_key_out.display());
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn init_key(_key_label: String, _public_key_out: PathBuf) -> Result<()> {
    bail!("Secure Enclave recovery approval is supported only on macOS")
}

#[cfg(target_os = "macos")]
fn resolve(
    state_dir: PathBuf,
    hub_public_key_file: PathBuf,
    key_label: String,
    decision: IndeterminateResolution,
    evidence: String,
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
        match authorization.decision {
            IndeterminateResolution::ConfirmedCompleted => "confirmed_completed",
            IndeterminateResolution::ConfirmedNotExecuted => "confirmed_not_executed",
        }
    );
    let key = MacRecoveryKey::load(&key_label).context("recovery key is not provisioned")?;
    let authorization = key
        .sign_authorization(authorization)
        .context("OS user-presence approval was not completed")?;
    store_authorization(&state_dir, &authorization)
        .context("failed to publish recovery authorization to Agent")?;
    println!("request_id={}", authorization.request_id);
    println!("operation_id={}", authorization.operation_id);
    println!("authorization=published");
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn resolve(
    _state_dir: PathBuf,
    _hub_public_key_file: PathBuf,
    _key_label: String,
    _decision: IndeterminateResolution,
    _evidence: String,
) -> Result<()> {
    bail!("local user-presence recovery approval is supported only on macOS")
}
