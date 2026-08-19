//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bounded binary spill decoding.

use std::collections::BTreeMap;

use uqa_core::{ArrayValue, DecimalValue, TemporalValue, Value};
use uqa_sql::ast::ColumnType;

use crate::batch::{Batch, ColumnIdentity, PhysicalRow, RowSchema};
use crate::physical::ExecResult;
use crate::spill::format::spill_error;

use super::MAX_VALUE_DEPTH;

const ROW_METADATA_LOCK_ORIGINS: u64 = 1;

pub(crate) fn decode_batch(record: &[u8]) -> ExecResult<Batch> {
    let mut reader = BinaryReader::new(record);
    let physical_width = reader.read_count("schema physical width", 1)?;
    let column_count = reader.read_count("schema column count", 16)?;
    let mut columns = Vec::new();
    let mut identities = Vec::new();
    let mut types = Vec::new();
    let mut slots = Vec::new();
    columns
        .try_reserve_exact(column_count)
        .map_err(|error| spill_error(format!("cannot allocate spill schema: {error}")))?;
    identities
        .try_reserve_exact(column_count)
        .map_err(|error| {
            spill_error(format!("cannot allocate spill schema identities: {error}"))
        })?;
    slots
        .try_reserve_exact(column_count)
        .map_err(|error| spill_error(format!("cannot allocate spill schema slots: {error}")))?;
    types
        .try_reserve_exact(column_count)
        .map_err(|error| spill_error(format!("cannot allocate spill schema types: {error}")))?;
    for _ in 0..column_count {
        columns.push(reader.read_string("schema column")?);
        identities.push(reader.read_identity("schema identity")?);
        slots.push(reader.read_slot("schema logical slot", physical_width)?);
        types.push(reader.read_column_type("schema column type")?);
    }
    let alias_count = reader.read_count("schema alias count", 16)?;
    let mut aliases = Vec::new();
    aliases
        .try_reserve_exact(alias_count)
        .map_err(|error| spill_error(format!("cannot allocate spill schema aliases: {error}")))?;
    for _ in 0..alias_count {
        aliases.push((
            reader.read_identity("schema alias")?,
            reader.read_slot("schema alias slot", physical_width)?,
            reader.read_column_type("schema alias type")?,
        ));
    }
    let schema =
        RowSchema::from_physical_layout(columns, identities, types, slots, physical_width, aliases)
            .map_err(|error| spill_error(format!("invalid spill schema: {error}")))?;
    let row_metadata = reader.read_u64("row metadata flags")?;
    if row_metadata & !ROW_METADATA_LOCK_ORIGINS != 0 {
        return Err(spill_error(format!(
            "unsupported row metadata flags {row_metadata:#x}"
        )));
    }
    let has_lock_origins = row_metadata & ROW_METADATA_LOCK_ORIGINS != 0;
    let row_count = reader.read_count("batch row count", 8)?;
    let mut rows = Vec::new();
    rows.try_reserve_exact(row_count)
        .map_err(|error| spill_error(format!("cannot allocate spill rows: {error}")))?;
    for _ in 0..row_count {
        rows.push(reader.read_row(physical_width, has_lock_origins)?);
    }
    reader.finish("spill batch")?;
    Ok(Batch::from_physical_rows(schema, rows))
}

pub(crate) fn decode_physical_row_record(
    record: &[u8],
    physical_width: usize,
) -> ExecResult<PhysicalRow> {
    let mut reader = BinaryReader::new(record);
    let mut row = reader.read_row(physical_width, false)?;
    if reader.remaining() != 0 {
        row = row.with_lock_origins(reader.read_lock_origins()?);
    }
    reader.finish("indexed spill row")?;
    Ok(row)
}

