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
    assert!(upgrade.contains("CUMG_V2_MACOS_CODESIGN_IDENTITY"));
    // Name collisions with revoked/older certificates must not make codesign ambiguous.
    // The helper resolves exactly one valid identity to its SHA-1 selector and otherwise fails closed.
    assert!(upgrade.contains("MACOS_CODESIGN_SELECTOR"));
    assert!(upgrade.contains("macos_codesign_identity_unavailable_or_ambiguous"));
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
