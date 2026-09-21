use computer_use_mcp_gateway::v2_managed_job::ManagedJobState;
use computer_use_mcp_gateway::v2_playwright_sandbox::{
    PlaywrightSandboxConfig, PlaywrightSandboxRunner, PlaywrightTestRequest,
};
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

#[test]
#[ignore = "trusted real Docker/Podman Playwright provider acceptance; requires explicit ACK, digest-pinned image, and prepared workspace"]
fn real_playwright_provider_runs_offline_and_proves_cleanup() {
    assert_eq!(
        std::env::var("CUMG_V2_PLAYWRIGHT_PROVIDER_E2E_ACK")
            .ok()
            .as_deref(),
        Some("1"),
        "explicit provider acceptance ACK required"
    );
    let runtime = PathBuf::from(
        std::env::var("CUMG_V2_PLAYWRIGHT_RUNTIME").expect("CUMG_V2_PLAYWRIGHT_RUNTIME required"),
    );
    let image =
        std::env::var("CUMG_V2_PLAYWRIGHT_IMAGE").expect("CUMG_V2_PLAYWRIGHT_IMAGE required");
    let workspace = PathBuf::from(
        std::env::var("CUMG_V2_PLAYWRIGHT_WORKSPACE")
            .expect("CUMG_V2_PLAYWRIGHT_WORKSPACE required"),
    );
    let state = workspace.parent().unwrap().join(format!(
        ".cumg-playwright-provider-acceptance-{}-{}",
        std::process::id(),
        rand::random::<u64>()
    ));
    std::fs::create_dir_all(&state).unwrap();

    let config = PlaywrightSandboxConfig::new(runtime, image, vec![workspace.clone()]).unwrap();
    let runner =
        PlaywrightSandboxRunner::new(config, &state, "provider-acceptance-device").unwrap();
    runner.probe_provider().unwrap();
    runner.recover_provider_orphans().unwrap();

    let (locator, _) = runner
        .start(&PlaywrightTestRequest {
            workspace: workspace.to_string_lossy().into_owned(),
            test_paths: vec!["tests/cumg-release.spec.js".into()],
            project: None,
            grep: None,
            workers: Some(1),
            hard_lifetime_ms: 60_000,
        })
        .unwrap();

    let deadline = Instant::now() + Duration::from_secs(75);
    let terminal = loop {
        let status = runner.status(&locator).unwrap();
        if status.job.state.is_terminal() {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "real provider test did not become terminal"
        );
        thread::sleep(Duration::from_millis(100));
    };

    assert_eq!(terminal.job.state, ManagedJobState::Completed);
    if terminal.job.exit_code != Some(0) {
        for stream in [
            computer_use_mcp_gateway::v2_m0::ProcessOutputStream::Stdout,
            computer_use_mcp_gateway::v2_m0::ProcessOutputStream::Stderr,
        ] {
            let output = runner.output(&locator, stream, 0, 64 * 1024).unwrap();
            eprintln!(
                "provider output {:?}: {}",
                stream,
                String::from_utf8_lossy(&output.bytes)
            );
        }
    }
    assert_eq!(terminal.job.exit_code, Some(0));
    assert!(!runner.has_indeterminate_termination().unwrap());
    runner.shutdown_all().unwrap();
    assert_eq!(runner.active_count().unwrap(), 0);
    std::fs::remove_dir_all(state).unwrap();
}
