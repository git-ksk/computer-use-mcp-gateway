use std::process::Command;

#[test]
fn agent_help_exposes_distinct_required_cwd_and_file_root_options() {
    let output = Command::new(env!("CARGO_BIN_EXE_v2_agent"))
        .arg("--help")
        .output()
        .expect("v2_agent --help");
    assert!(output.status.success());
    let help = String::from_utf8(output.stdout).expect("utf8 help");
    assert!(help.contains("--allowed-cwd-root"));
    assert!(help.contains("--allowed-file-root"));
    assert!(help.contains("CUMG_V2_ALLOWED_FILE_ROOTS"));
    assert!(help.contains("--workspace-mutation-mode"));
    assert!(help.contains("--allowed-write-root"));
    assert!(help.contains("--denied-write-subpath"));
}

fn minimum_args() -> Vec<&'static str> {
    vec![
        "--hub-endpoint",
        "https://127.0.0.1:7443",
        "--hub-domain",
        "localhost",
        "--device-id",
        "dev-test",
        "--device-secret-file",
        "/tmp/cumg-missing-device.key",
        "--hub-public-key-file",
        "/tmp/cumg-missing-hub.pub",
        "--grant-public-key-file",
        "/tmp/cumg-missing-grant.pub",
        "--tls-root-der-file",
        "/tmp/cumg-missing-tls.der",
        "--state-dir",
        "/tmp/cumg-agent-state",
        "--allowed-cwd-root",
        "/tmp",
        "--allowed-file-root",
        "/tmp",
    ]
}

#[test]
fn workspace_mutation_defaults_disabled_and_refuses_implicit_write_authority() {
    let mut args = minimum_args();
    args.extend(["--allowed-write-root", "/tmp/reviewed-write"]);
    let output = Command::new(env!("CARGO_BIN_EXE_v2_agent"))
        .args(args)
        .output()
        .expect("v2_agent default mutation mode");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("workspace mutation is disabled"),
        "{stderr}"
    );
}

#[test]
fn workspace_mutation_enabled_requires_explicit_write_root() {
    let mut args = minimum_args();
    args.extend(["--workspace-mutation-mode", "enabled"]);
    let output = Command::new(env!("CARGO_BIN_EXE_v2_agent"))
        .args(args)
        .output()
        .expect("v2_agent enabled mutation mode");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("no explicit allowed write root"),
        "{stderr}"
    );
}
