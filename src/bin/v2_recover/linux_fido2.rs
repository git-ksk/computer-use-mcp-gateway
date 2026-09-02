#![cfg_attr(not(target_os = "linux"), allow(unused_imports, dead_code))]

use super::{ExpectedRecoveryCompletion, verified_challenge, wait_for_completion};
use anyhow::{Context, Result, bail};
use computer_use_mcp_gateway::{
    v2_execution_safety::RetirementPolicy,
    v2_linux_fido2_recovery::{LinuxFido2Recovery, LinuxFido2UvMode},
    v2_m0_execution::IndeterminateResolution,
    v2_online_recovery::{
        RecoveryAuditAssessment, RecoveryAuthorization, WebAuthnRecoveryVerifierDocument,
        new_authorization, new_current_state_acceptance_authorization, recovery_decision_name,
        store_authorization,
    },
};
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub(super) struct LinuxFido2ProviderArgs {
    pub(super) tool_dir: PathBuf,
    pub(super) device: PathBuf,
    pub(super) uv_mode: LinuxFido2UvMode,
    pub(super) verifier_file: PathBuf,
}

#[cfg(target_os = "linux")]
pub(super) fn init_linux_fido2(
    tool_dir: PathBuf,
    device: PathBuf,
    uv_mode: LinuxFido2UvMode,
    verifier_out: PathBuf,
) -> Result<()> {
    let (_credential, verifier) = LinuxFido2Recovery::create_new(&tool_dir, &device, uv_mode)
        .context("Linux FIDO2 recovery credential provisioning was not completed")?;
    write_webauthn_verifier(&verifier_out, &verifier)?;
    println!("linux_fido2_recovery=provisioned");
    println!("user_verification=required");
    println!("uv_mode={}", uv_mode.name());
    println!("recovery_verifier={}", verifier_out.display());
    println!("hub_filename=recovery-webauthn-verifier.json");
    Ok(())
}

#[cfg(not(target_os = "linux"))]
pub(super) fn init_linux_fido2(
    _tool_dir: PathBuf,
    _device: PathBuf,
    _uv_mode: LinuxFido2UvMode,
    _verifier_out: PathBuf,
) -> Result<()> {
    bail!("Linux FIDO2 recovery provisioning is supported only on Linux")
}

#[cfg(target_os = "linux")]
fn load_linux_fido2(
    tool_dir: &Path,
    device: &Path,
    uv_mode: LinuxFido2UvMode,
    verifier_file: &Path,
) -> Result<LinuxFido2Recovery> {
    let metadata = std::fs::symlink_metadata(verifier_file)
        .context("failed to inspect Linux FIDO2 recovery verifier")?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() == 0
        || metadata.len() > 4096
    {
        bail!("Linux FIDO2 recovery verifier is unsafe or invalid");
    }
    let bytes =
        std::fs::read(verifier_file).context("failed to read Linux FIDO2 recovery verifier")?;
    LinuxFido2Recovery::from_verifier_document(tool_dir, device, uv_mode, &bytes)
        .context("Linux FIDO2 recovery verifier/provider is unavailable or invalid")
}

#[cfg(target_os = "linux")]
pub(super) fn resolve_linux_fido2(
    state_dir: PathBuf,
    hub_public_key_file: PathBuf,
    provider: LinuxFido2ProviderArgs,
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
    println!("approving_linux_fido2_recovery");
    println!("device_id={}", authorization.device_id);
    println!("operation_id={}", authorization.operation_id);
    println!("current_generation={}", authorization.current_generation);
    println!(
        "decision={}",
        recovery_decision_name(authorization.decision)
    );
    println!("linux_fido2_user_verification=required");
    println!("uv_mode={}", provider.uv_mode.name());
    let credential = load_linux_fido2(
        &provider.tool_dir,
        &provider.device,
        provider.uv_mode,
        &provider.verifier_file,
    )?;
    let authorization = credential
        .sign_authorization(authorization)
        .context("Linux FIDO2 user verification was not completed")?;
    store_authorization(&state_dir, &authorization)
        .context("failed to publish Linux FIDO2 recovery authorization")?;
    finish_authorization(&state_dir, &hub_public_key_file, &authorization, wait_secs)
}

#[cfg(not(target_os = "linux"))]
pub(super) fn resolve_linux_fido2(
    _state_dir: PathBuf,
    _hub_public_key_file: PathBuf,
    _provider: LinuxFido2ProviderArgs,
    _decision: IndeterminateResolution,
    _evidence: String,
    _wait_secs: u64,
) -> Result<()> {
    bail!("Linux FIDO2 recovery approval is supported only on Linux")
}

#[cfg(target_os = "linux")]
pub(super) fn accept_current_state_linux_fido2(
    state_dir: PathBuf,
    hub_public_key_file: PathBuf,
    provider: LinuxFido2ProviderArgs,
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
    println!("accepting_current_state_linux_fido2");
    println!("device_id={}", authorization.device_id);
    println!("operation_id={}", authorization.operation_id);
    println!("historical_execution_outcome=indeterminate");
    println!("operator_observation=current_state_accepted");
    println!("old_operation_replayed=false");
    println!("linux_fido2_user_verification=required");
    println!("uv_mode={}", provider.uv_mode.name());
    let credential = load_linux_fido2(
        &provider.tool_dir,
        &provider.device,
        provider.uv_mode,
        &provider.verifier_file,
    )?;
    let authorization = credential
        .sign_authorization(authorization)
        .context("Linux FIDO2 user verification was not completed")?;
    store_authorization(&state_dir, &authorization)
        .context("failed to publish Linux FIDO2 current-state authorization")?;
    finish_authorization(&state_dir, &hub_public_key_file, &authorization, wait_secs)
}

#[cfg(not(target_os = "linux"))]
pub(super) fn accept_current_state_linux_fido2(
    _state_dir: PathBuf,
    _hub_public_key_file: PathBuf,
    _provider: LinuxFido2ProviderArgs,
    _evidence: String,
    _wait_secs: u64,
) -> Result<()> {
    bail!("Linux FIDO2 current-state acceptance is supported only on Linux")
}

#[cfg(target_os = "linux")]
fn write_webauthn_verifier(
    verifier_out: &Path,
    verifier: &WebAuthnRecoveryVerifierDocument,
) -> Result<()> {
    use std::{fs::OpenOptions, io::Write as _, os::unix::fs::OpenOptionsExt as _};
    let encoded = serde_json::to_vec_pretty(verifier)?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true).mode(0o644);
    let mut file = options
        .open(verifier_out)
        .with_context(|| format!("refusing to overwrite {}", verifier_out.display()))?;
    file.write_all(&encoded)?;
    file.sync_all()?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn finish_authorization(
    state_dir: &Path,
    hub_public_key_file: &Path,
    authorization: &RecoveryAuthorization,
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
