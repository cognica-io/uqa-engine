//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bounded reads and evaluated row writes over the fixed native schema. Every size probe shares its caller's physical transaction with the corresponding payload read.

use rusqlite::{
    params_from_iter,
    types::{ToSqlOutput, ValueRef},
    Connection, OptionalExtension,
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

pub(super) fn get(
    connection: &Connection,
    layout: &NativeRecordLayout,
    physical_key: &[u8],
    control: &StorageReadControl,
) -> PhysicalResult<Option<BudgetedVec<u8>>> {
    let key = decode_row(physical_key, layout.primary_key.len(), control)?;
    read(connection, layout, &predicate(layout, "="), &key, control)
}

fn read(
    connection: &Connection,
    layout: &NativeRecordLayout,
    condition: &str,
    parameters: &[ValueRef<'_>],
    control: &StorageReadControl,
) -> PhysicalResult<Option<BudgetedVec<u8>>> {
    control.cancellation().check().map_err(VersionError::from)?;
    let _bindings = reserve_values(parameters, control)?;
    // octet_length reads the encoded byte size without loading the complete field. A UTF-16 database can expand to UTF-8; three times its bytes also bounds that conversion.
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
    let suffix = format!(
        "FROM {} WHERE {condition} ORDER BY {} LIMIT 1",
        layout.table,
        primary_columns(layout)
    );
    let size: Option<i64> = connection
        .query_row(
            &format!("SELECT {sizes} {suffix}"),
            params_from_iter(parameters.iter().copied().map(ToSqlOutput::Borrowed)),
            |row| row.get(0),
        )
        .optional()?;
    let Some(size) = size else { return Ok(None) };
    let size = usize::try_from(size)
        .map_err(|_| invalid("invalid native row size"))?
        .checked_mul(3)
        .ok_or(VersionError::from(MemoryError::SizeOverflow))?;
    let _payload = control.memory().reserve(size).map_err(VersionError::from)?;
    control.cancellation().check().map_err(VersionError::from)?;
    let mut statement = connection.prepare(&format!("SELECT {} {suffix}", columns(layout)))?;
    let mut rows = statement.query(params_from_iter(
        parameters.iter().copied().map(ToSqlOutput::Borrowed),
    ))?;
    let row = rows
        .next()?
        .ok_or_else(|| invalid("native row disappeared within a physical read"))?;
    let mut values = BudgetedVec::new(control.memory());
    let mut actual = 0usize;
    for slot in 0..layout.columns.len() {
        let value = row.get_ref(slot)?;
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
    Ok(Some(encode_row(&values, control)?))
}

pub(super) fn visit(
    connection: &Connection,
    layout: &NativeRecordLayout,
    control: &StorageReadControl,
    mut visit: impl FnMut(&[ValueRef<'_>]) -> PhysicalResult<()>,
) -> PhysicalResult<()> {
    let mut cursor: Option<BudgetedVec<u8>> = None;
    loop {
        let key = cursor
            .as_deref()
            .map(|bytes| decode_row(bytes, layout.primary_key.len(), control))
            .transpose()?;
        let condition = if key.is_some() {
            predicate(layout, ">")
        } else {
            "1".to_owned()
        };
        let Some(row) = read(
            connection,
            layout,
            &condition,
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
    layout: &NativeRecordLayout,
    key: &[u8],
    control: &StorageReadControl,
) -> PhysicalResult<()> {
    let values = decode_row(key, layout.primary_key.len(), control)?;
    let _bindings = reserve_values(&values, control)?;
    connection.execute(
        &format!(
            "DELETE FROM {} WHERE {}",
            layout.table,
            predicate(layout, "=")
        ),
        params_from_iter(values.iter().copied().map(ToSqlOutput::Borrowed)),
    )?;
    Ok(())
}

pub(super) fn upsert(
    connection: &Connection,
    layout: &NativeRecordLayout,
    values: &[ValueRef<'_>],
    control: &StorageReadControl,
) -> PhysicalResult<()> {
    layout.validate_values(values)?;
    let _bindings = reserve_values(values, control)?;
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
    connection.execute(
        &format!(
            "INSERT INTO {} ({}) VALUES ({}) ON CONFLICT ({}) DO {action}",
            layout.table,
            columns(layout),
            parameters(values.len()),
            primary_columns(layout)
        ),
        params_from_iter(values.iter().copied().map(ToSqlOutput::Borrowed)),
    )?;
    Ok(())
}
