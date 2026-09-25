//! PostgreSQL-backed authoritative Hub state for replaceable hosted deployments.
//!
//! One row contains the complete CUMG Hub state for one stable state key. PostgreSQL row locking
//! and revision/writer-epoch predicates provide the physical transaction boundary; CUMG's durable
//! fence inside the serialized payload remains the logical authority contract.

use crate::{
    v2_hub_state_store::{
        AsyncHubAuthoritativeStateStore, DurableHubState, HUB_DURABLE_RECORD_SCHEMA_VERSION,
        HubStateRevision, HubStateStoreError, HubWriterEpoch, HubWriterLease,
    },
    v2_m1_persistence::{HubPersistentState, MAX_CHECKPOINT_BYTES, PersistenceError},
};
use async_trait::async_trait;
#[cfg(unix)]
use std::path::Path;
use std::{fmt, future::Future, time::Duration};
use tokio::sync::{Mutex, MutexGuard};
use tokio_postgres::{Client, GenericClient, NoTls, Row};

const SELECT_CURRENT_SQL: &str = r#"
SELECT store_schema_version, revision, writer_epoch, state_payload
FROM cumg_hub_state
WHERE state_key = $1
"#;

const SELECT_CURRENT_FOR_UPDATE_SQL: &str = r#"
SELECT store_schema_version, revision, writer_epoch, state_payload
FROM cumg_hub_state
WHERE state_key = $1
FOR UPDATE
"#;

const INSERT_INITIAL_SQL: &str = r#"
INSERT INTO cumg_hub_state
    (state_key, store_schema_version, revision, writer_epoch, state_payload)
VALUES ($1, $2, $3, $4, $5)
ON CONFLICT (state_key) DO NOTHING
"#;

const UPDATE_CAS_SQL: &str = r#"
UPDATE cumg_hub_state
SET store_schema_version = $2,
    revision = $3,
    writer_epoch = $4,
    state_payload = $5,
    updated_at = clock_timestamp()
WHERE state_key = $1
  AND revision = $6
  AND writer_epoch = $7
"#;

const VERIFY_SCHEMA_SQL: &str = r#"
SELECT state_key, store_schema_version, revision, writer_epoch, state_payload
FROM cumg_hub_state
WHERE FALSE
"#;

const MAX_STATE_KEY_BYTES: usize = 256;
const MAX_CONNECTION_FIELD_BYTES: usize = 1024;
const MAX_ACQUIRE_RETRIES: usize = 4;
const MAX_PROVIDER_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone)]
pub struct PostgresHubStateStoreConfig {
    host: String,
    port: u16,
    database: String,
    user: String,
    password: Option<String>,
    state_key: String,
    connect_timeout: Duration,
    query_timeout: Duration,
}

impl fmt::Debug for PostgresHubStateStoreConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PostgresHubStateStoreConfig")
            .field(
                "transport",
                &if self.host.starts_with('/') {
                    "unix"
                } else {
                    "tcp"
                },
            )
            .field("port", &self.port)
            .field("database_configured", &!self.database.is_empty())
            .field("user_configured", &!self.user.is_empty())
            .field("password", &self.password.as_ref().map(|_| "[REDACTED]"))
            .field("state_key_present", &!self.state_key.is_empty())
            .field("connect_timeout", &self.connect_timeout)
            .field("query_timeout", &self.query_timeout)
            .finish()
    }
}

impl PostgresHubStateStoreConfig {
    pub fn new(
        host: impl Into<String>,
        database: impl Into<String>,
        user: impl Into<String>,
        state_key: impl Into<String>,
    ) -> Result<Self, HubStateStoreError> {
        let config = Self {
            host: host.into(),
            port: 5432,
            database: database.into(),
            user: user.into(),
            password: None,
            state_key: state_key.into(),
            connect_timeout: Duration::from_secs(5),
            query_timeout: Duration::from_secs(5),
        };
        config.validate()?;
        Ok(config)
    }

    pub fn with_password(
        mut self,
        password: impl Into<String>,
    ) -> Result<Self, HubStateStoreError> {
        let password = password.into();
        if password.is_empty() || password.len() > MAX_CONNECTION_FIELD_BYTES {
            return Err(HubStateStoreError::InvalidConfiguration);
        }
        self.password = Some(password);
        Ok(self)
    }

    pub fn with_port(mut self, port: u16) -> Result<Self, HubStateStoreError> {
        if port == 0 {
            return Err(HubStateStoreError::InvalidConfiguration);
        }
        self.port = port;
        Ok(self)
    }

