//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bounded reads and evaluated row writes over the fixed native schema. Every size probe shares its caller's physical transaction with the corresponding payload read, and reads a small row itself.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock};

use parking_lot::Mutex;
use rusqlite::{
    params_from_iter,
    types::{ToSqlOutput, ValueRef},
    Connection,
};
use uqa_core::memory::{BudgetedVec, MemoryError};
use uqa_storage::mvcc::VersionError;
use uqa_storage::read_control::StorageReadControl;

use super::{decode_row, encode_row, invalid, NativeColumnType, NativeRecordLayout};
use crate::mvcc::PhysicalResult;

pub(super) fn columns(layout: &NativeRecordLayout) -> String {
    layout
        .columns
        .iter()
        .map(|name| format!("\"{name}\""))
        .collect::<Vec<_>>()
        .join(", ")
}

pub(super) fn primary_columns(layout: &NativeRecordLayout) -> String {
    layout
        .primary_key
        .iter()
        .map(|&slot| format!("\"{}\"", layout.columns[slot]))
        .collect::<Vec<_>>()
        .join(", ")
}

pub(super) fn primary_values<'a>(
    layout: &NativeRecordLayout,
    values: &[ValueRef<'a>],
    control: &StorageReadControl,
) -> PhysicalResult<BudgetedVec<ValueRef<'a>>> {
    let mut primary = BudgetedVec::new(control.memory());
    for &column in layout.primary_key {
        let value = values[column];
        if !layout.column_types[column].accepts(value) {
            return Err(invalid("native physical primary key cannot be NULL or REAL").into());
        }
        primary.push(value).map_err(VersionError::from)?;
    }
    Ok(primary)
}

