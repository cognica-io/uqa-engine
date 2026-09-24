//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical notification registry transactions and encrypted connection ownership.

mod publication;
mod schema;
#[cfg(test)]
mod tests;

use crate::{ManagedConnection, SQLiteConnectionLease};
use rusqlite::params;
use std::{path::Path, time::Duration};
use uqa_storage::{
    notifications::{NotificationListenerRow, NotificationQueueEntry, NotificationQueueState},
    StorageBackendError, StorageBackendResult, StorageEncryptionKey,
};

const REGISTRY_BUSY_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone)]
pub struct NotificationRegistry {
    connection: ManagedConnection,
}

impl NotificationRegistry {
    pub fn open(path: &Path, key: Option<&StorageEncryptionKey>) -> StorageBackendResult<Self> {
        schema::open_registry(path, key)
            .map(|connection| Self { connection })
            .map_err(StorageBackendError::Other)
    }

    pub fn begin(&self) -> StorageBackendResult<NotificationRegistryTransaction> {
        open_registry_transaction(&self.connection)
    }
}

pub struct NotificationRegistryTransaction {
    connection: SQLiteConnectionLease,
    finished: bool,
    poisoned: bool,
    pending_acknowledgement: Option<[u8; 32]>,
}

impl NotificationRegistryTransaction {
    pub const fn pending_acknowledgement(&self) -> Option<[u8; 32]> {
        self.pending_acknowledgement
    }

    pub fn allocate_backend_process_id(&self) -> Result<i32, StorageBackendError> {
        let next = self
            .connection
            .query_row(
                "SELECT next_process_id FROM backend_process_id_state WHERE singleton = 1",
                [],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|error| registry_error("load backend process identifier", &error))?;
        let process_id = i32::try_from(next).map_err(|_| {
            StorageBackendError::Other(
                "exhausted positive cross-process backend process identifiers".into(),
            )
        })?;
        if process_id <= 0 {
            return Err(StorageBackendError::Other(format!(
                "corrupt asynchronous notification backend process identifier {process_id}"
            )));
        }
        self.connection
            .execute(
                "UPDATE backend_process_id_state SET next_process_id = ?1 WHERE singleton = 1",
                params![next + 1],
            )
            .map_err(|error| registry_error("advance backend process identifier", &error))?;
        Ok(process_id)
    }

    pub fn queue_state(&self) -> Result<NotificationQueueState, StorageBackendError> {
        self.connection
            .query_row(
                "SELECT next_sequence, head_position FROM queue_state WHERE singleton = 1",
                [],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
            )
            .map_err(|error| registry_error("load queue state", &error))
            .and_then(|(next_sequence, head_position)| {
                Ok(NotificationQueueState {
                    next_sequence: nonnegative_u64(next_sequence, "queue sequence")?,
                    head_position: nonnegative_u64(head_position, "queue position")?,
                })
            })
    }

    pub fn save_queue_state(
        &self,
        state: NotificationQueueState,
    ) -> Result<(), StorageBackendError> {
        self.connection
            .execute(
                "UPDATE queue_state SET next_sequence = ?1, head_position = ?2 WHERE singleton = 1",
                params![
                    sqlite_integer(state.next_sequence, "queue sequence")?,
                    sqlite_integer(state.head_position, "queue position")?,
                ],
            )
            .map_err(|error| registry_error("save queue state", &error))?;
        Ok(())
    }

    pub fn append_entries(
        &self,
        entries: &[NotificationQueueEntry],
    ) -> Result<(), StorageBackendError> {
        let mut statement = self
            .connection
            .prepare_cached(
                "INSERT INTO queue_entries (sequence, process_id, channel, payload) VALUES (?1, ?2, ?3, ?4)",
            )
            .map_err(|error| registry_error("prepare queue append", &error))?;
        for entry in entries {
            statement
                .execute(params![
                    sqlite_integer(entry.sequence, "entry sequence")?,
                    entry.process_id,
                    entry.channel,
                    entry.payload,
                ])
                .map_err(|error| registry_error("append queue entry", &error))?;
        }
        Ok(())
    }