    pub fn with_timeouts(
        mut self,
        connect_timeout: Duration,
        query_timeout: Duration,
    ) -> Result<Self, HubStateStoreError> {
        self.connect_timeout = connect_timeout;
        self.query_timeout = query_timeout;
        self.validate()?;
        Ok(self)
    }

    fn validate(&self) -> Result<(), HubStateStoreError> {
        if !valid_connection_field(&self.host)
            || !valid_connection_field(&self.database)
            || !valid_connection_field(&self.user)
            || !valid_state_key(&self.state_key)
            || self.port == 0
            || self.connect_timeout.is_zero()
            || self.connect_timeout > MAX_PROVIDER_TIMEOUT
            || self.query_timeout.is_zero()
            || self.query_timeout > MAX_PROVIDER_TIMEOUT
            || self.password.as_ref().is_some_and(|password| {
                password.is_empty() || password.len() > MAX_CONNECTION_FIELD_BYTES
            })
        {
            return Err(HubStateStoreError::InvalidConfiguration);
        }
        Ok(())
    }
}

pub struct PostgresHubStateStore {
    postgres: tokio_postgres::Config,
    client: Mutex<Option<Client>>,
    state_key: String,
    connect_timeout: Duration,
    query_timeout: Duration,
}

impl fmt::Debug for PostgresHubStateStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PostgresHubStateStore")
            .field("state_key_present", &true)
            .field("query_timeout", &self.query_timeout)
            .finish_non_exhaustive()
    }
}

impl PostgresHubStateStore {
    pub async fn connect(config: PostgresHubStateStoreConfig) -> Result<Self, HubStateStoreError> {
        config.validate()?;

        let mut postgres = tokio_postgres::Config::new();
        if config.host.starts_with('/') {
            #[cfg(unix)]
            postgres.host_path(Path::new(&config.host));
            #[cfg(not(unix))]
            return Err(HubStateStoreError::InvalidConfiguration);
        } else {
            postgres.host(&config.host).port(config.port);
        }
        postgres
            .dbname(&config.database)
            .user(&config.user)
            .application_name("cumg-v2-hub")
            .connect_timeout(config.connect_timeout);
        if let Some(password) = config.password.as_ref() {
            postgres.password(password);
        }

        let store = Self {
            postgres,
            client: Mutex::new(None),
            state_key: config.state_key,
            connect_timeout: config.connect_timeout,
            query_timeout: config.query_timeout,
        };
        store.verify_schema().await?;
        Ok(store)
    }

    async fn open_client(&self) -> Result<Client, HubStateStoreError> {
        let (client, connection) =
            match tokio::time::timeout(self.connect_timeout, self.postgres.connect(NoTls)).await {
                Ok(Ok(value)) => value,
                Ok(Err(_)) | Err(_) => return Err(HubStateStoreError::Unavailable),
            };
        tokio::spawn(async move {
            if connection.await.is_err() {
                tracing::error!(
                    event = "v2_hosted_state_provider_connection_lost",
                    provider = "postgres",
                    outcome = "failed_closed",
                    error_code = "hub_state_store_unavailable",
                    "hosted Hub-state provider connection ended"
                );
            }
        });

        let timeout_ms = u64::try_from(self.query_timeout.as_millis())
            .map_err(|_| HubStateStoreError::InvalidConfiguration)?;
        let setup =
            format!("SET statement_timeout = {timeout_ms}; SET lock_timeout = {timeout_ms};");
        match tokio::time::timeout(self.query_timeout, client.batch_execute(&setup)).await {
            Ok(Ok(())) => Ok(client),
            Ok(Err(error)) if is_schema_error(&error) => {
                Err(HubStateStoreError::ProviderSchemaMismatch)
            }
            Ok(Err(_)) | Err(_) => Err(HubStateStoreError::Unavailable),
        }
    }