pub(super) fn physical_key(
    layout: &NativeRecordLayout,
    values: &[ValueRef<'_>],
    control: &StorageReadControl,
) -> PhysicalResult<BudgetedVec<u8>> {
    Ok(encode_row(
        &primary_values(layout, values, control)?,
        control,
    )?)
}

fn parameters(count: usize) -> String {
    (1..=count)
        .map(|slot| format!("?{slot}"))
        .collect::<Vec<_>>()
        .join(", ")
}

pub(super) fn predicate(layout: &NativeRecordLayout, operator: &str) -> String {
    format!(
        "({}) {operator} ({})",
        primary_columns(layout),
        parameters(layout.primary_key.len())
    )
}

/// A row whose text and blob columns total at most this many bytes is read by the statement that measures it. A larger row is read by a second statement once its size is reserved.
const INLINE_ROW_BYTES: usize = 4096;

/// The rows a read selects relative to its primary key parameters.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Bound {
    Equal,
    After,
    First,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Statement {
    /// The row's size and, when it is small enough, its columns.
    Measure(Bound),
    /// Every column of a row whose size is reserved.
    Complete(Bound),
    Remove,
    Upsert,
}

/// The SQL of each fixed row statement, built once for its layout.
fn sql(layout: &'static NativeRecordLayout, statement: Statement) -> Arc<str> {
    type Texts = HashMap<(usize, Statement), Arc<str>>;
    static TEXTS: LazyLock<Mutex<Texts>> = LazyLock::new(Mutex::default);
    let key = (std::ptr::from_ref(layout) as usize, statement);
    if let Some(text) = TEXTS.lock().get(&key) {
        return Arc::clone(text);
    }
    let text: Arc<str> = match statement {
        Statement::Measure(bound) => {
            // octet_length reads the encoded byte size without loading the complete field.
            let sizes = layout
                .columns
                .iter()
                .zip(layout.column_types)
                .filter(|(_, kind)| **kind != NativeColumnType::Integer)
                .map(|(name, _)| format!("coalesce(octet_length(\"{name}\"), 0)"))
                .collect::<Vec<_>>();
            let sizes = if sizes.is_empty() {
                "0".to_owned()
            } else {
                sizes.join(" + ")
            };
            let inline = layout
                .columns
                .iter()
                .zip(layout.column_types)
                .map(|(name, kind)| {
                    if *kind == NativeColumnType::Integer {
                        format!("\"{name}\"")
                    } else {
                        format!("CASE WHEN {sizes} <= {INLINE_ROW_BYTES} THEN \"{name}\" END")
                    }
                })
                .collect::<Vec<_>>()
                .join(", ");
            format!("SELECT {sizes}, {inline} {}", selection(layout, bound))
        }
        Statement::Complete(bound) => {
            format!("SELECT {} {}", columns(layout), selection(layout, bound))
        }
        Statement::Remove => format!(
            "DELETE FROM {} WHERE {}",
            layout.table,
            predicate(layout, "=")
        ),
        Statement::Upsert => {
            let assignments = layout
                .columns
                .iter()
                .enumerate()
                .filter(|(slot, _)| !layout.primary_key.contains(slot))
                .map(|(_, name)| format!("\"{name}\" = excluded.\"{name}\""))
                .collect::<Vec<_>>();
            let action = if assignments.is_empty() {
                "NOTHING".to_owned()
            } else {
                format!("UPDATE SET {}", assignments.join(", "))
            };
            format!(
                "INSERT INTO {} ({}) VALUES ({}) ON CONFLICT ({}) DO {action}",
                layout.table,
                columns(layout),
                parameters(layout.columns.len()),
                primary_columns(layout)
            )
        }
    }
    .into();
    TEXTS.lock().insert(key, Arc::clone(&text));
    text
}

fn selection(layout: &NativeRecordLayout, bound: Bound) -> String {
    let condition = match bound {
        Bound::Equal => predicate(layout, "="),
        Bound::After => predicate(layout, ">"),
        Bound::First => "1".to_owned(),
    };
    format!(
        "FROM {} WHERE {condition} ORDER BY {} LIMIT 1",
        layout.table,
        primary_columns(layout)
    )
}

pub(super) fn get(
    connection: &Connection,
    layout: &'static NativeRecordLayout,
    physical_key: &[u8],
    control: &StorageReadControl,
) -> PhysicalResult<Option<BudgetedVec<u8>>> {
    let key = decode_row(physical_key, layout.primary_key.len(), control)?;
    read(connection, layout, Bound::Equal, &key, control)
}

fn read(
    connection: &Connection,
    layout: &'static NativeRecordLayout,
    bound: Bound,
    parameters: &[ValueRef<'_>],
    control: &StorageReadControl,
) -> PhysicalResult<Option<BudgetedVec<u8>>> {
    control.cancellation().check().map_err(VersionError::from)?;
    let _bindings = reserve_values(parameters, control)?;
    let bind = || params_from_iter(parameters.iter().copied().map(ToSqlOutput::Borrowed));
    let mut statement = connection.prepare_cached(&sql(layout, Statement::Measure(bound)))?;
    let mut rows = statement.query(bind())?;
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    let stored =
        usize::try_from(row.get::<_, i64>(0)?).map_err(|_| invalid("invalid native row size"))?;
    // A UTF-16 database can expand to UTF-8; three times its bytes also bounds that conversion.
    let size = stored
        .checked_mul(3)
        .ok_or(VersionError::from(MemoryError::SizeOverflow))?;
    let _payload = control.memory().reserve(size).map_err(VersionError::from)?;
    control.cancellation().check().map_err(VersionError::from)?;
    if stored <= INLINE_ROW_BYTES {
        return encode(layout, row, 1, size, control).map(Some);
    }
    drop(rows);
    drop(statement);
    let mut statement = connection.prepare_cached(&sql(layout, Statement::Complete(bound)))?;
    let mut rows = statement.query(bind())?;
    let row = rows
        .next()?
        .ok_or_else(|| invalid("native row disappeared within a physical read"))?;
    encode(layout, row, 0, size, control).map(Some)
}

/// Encode the columns of `row`, which start at `first` and whose text and blob bytes `size` bounds.
fn encode(
    layout: &NativeRecordLayout,
    row: &rusqlite::Row<'_>,
    first: usize,
    size: usize,
    control: &StorageReadControl,
) -> PhysicalResult<BudgetedVec<u8>> {
    let mut values = BudgetedVec::new(control.memory());
    let mut actual = 0usize;
    for slot in 0..layout.columns.len() {
        let value = row.get_ref(first + slot)?;
        if let ValueRef::Text(bytes) | ValueRef::Blob(bytes) = value {
            actual = actual
                .checked_add(bytes.len())
                .ok_or(VersionError::from(MemoryError::SizeOverflow))?;
        }
        values.push(value).map_err(VersionError::from)?;
    }
    if actual > size {
        return Err(invalid("native row exceeded its reserved size").into());
    }
    layout.validate_values(&values)?;
    Ok(encode_row(&values, control)?)
}

pub(super) fn visit(
    connection: &Connection,
    layout: &'static NativeRecordLayout,
    control: &StorageReadControl,
    mut visit: impl FnMut(&[ValueRef<'_>]) -> PhysicalResult<()>,
) -> PhysicalResult<()> {
    let mut cursor: Option<BudgetedVec<u8>> = None;
    loop {
        let key = cursor
            .as_deref()
            .map(|bytes| decode_row(bytes, layout.primary_key.len(), control))
            .transpose()?;
        let bound = if key.is_some() {
            Bound::After
        } else {
            Bound::First
        };
        let Some(row) = read(
            connection,
            layout,
            bound,
            key.as_deref().unwrap_or(&[]),
            control,
        )?
        else {
            break;
        };
        let values = decode_row(&row, layout.columns.len(), control)?;
        let next = physical_key(layout, &values, control)?;
        visit(&values)?;
        drop(key);
        cursor = Some(next);
    }
    Ok(())
}

pub(super) fn reserve_values(
    values: &[ValueRef<'_>],
    control: &StorageReadControl,
) -> PhysicalResult<uqa_core::memory::MemoryReservation> {
    let mut size = 0usize;
    for value in values {
        if let ValueRef::Text(bytes) | ValueRef::Blob(bytes) = value {
            size = size
                .checked_add(bytes.len())
                .ok_or(VersionError::from(MemoryError::SizeOverflow))?;
        }
    }
    Ok(control.memory().reserve(size).map_err(VersionError::from)?)
}

pub(super) fn remove(
    connection: &Connection,
    layout: &'static NativeRecordLayout,
    key: &[u8],
    control: &StorageReadControl,
) -> PhysicalResult<()> {
    let values = decode_row(key, layout.primary_key.len(), control)?;
    let _bindings = reserve_values(&values, control)?;
    connection
        .prepare_cached(&sql(layout, Statement::Remove))?
        .execute(params_from_iter(
            values.iter().copied().map(ToSqlOutput::Borrowed),
        ))?;
    Ok(())
}

pub(super) fn upsert(
    connection: &Connection,
    layout: &'static NativeRecordLayout,
    values: &[ValueRef<'_>],
    control: &StorageReadControl,
) -> PhysicalResult<()> {
    layout.validate_values(values)?;
    let _bindings = reserve_values(values, control)?;
    connection
        .prepare_cached(&sql(layout, Statement::Upsert))?
        .execute(params_from_iter(
            values.iter().copied().map(ToSqlOutput::Borrowed),
        ))?;
    Ok(())
}

#[cfg(test)]
mod tests;
