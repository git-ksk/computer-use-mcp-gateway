#[test]
fn single_mac_handoff_is_agent_owned_and_stably_codesigned_for_tcc() {
    let hub = include_str!("../packaging/launchd/single-mac/com.github.git-ksk.cumg-v2-hub.plist");
    let agent =
        include_str!("../packaging/launchd/single-mac/com.github.git-ksk.cumg-v2-agent.plist");
    let upgrade = include_str!("../scripts/v2-single-mac-upgrade.sh");

    // Hub owns only the local operator relay. Capture/runtime/TURN configuration
    // belongs to the controlled Agent host.
    assert!(hub.contains("CUMG_V2_HANDOFF_CONTROL_SOCKET"));
    assert!(!hub.contains("CUMG_V2_HANDOFF_RUNTIME_COMMAND"));
    assert!(!hub.contains("CUMG_V2_HANDOFF_RUNTIME_SCRIPT"));
    assert!(!hub.contains("CUMG_V2_HANDOFF_RUNTIME_ENV_FILE"));

    assert!(agent.contains("CUMG_V2_HANDOFF_RUNTIME_COMMAND"));
    assert!(agent.contains("CUMG_V2_HANDOFF_RUNTIME_SCRIPT"));
    assert!(agent.contains("CUMG_V2_HANDOFF_RUNTIME_ENV_FILE"));
    assert!(agent.contains("CUMG_MUTATION_AUTHORITY_DIR"));
    assert!(agent.contains("@ROOT@/mutation-authority"));

    // Ad-hoc signing has a cdhash-based designated requirement and therefore
    // cannot provide a durable TCC identity across rebuilt binaries. The reviewed
    // upgrade path requires a real Apple identity + Team ID and has no ad-hoc fallback.
    assert!(upgrade.contains("CUMG_V2_MACOS_CODESIGN_FINGERPRINT"));
    assert!(upgrade.contains("CUMG_V2_MACOS_CODESIGN_IDENTITY"));
    // A reviewed fingerprint is preferred; display-name fallback must still resolve uniquely.
    // The helper resolves exactly one valid identity to its SHA-1 selector and otherwise fails closed.
    assert!(upgrade.contains("MACOS_CODESIGN_SELECTOR"));
    assert!(upgrade.contains("macos_codesign_identity_unavailable_ambiguous_or_team_mismatch"));
    assert!(upgrade.contains("fingerprint.upper()"));
    assert!(upgrade.contains("CUMG_V2_MACOS_TEAM_ID"));
    assert!(upgrade.contains("certificate leaf[subject.OU]"));
    assert!(upgrade.contains("anchor apple generic"));
    assert!(upgrade.contains("com.github.git-ksk.cumg-v2-agent"));
    assert!(upgrade.contains("com.github.git-ksk.cumg-v2-recover"));
    assert!(upgrade.contains("recovery_cli_stable_codesign_failed"));
    assert!(upgrade.contains("--bin v2_recover"));
    assert!(upgrade.contains("build-macos-recovery-helper.sh"));
    assert!(upgrade.contains("v2_recovery_enclave_helper"));
    assert!(upgrade.contains("com.github.git-ksk.cumg-v2-recovery-helper"));
    assert!(upgrade.contains("recovery_helper_stable_codesign_failed"));
    assert!(upgrade.contains("com.github.git-ksk.cumg-v2-handoff-webrtc-host"));
    assert!(upgrade.contains("CUMG_V2_HANDOFF_WEBRTC_HOST_EXECUTABLE"));
    assert!(upgrade.contains("$ROOT\"/v2/handoff/*"));
    assert!(upgrade.contains("designated\" != *\"cdhash\""));
    assert!(!upgrade.contains("codesign --force --sign -"));
}

