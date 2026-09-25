use computer_use_mcp_gateway::{
    v2_execution_safety::AuthoritativeOperationController,
    v2_hub_state_store::{AsyncHubAuthoritativeStateStore, HubStateStoreError, HubWriterEpoch},
    v2_m0::{DeviceIdentity, DeviceRegistry},
    v2_m0_execution::AdmissionLimits,
    v2_m1_persistence::{HubPersistentState, MAX_CHECKPOINT_BYTES},
    v2_postgres_hub_state_store::{PostgresHubStateStore, PostgresHubStateStoreConfig},
};
use std::time::Duration;
use tokio_postgres::NoTls;

fn initial_state() -> HubPersistentState {
    let mut registry = DeviceRegistry::default();
    registry.provision_trusted_device(DeviceIdentity::generate().verifying_key());
    let execution = AuthoritativeOperationController::new(AdmissionLimits {
        max_global_active: 1,
        max_queued_per_device: 1,
    })
    .unwrap();
    HubPersistentState::capture(&registry, &execution)
}

fn config(state_key: &str) -> Option<PostgresHubStateStoreConfig> {
    std::env::var("CUMG_TEST_POSTGRES_URL").ok()?;
    let host = std::env::var("CUMG_TEST_POSTGRES_HOST").unwrap_or_else(|_| "127.0.0.1".into());
    let port = std::env::var("CUMG_TEST_POSTGRES_PORT")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(5432);
    let database = std::env::var("CUMG_TEST_POSTGRES_DB").unwrap_or_else(|_| "cumg".into());
    let user = std::env::var("CUMG_TEST_POSTGRES_USER").unwrap_or_else(|_| "postgres".into());
    let password =
        std::env::var("CUMG_TEST_POSTGRES_PASSWORD").unwrap_or_else(|_| "postgres".into());
    Some(
        PostgresHubStateStoreConfig::new(host, database, user, state_key)
            .unwrap()
            .with_port(port)
            .unwrap()
            .with_password(password)
            .unwrap()
            .with_timeouts(Duration::from_secs(5), Duration::from_secs(5))
            .unwrap(),
    )
}

async fn prepare_schema() -> bool {
    let Ok(url) = std::env::var("CUMG_TEST_POSTGRES_URL") else {
        return false;
    };
    let Ok((client, connection)) = tokio_postgres::connect(&url, NoTls).await else {
        panic!("CUMG_TEST_POSTGRES_URL is set but PostgreSQL is unavailable");
    };
    tokio::spawn(async move {
        connection.await.unwrap();
    });
    client
        .batch_execute("SELECT pg_advisory_lock(391284);")
        .await
        .unwrap();
    let migration = client
        .batch_execute(include_str!(
            "../packaging/postgres/001_hosted_hub_state.sql"
        ))
        .await;
    client
        .batch_execute("SELECT pg_advisory_unlock(391284);")
        .await
        .unwrap();
    migration.unwrap();
    true
}

#[tokio::test]
async fn postgres_backend_conforms_to_writer_epoch_and_cas_contract() {
    if !prepare_schema().await {
        return;
    }
    let state_key = format!("device-a-{}", rand::random::<u64>());
    let left = PostgresHubStateStore::connect(config(&state_key).unwrap())
        .await
        .unwrap();
    let right = PostgresHubStateStore::connect(config(&state_key).unwrap())
        .await
        .unwrap();
    let initial = initial_state();

    let writer_a = left.acquire_writer_async(&initial).await.unwrap();
    let writer_b = right.acquire_writer_async(&initial).await.unwrap();
    assert_eq!(writer_a.writer_epoch, HubWriterEpoch(1));
    assert_eq!(writer_b.writer_epoch, HubWriterEpoch(2));
    assert!(matches!(
        left.compare_and_commit_async(writer_a.lease(), &writer_a.state)
            .await,
        Err(HubStateStoreError::StaleWriter)
    ));

    let base = right.load_current_async().await.unwrap().unwrap();
    let left_race = PostgresHubStateStore::connect(config(&state_key).unwrap())
        .await
        .unwrap();
    let right_race = PostgresHubStateStore::connect(config(&state_key).unwrap())
        .await
        .unwrap();
    let left_state = base.state.clone();
    let right_state = base.state.clone();
    let lease = base.lease();
    let (a, b) = tokio::join!(
        left_race.compare_and_commit_async(lease, &left_state),
        right_race.compare_and_commit_async(lease, &right_state)
    );
    let successes = usize::from(a.is_ok()) + usize::from(b.is_ok());
    assert_eq!(successes, 1);
    let loser = if a.is_err() { a } else { b };
    assert!(matches!(
        loser,
        Err(HubStateStoreError::RevisionConflict) | Err(HubStateStoreError::StaleWriter)
    ));

    let current = right.load_current_async().await.unwrap().unwrap();
    assert_eq!(current.writer_epoch, HubWriterEpoch(2));
    let fence = current
        .state
        .durable_fence
        .expect("PostgreSQL round-trip must retain durable fence");
    assert_eq!(fence.revision, current.revision.0);
    assert_eq!(fence.writer_epoch, current.writer_epoch.0);
}

#[tokio::test]
async fn oversized_payload_fails_before_postgres_mutation() {
    if !prepare_schema().await {
        return;
    }
    let state_key = format!("oversized-{}", rand::random::<u64>());
    let store = PostgresHubStateStore::connect(config(&state_key).unwrap())
        .await
        .unwrap();
    let initial = initial_state();
    let current = store.acquire_writer_async(&initial).await.unwrap();
    let before = store.load_current_async().await.unwrap().unwrap();

    let mut oversized = current.state.clone();
    oversized.registry.revoked_device_ids = vec!["x".repeat(MAX_CHECKPOINT_BYTES as usize + 1)];
    assert!(matches!(
        store
            .compare_and_commit_async(current.lease(), &oversized)
            .await,
        Err(HubStateStoreError::Persistence(_))
    ));
    assert_eq!(store.load_current_async().await.unwrap().unwrap(), before);
}
