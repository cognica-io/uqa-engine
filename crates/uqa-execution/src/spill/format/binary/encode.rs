//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Binary spill sizing and encoding.

use std::fs::File;
use std::io::{BufWriter, Seek, SeekFrom, Write};

use crate::batch::{Batch, ColumnIdentity, PhysicalRow, RowSchema};
use crate::physical::ExecResult;
use crate::spill::format::{spill_error, RECORD_PREFIX_BYTES};
use uqa_core::{TemporalValue, Value};

use super::MAX_VALUE_DEPTH;

const ROW_METADATA_LOCK_ORIGINS: usize = 1;

pub(crate) fn encoded_batch_size(batch: &Batch) -> ExecResult<usize> {
    encoded_rows_size(&batch.schema, &batch.rows)
}

pub(crate) fn encode_physical_row_record(
    row: &PhysicalRow,
    physical_width: usize,
) -> ExecResult<Vec<u8>> {
    let bytes = encoded_physical_row_record_size(row, physical_width)?;
    let mut record = Vec::new();
    record
        .try_reserve_exact(bytes)
        .map_err(|error| spill_error(format!("cannot allocate spill row record: {error}")))?;
    write_u64(&mut record, physical_width)?;
    for slot in 0..physical_width {
        let value = row.value(slot).ok_or_else(|| {
            spill_error(format!(
                "physical row is missing slot {slot} of {physical_width}"
            ))
        })?;
        encode_value(&mut record, value, 0)?;
    }
    if !row.lock_origins().is_empty() {
        encode_lock_origins(&mut record, row)?;
    }
    debug_assert_eq!(record.len(), bytes);
    Ok(record)
}

pub(crate) fn encoded_physical_row_record_size(
    row: &PhysicalRow,
    physical_width: usize,
) -> ExecResult<usize> {
    let mut bytes = 8_usize;
    for slot in 0..physical_width {
        let value = row.value(slot).ok_or_else(|| {
            spill_error(format!(
                "physical row is missing slot {slot} of {physical_width}"
            ))
        })?;
        add_value_size(&mut bytes, value, 0)?;
    }
    if !row.lock_origins().is_empty() {
        add_lock_origins_size(&mut bytes, row)?;
    }
    if row.value(physical_width).is_some() {
        return Err(spill_error(format!(
            "physical row has more than {physical_width} slots"
        )));
    }
    Ok(bytes)
}

pub(crate) fn encoded_batch_overhead_size(schema: &RowSchema) -> ExecResult<usize> {
    encoded_rows_size(schema, &[])
}

fn encoded_rows_size(schema: &RowSchema, rows: &[PhysicalRow]) -> ExecResult<usize> {
    let mut bytes = RECORD_PREFIX_BYTES;
    add_size(&mut bytes, 8, "schema physical width")?;
    add_size(&mut bytes, 8, "schema column count")?;
    for (logical, column) in schema.columns().iter().enumerate() {
        add_string_size(&mut bytes, column, "schema column")?;
        add_identity_size(&mut bytes, &schema.identities()[logical], "schema identity")?;
        add_size(&mut bytes, 8, "schema logical slot")?;
        add_string_size(
            &mut bytes,
            &encoded_column_type(schema.column_type(logical))?,
            "schema column type",
        )?;
        if schema
            .slot(logical)
            .is_some_and(|slot| slot >= schema.physical_width())
        {
            return Err(spill_error(format!(
                "schema logical slot {logical} is outside physical width {}",
                schema.physical_width()
            )));
        }
    }
    let aliases = schema.lookup_aliases_with_types();
    add_size(&mut bytes, 8, "schema alias count")?;
    for (identity, slot, ty) in aliases {
        add_identity_size(&mut bytes, identity, "schema alias")?;
        add_size(&mut bytes, 8, "schema alias slot")?;
        add_string_size(&mut bytes, &encoded_column_type(ty)?, "schema alias type")?;
        if slot.is_some_and(|slot| slot >= schema.physical_width()) {
            return Err(spill_error(format!(
                "schema alias `{identity:?}` is outside physical width {}",
                schema.physical_width()
            )));
        }
    }
    let has_lock_origins = rows.iter().any(|row| !row.lock_origins().is_empty());
    add_size(&mut bytes, 8, "row metadata flags")?;
    add_size(&mut bytes, 8, "batch row count")?;
    for row in rows {
        let mut row_bytes = encoded_physical_row_record_size(row, schema.physical_width())?;
        if has_lock_origins && row.lock_origins().is_empty() {
            add_size(&mut row_bytes, 8, "lock origin count")?;
        }
        add_size(&mut bytes, row_bytes, "physical row record")?;
    }
    Ok(bytes)
}

