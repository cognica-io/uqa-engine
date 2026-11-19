//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrowed native cells keep direct field validation ahead of authority and owner validation.

use crate::catalog::{
    role_security::input::{sequence, take},
    sequences_views::codec::{decode_sequence_identity, decode_sequence_owner},
    Result, SQLiteError,
};
use rusqlite::types::{FromSql, FromSqlError, ValueRef};
use uqa_core::memory::{Budgeted, ProductionControl};
use uqa_storage::{
    read_control::StorageReadControl, RelationIdentity, SequenceOptions, SequenceRow,
};

pub(super) fn read(
    row: &[ValueRef<'_>],
    control: &StorageReadControl,
) -> Result<Budgeted<SequenceRow>> {
    control.check()?;
    let schema = text(row, 0, control)?;
    let name = text(row, 1, control)?;
    let object_id = borrowed_blob(row, 8)?;
    let generation = borrowed_blob(row, 14)?;
    let start = field(row, 3)?;
    let increment = field(row, 4)?;
    let current = field(row, 5)?;
    let called = field(row, 6)?;
    let persistence = text(row, 7, control)?;
    let data_type = text(row, 9, control)?;
    let min_value = field(row, 10)?;
    let max_value = field(row, 11)?;
    let cycle = field(row, 12)?;
    let cache_size = field(row, 13)?;
    let table = (row[15] != ValueRef::Null)
        .then(|| borrowed_blob(row, 15))
        .transpose()?;
    let column = (row[16] != ValueRef::Null)
        .then(|| borrowed_blob(row, 16))
        .transpose()?;
    let dependency = (row[17] != ValueRef::Null)
        .then(|| borrowed_text(row, 17))
        .transpose()?;
    let owner = row[18];
    let acl = (row[19] != ValueRef::Null)
        .then(|| borrowed_text(row, 19))
        .transpose()?;
    let log_count = field(row, 20)?;
    let mut memory = control.memory().empty_reservation();
    let relation = RelationIdentity {
        schema: take(schema, &mut memory),
        name: take(name, &mut memory),
    };
    let security = sequence(owner, acl, control)?;
    let owner = decode_sequence_owner(&relation, table, column, dependency)?;
    let object_id = decode_sequence_identity(&relation, "object identity", object_id)?;
    let definition_generation =
        decode_sequence_identity(&relation, "definition generation", generation)?;
    control.check()?;
    Ok(Budgeted::new(
        SequenceRow {
            relation,
            security: take(security, &mut memory),
            owner,
            object_id,
            definition_generation,
            start,
            increment,
            current,
            called,
            log_count,
            persistence: take(persistence, &mut memory),
            options: SequenceOptions {
                data_type: take(data_type, &mut memory),
                min_value: Some(min_value),
                max_value: Some(max_value),
                cycle,
                cache_size,
            },
        },
        memory,
    ))
}

fn error(index: usize, error: &FromSqlError) -> SQLiteError {
    SQLiteError::StorageBackend(format!("invalid native sequence column {index}: {error}"))
}
fn field<T: FromSql>(row: &[ValueRef<'_>], index: usize) -> Result<T> {
    T::column_result(row[index]).map_err(|cause| error(index, &cause))
}
fn borrowed_text<'a>(row: &[ValueRef<'a>], index: usize) -> Result<&'a str> {
    row[index].as_str().map_err(|cause| error(index, &cause))
}
fn borrowed_blob<'a>(row: &[ValueRef<'a>], index: usize) -> Result<&'a [u8]> {
    row[index].as_blob().map_err(|cause| error(index, &cause))
}
fn text(
    row: &[ValueRef<'_>],
    index: usize,
    control: &StorageReadControl,
) -> Result<Budgeted<String>> {
    Ok(ProductionControl::new(
        control.memory(),
        control.cancellation(),
        control.cancellation(),
    )
    .copy_text(borrowed_text(row, index)?)
    .map_err(uqa_storage::StorageBackendError::from)?
    .into_budgeted()
    .expect("admitted native sequence text"))
}

#[cfg(test)]
mod tests;
