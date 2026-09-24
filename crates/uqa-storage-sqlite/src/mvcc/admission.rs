//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Wait only at physical admission and publication boundaries, without replaying evaluated writes.

use rusqlite::{Connection, Transaction};
use std::time::Duration;
use uqa_storage::{mvcc::VersionError, read_control::StorageReadControl};

use super::{schema, Error, PhysicalResult};

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
    let result = retry(&transaction, false, control, || {
        transaction.execute_batch("COMMIT").map_err(Into::into)
    });
    drop(transaction);
    result
}

/// Record writes do their own cancellable admission. Restore the pool's previous timeout when returning the connection, including on an error or unwind.
pub(super) struct BusyTimeout<'a> {
    connection: &'a Connection,
    previous: Duration,
}

impl<'a> BusyTimeout<'a> {
    pub(super) fn new(connection: &'a Connection) -> rusqlite::Result<Self> {
        let milliseconds: u32 =
            connection.pragma_query_value(None, "busy_timeout", |row| row.get(0))?;
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
