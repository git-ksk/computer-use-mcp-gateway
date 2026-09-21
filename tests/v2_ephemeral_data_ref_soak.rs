use computer_use_mcp_gateway::v2_ephemeral_data_refs::{
    AgentEphemeralBinding, AgentEphemeralDataLimits, AgentEphemeralDataStore, EphemeralDataKind,
};
use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

const CYCLES: u64 = 200;
const OBJECTS_PER_CYCLE: usize = 8;
const OBJECT_BYTES: usize = 8 * 1024;
const WARMUP_CYCLE: u64 = 50;
const RSS_GROWTH_ALLOWANCE_BYTES: u64 = 32 * 1024 * 1024;

fn temp_parent() -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "cumg-v2-ephemeral-soak-{}-{}-{:016x}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        rand::random::<u64>()
    ));
    fs::create_dir_all(&path).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    path
}

fn disk_usage(root: &Path) -> (usize, u64) {
    if !root.exists() {
        return (0, 0);
    }
    let mut count = 0usize;
    let mut bytes = 0u64;
    for entry in fs::read_dir(root).unwrap() {
        let entry = entry.unwrap();
        let metadata = entry.metadata().unwrap();
        if metadata.is_file() {
            count = count.saturating_add(1);
            bytes = bytes.saturating_add(metadata.len());
        }
    }
    (count, bytes)
}

#[cfg(target_os = "linux")]
fn current_rss_bytes() -> Option<u64> {
    let status = fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|line| line.starts_with("VmRSS:"))?;
    let kib = line.split_whitespace().nth(1)?.parse::<u64>().ok()?;
    kib.checked_mul(1024)
}

#[cfg(not(target_os = "linux"))]
fn current_rss_bytes() -> Option<u64> {
    None
}

#[test]
fn ephemeral_data_stage_prune_soak_keeps_disk_and_rss_bounded() {
    let parent = temp_parent();
    let object_bytes = u64::try_from(OBJECT_BYTES).unwrap();
    let max_total_bytes = object_bytes * u64::try_from(OBJECTS_PER_CYCLE).unwrap();
    let limits = AgentEphemeralDataLimits {
        max_objects: OBJECTS_PER_CYCLE,
        max_total_bytes,
        max_object_bytes: object_bytes,
        max_read_bytes: OBJECT_BYTES,
        ttl_ms: 1,
    };
    let mut store = AgentEphemeralDataStore::new(&parent, limits).unwrap();
    let binding = AgentEphemeralBinding::new(
        "dev-soak",
        1,
        1,
        None,
        EphemeralDataKind::DirectoryContinuation,
    )
    .unwrap();
    let payload = vec![0x5a; OBJECT_BYTES];
    let storage_root = parent.join("workspace-ephemeral-data");
    let mut warmup_rss = None;

    for cycle in 0..CYCLES {
        let now_ms = cycle.saturating_mul(10);
        for _ in 0..OBJECTS_PER_CYCLE {
            store.stage(binding.clone(), &payload, now_ms).unwrap();
        }

        assert_eq!(store.len(), OBJECTS_PER_CYCLE);
        assert_eq!(store.total_bytes(), max_total_bytes);
        assert_eq!(
            disk_usage(&storage_root),
            (OBJECTS_PER_CYCLE, max_total_bytes)
        );

        if cycle == WARMUP_CYCLE {
            warmup_rss = current_rss_bytes();
        }

        assert_eq!(
            store.prune(now_ms.saturating_add(2)).unwrap(),
            OBJECTS_PER_CYCLE
        );
        assert!(store.is_empty());
        assert_eq!(store.total_bytes(), 0);
        assert_eq!(disk_usage(&storage_root), (0, 0));
    }

    if let (Some(warmup), Some(final_rss)) = (warmup_rss, current_rss_bytes()) {
        assert!(
            final_rss <= warmup.saturating_add(RSS_GROWTH_ALLOWANCE_BYTES),
            "ephemeral ref soak RSS did not plateau: warmup={warmup} final={final_rss} allowance={RSS_GROWTH_ALLOWANCE_BYTES}"
        );
    }

    drop(store);
    let _ = fs::remove_dir_all(parent);
}
