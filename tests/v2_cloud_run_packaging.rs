const DOCKERFILE: &str = include_str!("../packaging/cloud-run/Dockerfile");
const ENTRYPOINT: &str = include_str!("../packaging/cloud-run/entrypoint.sh");
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
    assert!(DOCKERFILE.contains("ENTRYPOINT [\"/usr/local/bin/cumg-cloud-run-entrypoint\"]"));
    assert!(DOCKERFILE.contains("CMD [\"/usr/local/bin/v2_hub\"]"));
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
            "!packaging/cloud-run/entrypoint.sh",
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
    let combined = [DOCKERFILE, ENTRYPOINT, CLOUDBUILD, README].join("\n");
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

#[test]
fn cloud_run_entrypoint_has_explicit_secret_allowlist() {
    for source in [
        "hub/value",
        "grant/value",
        "device/value",
        "postgres/value",
        "northbound-policy/value",
        "handoff-policy/value",
        "oauth-introspection/value",
    ] {
        assert!(ENTRYPOINT.contains(source));
    }
    assert!(ENTRYPOINT.contains("umask 077"));
    assert!(ENTRYPOINT.contains("chmod 700 \"$private_root\""));
    assert!(ENTRYPOINT.contains("chmod 600 \"$temp_path\""));
    assert!(ENTRYPOINT.contains("exec \"$@\""));
}

#[cfg(unix)]
mod unix {
    use std::{
        env, fs,
        os::unix::fs::{MetadataExt, PermissionsExt},
        path::PathBuf,
        process::{self, Command},
    };

    const ENTRYPOINT_PATH: &str = "packaging/cloud-run/entrypoint.sh";

    struct FixtureRoot(PathBuf);

    impl FixtureRoot {
        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }

    impl Drop for FixtureRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn fixture() -> FixtureRoot {
        let root = env::temp_dir().join(format!(
            "cumg-cloud-run-packaging-{}-{}",
            process::id(),
            rand::random::<u64>()
        ));
        fs::create_dir(&root).unwrap();
        let mount = root.join("mount");
        for (rel, value) in [
            ("hub/value", "hub-secret"),
            ("grant/value", "grant-secret"),
            ("device/value", "device-public"),
            ("postgres/value", "db-password"),
            ("northbound-policy/value", "northbound-policy"),
            ("handoff-policy/value", "handoff-policy"),
            ("oauth-introspection/value", "oauth-secret"),
        ] {
            let path = mount.join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, value).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o444)).unwrap();
        }
        FixtureRoot(root)
    }

    #[test]
    fn materializes_platform_mounts_as_private_runtime_files() {
        let root = fixture();
        let mount = root.path().join("mount");
        let private = root.path().join("private");

        let output = Command::new("/bin/sh")
            .arg(ENTRYPOINT_PATH)
            .arg("/bin/sh")
            .arg("-c")
            .arg("printf '%s\\n' \"$CUMG_V2_HUB_SECRET_FILE\" \"$CUMG_V2_POSTGRES_PASSWORD_FILE\"")
            .env("CUMG_V2_CLOUD_RUN_SECRET_MOUNT_DIR", &mount)
            .env("CUMG_V2_CLOUD_RUN_PRIVATE_DIR", &private)
            .output()
            .unwrap();

        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(stdout.contains(private.join("hub.key").to_str().unwrap()));
        assert!(stdout.contains(private.join("postgres.password").to_str().unwrap()));

        for name in [
            "hub.key",
            "grant.key",
            "device.pub",
            "postgres.password",
            "northbound-policy.json",
            "handoff-policy.json",
            "oauth-introspection.secret",
        ] {
            let metadata = fs::metadata(private.join(name)).unwrap();
            assert_eq!(metadata.mode() & 0o777, 0o600, "unexpected mode for {name}");
        }
        assert_eq!(fs::metadata(&private).unwrap().mode() & 0o777, 0o700);
        assert_eq!(
            fs::read_to_string(private.join("hub.key")).unwrap(),
            "hub-secret"
        );
    }

    #[test]
    fn missing_required_mount_fails_before_exec() {
        let root = fixture();
        let mount = root.path().join("mount");
        let private = root.path().join("private");
        fs::remove_file(mount.join("postgres/value")).unwrap();

        let output = Command::new("/bin/sh")
            .arg(ENTRYPOINT_PATH)
            .arg("/bin/sh")
            .arg("-c")
            .arg("exit 0")
            .env("CUMG_V2_CLOUD_RUN_SECRET_MOUNT_DIR", &mount)
            .env("CUMG_V2_CLOUD_RUN_PRIVATE_DIR", &private)
            .output()
            .unwrap();

        assert_eq!(output.status.code(), Some(78));
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(stderr.contains("postgres/value"));
        assert!(!stderr.contains("db-password"));
    }
}
