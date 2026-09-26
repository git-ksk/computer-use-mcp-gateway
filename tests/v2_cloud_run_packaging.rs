const DOCKERFILE: &str = include_str!("../packaging/cloud-run/Dockerfile");
const DOCKERIGNORE: &str = include_str!("../.dockerignore");
const GCLOUDIGNORE: &str = include_str!("../.gcloudignore");
const CLOUDBUILD: &str = include_str!("../packaging/cloud-run/cloudbuild.yaml");
const README: &str = include_str!("../packaging/cloud-run/README.md");

#[test]
fn cloud_run_image_is_non_root_and_uses_hosted_safety_defaults() {
    assert!(DOCKERFILE.contains("FROM rust:1.88-bookworm AS builder"));
    assert!(DOCKERFILE.contains("cargo build --locked --release --bin v2_hub"));
    assert!(
        DOCKERFILE.contains("COPY --from=builder /src/target/release/v2_hub /usr/local/bin/v2_hub")
    );
    assert!(DOCKERFILE.contains("USER 65532:65532"));
    assert!(DOCKERFILE.contains("CUMG_V2_HOSTED_PROFILE=true"));
    assert!(DOCKERFILE.contains("CUMG_V2_MAX_AGENT_SESSION_LIFETIME_SECS=3300"));
    assert!(DOCKERFILE.contains("CUMG_V2_AGENT_SESSION_REAUTH_DRAIN_SECS=30"));
    assert!(DOCKERFILE.contains("CUMG_V2_DRAIN_TIMEOUT_SECS=8"));
    assert!(DOCKERFILE.contains("ENTRYPOINT [\"/usr/local/bin/v2_hub\"]"));
}

#[test]
fn cloud_run_build_context_is_deny_all_allowlisted() {
    for ignore in [DOCKERIGNORE, GCLOUDIGNORE] {
        assert_eq!(ignore.lines().next(), Some("**"));
        for required in [
            "!Cargo.toml",
            "!Cargo.lock",
            "!build.rs",
            "!src/",
            "!src/**",
            "!proto/",
            "!proto/**",
            "!packaging/cloud-run/Dockerfile",
        ] {
            assert!(
                ignore.lines().any(|line| line == required),
                "missing {required}"
            );
        }
    }

    assert!(GCLOUDIGNORE.contains("!packaging/cloud-run/cloudbuild.yaml"));
}

#[test]
fn cloud_run_packaging_keeps_concrete_deployment_values_out_of_source() {
    let combined = [DOCKERFILE, CLOUDBUILD, README].join("\n");
    for forbidden in [
        "postgresql://",
        "postgres://",
        "neon.tech",
        "pkg.dev/",
        "@mcp-runtime",
        "secretmanager.googleapis.com/projects/",
    ] {
        assert!(
            !combined.contains(forbidden),
            "concrete deployment material must stay outside source control: {forbidden}"
        );
    }

    assert!(CLOUDBUILD.contains("${_IMAGE}"));
    assert!(README.contains("supplied outside source control"));
}
