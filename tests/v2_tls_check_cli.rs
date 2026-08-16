use rcgen::{CertifiedKey, generate_simple_self_signed};
use std::fs;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn temp_dir() -> std::path::PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("cumg-v2-tls-check-{unique}"))
}

#[test]
fn tls_check_emits_machine_visible_alert_and_nonzero_for_warning_window() {
    let directory = temp_dir();
    fs::create_dir(&directory).unwrap();
    let CertifiedKey { cert, .. } = generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let certificate = directory.join("server.pem");
    fs::write(&certificate, cert.pem()).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&certificate, fs::Permissions::from_mode(0o644)).unwrap();
    }

    let healthy = Command::new(env!("CARGO_BIN_EXE_v2_tls_check"))
        .args([
            "--certificate",
            certificate.to_str().unwrap(),
            "--format",
            "pem",
            "--warn-before-secs",
            "0",
        ])
        .output()
        .unwrap();
    assert!(healthy.status.success());
    assert!(String::from_utf8_lossy(&healthy.stdout).contains("CUMG_TLS_EXPIRY_OK status=healthy"));

    let warning = Command::new(env!("CARGO_BIN_EXE_v2_tls_check"))
        .args([
            "--certificate",
            certificate.to_str().unwrap(),
            "--format",
            "pem",
            "--warn-before-secs",
            &i64::MAX.to_string(),
        ])
        .output()
        .unwrap();
    assert_eq!(warning.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&warning.stderr).contains("CUMG_TLS_EXPIRY_ALERT status=expiring")
    );

    let invalid = directory.join("invalid.der");
    fs::write(&invalid, b"not-a-certificate").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&invalid, fs::Permissions::from_mode(0o644)).unwrap();
    }
    let invalid_output = Command::new(env!("CARGO_BIN_EXE_v2_tls_check"))
        .args([
            "--certificate",
            invalid.to_str().unwrap(),
            "--format",
            "der",
        ])
        .output()
        .unwrap();
    assert_eq!(invalid_output.status.code(), Some(4));
    assert!(
        String::from_utf8_lossy(&invalid_output.stderr)
            .contains("CUMG_TLS_EXPIRY_ALERT status=invalid error_code=invalid_certificate")
    );
    let _ = fs::remove_dir_all(directory);
}
