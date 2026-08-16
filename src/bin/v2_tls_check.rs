use clap::{Parser, ValueEnum};
use computer_use_mcp_gateway::v2_tls_lifecycle::{
    CertificateFormat, CertificateHealth, inspect_certificate_file,
};
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Debug, Clone, Copy, ValueEnum)]
enum InputFormat {
    Pem,
    Der,
}

impl From<InputFormat> for CertificateFormat {
    fn from(value: InputFormat) -> Self {
        match value {
            InputFormat::Pem => Self::Pem,
            InputFormat::Der => Self::Der,
        }
    }
}

#[derive(Debug, Parser)]
#[command(name = "v2_tls_check")]
#[command(about = "Check a V2 TLS certificate or trust anchor for expiry")]
struct Cli {
    #[arg(long)]
    certificate: PathBuf,
    #[arg(long, value_enum)]
    format: InputFormat,
    #[arg(long, default_value_t = 2_592_000)]
    warn_before_secs: u64,
}

fn main() -> ExitCode {
    let args = Cli::parse();
    let inspection = match inspect_certificate_file(
        &args.certificate,
        args.format.into(),
        args.warn_before_secs,
    ) {
        Ok(inspection) => inspection,
        Err(error) => {
            eprintln!(
                "CUMG_TLS_EXPIRY_ALERT status=invalid error_code={}",
                error.safe_error_code()
            );
            return ExitCode::from(4);
        }
    };

    let line = format!(
        "status={} not_after_unix_secs={} remaining_secs={}",
        inspection.health.as_str(),
        inspection.not_after_unix_secs,
        inspection.remaining_secs
    );
    match inspection.health {
        CertificateHealth::Healthy => {
            println!("CUMG_TLS_EXPIRY_OK {line}");
            ExitCode::SUCCESS
        }
        CertificateHealth::Expiring => {
            eprintln!("CUMG_TLS_EXPIRY_ALERT {line}");
            ExitCode::from(2)
        }
        CertificateHealth::Expired | CertificateHealth::NotYetValid => {
            eprintln!("CUMG_TLS_EXPIRY_ALERT {line}");
            ExitCode::from(3)
        }
    }
}