    async fn client_guard(&self) -> Result<MutexGuard<'_, Option<Client>>, HubStateStoreError> {
        let mut guard = self.client.lock().await;
        if guard.as_ref().is_none_or(Client::is_closed) {
            *guard = Some(self.open_client().await?);
        }
        Ok(guard)
    }

    fn invalidate_connection_on_provider_failure<T>(
        guard: &mut Option<Client>,
        result: &Result<T, HubStateStoreError>,
    ) {
        if matches!(
            result,
            Err(HubStateStoreError::Unavailable | HubStateStoreError::ReadAfterCommitMismatch)
        ) {
            *guard = None;
        }
    }

    async fn timed_db<T>(
        &self,
        future: impl Future<Output = Result<T, tokio_postgres::Error>>,
    ) -> Result<T, HubStateStoreError> {
        match tokio::time::timeout(self.query_timeout, future).await {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(error)) if is_schema_error(&error) => {
                Err(HubStateStoreError::ProviderSchemaMismatch)
            }
            Ok(Err(_)) | Err(_) => Err(HubStateStoreError::Unavailable),
        }
    }

    async fn verify_schema(&self) -> Result<(), HubStateStoreError> {
        let mut guard = self.client_guard().await?;
        let result = {
            let client = guard.as_ref().ok_or(HubStateStoreError::Unavailable)?;
            self.timed_db(client.query(VERIFY_SCHEMA_SQL, &[]))
                .await
                .map(|_| ())
        };
        Self::invalidate_connection_on_provider_failure(&mut guard, &result);
        result
    }

    async fn read_current<C: GenericClient + Sync>(
        &self,
        client: &C,
        for_update: bool,
    ) -> Result<Option<DurableHubState>, HubStateStoreError> {
        let sql = if for_update {
            SELECT_CURRENT_FOR_UPDATE_SQL
        } else {
            SELECT_CURRENT_SQL
        };
        let row = self
            .timed_db(client.query_opt(sql, &[&self.state_key]))
            .await?;
        row.map(decode_row).transpose()
    }

    async fn verify_exact(
        &self,
        client: &Client,
        expected: &DurableHubState,
    ) -> Result<DurableHubState, HubStateStoreError> {
        match self.read_current(client, false).await {
            Ok(Some(actual)) if &actual == expected => Ok(actual),
            _ => Err(HubStateStoreError::ReadAfterCommitMismatch),
        }
    }

    async fn acquire_once(
        &self,
        initial_state: &HubPersistentState,
    ) -> Result<Option<DurableHubState>, HubStateStoreError> {
        let mut guard = self.client_guard().await?;
        let result = {
            let client = guard.as_mut().ok_or(HubStateStoreError::Unavailable)?;
            self.acquire_once_with_client(client, initial_state).await
        };
        Self::invalidate_connection_on_provider_failure(&mut guard, &result);
        result
    }

    async fn acquire_once_with_client(
        &self,
        client: &mut Client,
        initial_state: &HubPersistentState,
    ) -> Result<Option<DurableHubState>, HubStateStoreError> {
        let transaction = self.timed_db(client.transaction()).await?;
        let current = self.read_current(&transaction, true).await?;

        let (candidate, expected) = match current {
            Some(current) => {
                let revision = current
                    .revision
                    .0
                    .checked_add(1)
                    .ok_or(HubStateStoreError::RevisionOverflow)?;
                let epoch = current
                    .writer_epoch
                    .0
                    .checked_add(1)
                    .ok_or(HubStateStoreError::EpochOverflow)?;
                (
                    DurableHubState::committed(
                        HubStateRevision(revision),
                        HubWriterEpoch(epoch),
                        current.state,
                    )?,
                    Some((current.revision, current.writer_epoch)),
                )
            }
            None => (
                DurableHubState::committed(
                    HubStateRevision(1),
                    HubWriterEpoch(1),
                    initial_state.clone(),
                )?,
                None,
            ),
        };
        let payload = encode_payload(&candidate)?;

        let affected = if let Some((revision, epoch)) = expected {
            let expected_revision = as_i64(revision.0, HubStateStoreError::RevisionOverflow)?;
            let expected_epoch = as_i64(epoch.0, HubStateStoreError::EpochOverflow)?;
            let revision = as_i64(candidate.revision.0, HubStateStoreError::RevisionOverflow)?;
            let epoch = as_i64(candidate.writer_epoch.0, HubStateStoreError::EpochOverflow)?;
            self.timed_db(transaction.execute(
                UPDATE_CAS_SQL,
                &[
                    &self.state_key,
                    &i32::from(candidate.store_schema_version),
                    &revision,
                    &epoch,
                    &payload,
                    &expected_revision,
                    &expected_epoch,
                ],
            ))
            .await?
        } else {
            let revision = as_i64(candidate.revision.0, HubStateStoreError::RevisionOverflow)?;
            let epoch = as_i64(candidate.writer_epoch.0, HubStateStoreError::EpochOverflow)?;
            self.timed_db(transaction.execute(
                INSERT_INITIAL_SQL,
                &[
                    &self.state_key,
                    &i32::from(candidate.store_schema_version),
                    &revision,
                    &epoch,
                    &payload,
                ],
            ))
            .await?
        };

        if affected != 1 {
            let _ = transaction.rollback().await;
            return Ok(None);
        }

        match tokio::time::timeout(self.query_timeout, transaction.commit()).await {
            Ok(Ok(())) => {}
            Ok(Err(_)) | Err(_) => return Err(HubStateStoreError::ReadAfterCommitMismatch),
        }
        let verified = self.verify_exact(client, &candidate).await?;
        Ok(Some(verified))
    }

    async fn compare_and_commit_inner(
        &self,
        lease: HubWriterLease,
        state: &HubPersistentState,
    ) -> Result<DurableHubState, HubStateStoreError> {
        let mut guard = self.client_guard().await?;
        let result = {
            let client = guard.as_mut().ok_or(HubStateStoreError::Unavailable)?;
            self.compare_and_commit_with_client(client, lease, state)
                .await
        };
        Self::invalidate_connection_on_provider_failure(&mut guard, &result);
        result
    }

    async fn compare_and_commit_with_client(
        &self,
        client: &mut Client,
        lease: HubWriterLease,
        state: &HubPersistentState,
    ) -> Result<DurableHubState, HubStateStoreError> {
        let transaction = self.timed_db(client.transaction()).await?;
        let Some(current) = self.read_current(&transaction, true).await? else {
            let _ = transaction.rollback().await;
            return Err(HubStateStoreError::RevisionConflict);
        };
        if current.writer_epoch != lease.epoch {
            let _ = transaction.rollback().await;
            return Err(HubStateStoreError::StaleWriter);
        }
        if current.revision != lease.revision {
            let _ = transaction.rollback().await;
            return Err(HubStateStoreError::RevisionConflict);
        }

        let revision = current
            .revision
            .0
            .checked_add(1)
            .ok_or(HubStateStoreError::RevisionOverflow)?;
        let candidate = DurableHubState::committed(
            HubStateRevision(revision),
            current.writer_epoch,
            state.clone(),
        )?;
        let payload = encode_payload(&candidate)?;
        let candidate_revision = as_i64(revision, HubStateStoreError::RevisionOverflow)?;
        let candidate_epoch = as_i64(candidate.writer_epoch.0, HubStateStoreError::EpochOverflow)?;
        let expected_revision = as_i64(lease.revision.0, HubStateStoreError::RevisionOverflow)?;
        let expected_epoch = as_i64(lease.epoch.0, HubStateStoreError::EpochOverflow)?;

        let affected = self
            .timed_db(transaction.execute(
                UPDATE_CAS_SQL,
                &[
                    &self.state_key,
                    &i32::from(candidate.store_schema_version),
                    &candidate_revision,
                    &candidate_epoch,
                    &payload,
                    &expected_revision,
                    &expected_epoch,
                ],
            ))
            .await?;
        if affected != 1 {
            let _ = transaction.rollback().await;
            return Err(HubStateStoreError::RevisionConflict);
        }

        match tokio::time::timeout(self.query_timeout, transaction.commit()).await {
            Ok(Ok(())) => {}
            Ok(Err(_)) | Err(_) => return Err(HubStateStoreError::ReadAfterCommitMismatch),
        }
        self.verify_exact(client, &candidate).await
    }
}