#[test]
fn single_mac_upgrade_pins_handoff_schema_and_cleanup_lifecycle() {
    let upgrade = include_str!("../scripts/v2-single-mac-upgrade.sh");
    let cleanup = include_str!("../scripts/v2_handoff_runtime_cleanup.py");
    let preflight = include_str!("../scripts/v2_handoff_runtime_preflight.py");

    assert!(upgrade.contains("CUMG_V2_EXPECTED_HANDOFF_COMMIT"));
    assert!(upgrade.contains("CUMG_V2_HANDOFF_SOURCE_ROOT"));
    assert!(upgrade.contains("handoff_source_commit"));
    assert!(upgrade.contains("runtime-generation-manifest.json"));
    assert!(upgrade.contains("npm ci --omit=dev --ignore-scripts --no-audit --no-fund"));
    assert!(upgrade.contains("staged_handoff_runtime_import_failed"));
    assert!(upgrade.contains("--require-export WindowHandoffAdapter"));
    assert!(upgrade.contains("--require-export TerminalHandoffAdapter"));
    assert!(!upgrade.contains("dist/experimental/terminal-pty.js"));
    assert!(!upgrade.contains("ExperimentalTerminalPtyAuthority"));
    assert!(!upgrade.contains("dist/experimental/terminal-webrtc.js"));
    assert!(!upgrade.contains("ExperimentalTerminalWebRtcTakeover"));
    assert!(upgrade.contains("HANDOFF_RUNTIME_COMMAND_RESOLVED"));
    assert!(upgrade.contains("v2_handoff_runtime_preflight.py"));
    assert!(upgrade.contains("verify-import"));
    assert!(preflight.contains("os.path.realpath"));
    assert!(preflight.contains("verify_import"));
    assert!(preflight.contains("subprocess.run"));
    assert!(preflight.contains("verify_generation"));
    assert!(upgrade.contains("verify-generation"));
    assert!(upgrade.contains("RUNTIME_REUSE_OK"));
    assert!(upgrade.contains("NEW_HANDOFF_RUNTIME_CREATED"));
    assert!(upgrade.contains("existing_handoff_host_codesign_invalid"));
    assert!(upgrade.contains("handoff_runtime_generation_existing_validation_failed"));
    assert!(upgrade.contains("node_modules/werift/package.json"));
    assert!(upgrade.contains("install_handoff_runtime_dependencies"));
    assert!(upgrade.contains("node_modules/.bin"));
    assert!(upgrade.contains("followlinks=False"));
    assert!(upgrade.contains("handoff_runtime_dependencies_install_or_symlink_validation_failed"));
    assert!(upgrade.contains("handoff_not_idle_or_status_unavailable"));
    assert!(upgrade.contains("hub_agent_schema_version"));
    assert!(upgrade.contains("\"schema_version\": 3"));
    assert!(upgrade.contains("--handoff-control-socket"));
    assert!(upgrade.contains("v2_handoff_runtime_cleanup.py"));
    assert!(upgrade.contains("--health-confirmed"));

    assert!(cleanup.contains("runtime_candidate_contains_symlink"));
    assert!(cleanup.contains("runtime_candidate_contains_forbidden_material"));
    assert!(cleanup.contains("active_runtime_unresolved"));
    assert!(cleanup.contains("archive_is_self_contained"));
    assert!(cleanup.contains("checkpoint.json"));
    assert!(cleanup.contains("checkpoint.key"));
    assert!(cleanup.contains("managed-runtime.env"));
}

#[test]
fn single_mac_upgrade_rejects_conflicting_launchd_families_and_retires_alternates() {
    let upgrade = include_str!("../scripts/v2-single-mac-upgrade.sh");
    let guard = include_str!("../scripts/v2_launchd_topology_guard.py");

    assert!(upgrade.contains("v2_launchd_topology_guard.py"));
    let preflight_exit = upgrade
        .find("if [[ \"$PRELIGHT_ONLY\" == \"1\" ]]; then")
        .unwrap();
    let topology_check = upgrade.find("\"$LAUNCHD_TOPOLOGY_GUARD\" check").unwrap();
    let retire_alternates = upgrade.find("retire-alternates").unwrap();
    assert!(topology_check < preflight_exit);
    assert!(preflight_exit < retire_alternates);
    assert!(upgrade.contains("retire-alternates"));
    assert!(upgrade.contains("alternate_launchd_retirement_failed"));
    assert!(upgrade.contains("conflicting_launchd_topology"));
    assert!(guard.contains("com.github.git-ksk.cumg-v2-hub"));
    assert!(guard.contains("com.github.git-ksk.cumg-v2-agent"));
    assert!(guard.contains("com.sawadakousuke.cumg-v2-hub"));
    assert!(guard.contains("com.sawadakousuke.cumg-v2-agent"));
    assert!(guard.contains("conflicting_launchd_labels"));
    assert!(guard.contains("mixed_launchd_families"));
    assert!(guard.contains("alternate_launchd_disable_failed"));
    assert!(!guard.contains("unlink("));
    assert!(!guard.contains("remove("));
}

