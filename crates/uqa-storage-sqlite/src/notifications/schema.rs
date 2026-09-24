//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Versioned physical registry initialization; no Engine or SQL dependency.

use super::{ManagedConnection, Path, StorageEncryptionKey, REGISTRY_BUSY_TIMEOUT};
use rusqlite::TransactionBehavior;
use std::path::PathBuf;

const REGISTRY_SCHEMA_VERSION: i64 = 2;
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
    register_writer(&connection)?;
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
            validate_publication_schema(&transaction)?;
            transaction.commit().map_err(|error| {
                format!("commit asynchronous notification registry validation: {error}")
            })
        }
        1 if application_id == REGISTRY_APPLICATION_ID => {
            validate_registry_schema(&transaction)?;
            create_publication_schema(&transaction)?;
            transaction.pragma_update(None, "user_version", REGISTRY_SCHEMA_VERSION)
                .map_err(|error| format!("upgrade asynchronous notification registry: {error}"))?;
            validate_publication_schema(&transaction)?;
            transaction.commit().map_err(|error| format!("commit asynchronous notification registry upgrade: {error}"))
        }
        0 if application_id == 0 => {
            create_registry_tables(&transaction)?;
            create_publication_schema(&transaction)?;
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
            validate_publication_schema(&transaction)?;
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

fn create_registry_tables(transaction: &rusqlite::Transaction<'_>) -> Result<(), String> {
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
        .map_err(|error| format!("initialize asynchronous notification registry: {error}"))
}

pub(super) fn register_writer(connection: &rusqlite::Connection) -> Result<(), String> {
    use rusqlite::functions::FunctionFlags;
    connection
        .create_scalar_function(
            "__uqa_notification_writer_format",
            0,
            FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_INNOCUOUS,
            |_| Ok(REGISTRY_SCHEMA_VERSION),
        )
        .map_err(|error| format!("register asynchronous notification writer format: {error}"))
}

pub(super) fn validate_writer(connection: &rusqlite::Connection) -> Result<(), String> {
    let version = connection
        .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
        .map_err(|error| format!("read asynchronous notification writer format: {error}"))?;
    let application_id = connection
        .pragma_query_value(None, "application_id", |row| row.get::<_, i64>(0))
        .map_err(|error| format!("read asynchronous notification writer identity: {error}"))?;
    if version != REGISTRY_SCHEMA_VERSION || application_id != REGISTRY_APPLICATION_ID {
        return Err(format!("asynchronous notification registry changed format or identity: version {version}, application id {application_id}"));
    }
    Ok(())
}

fn create_publication_schema(transaction: &rusqlite::Transaction<'_>) -> Result<(), String> {
    let mut registry_id = [0u8; 16];
    getrandom::fill(&mut registry_id).map_err(|error| {
        format!("allocate asynchronous notification registry identity: {error}")
    })?;
    if registry_id == [0; 16] {
        return Err("asynchronous notification registry identity cannot be zero".into());
    }
    transaction.execute_batch(
        "CREATE TABLE publication_state (
             singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
             registry_id BLOB NOT NULL CHECK (length(registry_id) = 16 AND registry_id != zeroblob(16)),
             next_publication INTEGER NOT NULL CHECK (next_publication >= 0),
             acknowledged_fingerprint BLOB CHECK (acknowledged_fingerprint IS NULL OR length(acknowledged_fingerprint) = 32)
         ) STRICT;"
    ).map_err(|error| format!("create asynchronous notification publication state: {error}"))?;
    transaction
        .execute(
            "INSERT INTO publication_state VALUES (1, ?1, 0, NULL)",
            [registry_id.as_slice()],
        )
        .map_err(|error| {
            format!("initialize asynchronous notification publication state: {error}")
        })?;
    // Old already-open connections lack this function. Their cached statements recompile after the schema change and fail before publishing incompatible queue state.
    for table in [
        "queue_state",
        "backend_process_id_state",
        "queue_entries",
        "listeners",
        "publication_state",
    ] {
        for operation in ["INSERT", "UPDATE", "DELETE"] {
            transaction.execute_batch(&format!(
                "CREATE TRIGGER notification_writer_{table}_{operation} BEFORE {operation} ON {table} BEGIN SELECT CASE WHEN __uqa_notification_writer_format() != {REGISTRY_SCHEMA_VERSION} THEN RAISE(ABORT, 'incompatible asynchronous notification writer') END; END;"
            )).map_err(|error| format!("fence asynchronous notification registry writers: {error}"))?;
        }
    }
    Ok(())
}

fn validate_publication_schema(transaction: &rusqlite::Transaction<'_>) -> Result<(), String> {
    let valid_rows = transaction.query_row(
        "SELECT count(*) FROM publication_state WHERE singleton = 1 AND typeof(registry_id) = 'blob' AND length(registry_id) = 16 AND registry_id != zeroblob(16) AND typeof(next_publication) = 'integer' AND next_publication >= 0 AND (acknowledged_fingerprint IS NULL OR (typeof(acknowledged_fingerprint) = 'blob' AND length(acknowledged_fingerprint) = 32))", [], |row| row.get::<_, i64>(0),
    ).map_err(|error| format!("validate asynchronous notification publication state: {error}"))?;
    let rows = transaction
        .query_row("SELECT count(*) FROM publication_state", [], |row| {
            row.get::<_, i64>(0)
        })
        .map_err(|error| format!("count asynchronous notification publication state: {error}"))?;
    if valid_rows != 1 || rows != 1 {
        return Err("asynchronous notification registry has invalid publication state".into());
    }
    for table in [
        "queue_state",
        "backend_process_id_state",
        "queue_entries",
        "listeners",
        "publication_state",
    ] {
        for operation in ["INSERT", "UPDATE", "DELETE"] {
            let name = format!("notification_writer_{table}_{operation}");
            let count = transaction.query_row("SELECT count(*) FROM sqlite_schema WHERE type = 'trigger' AND name = ?1 AND tbl_name = ?2", [&name, table], |row| row.get::<_, i64>(0))
                .map_err(|error| format!("validate asynchronous notification writer fence: {error}"))?;
            if count != 1 {
                return Err(format!(
                    "asynchronous notification registry is missing writer fence {name}"
                ));
            }
        }
    }
    Ok(())
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