fn add_size(total: &mut usize, bytes: usize, description: &str) -> ExecResult<()> {
    *total = total
        .checked_add(bytes)
        .ok_or_else(|| spill_error(format!("{description} size overflow")))?;
    Ok(())
}

fn add_string_size(total: &mut usize, value: &str, description: &str) -> ExecResult<()> {
    add_size(total, 8, description)?;
    add_size(total, value.len(), description)
}

fn add_identity_size(
    total: &mut usize,
    identity: &ColumnIdentity,
    description: &str,
) -> ExecResult<()> {
    add_string_size(total, identity.qualifier().unwrap_or(""), description)?;
    add_string_size(total, identity.column(), description)
}

fn encoded_column_type(ty: Option<&uqa_sql::ast::ColumnType>) -> ExecResult<String> {
    ty.map_or_else(
        || Ok(String::new()),
        |ty| {
            serde_json::to_string(ty)
                .map_err(|error| spill_error(format!("cannot encode spill column type: {error}")))
        },
    )
}

fn add_value_size(total: &mut usize, value: &Value, depth: usize) -> ExecResult<()> {
    if depth > MAX_VALUE_DEPTH {
        return Err(spill_error("spill value nesting exceeds 128 levels"));
    }
    add_size(total, 1, "value tag")?;
    match value {
        Value::Null => Ok(()),
        Value::Bool(_) => add_size(total, 1, "boolean value"),
        Value::Int(_) | Value::Float(_) => add_size(total, 8, "numeric value"),
        Value::Str(value) => add_string_size(total, value, "string value"),
        Value::FixedChar(value) => add_string_size(total, value, "fixed character value"),
        Value::Bytes(value) => {
            add_size(total, 8, "byte value length")?;
            add_size(total, value.len(), "byte value")
        }
        Value::Temporal(value) => add_size(total, temporal_payload_size(value), "temporal value"),
        Value::Decimal(value) => {
            add_size(total, 8, "decimal value length")?;
            add_size(total, value.sql_string_len(), "decimal value")
        }
        Value::Json(value) | Value::JsonB(value) => add_string_size(total, value, "JSON value"),
        Value::Array(array) => {
            add_size(total, 8, "array lower-bound count")?;
            add_size(
                total,
                array
                    .lower_bounds()
                    .len()
                    .checked_mul(4)
                    .ok_or_else(|| spill_error("array lower-bound size overflow"))?,
                "array lower bounds",
            )?;
            add_size(total, 8, "array element count")?;
            for value in array.elements() {
                add_value_size(total, value, depth + 1)?;
            }
            Ok(())
        }
        Value::List(values) => {
            add_size(total, 8, "list length")?;
            for value in values {
                add_value_size(total, value, depth + 1)?;
            }
            Ok(())
        }
        Value::Row(values) => {
            add_size(total, 8, "row length")?;
            for value in values {
                add_value_size(total, value, depth + 1)?;
            }
            Ok(())
        }
        Value::Record(fields) => {
            add_size(total, 8, "record length")?;
            for (name, value) in fields {
                add_string_size(total, name, "record field name")?;
                add_value_size(total, value, depth + 1)?;
            }
            Ok(())
        }
        Value::Map(values) => {
            add_size(total, 8, "map length")?;
            for (key, value) in values {
                add_string_size(total, key, "map key")?;
                add_value_size(total, value, depth + 1)?;
            }
            Ok(())
        }
    }
}

fn temporal_payload_size(value: &TemporalValue) -> usize {
    1 + match value {
        TemporalValue::Date { .. } => 4,
        TemporalValue::Time { .. }
        | TemporalValue::Timestamp { .. }
        | TemporalValue::TimestampTz { .. } => 8,
        TemporalValue::TimeTz { .. } => 12,
        TemporalValue::Interval { .. } => 16,
    }
}

pub(crate) fn append_batches(file: &mut File, batches: &[Batch]) -> ExecResult<()> {
    let original_len = file
        .seek(SeekFrom::End(0))
        .map_err(|error| spill_error(format!("failed to seek spill file: {error}")))?;
    let result = {
        let mut writer = BufWriter::new(&mut *file);
        let result = (|| {
            for batch in batches {
                let record_bytes = encoded_batch_size(batch)?;
                let payload_bytes = record_bytes
                    .checked_sub(RECORD_PREFIX_BYTES)
                    .ok_or_else(|| spill_error("spill batch record size underflow"))?;
                write_u64(&mut writer, payload_bytes)?;
                encode_batch(&mut writer, batch)?;
            }
            writer
                .flush()
                .map_err(|error| spill_error(format!("failed to flush spill file: {error}")))
        })();
        drop(writer);
        result
    };
    if let Err(error) = result {
        if let Err(rollback_error) = file.set_len(original_len) {
            return Err(spill_error(format!(
                "{error}; failed to roll back partial spill write: {rollback_error}"
            )));
        }
        file.seek(SeekFrom::End(0)).map_err(|rollback_error| {
            spill_error(format!(
                "{error}; failed to restore spill position: {rollback_error}"
            ))
        })?;
        return Err(error);
    }
    Ok(())
}