#[test]
fn single_mac_upgrade_enforces_cross_control_plane_mutation_authority() {
    let upgrade = include_str!("../scripts/v2-single-mac-upgrade.sh");
    let preflight = include_str!("../scripts/v2_mutation_authority_preflight.py");

    assert!(upgrade.contains(r#"MUTATION_AUTHORITY_DIR="$ROOT/mutation-authority""#));
    assert!(upgrade.contains("v2_mutation_authority_preflight.py"));
    assert!(upgrade.contains("--allow-v2-uninitialized"));
    assert!(preflight.contains("legacy_gateway_unfenced"));
    assert!(upgrade.contains("mutation-authority-init"));
    assert!(upgrade.contains("--owner v2"));
    assert!(upgrade.contains("CUMG_MUTATION_AUTHORITY_DIR"));
    assert!(upgrade.contains(r#"--mutation-authority-dir "$MUTATION_AUTHORITY_DIR""#));
    assert!(upgrade.contains(r#"fail_poststart "mutation_authority_preflight""#));
    assert!(preflight.contains("shared_mutation_authority_missing"));
    assert!(preflight.contains("shared_mutation_authority_mismatch"));
    assert!(preflight.contains("migration=required"));
}

#[test]
fn single_mac_upgrade_uses_explicit_one_shot_launchd_maintenance_jobs() {
    let upgrade = include_str!("../scripts/v2-single-mac-upgrade.sh");
    let runner = include_str!("../scripts/v2_launchd_maintenance_job.py");

    assert!(upgrade.contains("v2_launchd_maintenance_job.py"));
    assert!(upgrade.contains("assert-clear"));
    assert!(upgrade.contains("CUMG_V2_MAINTENANCE_JOB_LABEL"));
    assert!(upgrade.contains("current_maintenance_job_pid_mismatch"));
    assert!(runner.contains("unsafe_maintenance_job_dir"));
    assert!(runner.contains("unsafe_maintenance_plist"));
    assert!(runner.contains("O_NOFOLLOW"));
    assert!(runner.contains("\"RunAtLoad\": True"));
    assert!(runner.contains("\"KeepAlive\": False"));
    assert!(runner.contains("launchctl, \"bootstrap\""));
    assert!(runner.contains("launchctl, \"bootout\""));
    assert!(runner.contains("automatic_maintenance_relaunch_detected"));
    assert!(runner.contains("stale_maintenance_jobs"));
    assert!(runner.contains("com.git-ksk.cumg-v2-upgrade-"));
    assert!(!runner.contains("[launchctl, \"submit\""));
    assert!(!upgrade.contains("launchctl submit "));
}

#[test]
fn reviewed_handoff_manifest_matches_release_workflow_identity_and_version_guard() {
    let manifest: serde_json::Value =
        serde_json::from_str(include_str!("../packaging/release/single-mac-handoff.json")).unwrap();
    let workflow = include_str!("../.github/workflows/release-candidate.yml");

    assert_eq!(manifest["schema_version"], 1);
    assert_eq!(manifest["repository"], "git-ksk/mcp-execution-handoff");
    let source_commit = manifest["source_commit"].as_str().unwrap();
    assert_eq!(source_commit.len(), 40);
    assert!(source_commit.bytes().all(|byte| byte.is_ascii_hexdigit()));
    let package_version = manifest["package_version"].as_str().unwrap();
    assert!(!package_version.is_empty());

    assert!(workflow.contains(&format!("HANDOFF_SOURCE_COMMIT: {source_commit}")));
    assert!(workflow.contains("pinned_version="));
    assert!(workflow.contains("handoff_version="));
    assert!(workflow.contains("Handoff package version differs from reviewed package manifest"));
}
