//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The same typed encoder writes ordinary I/O and admitted constant records.

use super::MAX_VALUE_DEPTH;
use crate::physical::ExecResult;
use crate::spill::format::spill_error;
use std::io::Write;
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionVec},
    TemporalValue, Value,
};

trait ValueWriter {
    fn control(&self) -> ProductionControl<'_>;
    fn write(&mut self, bytes: &[u8], description: &str) -> ExecResult<()>;
}

struct OrdinaryWriter<'a, W>(&'a mut W);
impl<W: Write> ValueWriter for OrdinaryWriter<'_, W> {
    fn control(&self) -> ProductionControl<'_> {
        ProductionControl::uncontrolled()
    }
    fn write(&mut self, bytes: &[u8], description: &str) -> ExecResult<()> {
        super::write_raw(self.0, bytes, description)
    }
}

struct ConstantWriter<'a> {
    bytes: ProductionVec<'a, u8>,
    control: ProductionControl<'a>,
}
impl ValueWriter for ConstantWriter<'_> {
    fn control(&self) -> ProductionControl<'_> {
        self.control
    }
    fn write(&mut self, bytes: &[u8], _: &str) -> ExecResult<()> {
        self.bytes.extend_from_slice(bytes)?;
        Ok(())
    }
}

pub(super) fn encode_value(writer: &mut impl Write, value: &Value, depth: usize) -> ExecResult<()> {
    write_value(&mut OrdinaryWriter(writer), value, depth)
}

pub(crate) fn encode_constant_record_with_control(
    value: &Value,
    control: &ProductionControl<'_>,
) -> ExecResult<Produced<Vec<u8>>> {
    control.check()?;
    let mut writer = ConstantWriter {
        bytes: ProductionVec::new(*control),
        control: *control,
    };
    write_u64(&mut writer, 1)?;
    write_value(&mut writer, value, 0)?;
    Ok(writer.bytes.finish()?)
}

fn encode_record(
    writer: &mut impl ValueWriter,
    fields: &uqa_core::RecordValue,
    depth: usize,
) -> ExecResult<()> {
    if let Some(oid) = fields.type_oid() {
        write_tag(writer, 23)?;
        write_raw(writer, &oid.to_le_bytes(), "record type OID")?;
    } else {
        write_tag(writer, 14)?;
    }
    write_u64(writer, fields.len())?;
    for (name, value) in fields {
        write_bytes(writer, name.as_bytes())?;
        write_value(writer, value, depth + 1)?;
    }
    Ok(())
}