    pub fn entries_from(
        &self,
        from_sequence: u64,
    ) -> Result<Vec<NotificationQueueEntry>, StorageBackendError> {
        let mut statement = self
            .connection
            .prepare_cached(
                "SELECT sequence, process_id, channel, payload FROM queue_entries WHERE sequence >= ?1 ORDER BY sequence",
            )
            .map_err(|error| registry_error("prepare queue scan", &error))?;
        let rows = statement
            .query_map(
                params![sqlite_integer(from_sequence, "scan sequence")?],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i32>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                },
            )
            .map_err(|error| registry_error("scan queue entries", &error))?;
        rows.map(|row| {
            let (sequence, process_id, channel, payload) =
                row.map_err(|error| registry_error("read queue entry", &error))?;
            Ok(NotificationQueueEntry {
                sequence: nonnegative_u64(sequence, "entry sequence")?,
                process_id,
                channel,
                payload,
            })
        })
        .collect()
    }

    pub fn delete_entries_before(&self, sequence: u64) -> Result<(), StorageBackendError> {
        self.connection
            .execute(
                "DELETE FROM queue_entries WHERE sequence < ?1",
                params![sqlite_integer(sequence, "cleanup sequence")?],
            )
            .map_err(|error| registry_error("clean queue entries", &error))?;
        Ok(())
    }

    pub fn listeners(&self) -> Result<Vec<NotificationListenerRow>, StorageBackendError> {
        let mut statement = self
            .connection
            .prepare_cached(
                "SELECT owner_id, session_id, process_id, wake_port, channels_json, transaction_open, next_sequence, position FROM listeners ORDER BY owner_id, session_id",
            )
            .map_err(|error| registry_error("prepare listener scan", &error))?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, Vec<u8>>(0)?,
                    row.get::<_, Vec<u8>>(1)?,
                    row.get::<_, i32>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, bool>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, i64>(7)?,
                ))
            })
            .map_err(|error| registry_error("scan listeners", &error))?;
        rows.map(|row| {
            let (
                owner_id,
                session_id,
                process_id,
                wake_port,
                channels_json,
                transaction_open,
                next_sequence,
                position,
            ) = row.map_err(|error| registry_error("read listener", &error))?;
            Ok(NotificationListenerRow {
                owner_id: fixed_bytes(owner_id, "owner identity")?,
                session_id: u64::from_be_bytes(fixed_bytes(session_id, "session identity")?),
                process_id,
                wake_port: u16::try_from(wake_port).map_err(|_| {
                    StorageBackendError::Other(format!(
                        "corrupt asynchronous notification wake port {wake_port}"
                    ))
                })?,
                channels: serde_json::from_str(&channels_json).map_err(|error| {
                    StorageBackendError::Other(format!(
                        "decode asynchronous notification listener channels: {error}"
                    ))
                })?,
                transaction_open,
                next_sequence: nonnegative_u64(next_sequence, "listener sequence")?,
                position: nonnegative_u64(position, "listener position")?,
            })
        })
        .collect()
    }

    pub fn save_listener(
        &self,
        listener: &NotificationListenerRow,
    ) -> Result<(), StorageBackendError> {
        let channels_json = serde_json::to_string(&listener.channels).map_err(|error| {
            StorageBackendError::Other(format!(
                "encode asynchronous notification listener channels: {error}"
            ))
        })?;
        self.connection
            .execute(
                "INSERT INTO listeners (owner_id, session_id, process_id, wake_port, channels_json, transaction_open, next_sequence, position) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8) ON CONFLICT(owner_id, session_id) DO UPDATE SET process_id = excluded.process_id, wake_port = excluded.wake_port, channels_json = excluded.channels_json, transaction_open = excluded.transaction_open, next_sequence = excluded.next_sequence, position = excluded.position",
                params![
                    listener.owner_id.as_slice(),
                    listener.session_id.to_be_bytes().as_slice(),
                    listener.process_id,
                    i64::from(listener.wake_port),
                    channels_json,
                    listener.transaction_open,
                    sqlite_integer(listener.next_sequence, "listener sequence")?,
                    sqlite_integer(listener.position, "listener position")?,
                ],
            )
            .map_err(|error| registry_error("save listener", &error))?;
        Ok(())
    }

    pub fn drop_listener(
        &self,
        owner_id: [u8; 16],
        session_id: u64,
    ) -> Result<(), StorageBackendError> {
        self.connection
            .execute(
                "DELETE FROM listeners WHERE owner_id = ?1 AND session_id = ?2",
                params![owner_id.as_slice(), session_id.to_be_bytes().as_slice()],
            )
            .map_err(|error| registry_error("remove listener", &error))?;
        Ok(())
    }

    pub fn commit(mut self) -> Result<(), StorageBackendError> {
        if self.poisoned {
            return Err(StorageBackendError::Other(
                "cannot commit notification registry after failed publication rollback".into(),
            ));
        }
        self.connection
            .execute_batch("COMMIT")
            .map_err(|error| registry_error("commit registry transaction", &error))?;
        self.finished = true;
        Ok(())
    }
}

impl Drop for NotificationRegistryTransaction {
    fn drop(&mut self) {
        if !self.finished {
            let _ = self.connection.execute_batch("ROLLBACK");
        }
    }
}

fn registry_error(action: &str, error: &rusqlite::Error) -> StorageBackendError {
    StorageBackendError::Other(format!(
        "{action} in asynchronous notification registry: {error}"
    ))
}

fn open_registry_transaction(
    registry: &ManagedConnection,
) -> Result<NotificationRegistryTransaction, StorageBackendError> {
    let connection = registry.lease_connection().map_err(|error| {
        StorageBackendError::Other(format!(
            "lease asynchronous notification registry connection: {error}"
        ))
    })?;
    schema::register_writer(&connection).map_err(StorageBackendError::Other)?;
    connection
        .busy_timeout(REGISTRY_BUSY_TIMEOUT)
        .map_err(|error| registry_error("set registry busy timeout", &error))?;
    connection
        .pragma_update(None, "secure_delete", true)
        .map_err(|error| registry_error("enable registry secure deletion", &error))?;
    connection
        .execute_batch("BEGIN IMMEDIATE")
        .map_err(|error| registry_error("begin registry transaction", &error))?;
    let transaction = NotificationRegistryTransaction {
        connection,
        finished: false,
        poisoned: false,
        pending_acknowledgement: None,
    };
    schema::validate_writer(&transaction.connection).map_err(StorageBackendError::Other)?;
    Ok(transaction)
}

fn sqlite_integer(value: u64, label: &str) -> Result<i64, StorageBackendError> {
    i64::try_from(value).map_err(|_| {
        StorageBackendError::Other(format!(
            "asynchronous notification {label} {value} exceeds SQLite INTEGER"
        ))
    })
}

fn nonnegative_u64(value: i64, label: &str) -> Result<u64, StorageBackendError> {
    u64::try_from(value).map_err(|_| {
        StorageBackendError::Other(format!("corrupt asynchronous notification {label} {value}"))
    })
}

fn fixed_bytes<const N: usize>(
    bytes: Vec<u8>,
    label: &str,
) -> Result<[u8; N], StorageBackendError> {
    let length = bytes.len();
    bytes.try_into().map_err(|_| {
        StorageBackendError::Other(format!(
            "corrupt asynchronous notification {label} has {length} bytes"
        ))
    })
}
