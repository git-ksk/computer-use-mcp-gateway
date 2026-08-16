#![cfg_attr(not(unix), allow(dead_code))]

#[cfg(unix)]
use anyhow::{Context, Result};
#[cfg(unix)]
use clap::Parser;
#[cfg(unix)]
use computer_use_mcp_gateway::{
    v2_grant_signer::{GrantSigningPolicy, GrantSigningPolicyDocument, serve_unix_grant_signer},
    v2_m1_keys::{load_grant_authority, load_trusted_text},
};
#[cfg(unix)]
use std::path::PathBuf;

#[cfg(unix)]
const MAX_SIGNER_POLICY_BYTES: u64 = 64 * 1024;

#[cfg(unix)]
#[derive(Debug, Parser)]
#[command(name = "v2_grant_signer")]
#[command(about = "External V2 capability-grant signing authority over a Unix socket")]
struct Args {
    #[arg(long, env = "CUMG_V2_GRANT_SIGNER_SOCKET")]
    socket: PathBuf,
    #[arg(long, env = "CUMG_V2_GRANT_SECRET_FILE")]
    grant_secret_file: PathBuf,
    #[arg(long, env = "CUMG_V2_GRANT_SIGNER_POLICY_FILE")]
    policy_file: PathBuf,
}

#[cfg(unix)]
fn main() -> Result<()> {
    let _observability = computer_use_mcp_gateway::v2_observability::init("cumg-v2-grant-signer")?;
    let args = Args::parse();
    let authority = load_grant_authority(&args.grant_secret_file)
        .context("failed to load external grant-signing identity")?;
    let document = load_trusted_text(&args.policy_file, MAX_SIGNER_POLICY_BYTES)
        .context("failed to load grant-signer policy")?;
    let policy = GrantSigningPolicy::from_document(
        serde_json::from_str::<GrantSigningPolicyDocument>(&document)
            .context("invalid grant-signer policy document")?,
    )
    .context("grant-signer policy rejected")?;
    tracing::info!(
        event = "v2_grant_signer_started",
        signer_key_id = %authority.key_id(),
        outcome = "ready",
        "external grant signer started; private key remains in signer process"
    );
    serve_unix_grant_signer(&args.socket, authority, policy)
        .context("external grant signer stopped")
}

#[cfg(not(unix))]
fn main() {
    eprintln!("v2_grant_signer is supported only on Unix hosts");
    std::process::exit(2);
}
