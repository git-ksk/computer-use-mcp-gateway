use std::process::Command;

#[test]
fn hub_help_exposes_oidc_jwt_authentication_settings() {
    let output = Command::new(env!("CARGO_BIN_EXE_v2_hub"))
        .arg("--help")
        .output()
        .expect("v2_hub --help");
    assert!(output.status.success());
    let help = String::from_utf8(output.stdout).expect("utf8 help");
    for expected in [
        "--oidc-audience",
        "--oidc-jwks-uri",
        "--oidc-allowed-algorithms",
        "CUMG_V2_OIDC_AUDIENCE",
        "CUMG_V2_OIDC_JWKS_URI",
        "CUMG_V2_OIDC_ALLOWED_ALGORITHMS",
    ] {
        assert!(
            help.contains(expected),
            "missing {expected} in v2_hub --help"
        );
    }
}