fn write_value(writer: &mut impl ValueWriter, value: &Value, depth: usize) -> ExecResult<()> {
    writer.control().check()?;
    if depth > MAX_VALUE_DEPTH {
        return Err(spill_error("spill value nesting exceeds 128 levels"));
    }
    match value {
        Value::Datum(value) => {
            write_tag(writer, 20)?;
            write_raw(writer, &value.type_oid().to_le_bytes(), "datum type OID")?;
            write_raw(writer, &value.offset().to_le_bytes(), "datum offset")?;
            write_bytes(writer, value.bytes())
        }
        Value::Null => write_tag(writer, 0),
        Value::Void => write_tag(writer, 16),
        Value::Enum(value) => {
            write_tag(writer, if value.label_oid().is_some() { 22 } else { 18 })?;
            write_raw(writer, &value.type_oid().to_le_bytes(), "enum type OID")?;
            if let Some(oid) = value.label_oid() {
                write_raw(writer, &oid.to_le_bytes(), "enum label OID")?;
            }
            write_bytes(writer, value.key().as_bytes())
        }
        Value::LegacyVector(vector) => encode_legacy_vector(writer, vector, depth),
        Value::Bool(value) => {
            write_tag(writer, 1)?;
            write_raw(writer, &[u8::from(*value)], "boolean")
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
        Value::BitString(value) => {
            write_tag(writer, 24)?;
            write_raw(writer, &value.bit_len().to_le_bytes(), "bit length")?;
            write_bytes(writer, value.as_bytes())
        }
        Value::Temporal(value) => {
            write_tag(writer, 6)?;
            encode_temporal(writer, value)
        }
        Value::Decimal(value) => {
            write_tag(writer, 7)?;
            let text = value.to_sql_string_with_control(&writer.control())?;
            write_bytes(writer, text.as_bytes())
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
            if let Some(oid) = array.element_type_oid() {
                write_tag(writer, 21)?;
                write_raw(writer, &oid.to_le_bytes(), "array element type OID")?;
            } else {
                write_tag(writer, 15)?;
            }
            write_u64(writer, array.lower_bounds().len())?;
            for lower_bound in array.lower_bounds() {
                write_raw(writer, &lower_bound.to_le_bytes(), "array lower bound")?;
            }
            encode_values(writer, array.elements(), depth)
        }
        Value::List(values) => {
            write_tag(writer, 8)?;
            encode_values(writer, values, depth)
        }
        Value::Row(values) => encode_row(writer, values, depth),
        Value::Record(fields) => encode_record(writer, fields, depth),
        Value::Map(values) => {
            write_tag(writer, 9)?;
            write_u64(writer, values.len())?;
            for (key, value) in values {
                write_bytes(writer, key.as_bytes())?;
                write_value(writer, value, depth + 1)?;
            }
            Ok(())
        }
    }
}

fn encode_row(
    writer: &mut impl ValueWriter,
    values: &uqa_core::RowValue,
    depth: usize,
) -> ExecResult<()> {
    if let Some(fields) = values.field_types() {
        write_tag(writer, 25)?;
        write_u64(writer, fields.len())?;
        for field in fields {
            write_raw(writer, &field.oid.to_le_bytes(), "row field type OID")?;
            write_raw(
                writer,
                &field.type_modifier.to_le_bytes(),
                "row field type modifier",
            )?;
            write_tag(writer, u8::from(field.collation_oid.is_some()))?;
            if let Some(oid) = field.collation_oid {
                write_raw(writer, &oid.to_le_bytes(), "row field collation OID")?;
            }
        }
    } else {
        write_tag(writer, 13)?;
    }
    encode_values(writer, values, depth)
}

/// Write the count and nested values of a list-shaped value after its tag.
fn encode_values(writer: &mut impl ValueWriter, values: &[Value], depth: usize) -> ExecResult<()> {
    write_u64(writer, values.len())?;
    for value in values {
        write_value(writer, value, depth + 1)?;
    }
    Ok(())
}

fn encode_legacy_vector(
    writer: &mut impl ValueWriter,
    vector: &uqa_core::LegacyVectorValue,
    depth: usize,
) -> ExecResult<()> {
    write_tag(writer, 17)?;
    write_tag(
        writer,
        match vector.kind() {
            uqa_core::LegacyVectorKind::SmallInteger => 0,
            uqa_core::LegacyVectorKind::Oid => 1,
        },
    )?;
    write_u64(writer, vector.as_array().lower_bounds().len())?;
    for bound in vector.as_array().lower_bounds() {
        write_raw(writer, &bound.to_le_bytes(), "legacy vector lower bound")?;
    }
    write_u64(writer, vector.elements().len())?;
    for value in vector.elements() {
        write_value(writer, value, depth + 1)?;
    }
    Ok(())
}

fn encode_temporal(writer: &mut impl ValueWriter, value: &TemporalValue) -> ExecResult<()> {
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
            offset_seconds,
        } => {
            write_tag(writer, 2)?;
            write_raw(writer, &micros.to_le_bytes(), "time with time zone")?;
            write_raw(writer, &offset_seconds.to_le_bytes(), "time zone offset")
        }
        TemporalValue::Timestamp {
            postgres_micros: micros,
        } => {
            write_tag(writer, 3)?;
            write_raw(writer, &micros.to_le_bytes(), "timestamp")
        }
        TemporalValue::TimestampTz {
            postgres_micros: micros,
        } => {
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

fn write_tag(writer: &mut impl ValueWriter, value: u8) -> ExecResult<()> {
    write_raw(writer, &[value], "value tag")
}
fn write_u64(writer: &mut impl ValueWriter, value: usize) -> ExecResult<()> {
    let value = u64::try_from(value).map_err(|_| spill_error("spill length exceeds u64"))?;
    write_raw(writer, &value.to_le_bytes(), "length")
}
fn write_bytes(writer: &mut impl ValueWriter, value: &[u8]) -> ExecResult<()> {
    write_u64(writer, value.len())?;
    write_raw(writer, value, "byte payload")
}
fn write_raw(writer: &mut impl ValueWriter, value: &[u8], description: &str) -> ExecResult<()> {
    writer.control().check()?;
    writer.write(value, description)
}

#[cfg(test)]
mod tests;
