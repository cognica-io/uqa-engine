//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrowed serde observations admit numeric/byte formatting and preserve the original typed resource error.

use super::Value;
use crate::{
    memory::{Produced, ProductionControl, ProductionString},
    ValueRetentionError,
};
use serde::{
    ser::{SerializeMap, SerializeSeq, SerializeStruct},
    Serialize, Serializer,
};
use std::cell::RefCell;

/// One serialization's value scratch and cancellation scope. Serializer errors cannot carry Core's resource errors, so the caller takes that exact error before interpreting the serializer's diagnostic.
pub struct ValueSerialization<'a> {
    control: ProductionControl<'a>,
    error: RefCell<Option<ValueRetentionError>>,
}

impl<'a> ValueSerialization<'a> {
    pub fn new(control: ProductionControl<'a>) -> Self {
        Self {
            control,
            error: RefCell::new(None),
        }
    }

    pub fn take_error(&self) -> Option<ValueRetentionError> {
        self.error.borrow_mut().take()
    }

    pub fn value<'v>(&'v self, value: &'v Value) -> impl Serialize + 'v {
        ControlledValue {
            value,
            context: self,
        }
    }

    fn result<T, E: serde::ser::Error>(
        &self,
        result: Result<T, ValueRetentionError>,
    ) -> Result<T, E> {
        result.map_err(|error| {
            *self.error.borrow_mut() = Some(error);
            E::custom("controlled value serialization failed")
        })
    }

    fn serialize_array<S: Serializer>(
        &self,
        array: &crate::ArrayValue,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        let context = self;
        let mut output = serializer.serialize_struct(
            "TaggedArray",
            3 + usize::from(array.element_type_oid().is_some()),
        )?;
        output.serialize_field("$uqa_type", "array")?;
        output.serialize_field("lower_bounds", array.lower_bounds())?;
        output.serialize_field(
            "values",
            &Values {
                values: array.elements(),
                context,
            },
        )?;
        if let Some(oid) = array.element_type_oid() {
            output.serialize_field("element_type_oid", &oid)?;
        }
        output.end()
    }

    fn hex<E: serde::ser::Error>(&self, bytes: &[u8]) -> Result<Produced<String>, E> {
        const DIGITS: &[u8; 16] = b"0123456789abcdef";
        let mut output = ProductionString::new(self.control);
        for chunk in bytes.chunks(2048) {
            self.result(output.reserve(chunk.len() * 2))?;
            for byte in chunk {
                self.result(output.push(char::from(DIGITS[usize::from(byte >> 4)])))?;
                self.result(output.push(char::from(DIGITS[usize::from(byte & 15)])))?;
            }
        }
        self.result(output.finish())
    }
}

struct ControlledValue<'a, 'c> {
    value: &'a Value,
    context: &'a ValueSerialization<'c>,
}
struct Values<'a, 'c> {
    values: &'a [Value],
    context: &'a ValueSerialization<'c>,
}

impl Serialize for Values<'_, '_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut output = serializer.serialize_seq(Some(self.values.len()))?;
        for value in self.values {
            output.serialize_element(&self.context.value(value))?;
        }
        output.end()
    }
}

impl Serialize for ControlledValue<'_, '_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let context = self.context;
        context.result::<_, S::Error>(context.control.check())?;
        match self.value {
            Value::Bytes(bytes) => super::TaggedBytes {
                kind: "bytes",
                hex: &context.hex::<S::Error>(bytes)?,
            }
            .serialize(serializer),
            Value::BitString(bits) => super::TaggedBitString {
                kind: "bit_string",
                bit_len: bits.bit_len(),
                hex: &context.hex::<S::Error>(bits.as_bytes())?,
            }
            .serialize(serializer),
            Value::Datum(value) => super::TaggedDatum {
                kind: "datum",
                type_oid: value.type_oid(),
                offset: value.offset(),
                hex: &context.hex::<S::Error>(value.bytes())?,
                record_fields: value.record_fields(),
            }
            .serialize(serializer),
            Value::Decimal(value) => super::TaggedText {
                kind: "decimal",
                value: &context
                    .result::<_, S::Error>(value.to_sql_string_with_control(&context.control))?,
            }
            .serialize(serializer),
            Value::List(values) => Values { values, context }.serialize(serializer),
            Value::Array(array) => context.serialize_array(array, serializer),
            Value::LegacyVector(vector) => {
                let bounds = vector.as_array().lower_bounds();
                let mut output =
                    serializer.serialize_struct("Encoded", 2 + usize::from(bounds != [0]))?;
                output.serialize_field("$uqa_type", vector.kind().type_name())?;
                output.serialize_field(
                    "values",
                    &Values {
                        values: vector.elements(),
                        context,
                    },
                )?;
                if bounds != [0] {
                    output.serialize_field("lower_bounds", bounds)?;
                }
                output.end()
            }
            Value::Row(row) => {
                let mut output = serializer
                    .serialize_struct("TaggedRow", 2 + usize::from(row.field_types().is_some()))?;
                output.serialize_field("$uqa_type", "row")?;
                output.serialize_field(
                    "values",
                    &Values {
                        values: row.values(),
                        context,
                    },
                )?;
                if let Some(types) = row.field_types() {
                    output.serialize_field("field_types", types)?;
                }
                output.end()
            }
            Value::Record(record) => {
                let mut output = serializer.serialize_struct(
                    "TaggedRecord",
                    2 + usize::from(record.type_oid().is_some()),
                )?;
                output.serialize_field("$uqa_type", "record")?;
                output.serialize_field("fields", &Fields { record, context })?;
                if let Some(oid) = record.type_oid() {
                    output.serialize_field("type_oid", &oid)?;
                }
                output.end()
            }
            Value::Map(values) => {
                let mut output = serializer.serialize_map(Some(values.len()))?;
                for (key, value) in values {
                    output.serialize_entry(key, &context.value(value))?;
                }
                output.end()
            }
            Value::Null
            | Value::Void
            | Value::Bool(_)
            | Value::Int(_)
            | Value::Float(_)
            | Value::Str(_)
            | Value::FixedChar(_)
            | Value::Temporal(_)
            | Value::Json(_)
            | Value::JsonB(_)
            | Value::Enum(_) => self.value.serialize(serializer),
        }
    }
}

struct Fields<'a, 'c> {
    record: &'a crate::RecordValue,
    context: &'a ValueSerialization<'c>,
}
impl Serialize for Fields<'_, '_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut output = serializer.serialize_seq(Some(self.record.len()))?;
        for (name, value) in self.record {
            output.serialize_element(&(name, self.context.value(value)))?;
        }
        output.end()
    }
}

#[cfg(test)]
mod tests;
