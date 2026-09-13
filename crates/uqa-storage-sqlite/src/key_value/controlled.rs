//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Size probes precede BLOB materialization in the same retained `SQLite` snapshot.

use crate::{
    read_control::{blob, copy_bytes, payload_length, reserve_bindings},
    Result, SQLiteError,
};
use rusqlite::{params, Connection, OptionalExtension};
use uqa_core::memory::{BudgetedVec, MemoryError};
use uqa_storage::read_control::{KeyValueReadVisitor, StorageReadControl, ValueReadVisitor};

pub(super) fn value(
    connection: &Connection,
    key: &[u8],
    control: &StorageReadControl,
    visit: &mut ValueReadVisitor<'_>,
) -> Result<()> {
    let _bindings = reserve_bindings(control, &[key])?;
    let length: Option<i64> = connection.query_row("SELECT CASE WHEN typeof(value) = 'blob' THEN length(value) ELSE -1 END FROM _key_value WHERE key = ?1", params![key], |row| row.get(0)).optional()?;
    let Some(length) = length else {
        control.check()?;
        visit(None)?;
        control.check()?;
        return Ok(());
    };
    let length = payload_length(length)?;
    control.check()?;
    let _payload = control.memory().reserve(length)?;
    let mut statement = connection.prepare("SELECT value FROM _key_value WHERE key = ?1")?;
    let mut rows = statement.query(params![key])?;
    let row = rows.next()?.ok_or_else(|| {
        SQLiteError::StorageBackend("encoded value disappeared from its read snapshot".into())
    })?;
    let bytes = blob(row, 0)?;
    if bytes.len() != length {
        return Err(SQLiteError::StorageBackend(
            "encoded value size changed within its read snapshot".into(),
        ));
    }
    control.check()?;
    visit(Some(bytes))?;
    control.check()?;
    Ok(())
}

pub(super) fn prefix(
    connection: &Connection,
    prefix: &[u8],
    after: Option<&[u8]>,
    limit: usize,
    control: &StorageReadControl,
    visit: &mut KeyValueReadVisitor<'_>,
) -> Result<()> {
    control.check()?;
    let upper = upper_bound(prefix, control)?;
    let mut lower = match after.filter(|after| *after >= prefix) {
        Some(after) => {
            let mut bytes = copy_bytes(after, 1, control)?;
            bytes.push(0)?;
            bytes
        }
        None => copy_bytes(prefix, 0, control)?,
    };
    for at in 0..limit {
        control.check()?;
        let next = row_after(
            connection,
            &lower,
            upper.as_deref(),
            at + 1 < limit,
            control,
            visit,
        )?;
        let Some(next) = next else {
            break;
        };
        lower = next;
    }
    control.check()?;
    Ok(())
}

fn row_after(
    connection: &Connection,
    lower: &[u8],
    upper: Option<&[u8]>,
    continue_after: bool,
    control: &StorageReadControl,
    visit: &mut KeyValueReadVisitor<'_>,
) -> Result<Option<BudgetedVec<u8>>> {
    let _bindings = reserve_bindings(control, &[lower, upper.unwrap_or_default()])?;
    let sizes_sql = if upper.is_some() {
        "SELECT CASE WHEN typeof(key) = 'blob' THEN length(key) ELSE -1 END, CASE WHEN typeof(value) = 'blob' THEN length(value) ELSE -1 END FROM _key_value WHERE key >= ?1 AND key < ?2 ORDER BY key LIMIT 1"
    } else {
        "SELECT CASE WHEN typeof(key) = 'blob' THEN length(key) ELSE -1 END, CASE WHEN typeof(value) = 'blob' THEN length(value) ELSE -1 END FROM _key_value WHERE key >= ?1 ORDER BY key LIMIT 1"
    };
    let sizes: Option<(i64, i64)> = if let Some(upper) = upper {
        connection
            .query_row(sizes_sql, params![lower, upper], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .optional()?
    } else {
        connection
            .query_row(sizes_sql, params![lower], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .optional()?
    };
    let Some((key_size, value_size)) = sizes else {
        return Ok(None);
    };
    let key_size = payload_length(key_size)?;
    let value_size = payload_length(value_size)?;
    control.check()?;
    let _payload = control.memory().reserve(
        key_size
            .checked_add(value_size)
            .ok_or(MemoryError::SizeOverflow)?,
    )?;
    let sql = if upper.is_some() {
        "SELECT key, value FROM _key_value WHERE key >= ?1 AND key < ?2 ORDER BY key LIMIT 1"
    } else {
        "SELECT key, value FROM _key_value WHERE key >= ?1 ORDER BY key LIMIT 1"
    };
    let mut statement = connection.prepare(sql)?;
    let mut rows = if let Some(upper) = upper {
        statement.query(params![lower, upper])?
    } else {
        statement.query(params![lower])?
    };
    let row = rows.next()?.ok_or_else(|| {
        SQLiteError::StorageBackend("encoded entry disappeared from its read snapshot".into())
    })?;
    let key = blob(row, 0)?;
    let value = blob(row, 1)?;
    if key.len() != key_size || value.len() != value_size {
        return Err(SQLiteError::StorageBackend(
            "encoded entry size changed within its read snapshot".into(),
        ));
    }
    control.check()?;
    visit(key, value)?;
    let next = if continue_after {
        let mut bytes = copy_bytes(key, 1, control)?;
        bytes.push(0)?;
        Some(bytes)
    } else {
        None
    };
    control.check()?;
    Ok(next)
}

fn upper_bound(prefix: &[u8], control: &StorageReadControl) -> Result<Option<BudgetedVec<u8>>> {
    let mut upper = copy_bytes(prefix, 0, control)?;
    loop {
        control.check()?;
        match upper.pop() {
            Some(255) => {}
            Some(byte) => {
                upper.push(byte + 1)?;
                return Ok(Some(upper));
            }
            None => return Ok(None),
        }
    }
}

pub(super) fn contains_prefix(
    connection: &Connection,
    prefix: &[u8],
    control: &StorageReadControl,
) -> Result<bool> {
    let upper = upper_bound(prefix, control)?;
    let _bindings = reserve_bindings(control, &[prefix, upper.as_deref().unwrap_or_default()])?;
    let found = if let Some(upper) = upper.as_deref() {
        connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM _key_value WHERE key >= ?1 AND key < ?2)",
            params![prefix, upper],
            |row| row.get(0),
        )?
    } else {
        connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM _key_value WHERE key >= ?1)",
            params![prefix],
            |row| row.get(0),
        )?
    };
    control.check()?;
    Ok(found)
}

#[cfg(test)]
mod tests;
