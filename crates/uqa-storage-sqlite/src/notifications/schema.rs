//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Versioned physical registry initialization; no Engine or SQL dependency.

use super::{ManagedConnection, Path, StorageEncryptionKey, REGISTRY_BUSY_TIMEOUT};
use rusqlite::TransactionBehavior;
use std::path::PathBuf;

const REGISTRY_SCHEMA_VERSION: i64 = 1;
const REGISTRY_APPLICATION_ID: i64 = 0x5551_4e31;

pub(super) fn open_registry(
    database_path: &Path,
    key: Option<&StorageEncryptionKey>,
) -> Result<ManagedConnection, String> {
    let path = suffixed_path(database_path, ".uqa-notification-state");
    let registry = ManagedConnection::open_auxiliary(&path, key.cloned())
        .map_err(|error| format!("open asynchronous notification registry: {error}"))?;
    initialize_registry(&registry)?;
    Ok(registry)
}

pub(super) fn initialize_registry(registry: &ManagedConnection) -> Result<(), String> {
    let mut connection = registry
        .lease_connection()
        .map_err(|error| format!("open asynchronous notification registry: {error}"))?;
    connection
        .busy_timeout(REGISTRY_BUSY_TIMEOUT)
        .map_err(|error| format!("set asynchronous notification registry timeout: {error}"))?;
    connection
        .pragma_update(None, "secure_delete", true)
        .map_err(|error| format!("enable asynchronous notification secure deletion: {error}"))?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| {
            format!("begin asynchronous notification registry initialization: {error}")
        })?;
    let version = transaction
        .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
        .map_err(|error| format!("read asynchronous notification registry version: {error}"))?;
    let application_id = transaction
        .pragma_query_value(None, "application_id", |row| row.get::<_, i64>(0))
        .map_err(|error| {
            format!("read asynchronous notification registry application id: {error}")
        })?;
    match version {
        REGISTRY_SCHEMA_VERSION if application_id == REGISTRY_APPLICATION_ID => {
            validate_registry_schema(&transaction)?;
            transaction.commit().map_err(|error| {
                format!("commit asynchronous notification registry validation: {error}")
            })
        }
        0 if application_id == 0 => {
            transaction
                .execute_batch(
                    "CREATE TABLE queue_state (
                         singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
                         next_sequence INTEGER NOT NULL CHECK (next_sequence >= 0),
                         head_position INTEGER NOT NULL CHECK (head_position >= 0)
                     ) STRICT;
                     INSERT INTO queue_state (singleton, next_sequence, head_position)
                     VALUES (1, 0, 0);
                     CREATE TABLE backend_process_id_state (
                         singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
                         next_process_id INTEGER NOT NULL CHECK (next_process_id BETWEEN 1 AND 2147483648)
                     ) STRICT;
                     INSERT INTO backend_process_id_state (singleton, next_process_id)
                     VALUES (1, 1);
                     CREATE TABLE queue_entries (
                         sequence INTEGER PRIMARY KEY CHECK (sequence >= 0),
                         process_id INTEGER NOT NULL CHECK (process_id > 0),
                         channel TEXT NOT NULL,
                         payload TEXT NOT NULL
                     ) STRICT;
                     CREATE TABLE listeners (
                         owner_id BLOB NOT NULL CHECK (length(owner_id) = 16),
                         session_id BLOB NOT NULL CHECK (length(session_id) = 8),
                         process_id INTEGER NOT NULL CHECK (process_id > 0),
                         wake_port INTEGER NOT NULL CHECK (wake_port BETWEEN 1 AND 65535),
                         channels_json TEXT NOT NULL,
                         transaction_open INTEGER NOT NULL CHECK (transaction_open IN (0, 1)),
                         next_sequence INTEGER NOT NULL CHECK (next_sequence >= 0),
                         position INTEGER NOT NULL CHECK (position >= 0),
                         PRIMARY KEY (owner_id, session_id)
                     ) STRICT;",
                )
                .map_err(|error| {
                    format!("initialize asynchronous notification registry: {error}")
                })?;
            transaction
                .pragma_update(None, "application_id", REGISTRY_APPLICATION_ID)
                .map_err(|error| {
                    format!("identify asynchronous notification registry: {error}")
                })?;
            transaction
                .pragma_update(None, "user_version", REGISTRY_SCHEMA_VERSION)
                .map_err(|error| {
                    format!("version asynchronous notification registry: {error}")
                })?;
            validate_registry_schema(&transaction)?;
            transaction.commit().map_err(|error| {
                format!("commit asynchronous notification registry initialization: {error}")
            })
        }
        REGISTRY_SCHEMA_VERSION => Err(format!(
            "asynchronous notification registry has application id {application_id}, expected {REGISTRY_APPLICATION_ID}"
        )),
        version if version > REGISTRY_SCHEMA_VERSION => Err(format!(
            "asynchronous notification registry schema version {version} is newer than supported version {REGISTRY_SCHEMA_VERSION}"
        )),
        version => Err(format!(
            "asynchronous notification registry has unsupported schema version {version} and application id {application_id}"
        )),
    }
}

fn validate_registry_schema(transaction: &rusqlite::Transaction<'_>) -> Result<(), String> {
    let queue_state_rows = transaction
        .query_row("SELECT count(*) FROM queue_state", [], |row| {
            row.get::<_, i64>(0)
        })
        .map_err(|error| format!("validate asynchronous notification queue state: {error}"))?;
    if queue_state_rows != 1 {
        return Err(format!(
            "asynchronous notification registry has {queue_state_rows} queue state rows, expected 1"
        ));
    }
    let process_id_state_rows = transaction
        .query_row("SELECT count(*) FROM backend_process_id_state", [], |row| {
            row.get::<_, i64>(0)
        })
        .map_err(|error| {
            format!("validate asynchronous notification backend process identifier state: {error}")
        })?;
    if process_id_state_rows != 1 {
        return Err(format!(
            "asynchronous notification registry has {process_id_state_rows} backend process identifier state rows, expected 1"
        ));
    }
    for (name, query) in [
        (
            "queue state",
            "SELECT singleton, next_sequence, head_position FROM queue_state LIMIT 0",
        ),
        (
            "backend process identifier state",
            "SELECT singleton, next_process_id FROM backend_process_id_state LIMIT 0",
        ),
        (
            "queue entries",
            "SELECT sequence, process_id, channel, payload FROM queue_entries LIMIT 0",
        ),
        (
            "listeners",
            "SELECT owner_id, session_id, process_id, wake_port, channels_json, transaction_open, next_sequence, position FROM listeners LIMIT 0",
        ),
    ] {
        transaction.prepare(query).map_err(|error| {
            format!("validate asynchronous notification registry {name}: {error}")
        })?;
    }
    Ok(())
}

fn suffixed_path(database_path: &Path, suffix: &str) -> PathBuf {
    let mut path = database_path.as_os_str().to_owned();
    path.push(suffix);
    PathBuf::from(path)
}
