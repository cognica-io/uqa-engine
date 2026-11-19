//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Frame, Header, Pending, ValueImageError, ValueImageReader, ValueImageResult};
use crate::{memory::ProductionControl, LegacyVectorKind, Value};

impl<'a, I: ExactSizeIterator<Item = (&'a str, &'a Value)>> ValueImageReader<'a, '_, I> {
    pub(super) fn advance(&mut self, frame: Frame<'a>) -> ValueImageResult<()> {
        let mut header = Header::new();
        match frame {
            Frame::Value(value, depth) => self.value(value, depth, &mut header)?,
            Frame::Bytes(bytes) => {
                self.pending = Pending::Bytes(bytes);
                return Ok(());
            }
            Frame::Decimal(text) => {
                self.pending = Pending::Decimal(text, 0);
                return Ok(());
            }
            Frame::Values(values, depth) => {
                if let Some((value, rest)) = values.split_first() {
                    self.push(Frame::Values(rest, depth))?;
                    self.push(Frame::Value(value, depth + 1))?;
                }
            }
            Frame::Array(array, index, depth) => {
                if let Some(dimension) = array.dimensions().get(index) {
                    header.length(*dimension)?;
                    header.bytes(&array.lower_bounds()[index].to_le_bytes());
                    self.push(Frame::Array(array, index + 1, depth))?;
                } else {
                    header.optional_oid(array.element_type_oid());
                    header.length(array.elements().len())?;
                    self.push(Frame::Values(array.elements(), depth))?;
                }
            }
            Frame::Row(row, index, depth) => {
                if let Some(value) = row.values().get(index) {
                    self.push(Frame::Row(row, index + 1, depth))?;
                    self.push(Frame::Value(value, depth + 1))?;
                } else {
                    header.byte(u8::from(row.field_types().is_some()));
                    if let Some(types) = row.field_types() {
                        self.push(Frame::RowTypes(types))?;
                    }
                }
            }
            Frame::RowTypes(types) => {
                if let Some((ty, rest)) = types.split_first() {
                    header.bytes(&ty.oid.to_le_bytes());
                    header.bytes(&ty.type_modifier.to_le_bytes());
                    header.optional_oid(ty.collation_oid);
                    self.push(Frame::RowTypes(rest))?;
                }
            }
            Frame::Record(record, index, depth) => {
                if let Some((name, value)) = record.get(index) {
                    self.push(Frame::Record(record, index + 1, depth))?;
                    self.field(name, value, depth, &mut header)?;
                }
            }
            Frame::Map(mut fields, depth) => {
                if let Some((name, value)) = fields.next() {
                    self.push(Frame::Map(fields, depth))?;
                    self.field(name, value, depth, &mut header)?;
                }
            }
            Frame::FieldsStart => {
                self.expected = self.fields.as_ref().expect("borrowed fields").len();
                header.byte(17);
                header.length(self.expected)?;
                self.push(Frame::Fields)?;
            }
            Frame::Fields => self.next_field(&mut header)?,
            Frame::DatumFields(fields) => {
                header.byte(u8::from(fields.is_some()));
                if let Some(fields) = fields {
                    header.length(fields.len())?;
                    self.push(Frame::DatumFieldsNext(fields))?;
                }
            }
            Frame::DatumFieldsNext(fields) => {
                if let Some((field, rest)) = fields.split_first() {
                    self.push(Frame::DatumFieldsNext(rest))?;
                    self.push(Frame::DatumFieldMetadata(field))?;
                    self.blob(field.name.as_bytes(), &mut header)?;
                }
            }
            Frame::DatumFieldMetadata(field) => {
                header.bytes(&field.type_oid.to_le_bytes());
                header.bytes(&field.type_modifier.to_le_bytes());
                header.optional_oid(field.collation_oid);
                header.bytes(&field.length.to_le_bytes());
                header.byte(field.alignment);
                header.byte(u8::from(field.dropped));
            }
        }
        self.pending = Pending::Header(header, 0);
        Ok(())
    }

    fn next_field(&mut self, header: &mut Header) -> ValueImageResult<()> {
        if let Some((name, value)) = self.fields.as_mut().expect("borrowed fields").next() {
            if self.count == self.expected || self.previous.is_some_and(|previous| previous >= name)
            {
                return Err(ValueImageError::Invalid("ordered fields"));
            }
            self.count += 1;
            self.previous = Some(name);
            self.push(Frame::Fields)?;
            self.field(name, value, 0, header)?;
        } else if self.count != self.expected {
            return Err(ValueImageError::Invalid("field count"));
        }
        Ok(())
    }

    fn blob(&mut self, bytes: &'a [u8], header: &mut Header) -> ValueImageResult<()> {
        header.length(bytes.len())?;
        self.push(Frame::Bytes(bytes))
    }

    fn field(
        &mut self,
        name: &'a str,
        value: &'a Value,
        depth: usize,
        header: &mut Header,
    ) -> ValueImageResult<()> {
        self.push(Frame::Value(value, depth + 1))?;
        self.blob(name.as_bytes(), header)
    }

    fn value(
        &mut self,
        value: &'a Value,
        depth: usize,
        header: &mut Header,
    ) -> ValueImageResult<()> {
        super::super::check_depth(depth)?;
        header.byte(value_tag(value));
        match value {
            Value::Null | Value::Void => {}
            Value::Bool(value) => header.byte(u8::from(*value)),
            Value::Int(value) => header.bytes(&value.to_le_bytes()),
            Value::Float(value) => header.bytes(&value.to_bits().to_le_bytes()),
            Value::Str(value)
            | Value::FixedChar(value)
            | Value::Json(value)
            | Value::JsonB(value) => self.blob(value.as_bytes(), header)?,
            Value::Bytes(value) => self.blob(value, header)?,
            Value::Temporal(value) => header.temporal(value),
            Value::Decimal(value) => {
                let control =
                    ProductionControl::new(self.memory, self.cancellation, self.cancellation);
                let text = value.to_sql_string_with_control(&control)?;
                header.length(text.len())?;
                self.push(Frame::Decimal(text))?;
            }
            Value::Array(value) => {
                header.length(value.dimensions().len())?;
                self.push(Frame::Array(value, 0, depth))?;
            }
            Value::LegacyVector(value) => {
                header.byte(match value.kind() {
                    LegacyVectorKind::SmallInteger => 0,
                    LegacyVectorKind::Oid => 1,
                });
                header.length(value.as_array().dimensions().len())?;
                self.push(Frame::Array(value.as_array(), 0, depth))?;
            }
            Value::List(values) => {
                header.length(values.len())?;
                self.push(Frame::Values(values, depth))?;
            }
            Value::Row(row) => {
                header.length(row.values().len())?;
                self.push(Frame::Row(row, 0, depth))?;
            }
            Value::Record(record) => {
                header.optional_oid(record.type_oid());
                header.length(record.len())?;
                self.push(Frame::Record(record, 0, depth))?;
            }
            Value::Map(fields) => {
                header.length(fields.len())?;
                self.push(Frame::Map(fields.iter(), depth))?;
            }
            Value::Enum(value) => {
                header.bytes(&value.type_oid().to_le_bytes());
                header.optional_oid(value.label_oid());
                self.blob(value.key().as_bytes(), header)?;
            }
            Value::Datum(value) => {
                header.bytes(&value.type_oid().to_le_bytes());
                header.bytes(&value.offset().to_le_bytes());
                self.push(Frame::DatumFields(value.record_fields()))?;
                self.blob(value.bytes(), header)?;
            }
            Value::BitString(value) => {
                header.bytes(&value.bit_len().to_le_bytes());
                self.blob(value.as_bytes(), header)?;
            }
        }
        Ok(())
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
