use computer_use_mcp_gateway::v2_m0::GrantAuthority;
use computer_use_mcp_gateway::v2_m0_transport::HubIdentity;
use computer_use_mcp_gateway::v2_m1_keys::write_new_verifying_key;
use rcgen::{CertifiedKey, generate_simple_self_signed};
use std::fs;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(windows)]
fn lock_down_windows_directory(path: &std::path::Path) {
    let output = Command::new("whoami").output().expect("run whoami");
    assert!(output.status.success());
    let identity = String::from_utf8(output.stdout).unwrap().trim().to_owned();
    let output = Command::new("icacls.exe")
        .arg(path)
        .args([
            "/inheritance:r",
            "/grant:r",
            &format!("{identity}:(OI)(CI)F"),
            "*S-1-5-18:(OI)(CI)F",
            "*S-1-5-32-544:(OI)(CI)F",
            "/Q",
        ])
        .output()
        .expect("run icacls");
    assert!(output.status.success());
}

fn private_temp_dir() -> std::path::PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!("cumg-v2-enrollment-cli-{unique}"));
    fs::create_dir(&path).unwrap();
    #[cfg(windows)]
    lock_down_windows_directory(&path);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    path
}

#[test]
fn keyctl_prepare_agent_enrollment_produces_transfer_and_registration_artifacts() {
    let root = private_temp_dir();
    let hub = HubIdentity::generate();
    let grants = GrantAuthority::generate();
    let hub_public = root.join("hub.pub");
    let grant_public = root.join("grant.pub");
    let tls_root = root.join("tls-root.der");
    write_new_verifying_key(&hub_public, &hub.verifier()).unwrap();
    write_new_verifying_key(&grant_public, &grants.verifier()).unwrap();
    let CertifiedKey { cert, .. } = generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    fs::write(&tls_root, cert.der().as_ref()).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&tls_root, fs::Permissions::from_mode(0o644)).unwrap();
    }

    let bundle = root.join("fresh-agent");
    let output = Command::new(env!("CARGO_BIN_EXE_v2_keyctl"))
        .args([
            "prepare-agent-enrollment",
            "--output-dir",
            bundle.to_str().unwrap(),
            "--hub-public",
            hub_public.to_str().unwrap(),
            "--grant-public",
            grant_public.to_str().unwrap(),
            "--tls-root-der",
            tls_root.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("device_id=dev_"));
    assert!(bundle.join("agent/secrets/device.key").is_file());
    assert!(bundle.join("agent/trust/hub.pub").is_file());
    assert!(bundle.join("agent/trust/grant.pub").is_file());
    assert!(bundle.join("agent/trust/tls-root.der").is_file());
    assert!(bundle.join("hub/device.pub").is_file());
    assert!(bundle.join("enrollment.json").is_file());

    let second = Command::new(env!("CARGO_BIN_EXE_v2_keyctl"))
        .args([
            "prepare-agent-enrollment",
            "--output-dir",
            bundle.to_str().unwrap(),
            "--hub-public",
            hub_public.to_str().unwrap(),
            "--grant-public",
            grant_public.to_str().unwrap(),
            "--tls-root-der",
            tls_root.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(!second.status.success());
    let _ = fs::remove_dir_all(root);
}
