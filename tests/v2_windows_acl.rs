#![cfg(windows)]

use computer_use_mcp_gateway::v2_m1_keys::{
    KeyMaterialError, create_new_device_identity, load_device_identity, load_trusted_text,
    write_new_trusted_text,
};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

fn temp_directory() -> PathBuf {
    let directory = std::env::temp_dir().join(format!(
        "cumg-windows-acl-integration-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir(&directory).unwrap();
    lock_down(&directory, true);
    directory
}

fn current_identity() -> String {
    let output = Command::new("whoami").output().expect("run whoami");
    assert!(output.status.success());
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn icacls(path: &Path, args: &[String]) {
    let mut command = Command::new("icacls.exe");
    command.arg(path);
    for argument in args {
        command.arg(argument);
    }
    let output = command.output().expect("run icacls");
    assert!(
        output.status.success(),
        "icacls failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn lock_down(path: &Path, directory: bool) {
    let identity = current_identity();
    let rights = if directory { "(OI)(CI)F" } else { "F" };
    icacls(
        path,
        &[
            "/inheritance:r".into(),
            "/grant:r".into(),
            format!("{identity}:{rights}"),
            format!("*S-1-5-18:{rights}"),
            format!("*S-1-5-32-544:{rights}"),
            "/Q".into(),
        ],
    );
}

fn grant_users(path: &Path, rights: &str) {
    icacls(
        path,
        &[
            "/grant".into(),
            format!("*S-1-5-32-545:{rights}"),
            "/Q".into(),
        ],
    );
}

#[test]
fn runtime_rejects_unrelated_secret_read_and_public_trust_write() {
    let directory = temp_directory();

    let secret = directory.join("device.key");
    create_new_device_identity(&secret).unwrap();
    grant_users(&secret, "R");
    assert!(matches!(
        load_device_identity(&secret),
        Err(KeyMaterialError::UnsafeSecretPermissions)
    ));

    let trust = directory.join("trust.txt");
    write_new_trusted_text(&trust, "public-trust-material").unwrap();
    grant_users(&trust, "R");
    assert_eq!(
        load_trusted_text(&trust, 1024).unwrap(),
        "public-trust-material\n"
    );
    grant_users(&trust, "W");
    assert!(matches!(
        load_trusted_text(&trust, 1024),
        Err(KeyMaterialError::WritableTrustAnchor)
    ));

    fs::remove_dir_all(directory).unwrap();
}
