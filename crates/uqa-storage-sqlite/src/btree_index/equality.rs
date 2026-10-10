//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Versioned equality keys for the physical scalar and tuple lookup index. Unsupported SQL comparison domains remain NULL so a probe can decline them with one index seek.

use super::{decode_value, Result, Value};
use rusqlite::{functions::FunctionFlags, Connection};
use uqa_core::memory::BudgetedVec;
use uqa_storage::read_control::StorageReadControl;

pub(crate) const INDEX_NAME: &str = "_btree_index_equal_v1";
pub(crate) const INDEX_SQL: &str = "CREATE INDEX _btree_index_equal_v1 ON _btree_index_entries(table_name, field, __uqa_btree_equal_v1(value_json), doc_id)";

/// Register once per physical connection, before any schema or statement uses the function. Replacing it on every probe would invalidate the prepared statement cache.
pub(crate) fn register(connection: &Connection) -> rusqlite::Result<()> {
    let registered: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_function_list WHERE name = '__uqa_btree_equal_v1')",
        [],
        |row| row.get(0),
    )?;
    if !registered {
        connection.create_scalar_function(
            "__uqa_btree_equal_v1",
            1,
            FunctionFlags::SQLITE_UTF8
                | FunctionFlags::SQLITE_INNOCUOUS
                | FunctionFlags::SQLITE_DETERMINISTIC,
            |context| {
                let encoded = context.get_raw(0).as_str()?;
                let value = decode_value(encoded)
                    .map_err(|error| rusqlite::Error::UserFunctionError(Box::new(error)))?;
                key(&value, &StorageReadControl::with_limit(usize::MAX))
                    .map(|key| key.map(|key| key.into_parts().0))
                    .map_err(|error| rusqlite::Error::UserFunctionError(Box::new(error)))
            },
        )?;
    }
    Ok(())
}

pub(crate) fn install(connection: &Connection) -> Result<()> {
    register(connection)?;
    connection.execute_batch("DROP INDEX IF EXISTS _btree_index_value_idx")?;
    let exists: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'index' AND name = ?1)",
        [INDEX_NAME],
        |row| row.get(0),
    )?;
    if !exists {
        connection.execute_batch(INDEX_SQL)?;
    }
    Ok(())
}

/// Core equality for the infallible scalar domain and rows composed of it. Every component has an explicit tag and length; integral floats, booleans and integers share a key only when Core compares their exact represented values as equal.
pub(super) fn key(value: &Value, control: &StorageReadControl) -> Result<Option<BudgetedVec<u8>>> {
    let mut output = BudgetedVec::new(control.memory());
    let mut pending = BudgetedVec::new(control.memory());
    pending.push(value)?;
    while let Some(value) = pending.pop() {
        control.check()?;
        match value {
            Value::Null => output.push(0)?,
            Value::Bool(value) => integer(i64::from(*value), &mut output)?,
            Value::Int(value) => integer(*value, &mut output)?,
            Value::Float(value) => {
                if value.is_nan() {
                    return Ok(None);
                }
                let integral = *value as i64;
                if Value::Int(integral) == Value::Float(*value) {
                    integer(integral, &mut output)?;
                } else {
                    output.push(2)?;
                    output.extend_from_slice(&value.to_bits().to_be_bytes())?;
                }
            }
            Value::Str(value) => bytes(3, value.as_bytes(), &mut output)?,
            Value::Bytes(value) => bytes(4, value, &mut output)?,
            Value::Row(values) => {
                output.push(5)?;
                output.extend_from_slice(&(values.len() as u64).to_be_bytes())?;
                for child in values.iter().rev() {
                    pending.push(child)?;
                }
            }
            _ => return Ok(None),
        }
    }
    Ok(Some(output))
}

fn integer(value: i64, output: &mut BudgetedVec<u8>) -> Result<()> {
    output.push(1)?;
    output.extend_from_slice(&value.to_be_bytes())?;
    Ok(())
}

fn bytes(tag: u8, value: &[u8], output: &mut BudgetedVec<u8>) -> Result<()> {
    output.push(tag)?;
    output.extend_from_slice(&(value.len() as u64).to_be_bytes())?;
    output.extend_from_slice(value)?;
    Ok(())
}