#[async_trait]
impl AsyncHubAuthoritativeStateStore for PostgresHubStateStore {
    async fn load_current_async(&self) -> Result<Option<DurableHubState>, HubStateStoreError> {
        let mut guard = self.client_guard().await?;
        let result = {
            let client = guard.as_ref().ok_or(HubStateStoreError::Unavailable)?;
            self.read_current(client, false).await
        };
        Self::invalidate_connection_on_provider_failure(&mut guard, &result);
        result
    }

    async fn acquire_writer_async(
        &self,
        initial_state: &HubPersistentState,
    ) -> Result<DurableHubState, HubStateStoreError> {
        for _ in 0..MAX_ACQUIRE_RETRIES {
            if let Some(acquired) = self.acquire_once(initial_state).await? {
                return Ok(acquired);
            }
            tokio::task::yield_now().await;
        }
        Err(HubStateStoreError::RevisionConflict)
    }

    async fn compare_and_commit_async(
        &self,
        lease: HubWriterLease,
        state: &HubPersistentState,
    ) -> Result<DurableHubState, HubStateStoreError> {
        self.compare_and_commit_inner(lease, state).await
    }
}

fn valid_connection_field(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_CONNECTION_FIELD_BYTES
        && !value.as_bytes().iter().any(|byte| byte.is_ascii_control())
}

fn valid_state_key(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_STATE_KEY_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
}

fn encode_payload(state: &DurableHubState) -> Result<Vec<u8>, HubStateStoreError> {
    state.validate_committed()?;
    let payload = serde_json::to_vec(&state.state)
        .map_err(|error| HubStateStoreError::Persistence(PersistenceError::Serialization(error)))?;
    if payload.len() > usize::try_from(MAX_CHECKPOINT_BYTES).unwrap_or(usize::MAX) {
        return Err(HubStateStoreError::Persistence(
            PersistenceError::CheckpointTooLarge,
        ));
    }
    Ok(payload)
}

