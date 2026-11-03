//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{check_depth, ValueImageError, ValueImageResult, WINDOW};
use crate::{
    memory::{MemoryBudget, MemoryError, ProductionControl},
    ArrayValue, CancellationToken, LegacyVectorKind, TemporalValue, Value,
};
use std::io::Write;

pub(super) struct Writer<'a> {
    pub output: &'a mut dyn Write,
    pub memory: &'a MemoryBudget,
    pub cancellation: &'a CancellationToken,
    pub written: u64,
}

impl Writer<'_> {
    pub(super) fn bytes(&mut self, bytes: &[u8]) -> ValueImageResult<()> {
        self.cancellation.check()?;
        for chunk in bytes.chunks(WINDOW) {
            let mut remaining = chunk;
            while !remaining.is_empty() {
                self.cancellation.check()?;
                match self.output.write(remaining) {
                    Ok(0) => return Err(std::io::Error::from(std::io::ErrorKind::WriteZero).into()),
                    Ok(count) if count <= remaining.len() => {
                        remaining = &remaining[count..];
                        self.written = self
                            .written
                            .checked_add(count as u64)
                            .ok_or(MemoryError::SizeOverflow)?;
                    }
                    Ok(_) => return Err(ValueImageError::Invalid("writer byte count")),
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(error) => return Err(error.into()),
                }
            }
        }
        self.cancellation.check()?;
        Ok(())
    }

    fn byte(&mut self, value: u8) -> ValueImageResult<()> {
        self.bytes(&[value])
    }

    fn length(&mut self, value: usize) -> ValueImageResult<()> {
        self.bytes(
            &u64::try_from(value)
                .map_err(|_| MemoryError::SizeOverflow)?
                .to_le_bytes(),
        )
    }

    fn blob(&mut self, bytes: &[u8]) -> ValueImageResult<()> {
        self.length(bytes.len())?;
        self.bytes(bytes)
    }

    fn optional_oid(&mut self, oid: Option<u32>) -> ValueImageResult<()> {
        self.byte(u8::from(oid.is_some()))?;
        if let Some(oid) = oid {
            self.bytes(&oid.to_le_bytes())?;
        }
        Ok(())
    }

    fn values(&mut self, values: &[Value], depth: usize) -> ValueImageResult<()> {
        self.length(values.len())?;
        for value in values {
            self.value(value, depth + 1)?;
        }
        Ok(())
    }

    fn array(&mut self, value: &ArrayValue, depth: usize) -> ValueImageResult<()> {
        self.length(value.dimensions().len())?;
        for (dimension, bound) in value.dimensions().iter().zip(value.lower_bounds()) {
            self.length(*dimension)?;
            self.bytes(&bound.to_le_bytes())?;
        }
        self.optional_oid(value.element_type_oid())?;
        self.values(value.elements(), depth)
    }

    pub(super) fn fields_payload<'a>(
        &mut self,
        fields: impl ExactSizeIterator<Item = (&'a str, &'a Value)>,
        depth: usize,
    ) -> ValueImageResult<()> {
        check_depth(depth)?;
        let expected = fields.len();
        self.length(expected)?;
        let mut count = 0;
        let mut previous = None;
        for (name, value) in fields {
            if count == expected || previous.is_some_and(|previous| previous >= name) {
                return Err(ValueImageError::Invalid("ordered fields"));
            }
            self.blob(name.as_bytes())?;
            self.value(value, depth + 1)?;
            count += 1;
            previous = Some(name);
        }
        if count != expected {
            return Err(ValueImageError::Invalid("field count"));
        }
        Ok(())
    }

    pub(super) fn value(&mut self, value: &Value, depth: usize) -> ValueImageResult<()> {
        self.cancellation.check()?;
        check_depth(depth)?;
        self.byte(value_tag(value))?;
        match value {
            Value::Null | Value::Void => Ok(()),
            Value::Bool(value) => self.byte(u8::from(*value)),
            Value::Int(value) => self.bytes(&value.to_le_bytes()),
            Value::Float(value) => self.bytes(&value.to_bits().to_le_bytes()),
            Value::Str(value)
            | Value::FixedChar(value)
            | Value::Json(value)
            | Value::JsonB(value) => self.blob(value.as_bytes()),
            Value::Bytes(value) => self.blob(value),
            Value::Temporal(value) => self.temporal(value),
            Value::Decimal(value) => {
                let control =
                    ProductionControl::new(self.memory, self.cancellation, self.cancellation);
                let text = value.to_sql_string_with_control(&control)?;
                self.blob(text.as_bytes())
            }
            Value::Array(value) => self.array(value, depth),
            Value::LegacyVector(value) => {
                self.byte(match value.kind() {
                    LegacyVectorKind::SmallInteger => 0,
                    LegacyVectorKind::Oid => 1,
                })?;
                self.array(value.as_array(), depth)
            }
            Value::List(values) => self.values(values, depth),
            Value::Row(row) => {
                self.values(row.values(), depth)?;
                self.byte(u8::from(row.field_types().is_some()))?;
                if let Some(types) = row.field_types() {
                    for ty in types {
                        self.bytes(&ty.oid.to_le_bytes())?;
                        self.bytes(&ty.type_modifier.to_le_bytes())?;
                        self.optional_oid(ty.collation_oid)?;
                    }
                }
                Ok(())
            }
            Value::Record(record) => {
                self.optional_oid(record.type_oid())?;
                self.length(record.len())?;
                for (name, value) in record {
                    self.blob(name.as_bytes())?;
                    self.value(value, depth + 1)?;
                }
                Ok(())
            }
            Value::Map(fields) => self.fields_payload(
                fields.iter().map(|(name, value)| (name.as_str(), value)),
                depth,
            ),
            Value::Enum(value) => {
                self.bytes(&value.type_oid().to_le_bytes())?;
                self.optional_oid(value.label_oid())?;
                self.blob(value.key().as_bytes())
            }
            Value::Datum(value) => {
                self.bytes(&value.type_oid().to_le_bytes())?;
                self.bytes(&value.offset().to_le_bytes())?;
                self.blob(value.bytes())?;
                self.byte(u8::from(value.record_fields().is_some()))?;
                if let Some(fields) = value.record_fields() {
                    self.length(fields.len())?;
                    for field in fields {
                        self.blob(field.name.as_bytes())?;
                        self.bytes(&field.type_oid.to_le_bytes())?;
                        self.bytes(&field.type_modifier.to_le_bytes())?;
                        self.optional_oid(field.collation_oid)?;
                        self.bytes(&field.length.to_le_bytes())?;
                        self.byte(field.alignment)?;
                        self.byte(u8::from(field.dropped))?;
                    }
                }
                Ok(())
            }
            Value::BitString(value) => {
                self.bytes(&value.bit_len().to_le_bytes())?;
                self.blob(value.as_bytes())
            }
        }
    }

    fn temporal(&mut self, value: &TemporalValue) -> ValueImageResult<()> {
        match value {
            TemporalValue::Date { days } => {
                self.byte(0)?;
                self.bytes(&days.to_le_bytes())
            }
            TemporalValue::Time { micros } => {
                self.byte(1)?;
                self.bytes(&micros.to_le_bytes())
            }
            TemporalValue::TimeTz {
                micros,
                offset_seconds,
            } => {
                self.byte(2)?;
                self.bytes(&micros.to_le_bytes())?;
                self.bytes(&offset_seconds.to_le_bytes())
            }
            TemporalValue::Timestamp { postgres_micros } => {
                self.byte(3)?;
                self.bytes(&postgres_micros.to_le_bytes())
            }
            TemporalValue::TimestampTz { postgres_micros } => {
                self.byte(4)?;
                self.bytes(&postgres_micros.to_le_bytes())
            }
            TemporalValue::Interval {
                months,
                days,
                micros,
            } => {
                self.byte(5)?;
                self.bytes(&months.to_le_bytes())?;
                self.bytes(&days.to_le_bytes())?;
                self.bytes(&micros.to_le_bytes())
            }
        }
    }
}

fn value_tag(value: &Value) -> u8 {
    match value {
        Value::Null => 0,
        Value::Void => 1,
        Value::Bool(_) => 2,
        Value::Int(_) => 3,
        Value::Float(_) => 4,
        Value::Str(_) => 5,
        Value::FixedChar(_) => 6,
        Value::Bytes(_) => 7,
        Value::Temporal(_) => 8,
        Value::Decimal(_) => 9,
        Value::Json(_) => 10,
        Value::JsonB(_) => 11,
        Value::Array(_) => 12,
        Value::LegacyVector(_) => 13,
        Value::List(_) => 14,
        Value::Row(_) => 15,
        Value::Record(_) => 16,
        Value::Map(_) => 17,
        Value::Enum(_) => 18,
        Value::Datum(_) => 19,
        Value::BitString(_) => 20,
    }
}
