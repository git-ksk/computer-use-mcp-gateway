use computer_use_mcp_gateway::{
    v2_execution_safety::AuthoritativeOperationController,
    v2_hub_state_store::{AsyncHubAuthoritativeStateStore, HubStateStoreError, HubWriterEpoch},
    v2_m0::{DeviceIdentity, DeviceRegistry},
    v2_m0_execution::AdmissionLimits,
    v2_m1_persistence::{HubPersistentState, MAX_CHECKPOINT_BYTES},
    v2_postgres_hub_state_store::{
        PostgresHubStateStore, PostgresHubStateStoreConfig, PostgresTlsMode,
    },
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

fn tls_test_material() -> Option<(String, String, Vec<u8>)> {
    let ca_file = std::env::var("CUMG_TEST_POSTGRES_TLS_CA_FILE").ok()?;
    let host = std::env::var("CUMG_TEST_POSTGRES_TLS_HOST").unwrap_or_else(|_| "localhost".into());
    let mismatch_host = std::env::var("CUMG_TEST_POSTGRES_TLS_MISMATCH_HOST")
        .unwrap_or_else(|_| "127.0.0.1".into());
    let ca_pem = std::fs::read(ca_file).ok()?;
    Some((host, mismatch_host, ca_pem))
}

fn tls_config_for_user(
    state_key: &str,
    host: &str,
    user: &str,
    ca_pem: Option<Vec<u8>>,
) -> Option<PostgresHubStateStoreConfig> {
    std::env::var("CUMG_TEST_POSTGRES_URL").ok()?;
    let port = std::env::var("CUMG_TEST_POSTGRES_PORT")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(5432);
    let database = std::env::var("CUMG_TEST_POSTGRES_DB").unwrap_or_else(|_| "cumg".into());
    let mut config = PostgresHubStateStoreConfig::new(host, database, user, state_key)
        .unwrap()
        .with_port(port)
        .unwrap()
        .with_timeouts(Duration::from_secs(5), Duration::from_secs(5))
        .unwrap()
        .with_tls_mode(PostgresTlsMode::VerifyFull)
        .unwrap();
    if let Some(ca_pem) = ca_pem {
        config = config.with_custom_ca_pem(ca_pem).unwrap();
    }
    Some(config)
}

async fn admin_client() -> Option<tokio_postgres::Client> {
    let url = std::env::var("CUMG_TEST_POSTGRES_URL").ok()?;
    let (client, connection) = tokio_postgres::connect(&url, NoTls)
        .await
        .expect("CUMG_TEST_POSTGRES_URL is set but PostgreSQL is unavailable");
    tokio::spawn(async move {
        let _ = connection.await;
    });
    Some(client)
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
    let mut config = PostgresHubStateStoreConfig::new(host, database, user, state_key)
        .unwrap()
        .with_port(port)
        .unwrap()
        .with_timeouts(Duration::from_secs(5), Duration::from_secs(5))
        .unwrap();
    if let Ok(password) = std::env::var("CUMG_TEST_POSTGRES_PASSWORD") {
        config = config.with_password(password).unwrap();
    }
    Some(config)
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

#[tokio::test]
async fn postgres_tls_verify_full_accepts_trusted_matching_hostname() {
    if !prepare_schema().await {
        return;
    }
    let Some((host, _, ca_pem)) = tls_test_material() else {
        return;
    };
    let user = std::env::var("CUMG_TEST_POSTGRES_USER").unwrap_or_else(|_| "postgres".into());
    let state_key = format!("tls-ok-{}", rand::random::<u64>());
    let store = PostgresHubStateStore::connect(
        tls_config_for_user(&state_key, &host, &user, Some(ca_pem)).unwrap(),
    )
    .await
    .expect("verify-full must accept the trusted matching PostgreSQL certificate");
    let current = store.acquire_writer_async(&initial_state()).await.unwrap();
    assert_eq!(current.writer_epoch, HubWriterEpoch(1));
}

#[tokio::test]
async fn postgres_tls_verify_full_rejects_untrusted_ca() {
    if !prepare_schema().await {
        return;
    }
    let Some((host, _, _)) = tls_test_material() else {
        return;
    };
    let user = std::env::var("CUMG_TEST_POSTGRES_USER").unwrap_or_else(|_| "postgres".into());
    let state_key = format!("tls-untrusted-{}", rand::random::<u64>());
    assert!(matches!(
        PostgresHubStateStore::connect(
            tls_config_for_user(&state_key, &host, &user, None).unwrap()
        )
        .await,
        Err(HubStateStoreError::Unavailable)
    ));
}

#[tokio::test]
async fn postgres_tls_verify_full_rejects_hostname_mismatch() {
    if !prepare_schema().await {
        return;
    }
    let Some((_, mismatch_host, ca_pem)) = tls_test_material() else {
        return;
    };
    let user = std::env::var("CUMG_TEST_POSTGRES_USER").unwrap_or_else(|_| "postgres".into());
    let state_key = format!("tls-hostname-{}", rand::random::<u64>());
    assert!(matches!(
        PostgresHubStateStore::connect(
            tls_config_for_user(&state_key, &mismatch_host, &user, Some(ca_pem)).unwrap()
        )
        .await,
        Err(HubStateStoreError::Unavailable)
    ));
}

#[tokio::test]
async fn postgres_tls_provider_loss_fails_closed_and_reconnects() {
    if !prepare_schema().await {
        return;
    }
    let Some((host, _, ca_pem)) = tls_test_material() else {
        return;
    };
    let admin = admin_client().await.unwrap();
    let role = format!("cumg_tls_{}", rand::random::<u64>());
    admin
        .batch_execute(&format!(
            "CREATE ROLE {role} LOGIN;              GRANT USAGE ON SCHEMA public TO {role};              GRANT SELECT, INSERT, UPDATE ON TABLE cumg_hub_state TO {role};"
        ))
        .await
        .unwrap();

    let state_key = format!("tls-reconnect-{}", rand::random::<u64>());
    let store = PostgresHubStateStore::connect(
        tls_config_for_user(&state_key, &host, &role, Some(ca_pem)).unwrap(),
    )
    .await
    .unwrap();
    let acquired = store.acquire_writer_async(&initial_state()).await.unwrap();

    admin
        .batch_execute(&format!("ALTER ROLE {role} NOLOGIN;"))
        .await
        .unwrap();
    let terminated = admin
        .query(
            "SELECT pg_terminate_backend(pid)              FROM pg_stat_activity              WHERE usename = $1 AND application_name = 'cumg-v2-hub' AND pid <> pg_backend_pid()",
            &[&role],
        )
        .await
        .unwrap();
    assert!(!terminated.is_empty());

    assert!(matches!(
        store.load_current_async().await,
        Err(HubStateStoreError::Unavailable)
    ));

    admin
        .batch_execute(&format!("ALTER ROLE {role} LOGIN;"))
        .await
        .unwrap();
    let restored = store
        .load_current_async()
        .await
        .expect("TLS provider must reconnect after availability returns")
        .expect("durable row remains present");
    assert_eq!(restored.revision, acquired.revision);
    assert_eq!(restored.writer_epoch, acquired.writer_epoch);

    drop(store);
    let _ = admin
        .query(
            "SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE usename = $1",
            &[&role],
        )
        .await;
    admin
        .batch_execute(&format!("DROP OWNED BY {role}; DROP ROLE {role};"))
        .await
        .unwrap();
}
