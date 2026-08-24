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

    assert!(upgrade.contains("CUMG_V2_EXPECTED_HANDOFF_COMMIT"));
    assert!(upgrade.contains("CUMG_V2_HANDOFF_SOURCE_ROOT"));
    assert!(upgrade.contains("handoff_source_commit"));
    assert!(upgrade.contains("runtime-generation-manifest.json"));
    assert!(upgrade.contains("npm ci --omit=dev --ignore-scripts --no-audit --no-fund"));
    assert!(upgrade.contains("staged_handoff_runtime_import_failed"));
    assert!(upgrade.contains("HANDOFF_RUNTIME_COMMAND_RESOLVED"));
    assert!(upgrade.contains("os.path.realpath"));
    assert!(upgrade.contains("node_modules/werift/package.json"));
    assert!(upgrade.contains("install_handoff_runtime_dependencies"));
    assert!(upgrade.contains("node_modules/.bin"));
    assert!(upgrade.contains("followlinks=False"));
    assert!(upgrade.contains("handoff_runtime_dependencies_install_or_symlink_validation_failed"));
    assert!(upgrade.contains("handoff_not_idle_or_status_unavailable"));
    assert!(upgrade.contains("hub_agent_schema_version"));
    assert!(upgrade.contains("\"schema_version\": 2"));
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
