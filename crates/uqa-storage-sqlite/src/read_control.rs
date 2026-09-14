//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retain one snapshot while checking payload sizes and reading reserved provider buffers.

use crate::{Result, SQLiteError};
use rusqlite::{Connection, Transaction, TransactionBehavior};
use uqa_core::memory::{BudgetedVec, MemoryError, MemoryReservation};
use uqa_storage::read_control::StorageReadControl;

pub(crate) fn read_snapshot<T>(
    connection: &Connection,
    read: impl FnOnce(&Connection) -> Result<T>,
) -> Result<T> {
    if !connection.is_autocommit() {
        return read(connection);
    }
    let transaction = Transaction::new_unchecked(connection, TransactionBehavior::Deferred)?;
    match read(&transaction) {
        Ok(output) => {
            transaction.commit()?;
            Ok(output)
        }
        Err(error) => {
            transaction.rollback()?;
            Err(error)
        }
    }
}

pub(crate) fn reserve_bindings(
    control: &StorageReadControl,
    values: &[&[u8]],
) -> Result<MemoryReservation> {
    control.check()?;
    let bytes = values.iter().try_fold(0usize, |size, value| {
        size.checked_add(value.len())
            .ok_or(MemoryError::SizeOverflow)
    })?;
    Ok(control.memory().reserve(bytes)?)
}

pub(crate) fn blob<'a>(row: &'a rusqlite::Row<'_>, column: usize) -> Result<&'a [u8]> {
    match row.get_ref(column)? {
        rusqlite::types::ValueRef::Blob(value) => Ok(value),
        value => Err(rusqlite::Error::InvalidColumnType(
            column,
            "encoded payload".into(),
            value.data_type(),
        )
        .into()),
    }
}

pub(crate) fn payload_length(value: i64) -> Result<usize> {
    usize::try_from(value)
        .map_err(|_| SQLiteError::StorageBackend("encoded payload is not a BLOB".into()))
}

pub(crate) fn copy_bytes(
    bytes: &[u8],
    extra: usize,
    control: &StorageReadControl,
) -> Result<BudgetedVec<u8>> {
    control.check()?;
    let mut output = BudgetedVec::new(control.memory());
    output.reserve(
        bytes
            .len()
            .checked_add(extra)
            .ok_or(MemoryError::SizeOverflow)?,
    )?;
    for (index, byte) in bytes.iter().copied().enumerate() {
        if index % 1024 == 0 {
            control.check()?;
        }
        output.push(byte)?;
    }
    control.check()?;
    Ok(output)
}
