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
}