struct BinaryReader<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> BinaryReader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.position)
    }

    fn read_exact<const N: usize>(&mut self, description: &str) -> ExecResult<[u8; N]> {
        let end = self
            .position
            .checked_add(N)
            .ok_or_else(|| spill_error(format!("{description} offset overflow")))?;
        let slice = self
            .bytes
            .get(self.position..end)
            .ok_or_else(|| spill_error(format!("truncated {description}")))?;
        self.position = end;
        slice
            .try_into()
            .map_err(|_| spill_error(format!("invalid {description} width")))
    }

    fn read_u8(&mut self, description: &str) -> ExecResult<u8> {
        Ok(self.read_exact::<1>(description)?[0])
    }

    fn read_u64(&mut self, description: &str) -> ExecResult<u64> {
        self.read_exact::<8>(description).map(u64::from_le_bytes)
    }

    fn read_i64(&mut self, description: &str) -> ExecResult<i64> {
        self.read_exact::<8>(description).map(i64::from_le_bytes)
    }

    fn read_i32(&mut self, description: &str) -> ExecResult<i32> {
        self.read_exact::<4>(description).map(i32::from_le_bytes)
    }

    fn read_count(&mut self, description: &str, minimum_item_bytes: usize) -> ExecResult<usize> {
        let count = usize::try_from(self.read_u64(description)?)
            .map_err(|_| spill_error(format!("{description} exceeds address space")))?;
        if count > self.remaining().saturating_div(minimum_item_bytes.max(1)) {
            return Err(spill_error(format!("invalid {description} {count}")));
        }
        Ok(count)
    }

    fn read_bytes(&mut self, description: &str) -> ExecResult<&'a [u8]> {
        let length = usize::try_from(self.read_u64(description)?)
            .map_err(|_| spill_error(format!("{description} length exceeds address space")))?;
        let end = self
            .position
            .checked_add(length)
            .ok_or_else(|| spill_error(format!("{description} offset overflow")))?;
        let value = self
            .bytes
            .get(self.position..end)
            .ok_or_else(|| spill_error(format!("truncated {description}")))?;
        self.position = end;
        Ok(value)
    }

    fn read_string(&mut self, description: &str) -> ExecResult<String> {
        let bytes = self.read_bytes(description)?;
        String::from_utf8(bytes.to_vec())
            .map_err(|error| spill_error(format!("invalid UTF-8 in {description}: {error}")))
    }

    fn read_identity(&mut self, description: &str) -> ExecResult<ColumnIdentity> {
        let qualifier = self.read_string(description)?;
        let column = self.read_string(description)?;
        if column.is_empty() {
            return Err(spill_error(format!(
                "{description} has an empty column name"
            )));
        }
        Ok(if qualifier.is_empty() {
            ColumnIdentity::unqualified(column)
        } else {
            ColumnIdentity::qualified(qualifier, column)
        })
    }

    fn read_column_type(&mut self, description: &str) -> ExecResult<Option<ColumnType>> {
        let encoded = self.read_string(description)?;
        if encoded.is_empty() {
            return Ok(None);
        }
        serde_json::from_str(&encoded)
            .map(Some)
            .map_err(|error| spill_error(format!("invalid {description}: {error}")))
    }

    fn read_slot(&mut self, description: &str, physical_width: usize) -> ExecResult<Option<usize>> {
        let encoded = self.read_u64(description)?;
        if encoded == u64::MAX {
            return Ok(None);
        }
        let slot = usize::try_from(encoded)
            .map_err(|_| spill_error(format!("{description} exceeds address space")))?;
        if slot >= physical_width {
            return Err(spill_error(format!(
                "{description} {slot} is outside physical width {physical_width}"
            )));
        }
        Ok(Some(slot))
    }

    fn read_row(
        &mut self,
        expected_values: usize,
        has_lock_origins: bool,
    ) -> ExecResult<PhysicalRow> {
        let value_count = self.read_count("row value count", 1)?;
        if value_count != expected_values {
            return Err(spill_error(format!(
                "spill row has {value_count} values for {expected_values} columns"
            )));
        }
        let mut values = Vec::new();
        values
            .try_reserve_exact(value_count)
            .map_err(|error| spill_error(format!("cannot allocate spill row: {error}")))?;
        for _ in 0..value_count {
            values.push(self.read_value(0)?);
        }
        let row = PhysicalRow::from_values(values);
        if !has_lock_origins {
            return Ok(row);
        }
        Ok(row.with_lock_origins(self.read_lock_origins()?))
    }

    fn read_lock_origins(&mut self) -> ExecResult<Vec<crate::RowLockOrigin>> {
        let origin_count = self.read_count("lock origin count", 32)?;
        let mut origins = Vec::new();
        origins
            .try_reserve_exact(origin_count)
            .map_err(|error| spill_error(format!("cannot allocate lock origins: {error}")))?;
        for _ in 0..origin_count {
            let qualifier = self.read_string("lock origin qualifier")?;
            let scan_qualifier = self.read_string("lock origin scan qualifier")?;
            let storage_name = self.read_string("lock origin storage")?;
            let doc_id = self.read_u64("lock origin doc id")?;
            origins.push(crate::RowLockOrigin {
                qualifier: std::sync::Arc::from(qualifier),
                scan_qualifier: std::sync::Arc::from(scan_qualifier),
                storage_name: std::sync::Arc::from(storage_name),
                doc_id,
            });
        }
        Ok(origins)
    }

    fn read_value(&mut self, depth: usize) -> ExecResult<Value> {
        if depth > MAX_VALUE_DEPTH {
            return Err(spill_error("spill value nesting exceeds 128 levels"));
        }
        match self.read_u8("value tag")? {
            0 => Ok(Value::Null),
            1 => match self.read_u8("boolean value")? {
                0 => Ok(Value::Bool(false)),
                1 => Ok(Value::Bool(true)),
                value => Err(spill_error(format!("invalid boolean value {value}"))),
            },
            2 => self.read_i64("integer value").map(Value::Int),
            3 => self
                .read_u64("float value")
                .map(|bits| Value::Float(f64::from_bits(bits))),
            4 => self.read_string("string value").map(Value::Str),
            5 => self
                .read_bytes("byte value")
                .map(|value| Value::Bytes(value.to_vec())),
            6 => self.read_temporal().map(Value::Temporal),
            7 => {
                let value = self.read_string("decimal value")?;
                DecimalValue::parse(&value)
                    .map(Value::Decimal)
                    .ok_or_else(|| spill_error(format!("invalid decimal in spill file: {value}")))
            }
            8 => {
                let count = self.read_count("list length", 1)?;
                let mut values = Vec::new();
                values
                    .try_reserve_exact(count)
                    .map_err(|error| spill_error(format!("cannot allocate spill list: {error}")))?;
                for _ in 0..count {
                    values.push(self.read_value(depth + 1)?);
                }
                Ok(Value::List(values))
            }
            9 => {
                let count = self.read_count("map length", 9)?;
                let mut values = BTreeMap::new();
                for _ in 0..count {
                    let key = self.read_string("map key")?;
                    let value = self.read_value(depth + 1)?;
                    if values.insert(key.clone(), value).is_some() {
                        return Err(spill_error(format!("duplicate key `{key}` in spill map")));
                    }
                }
                Ok(Value::Map(values))
            }
            10 => self
                .read_string("fixed character value")
                .map(Value::FixedChar),
            11 => self.read_string("JSON value").map(Value::Json),
            12 => self.read_string("JSONB value").map(Value::JsonB),
            13 => {
                let count = self.read_count("row length", 1)?;
                let mut values = Vec::new();
                values
                    .try_reserve_exact(count)
                    .map_err(|error| spill_error(format!("cannot allocate spill row: {error}")))?;
                for _ in 0..count {
                    values.push(self.read_value(depth + 1)?);
                }
                Ok(Value::Row(values))
            }
            14 => {
                let count = self.read_count("record length", 9)?;
                let mut fields = Vec::new();
                fields.try_reserve_exact(count).map_err(|error| {
                    spill_error(format!("cannot allocate spill record: {error}"))
                })?;
                for _ in 0..count {
                    let name = self.read_string("record field name")?;
                    let value = self.read_value(depth + 1)?;
                    fields.push((name, value));
                }
                Ok(Value::Record(fields))
            }
            15 => {
                let bound_count = self.read_count("array lower-bound count", 4)?;
                let mut lower_bounds = Vec::new();
                lower_bounds
                    .try_reserve_exact(bound_count)
                    .map_err(|error| {
                        spill_error(format!("cannot allocate spill array bounds: {error}"))
                    })?;
                for _ in 0..bound_count {
                    lower_bounds.push(self.read_i32("array lower bound")?);
                }
                let element_count = self.read_count("array element count", 1)?;
                let mut elements = Vec::new();
                elements.try_reserve_exact(element_count).map_err(|error| {
                    spill_error(format!("cannot allocate spill array: {error}"))
                })?;
                for _ in 0..element_count {
                    elements.push(self.read_value(depth + 1)?);
                }
                ArrayValue::with_lower_bounds(elements, lower_bounds)
                    .map(Value::Array)
                    .ok_or_else(|| spill_error("invalid array dimensions in spill file"))
            }
            tag => Err(spill_error(format!("invalid spill value tag {tag}"))),
        }
    }

    fn read_temporal(&mut self) -> ExecResult<TemporalValue> {
        match self.read_u8("temporal tag")? {
            0 => Ok(TemporalValue::Date {
                days: self.read_i32("date")?,
            }),
            1 => Ok(TemporalValue::Time {
                micros: self.read_i64("time")?,
            }),
            2 => Ok(TemporalValue::TimeTz {
                micros: self.read_i64("time with time zone")?,
                offset_minutes: self.read_i32("time zone offset")?,
            }),
            3 => Ok(TemporalValue::Timestamp {
                micros: self.read_i64("timestamp")?,
            }),
            4 => Ok(TemporalValue::TimestampTz {
                micros: self.read_i64("timestamp with time zone")?,
            }),
            5 => Ok(TemporalValue::Interval {
                months: self.read_i32("interval months")?,
                days: self.read_i32("interval days")?,
                micros: self.read_i64("interval micros")?,
            }),
            tag => Err(spill_error(format!("invalid temporal tag {tag}"))),
        }
    }

    fn finish(self, description: &str) -> ExecResult<()> {
        if self.position == self.bytes.len() {
            Ok(())
        } else {
            Err(spill_error(format!(
                "{description} has {} trailing bytes",
                self.bytes.len() - self.position
            )))
        }
    }
}