fn encode_batch(writer: &mut impl Write, batch: &Batch) -> ExecResult<()> {
    write_u64(writer, batch.schema.physical_width())?;
    write_u64(writer, batch.schema.columns().len())?;
    for (logical, column) in batch.schema.columns().iter().enumerate() {
        write_bytes(writer, column.as_bytes())?;
        write_identity(writer, &batch.schema.identities()[logical])?;
        write_slot(writer, batch.schema.slot(logical))?;
        write_bytes(
            writer,
            encoded_column_type(batch.schema.column_type(logical))?.as_bytes(),
        )?;
    }
    let aliases = batch.schema.lookup_aliases_with_types();
    write_u64(writer, aliases.len())?;
    for (identity, slot, ty) in aliases {
        write_identity(writer, identity)?;
        write_slot(writer, slot)?;
        write_bytes(writer, encoded_column_type(ty)?.as_bytes())?;
    }
    let has_lock_origins = batch.rows.iter().any(|row| !row.lock_origins().is_empty());
    write_u64(
        writer,
        if has_lock_origins {
            ROW_METADATA_LOCK_ORIGINS
        } else {
            0
        },
    )?;
    write_u64(writer, batch.rows.len())?;
    for row in &batch.rows {
        write_u64(writer, batch.schema.physical_width())?;
        for slot in 0..batch.schema.physical_width() {
            let value = row.value(slot).ok_or_else(|| {
                spill_error(format!(
                    "physical row is missing slot {slot} of {}",
                    batch.schema.physical_width()
                ))
            })?;
            encode_value(writer, value, 0)?;
        }
        if has_lock_origins {
            encode_lock_origins(writer, row)?;
        }
    }
    Ok(())
}

fn encode_lock_origins(writer: &mut impl Write, row: &PhysicalRow) -> ExecResult<()> {
    write_u64(writer, row.lock_origins().len())?;
    for origin in row.lock_origins() {
        write_bytes(writer, origin.qualifier.as_bytes())?;
        write_bytes(writer, origin.scan_qualifier.as_bytes())?;
        write_bytes(writer, origin.storage_name.as_bytes())?;
        write_raw(writer, &origin.doc_id.to_le_bytes(), "lock origin doc id")?;
    }
    Ok(())
}

fn add_lock_origins_size(bytes: &mut usize, row: &PhysicalRow) -> ExecResult<()> {
    add_size(bytes, 8, "lock origin count")?;
    for origin in row.lock_origins() {
        add_string_size(bytes, origin.qualifier.as_ref(), "lock origin qualifier")?;
        add_string_size(
            bytes,
            origin.scan_qualifier.as_ref(),
            "lock origin scan qualifier",
        )?;
        add_string_size(bytes, origin.storage_name.as_ref(), "lock origin storage")?;
        add_size(bytes, 8, "lock origin doc id")?;
    }
    Ok(())
}

fn write_identity(writer: &mut impl Write, identity: &ColumnIdentity) -> ExecResult<()> {
    write_bytes(writer, identity.qualifier().unwrap_or("").as_bytes())?;
    write_bytes(writer, identity.column().as_bytes())
}

fn write_slot(writer: &mut impl Write, slot: Option<usize>) -> ExecResult<()> {
    match slot {
        Some(slot) => write_u64(writer, slot),
        None => write_raw(writer, &u64::MAX.to_le_bytes(), "absent schema slot"),
    }
}

