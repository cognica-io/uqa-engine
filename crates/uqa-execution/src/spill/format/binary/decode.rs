//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bounded binary spill decoding.

use std::collections::BTreeMap;

use uqa_core::{DecimalValue, TemporalValue, Value};
use uqa_sql::ResultRow;

use crate::batch::{Batch, RowSchema};
use crate::physical::ExecResult;
use crate::spill::format::spill_error;

use super::MAX_VALUE_DEPTH;

pub(crate) fn decode_batch(record: &[u8]) -> ExecResult<Batch> {
    let mut reader = BinaryReader::new(record);
    let column_count = reader.read_count("schema column count", 8)?;
    let mut columns = Vec::new();
    columns
        .try_reserve_exact(column_count)
        .map_err(|error| spill_error(format!("cannot allocate spill schema: {error}")))?;
    for _ in 0..column_count {
        columns.push(reader.read_string("schema column")?);
    }
    let row_count = reader.read_count("batch row count", 8)?;
    let mut rows = Vec::new();
    rows.try_reserve_exact(row_count)
        .map_err(|error| spill_error(format!("cannot allocate spill rows: {error}")))?;
    for _ in 0..row_count {
        rows.push(reader.read_row()?);
    }
    reader.finish("spill batch")?;
    Ok(Batch::new(RowSchema::new(columns), rows))
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

    fn read_row(&mut self) -> ExecResult<ResultRow> {
        let field_count = self.read_count("row field count", 9)?;
        let mut row = ResultRow::new();
        for _ in 0..field_count {
            let name = self.read_string("row field name")?;
            let value = self.read_value(0)?;
            if row.insert(name.clone(), value).is_some() {
                return Err(spill_error(format!(
                    "duplicate field `{name}` in spill row"
                )));
            }
        }
        Ok(row)
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
