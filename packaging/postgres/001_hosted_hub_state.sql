-- CUMG V2 hosted authoritative Hub-state store schema.
-- Apply this migration with an operator/migration identity, not the serving Hub role.
CREATE TABLE IF NOT EXISTS cumg_hub_state (
    state_key TEXT PRIMARY KEY CHECK (octet_length(state_key) <= 256),
    store_schema_version INTEGER NOT NULL CHECK (store_schema_version > 0),
    revision BIGINT NOT NULL CHECK (revision > 0),
    writer_epoch BIGINT NOT NULL CHECK (writer_epoch > 0),
    state_payload BYTEA NOT NULL CHECK (octet_length(state_payload) <= 1048576),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
);

-- The serving Hub role requires SELECT, INSERT, UPDATE only on this table.
-- Example (replace cumg_hub_runtime with the reviewed runtime role):
-- GRANT SELECT, INSERT, UPDATE ON TABLE cumg_hub_state TO cumg_hub_runtime;