fn encode_value(writer: &mut impl Write, value: &Value, depth: usize) -> ExecResult<()> {
    if depth > MAX_VALUE_DEPTH {
        return Err(spill_error("spill value nesting exceeds 128 levels"));
    }
    match value {
        Value::Null => write_tag(writer, 0),
        Value::Bool(value) => {
            write_tag(writer, 1)?;
            writer
                .write_all(&[u8::from(*value)])
                .map_err(|error| spill_error(format!("failed to write boolean: {error}")))
        }
        Value::Int(value) => {
            write_tag(writer, 2)?;
            write_raw(writer, &value.to_le_bytes(), "integer")
        }
        Value::Float(value) => {
            write_tag(writer, 3)?;
            write_raw(writer, &value.to_bits().to_le_bytes(), "float")
        }
        Value::Str(value) => {
            write_tag(writer, 4)?;
            write_bytes(writer, value.as_bytes())
        }
        Value::FixedChar(value) => {
            write_tag(writer, 10)?;
            write_bytes(writer, value.as_bytes())
        }
        Value::Bytes(value) => {
            write_tag(writer, 5)?;
            write_bytes(writer, value)
        }
        Value::Temporal(value) => {
            write_tag(writer, 6)?;
            encode_temporal(writer, value)
        }
        Value::Decimal(value) => {
            write_tag(writer, 7)?;
            write_bytes(writer, value.to_sql_string().as_bytes())
        }
        Value::Json(value) => {
            write_tag(writer, 11)?;
            write_bytes(writer, value.as_bytes())
        }
        Value::JsonB(value) => {
            write_tag(writer, 12)?;
            write_bytes(writer, value.as_bytes())
        }
        Value::Array(array) => {
            write_tag(writer, 15)?;
            write_u64(writer, array.lower_bounds().len())?;
            for lower_bound in array.lower_bounds() {
                write_raw(writer, &lower_bound.to_le_bytes(), "array lower bound")?;
            }
            write_u64(writer, array.elements().len())?;
            for value in array.elements() {
                encode_value(writer, value, depth + 1)?;
            }
            Ok(())
        }
        Value::List(values) => {
            write_tag(writer, 8)?;
            write_u64(writer, values.len())?;
            for value in values {
                encode_value(writer, value, depth + 1)?;
            }
            Ok(())
        }
        Value::Row(values) => {
            write_tag(writer, 13)?;
            write_u64(writer, values.len())?;
            for value in values {
                encode_value(writer, value, depth + 1)?;
            }
            Ok(())
        }
        Value::Record(fields) => {
            write_tag(writer, 14)?;
            write_u64(writer, fields.len())?;
            for (name, value) in fields {
                write_bytes(writer, name.as_bytes())?;
                encode_value(writer, value, depth + 1)?;
            }
            Ok(())
        }
        Value::Map(values) => {
            write_tag(writer, 9)?;
            write_u64(writer, values.len())?;
            for (key, value) in values {
                write_bytes(writer, key.as_bytes())?;
                encode_value(writer, value, depth + 1)?;
            }
            Ok(())
        }
    }
}

fn encode_temporal(writer: &mut impl Write, value: &TemporalValue) -> ExecResult<()> {
    match value {
        TemporalValue::Date { days } => {
            write_tag(writer, 0)?;
            write_raw(writer, &days.to_le_bytes(), "date")
        }
        TemporalValue::Time { micros } => {
            write_tag(writer, 1)?;
            write_raw(writer, &micros.to_le_bytes(), "time")
        }
        TemporalValue::TimeTz {
            micros,
            offset_minutes,
        } => {
            write_tag(writer, 2)?;
            write_raw(writer, &micros.to_le_bytes(), "time with time zone")?;
            write_raw(writer, &offset_minutes.to_le_bytes(), "time zone offset")
        }
        TemporalValue::Timestamp { micros } => {
            write_tag(writer, 3)?;
            write_raw(writer, &micros.to_le_bytes(), "timestamp")
        }
        TemporalValue::TimestampTz { micros } => {
            write_tag(writer, 4)?;
            write_raw(writer, &micros.to_le_bytes(), "timestamp with time zone")
        }
        TemporalValue::Interval {
            months,
            days,
            micros,
        } => {
            write_tag(writer, 5)?;
            write_raw(writer, &months.to_le_bytes(), "interval months")?;
            write_raw(writer, &days.to_le_bytes(), "interval days")?;
            write_raw(writer, &micros.to_le_bytes(), "interval micros")
        }
    }
}

fn write_tag(writer: &mut impl Write, tag: u8) -> ExecResult<()> {
    write_raw(writer, &[tag], "value tag")
}

fn write_u64(writer: &mut impl Write, value: usize) -> ExecResult<()> {
    let value = u64::try_from(value).map_err(|_| spill_error("spill length exceeds u64"))?;
    write_raw(writer, &value.to_le_bytes(), "length")
}

fn write_bytes(writer: &mut impl Write, value: &[u8]) -> ExecResult<()> {
    write_u64(writer, value.len())?;
    write_raw(writer, value, "byte payload")
}

fn write_raw(writer: &mut impl Write, value: &[u8], description: &str) -> ExecResult<()> {
    writer
        .write_all(value)
        .map_err(|error| spill_error(format!("failed to write spill {description}: {error}")))
}