fn decode_row(row: Row) -> Result<DurableHubState, HubStateStoreError> {
    let store_schema: i32 = row
        .try_get("store_schema_version")
        .map_err(|_| HubStateStoreError::InvalidState)?;
    let revision: i64 = row
        .try_get("revision")
        .map_err(|_| HubStateStoreError::InvalidState)?;
    let writer_epoch: i64 = row
        .try_get("writer_epoch")
        .map_err(|_| HubStateStoreError::InvalidState)?;
    let payload: Vec<u8> = row
        .try_get("state_payload")
        .map_err(|_| HubStateStoreError::InvalidState)?;
    if payload.len() > usize::try_from(MAX_CHECKPOINT_BYTES).unwrap_or(usize::MAX)
        || revision <= 0
        || writer_epoch <= 0
    {
        return Err(HubStateStoreError::InvalidState);
    }
    if store_schema != i32::from(HUB_DURABLE_RECORD_SCHEMA_VERSION) {
        return Err(HubStateStoreError::ProviderSchemaMismatch);
    }
    let state: HubPersistentState =
        serde_json::from_slice(&payload).map_err(|_| HubStateStoreError::InvalidState)?;
    let durable = DurableHubState {
        store_schema_version: u16::try_from(store_schema)
            .map_err(|_| HubStateStoreError::InvalidState)?,
        revision: HubStateRevision(
            u64::try_from(revision).map_err(|_| HubStateStoreError::InvalidState)?,
        ),
        writer_epoch: HubWriterEpoch(
            u64::try_from(writer_epoch).map_err(|_| HubStateStoreError::InvalidState)?,
        ),
        state,
    };
    durable.validate_committed()?;
    Ok(durable)
}

fn as_i64(value: u64, error: HubStateStoreError) -> Result<i64, HubStateStoreError> {
    i64::try_from(value).map_err(|_| error)
}

fn is_schema_error(error: &tokio_postgres::Error) -> bool {
    error
        .as_db_error()
        .is_some_and(|db| matches!(db.code().code(), "42P01" | "42703" | "42804"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        v2_execution_safety::AuthoritativeOperationController, v2_m0::DeviceRegistry,
        v2_m0_execution::AdmissionLimits,
    };

    fn state() -> HubPersistentState {
        let registry = DeviceRegistry::default();
        let execution = AuthoritativeOperationController::new(AdmissionLimits {
            max_global_active: 1,
            max_queued_per_device: 1,
        })
        .unwrap();
        HubPersistentState::capture(&registry, &execution)
    }

    #[test]
    fn config_debug_redacts_password_and_connection_locator() {
        let password = format!("test-secret-{}", rand::random::<u64>());
        let config = PostgresHubStateStoreConfig::new(
            "/cloudsql/private-project:region:instance",
            "private_database",
            "private_user",
            "device-state",
        )
        .unwrap()
        .with_password(password.clone())
        .unwrap();
        let rendered = format!("{config:?}");
        assert!(rendered.contains("[REDACTED]"));
        assert!(!rendered.contains(&password));
        assert!(!rendered.contains("private-project"));
        assert!(!rendered.contains("private_database"));
        assert!(!rendered.contains("private_user"));
    }

    #[test]
    fn state_key_and_timeouts_are_bounded() {
        assert!(
            PostgresHubStateStoreConfig::new("localhost", "db", "user", "../unsafe/key").is_err()
        );
        assert!(
            PostgresHubStateStoreConfig::new("localhost", "db", "user", "safe-key")
                .unwrap()
                .with_timeouts(Duration::ZERO, Duration::from_secs(1))
                .is_err()
        );
        assert!(
            PostgresHubStateStoreConfig::new("localhost", "db", "user", "safe-key")
                .unwrap()
                .with_timeouts(Duration::from_secs(1), Duration::from_secs(31))
                .is_err()
        );
    }

    #[test]
    fn payload_ceiling_is_checked_before_provider_mutation() {
        let mut state = state();
        state.registry.revoked_device_ids =
            vec!["x".repeat(usize::try_from(MAX_CHECKPOINT_BYTES).unwrap())];
        let durable =
            DurableHubState::committed(HubStateRevision(1), HubWriterEpoch(1), state).unwrap();
        assert!(matches!(
            encode_payload(&durable),
            Err(HubStateStoreError::Persistence(
                PersistenceError::CheckpointTooLarge
            ))
        ));
    }
}
