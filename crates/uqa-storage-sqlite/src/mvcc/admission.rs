//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Wait at physical read, write and publication boundaries without replaying caller operations.

use rusqlite::{Connection, Transaction};
use std::time::Duration;
use uqa_storage::{mvcc::VersionError, read_control::StorageReadControl};

use super::{codec, native, schema, Error, PhysicalResult};

pub(super) fn is_busy(error: &rusqlite::Error) -> bool {
    error.sqlite_error_code() == Some(rusqlite::ErrorCode::DatabaseBusy)
}

pub(super) fn wait(control: &StorageReadControl) -> PhysicalResult<()> {
    control.cancellation().check().map_err(VersionError::from)?;
    std::thread::sleep(Duration::from_millis(2));
    control.cancellation().check().map_err(VersionError::from)?;
    Ok(())
}

pub(super) fn retry<T>(
    connection: &Connection,
    autocommit: bool,
    control: &StorageReadControl,
    mut operation: impl FnMut() -> PhysicalResult<T>,
) -> PhysicalResult<T> {
    loop {
        control.cancellation().check().map_err(VersionError::from)?;
        match operation() {
            Err(Error::SQLite(error))
                if is_busy(&error) && connection.is_autocommit() == autocommit =>
            {
                wait(control)?;
            }
            result => return result,
        }
    }
}

/// BEGIN DEFERRED acquires no read lock. Retry only the first physical lookup until it pins the database. The consumer runs once, even if its own callback returns BUSY.
pub(super) fn read<T, R>(
    connection: &Connection,
    control: &StorageReadControl,
    mut acquire: impl FnMut(&Connection) -> PhysicalResult<T>,
    consume: impl FnOnce(&Connection, T) -> PhysicalResult<R>,
) -> PhysicalResult<R> {
    let (transaction, input) = retry(connection, true, control, || {
        let transaction = connection.unchecked_transaction()?;
        let input = acquire(&transaction)?;
        Ok((transaction, input))
    })?;
    control.check().map_err(VersionError::from)?;
    let result = consume(&transaction, input)?;
    transaction.commit()?;
    Ok(result)
}

/// Capture the physical format and history identity in the same read as its consumer.
pub(super) fn read_header<R>(
    connection: &Connection,
    database: uqa_storage::mvcc::DatabaseId,
    namespace: Option<native::NativeRecordNamespace>,
    control: &StorageReadControl,
    consume: impl FnOnce(&Connection, codec::Header) -> PhysicalResult<R>,
) -> PhysicalResult<R> {
    read(
        connection,
        control,
        |transaction| {
            native::check_mapping(transaction, namespace)?;
            codec::header(transaction, database)
        },
        consume,
    )
}

pub(super) fn permit(
    connection: &Connection,
    control: &StorageReadControl,
) -> PhysicalResult<schema::WritePermit> {
    retry(connection, true, control, || {
        schema::WritePermit::acquire(connection)
    })
}

pub(super) fn begin<'a>(
    connection: &'a Connection,
    control: &StorageReadControl,
) -> PhysicalResult<Transaction<'a>> {
    retry(connection, true, control, || schema::begin(connection))
}

pub(super) fn commit(
    transaction: Transaction<'_>,
    control: &StorageReadControl,
) -> PhysicalResult<()> {
    // A BUSY COMMIT retains the transaction. Keep its staged effects and retry only COMMIT; dropping it on cancellation rolls those effects back. Other errors retain the caller's uncertain-outcome handling.
    let result = commit_open(&transaction, control);
    drop(transaction);
    result
}

pub(super) fn commit_open(
    connection: &Connection,
    control: &StorageReadControl,
) -> PhysicalResult<()> {
    retry(connection, false, control, || {
        connection.execute_batch("COMMIT").map_err(Into::into)
    })
}

/// Record operations use cancellable lock admission. Restore the pool's previous timeout on return, error or unwind.
pub(super) struct BusyTimeout<'a> {
    connection: &'a Connection,
    previous: Duration,
}

impl<'a> BusyTimeout<'a> {
    pub(super) fn new(connection: &'a Connection) -> rusqlite::Result<Self> {
        // Reuse the timeout query across physical checkouts.
        let milliseconds: u32 = connection
            .prepare_cached("PRAGMA busy_timeout")?
            .query_row([], |row| row.get(0))?;
        connection.busy_timeout(Duration::ZERO)?;
        Ok(Self {
            connection,
            previous: Duration::from_millis(u64::from(milliseconds)),
        })
    }
}

impl Drop for BusyTimeout<'_> {
    fn drop(&mut self) {
        let _ = self.connection.busy_timeout(self.previous);
    }
}
