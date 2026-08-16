use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use computer_use_mcp_gateway::{
    v2_enrollment::prepare_agent_enrollment,
    v2_m0_trust::{build_device_key_rotation, build_hub_key_rotation},
    v2_m1_keys::{
        create_new_device_identity, create_new_grant_authority, create_new_hub_identity,
        load_device_identity, load_hub_identity, write_new_trusted_text, write_new_verifying_key,
    },
};
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(name = "v2_keyctl")]
#[command(about = "Offline V2 application-key generation and signed rotation documents")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    PrepareAgentEnrollment {
        #[arg(long)]
        output_dir: PathBuf,
        #[arg(long)]
        hub_public: PathBuf,
        #[arg(long)]
        grant_public: PathBuf,
        #[arg(long)]
        tls_root_der: PathBuf,
    },
    GenerateDevice {
        secret: PathBuf,
        public: PathBuf,
    },
    GenerateHub {
        secret: PathBuf,
        public: PathBuf,
    },
    GenerateGrant {
        secret: PathBuf,
        public: PathBuf,
    },
    RotateDevice {
        #[arg(long)]
        device_id: String,
        #[arg(long)]
        old_secret: PathBuf,
        #[arg(long)]
        new_secret: PathBuf,
        #[arg(long)]
        new_public: PathBuf,
        #[arg(long)]
        rotation: PathBuf,
        #[arg(long)]
        epoch: u64,
    },
    RotateHub {
        #[arg(long)]
        old_secret: PathBuf,
        #[arg(long)]
        new_secret: PathBuf,
        #[arg(long)]
        new_public: PathBuf,
        #[arg(long)]
        rotation: PathBuf,
        #[arg(long)]
        epoch: u64,
    },
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::PrepareAgentEnrollment {
            output_dir,
            hub_public,
            grant_public,
            tls_root_der,
        } => {
            let manifest =
                prepare_agent_enrollment(&output_dir, &hub_public, &grant_public, &tls_root_der)
                    .context("prepare Agent enrollment bundle")?;
            println!("device_id={}", manifest.device_id);
            println!("agent_bundle={}", output_dir.join("agent").display());
            println!(
                "hub_device_public_key={}",
                output_dir
                    .join(&manifest.hub_device_public_key_file)
                    .display()
            );
        }
        Command::GenerateDevice { secret, public } => {
            let identity = create_new_device_identity(&secret).context("create device secret")?;
            write_new_verifying_key(&public, &identity.verifying_key())
                .context("write device public key")?;
        }
        Command::GenerateHub { secret, public } => {
            let identity = create_new_hub_identity(&secret).context("create Hub secret")?;
            write_new_verifying_key(&public, &identity.verifier())
                .context("write Hub public key")?;
        }
        Command::GenerateGrant { secret, public } => {
            let authority = create_new_grant_authority(&secret).context("create grant secret")?;
            write_new_verifying_key(&public, &authority.verifier())
                .context("write grant public key")?;
        }
        Command::RotateDevice {
            device_id,
            old_secret,
            new_secret,
            new_public,
            rotation,
            epoch,
        } => {
            ensure!(
                !device_id.trim().is_empty() && epoch > 0,
                "device id and non-zero epoch are required"
            );
            let old = load_device_identity(&old_secret).context("load old device identity")?;
            let new = create_new_device_identity(&new_secret)
                .context("create replacement device identity")?;
            let document = build_device_key_rotation(&device_id, &old, &new, epoch)
                .context("build signed device rotation")?;
            write_new_verifying_key(&new_public, &new.verifying_key())
                .context("write new device public key")?;
            write_new_trusted_text(&rotation, &serde_json::to_string_pretty(&document)?)
                .context("write device rotation document")?;
        }
        Command::RotateHub {
            old_secret,
            new_secret,
            new_public,
            rotation,
            epoch,
        } => {
            ensure!(epoch > 0, "non-zero epoch is required");
            let old = load_hub_identity(&old_secret).context("load old Hub identity")?;
            let new =
                create_new_hub_identity(&new_secret).context("create replacement Hub identity")?;
            let document =
                build_hub_key_rotation(&old, &new, epoch).context("build signed Hub rotation")?;
            write_new_verifying_key(&new_public, &new.verifier())
                .context("write new Hub public key")?;
            write_new_trusted_text(&rotation, &serde_json::to_string_pretty(&document)?)
                .context("write Hub rotation document")?;
        }
    }
    Ok(())
}
