//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Page transaction-local SQL work queues without retaining a database-sized Rust collection.

use std::fmt::Write as _;

use rusqlite::{params, types::ValueRef, Connection};
use uqa_core::memory::BudgetedVec;
use uqa_storage::{mvcc::VersionError, read_control::StorageReadControl};

use super::{invalid, NativeRecordFamily};
use crate::mvcc::{codec, PhysicalResult};

/// Rows one statement reads. Their keys are copied before any of them is visited.
const PAGE: usize = 64;
/// A key of at most this many bytes is read with its page. A longer key ends the page and is read by itself once its length is reserved.
const INLINE_KEY_BYTES: usize = 1024;
/// A value of at most this many bytes is read with its page. A longer one is left to its visitor.
const INLINE_VALUE_BYTES: usize = 4096;

/// The value column of a visited row.
pub(super) enum QueuedValue<'a> {
    Read(Option<&'a [u8]>),
    /// Too long to read with its page.
    Unread,
}

enum Value {
    Read(Option<BudgetedVec<u8>>),
    Unread,
}

/// One row of a page: its family, its value and its key, or the length of a key too long to read with the page.
struct Queued {
    family: u16,
    key: Result<BudgetedVec<u8>, usize>,
    value: Value,
}

/// Visit the rows of `table` that satisfy `condition` in key order. `visit` may change other tables, but must not add rows to `table`: a page of keys is read before its rows are visited.
pub(super) fn visit(
    connection: &Connection,
    table: &'static str,
    condition: &str,
    control: &StorageReadControl,
    mut visit: impl FnMut(NativeRecordFamily, &[u8]) -> PhysicalResult<()>,
) -> PhysicalResult<()> {
    pages(
        connection,
        table,
        condition,
        None,
        control,
        |family, key, _| visit(family, key),
    )
}

/// [`visit`] with each row's `value` column, which is a BLOB or NULL.
pub(super) fn visit_with_value(
    connection: &Connection,
    table: &'static str,
    condition: &str,
    value: &'static str,
    control: &StorageReadControl,
    visit: impl FnMut(NativeRecordFamily, &[u8], QueuedValue<'_>) -> PhysicalResult<()>,
) -> PhysicalResult<()> {
    pages(connection, table, condition, Some(value), control, visit)
}

fn pages(
    connection: &Connection,
    table: &'static str,
    condition: &str,
    value: Option<&'static str>,
    control: &StorageReadControl,
    mut visit: impl FnMut(NativeRecordFamily, &[u8], QueuedValue<'_>) -> PhysicalResult<()>,
) -> PhysicalResult<()> {
    // The row value comparison seeks both primary-key columns even when the condition also filters a constant family.
    let order = "ORDER BY family, physical_key";
    let first = format!("FROM {table} WHERE ({condition}) {order}");
    let next =
        format!("FROM {table} WHERE ({condition}) AND (family, physical_key) > (?1, ?2) {order}");
    let mut page = format!("SELECT family, length(physical_key), CASE WHEN length(physical_key) <= {INLINE_KEY_BYTES} THEN physical_key END");
    if let Some(value) = value {
        let _ = write!(
            page,
            ", {value} IS NULL, CASE WHEN length({value}) <= {INLINE_VALUE_BYTES} THEN {value} END"
        );
    }
    let mut after: Option<(u16, BudgetedVec<u8>)> = None;
    loop {
        control.cancellation().check().map_err(VersionError::from)?;
        let suffix = if after.is_some() { &next } else { &first };
        let mut queued = Vec::with_capacity(PAGE);
        {
            let _bindings = crate::read_control::reserve_bindings(
                control,
                &[after.as_ref().map_or(&[][..], |(_, key)| key)],
            )?;
            let mut statement =
                connection.prepare_cached(&format!("{page} {suffix} LIMIT {PAGE}"))?;
            let mut rows = match &after {
                Some((family, key)) => statement.query(params![family, &key[..]])?,
                None => statement.query([])?,
            };
            while let Some(row) = rows.next()? {
                let length = usize::try_from(row.get::<_, i64>(1)?)
                    .map_err(|_| invalid("invalid native work queue key length"))?;
                let key = match row.get_ref(2)? {
                    ValueRef::Blob(bytes) if bytes.len() == length => Ok(copy(bytes, control)?),
                    ValueRef::Null if length > INLINE_KEY_BYTES => Err(length),
                    _ => return Err(invalid("invalid native work queue key").into()),
                };
                let value = if value.is_none() {
                    Value::Unread
                } else if row.get::<_, bool>(3)? {
                    Value::Read(None)
                } else {
                    match row.get_ref(4)? {
                        ValueRef::Blob(bytes) => Value::Read(Some(copy(bytes, control)?)),
                        ValueRef::Null => Value::Unread,
                        _ => return Err(invalid("invalid native work queue value").into()),
                    }
                };
                let long = key.is_err();
                queued.push(Queued {
                    family: row.get(0)?,
                    key,
                    value,
                });
                if long {
                    break;
                }
            }
        }
        if queued.is_empty() {
            return Ok(());
        }
        for Queued { family, key, value } in queued {
            control.cancellation().check().map_err(VersionError::from)?;
            let key = match key {
                Ok(key) => key,
                Err(length) => {
                    // Every row before this one in the page has been visited, so it is the first row after the last visited key.
                    let _payload = control
                        .memory()
                        .reserve(length)
                        .map_err(VersionError::from)?;
                    let suffix = if after.is_some() { &next } else { &first };
                    let mut statement = connection
                        .prepare_cached(&format!("SELECT physical_key {suffix} LIMIT 1"))?;
                    let mut rows = match &after {
                        Some((family, key)) => statement.query(params![family, &key[..]])?,
                        None => statement.query([])?,
                    };
                    let row = rows.next()?.ok_or_else(|| {
                        invalid("native work queue changed within its transaction")
                    })?;
                    let bytes = codec::bytes(row, 0)?;
                    if bytes.len() != length {
                        return Err(invalid(
                            "native work queue key changed within its transaction",
                        )
                        .into());
                    }
                    copy(bytes, control)?
                }
            };
            let parsed = NativeRecordFamily::from_id(family)
                .ok_or_else(|| invalid("unknown native work queue family"))?;
            let value = match &value {
                Value::Read(value) => QueuedValue::Read(value.as_deref()),
                Value::Unread => QueuedValue::Unread,
            };
            visit(parsed, &key, value)?;
            after = Some((family, key));
        }
    }
}

fn copy(bytes: &[u8], control: &StorageReadControl) -> PhysicalResult<BudgetedVec<u8>> {
    let mut copied = BudgetedVec::new(control.memory());
    copied
        .extend_from_slice(bytes)
        .map_err(VersionError::from)?;
    Ok(copied)
}

#[cfg(test)]
mod tests;
