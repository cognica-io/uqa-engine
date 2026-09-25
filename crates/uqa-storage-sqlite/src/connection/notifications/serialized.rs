//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A physical `SQLite` transaction retains the same bounded metadata slot as native records.

use super::{
    ManagedConnection, NotificationPublication, NotificationPublicationView, StorageBackendResult,
    StorageReadControl,
};
use crate::SQLiteError;
use rusqlite::{params, Connection};
use sha2::{Digest, Sha256};
use std::time::Duration;
use uqa_storage::mvcc::NOTIFICATION_PUBLICATION_KEY;

fn bytes<'a>(row: &'a rusqlite::Row<'_>) -> crate::Result<&'a [u8]> {
    match row.get_ref(0)? {
        rusqlite::types::ValueRef::Text(bytes) => Ok(bytes),
        _ => Err(SQLiteError::StorageBackend(
            "notification publication metadata is not TEXT".into(),
        )),
    }
}

fn fingerprint(connection: &Connection) -> crate::Result<Option<[u8; 32]>> {
    let mut statement = connection.prepare_cached("SELECT value FROM _metadata WHERE key = ?1")?;
    let mut rows = statement.query([NOTIFICATION_PUBLICATION_KEY])?;
    rows.next()?
        .map(|row| {
            let bytes = bytes(row)?;
            Ok(Sha256::digest(bytes).into())
        })
        .transpose()
}

pub(super) fn stage(
    connection: &ManagedConnection,
    publication: &NotificationPublication,
    acknowledged: Option<[u8; 32]>,
) -> StorageBackendResult<()> {
    connection.write_cancellation().check()?;
    if !connection.in_transaction() {
        return Err(SQLiteError::NoActiveTransaction.into());
    }
    connection.with(|sqlite| {
        if let Some(pending) = fingerprint(sqlite)? {
            if acknowledged != Some(pending) {
                return Err(SQLiteError::StorageBackend(
                    "committed notification publication must be acknowledged before replacement".into(),
                ));
            }
        }
        let value = std::str::from_utf8(publication.bytes()).map_err(|_| SQLiteError::StorageBackend("invalid notification publication text".into()))?;
        sqlite.execute("INSERT OR REPLACE INTO _metadata (key, value) VALUES (?1, ?2)", params![NOTIFICATION_PUBLICATION_KEY, value])?;
        Ok(())
    }).map_err(Into::into)
}

pub(super) fn visit(
    connection: &ManagedConnection,
    control: &StorageReadControl,
    visit: &mut dyn FnMut(Option<NotificationPublicationView<'_>>) -> StorageBackendResult<()>,
) -> StorageBackendResult<()> {
    control.check()?;
    connection
        .new_session()
        .with(|sqlite| {
            let mut statement =
                sqlite.prepare_cached("SELECT value FROM _metadata WHERE key = ?1")?;
            let mut rows = statement.query([NOTIFICATION_PUBLICATION_KEY])?;
            let view = rows
                .next()?
                .map(|row| {
                    let value = bytes(row)?;
                    NotificationPublicationView::decode(value, control)
                        .map_err(|error| SQLiteError::from(error.into_storage_error()))
                })
                .transpose()?;
            visit(view).map_err(Into::into)
        })
        .map_err(uqa_storage::StorageBackendError::from)?;
    control.check()
}

pub(super) fn acknowledge(
    connection: &ManagedConnection,
    expected: [u8; 32],
    control: &StorageReadControl,
    nonblocking: bool,
) -> StorageBackendResult<bool> {
    control.check()?;
    connection
        .new_session()
        .with(|sqlite| {
            let timeout =
                sqlite.pragma_query_value(None, "busy_timeout", |row| row.get::<_, u32>(0))?;
            if nonblocking {
                sqlite.busy_timeout(Duration::ZERO)?;
            }
            let begin = sqlite.execute_batch("BEGIN IMMEDIATE");
            sqlite.busy_timeout(Duration::from_millis(u64::from(timeout)))?;
            if let Err(error) = begin {
                if nonblocking
                    && matches!(
                        error.sqlite_error_code(),
                        Some(
                            rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
                        )
                    )
                {
                    return Ok(false);
                }
                return Err(error.into());
            }
            let result = (|| {
                if fingerprint(sqlite)? == Some(expected) {
                    sqlite.execute(
                        "DELETE FROM _metadata WHERE key = ?1",
                        [NOTIFICATION_PUBLICATION_KEY],
                    )?;
                }
                sqlite.execute_batch("COMMIT")?;
                Ok(true)
            })();
            if result.is_err() {
                let _ = sqlite.execute_batch("ROLLBACK");
            }
            result
        })
        .map_err(Into::into)
}
