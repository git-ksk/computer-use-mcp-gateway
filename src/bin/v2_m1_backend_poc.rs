use anyhow::{Result, bail};
use computer_use_mcp_gateway::{
    v2_m0::{DeviceCommand, DeviceResult},
    v2_m1_backend::{BackendExecutionOutcome, CuaMcpAdapter},
};
use serde_json::json;
use std::{env, process::Command, time::Duration};
use tokio::sync::watch;

#[tokio::main]
async fn main() -> Result<()> {
    let command = env::var("CUMG_BACKEND_COMMAND").unwrap_or_else(|_| "cua-driver".into());
    let backend_version = backend_version(&command).unwrap_or_else(|| "unknown".into());
    let adapter = CuaMcpAdapter::new(
        command,
        vec!["mcp".into()],
        backend_version.clone(),
        format!("{}-{}", env::consts::OS, env::consts::ARCH),
        1,
        Duration::from_secs(10),
        Duration::from_secs(30),
        1,
        Duration::from_millis(100),
    );
    adapter.connect().await?;
    let (_cancel_tx, cancel_rx) = watch::channel(false);

    let app_count = match adapter
        .execute(&DeviceCommand::ListApplications, cancel_rx.clone())
        .await?
    {
        BackendExecutionOutcome::Completed(DeviceResult::Applications { count }) => count,
        other => bail!("unexpected list-applications outcome: {other:?}"),
    };
    let (width_points, height_points, scale_factor_milli) = match adapter
        .execute(&DeviceCommand::ScreenGeometry, cancel_rx)
        .await?
    {
        BackendExecutionOutcome::Completed(DeviceResult::ScreenGeometry {
            width_points,
            height_points,
            scale_factor_milli,
        }) => (width_points, height_points, scale_factor_milli),
        other => bail!("unexpected screen-geometry outcome: {other:?}"),
    };
    adapter.shutdown().await?;

    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "status": "pass",
            "backend": {
                "name": "cua",
                "version": backend_version,
            },
            "semantic_results": {
                "application_count": app_count,
                "screen_geometry": {
                    "width_points": width_points,
                    "height_points": height_points,
                    "scale_factor_milli": scale_factor_milli,
                },
            },
            "raw_backend_output_logged": false,
        }))?
    );
    Ok(())
}

fn backend_version(command: &str) -> Option<String> {
    let output = Command::new(command).arg("--version").output().ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout)
        .ok()?
        .split_whitespace()
        .last()
        .map(ToOwned::to_owned)
}
