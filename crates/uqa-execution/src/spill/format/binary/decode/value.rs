//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordinary and admitted spill reads use the same tagged value constructors.

use super::{spill_error, BinaryReader, ExecResult, MAX_VALUE_DEPTH};
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionVec},
    ArrayValue, Value,
};

impl BinaryReader<'_> {
    #[expect(
        clippy::too_many_lines,
        reason = "exhaustive binary value tags preserve their validation order"
    )]
    pub(super) fn read_value_with_control(
        &mut self,
        depth: usize,
        control: &ProductionControl<'_>,
    ) -> ExecResult<Produced<Value>> {
        control.check()?;
        if depth > MAX_VALUE_DEPTH {
            return Err(spill_error("spill value nesting exceeds 128 levels"));
        }
        let value = match self.read_u8("value tag")? {
            0 => Value::Null,
            1 => match self.read_u8("boolean value")? {
                0 => Value::Bool(false),
                1 => Value::Bool(true),
                value => return Err(spill_error(format!("invalid boolean value {value}"))),
            },
            2 => Value::Int(self.read_i64("integer value")?),
            3 => Value::Float(f64::from_bits(self.read_u64("float value")?)),
            tag @ (4 | 10 | 11 | 12) => {
                let description = match tag {
                    4 => "string value",
                    10 => "fixed character value",
                    11 => "JSON value",
                    _ => "JSONB value",
                };
                let text = self.read_text_with_control(description, control)?;
                let (text, memory) = text.into_parts();
                let value = match tag {
                    4 => Value::Str(text),
                    10 => Value::FixedChar(text),
                    11 => Value::Json(text),
                    _ => Value::JsonB(text),
                };
                return Ok(control.finish(value, memory)?);
            }
            5 => {
                let (bytes, memory) = self
                    .read_bytes_with_control("byte value", control)?
                    .into_parts();
                return Ok(control.finish(Value::Bytes(bytes), memory)?);
            }
            6 => Value::Temporal(self.read_temporal()?),
            7 => {
                let bytes = self.read_bytes("decimal value")?;
                let text = std::str::from_utf8(bytes).map_err(|error| {
                    spill_error(format!("invalid UTF-8 in decimal value: {error}"))
                })?;
                let value = uqa_core::DecimalValue::parse_with_control(text, control)?
                    .ok_or_else(|| spill_error(format!("invalid decimal in spill file: {text}")))?;
                let (value, memory) = value.into_parts();
                return Ok(control.finish(Value::Decimal(value), memory)?);
            }
            8 => {
                let (values, memory) = self
                    .read_values("list length", depth, control)?
                    .into_parts();
                return Ok(control.finish(Value::List(values), memory)?);
            }
            9 => return self.read_map(depth, control),
            tag @ (13 | 19 | 25) => return self.read_row_value(tag, depth, control),
            tag @ (14 | 23) => return self.read_record_value(tag, depth, control),
            tag @ (15 | 21) => {
                let oid = if tag == 21 {
                    Some(self.read_oid("array element type OID")?)
                } else {
                    None
                };
                let bounds = self.read_bounds(
                    "array lower-bound count",
                    "array lower bound",
                    false,
                    control,
                )?;
                let elements = self.read_values("array element count", depth, control)?;
                let array = ArrayValue::with_lower_bounds_with_control(elements, bounds, control)?
                    .ok_or_else(|| spill_error("invalid array dimensions in spill file"))?;
                let (array, memory) = array.into_parts();
                return Ok(control.finish(Value::Array(array.with_element_type_oid(oid)), memory)?);
            }
            20 => {
                let oid = self.read_oid("datum type OID")?;
                let offset = self.read_oid("datum offset")?;
                let bytes = self.read_bytes_with_control("datum bytes", control)?;
                let (datum, memory) =
                    uqa_core::DatumValue::with_control(oid, offset, bytes, control)?.into_parts();
                return Ok(control.finish(Value::Datum(datum), memory)?);
            }
            16 => Value::Void,
            24 => {
                let length = self.read_oid("bit length")?;
                let bytes = self.read_bytes("packed bit data")?;
                if bytes.len() != length.div_ceil(8) as usize {
                    return Err(spill_error("invalid packed bit length in spill file"));
                }
                let mut output = ProductionVec::new(*control);
                output.reserve(bytes.len())?;
                output.extend_from_slice(bytes)?;
                let bits = uqa_core::BitStringValue::try_from_packed_with_control(
                    output.finish()?,
                    length,
                    control,
                )?
                .ok_or_else(|| spill_error("invalid packed bit padding in spill file"))?;
                let (bits, memory) = bits.into_parts();
                return Ok(control.finish(Value::BitString(bits), memory)?);
            }
            tag @ (18 | 22) => {
                let oid = self.read_oid("enum type OID")?;
                let label = if tag == 22 {
                    Some(self.read_oid("enum label OID")?)
                } else {
                    None
                };
                let bytes = self.read_bytes("enum label key")?;
                let memory = control.reserve(bytes.len())?;
                // This exact-length copy has no spare capacity to shrink when the key becomes a boxed slice.
                let key = uqa_core::EnumLabelKey::from_bytes(bytes.to_vec()).map_err(|error| {
                    spill_error(format!("invalid enum label key in spill file: {error}"))
                })?;
                let header = control.reserve(uqa_core::EnumValue::retained_header_bytes())?;
                let value = Value::Enum(uqa_core::EnumValue::new(oid, key).with_label_oid(label));
                return Ok(control.finish(value, control.combine(memory, header))?);
            }
            17 => {
                let kind = match self.read_u8("legacy vector kind")? {
                    0 => uqa_core::LegacyVectorKind::SmallInteger,
                    1 => uqa_core::LegacyVectorKind::Oid,
                    _ => return Err(spill_error("invalid legacy vector kind")),
                };
                let bounds = self.read_bounds(
                    "legacy vector dimensions",
                    "legacy vector lower bound",
                    true,
                    control,
                )?;
                let elements = self.read_values("legacy vector length", depth, control)?;
                let array = ArrayValue::with_lower_bounds_with_control(elements, bounds, control)?
                    .ok_or_else(|| spill_error("invalid legacy vector elements"))?;
                let vector =
                    uqa_core::LegacyVectorValue::try_from_array_with_control(kind, array, control)?
                        .ok_or_else(|| spill_error("invalid legacy vector elements"))?;
                let (vector, memory) = vector.into_parts();
                return Ok(control.finish(Value::LegacyVector(vector), memory)?);
            }
            tag => return Err(spill_error(format!("invalid spill value tag {tag}"))),
        };
        Ok(control.finish(value, control.empty_reservation())?)
    }

    fn read_oid(&mut self, description: &str) -> ExecResult<u32> {
        Ok(u32::from_le_bytes(
            self.read_i32(description)?.to_le_bytes(),
        ))
    }

    fn read_text_with_control(
        &mut self,
        description: &str,
        control: &ProductionControl<'_>,
    ) -> ExecResult<Produced<String>> {
        let bytes = self.read_bytes(description)?;
        let text = std::str::from_utf8(bytes)
            .map_err(|error| spill_error(format!("invalid UTF-8 in {description}: {error}")))?;
        Ok(control.copy_text(text)?)
    }

    fn read_bytes_with_control(
        &mut self,
        description: &str,
        control: &ProductionControl<'_>,
    ) -> ExecResult<Produced<Vec<u8>>> {
        let bytes = self.read_bytes(description)?;
        let mut output = ProductionVec::new(*control);
        output.reserve(bytes.len())?;
        output.extend_from_slice(bytes)?;
        Ok(output.finish()?)
    }

    fn read_values(
        &mut self,
        description: &str,
        depth: usize,
        control: &ProductionControl<'_>,
    ) -> ExecResult<Produced<Vec<Value>>> {
        let count = self.read_count(description, 1)?;
        let mut values = ProductionVec::new(*control);
        values.reserve(count)?;
        for _ in 0..count {
            values.push_produced(self.read_value_with_control(depth + 1, control)?)?;
        }
        Ok(values.finish()?)
    }

    fn read_bounds(
        &mut self,
        count_description: &str,
        description: &str,
        legacy: bool,
        control: &ProductionControl<'_>,
    ) -> ExecResult<Produced<Vec<i32>>> {
        let count = self.read_count(count_description, 4)?;
        if legacy && count > 1 {
            return Err(spill_error("invalid legacy vector dimensions"));
        }
        let mut bounds = ProductionVec::new(*control);
        bounds.reserve(count)?;
        for _ in 0..count {
            bounds.push_copy(self.read_i32(description)?)?;
        }
        Ok(bounds.finish()?)
    }

    fn read_map(
        &mut self,
        depth: usize,
        control: &ProductionControl<'_>,
    ) -> ExecResult<Produced<Value>> {
        let count = self.read_count("map length", 9)?;
        let mut memory = control.empty_reservation();
        let mut values = std::collections::BTreeMap::new();
        for _ in 0..count {
            // Match Core's logical map-entry payload admission; standard-library node bookkeeping is opaque to the value owner.
            let entry = control.reserve(size_of::<(String, Value)>())?;
            let key = self.read_text_with_control("map key", control)?;
            let value = self.read_value_with_control(depth + 1, control)?;
            if values.contains_key(&*key) {
                return Err(spill_error(format!(
                    "duplicate key `{}` in spill map",
                    &*key
                )));
            }
            let (key, key_memory) = key.into_parts();
            let (value, value_memory) = value.into_parts();
            values.insert(key, value);
            memory = control.combine(
                memory,
                control.combine(entry, control.combine(key_memory, value_memory)),
            );
        }
        Ok(control.finish(Value::Map(values), memory)?)
    }

    fn read_row_value(
        &mut self,
        tag: u8,
        depth: usize,
        control: &ProductionControl<'_>,
    ) -> ExecResult<Produced<Value>> {
        let fields = if tag == 13 {
            None
        } else {
            let count = self.read_count("row descriptor length", if tag == 25 { 9 } else { 8 })?;
            let mut fields = ProductionVec::new(*control);
            fields.reserve(count)?;
            for _ in 0..count {
                let oid = self.read_oid("row field type OID")?;
                let type_modifier = self.read_i32("row field type modifier")?;
                let collation_oid = if tag == 25 {
                    match self.read_u8("row field collation presence")? {
                        0 => None,
                        1 => Some(self.read_oid("row field collation OID")?),
                        _ => return Err(spill_error("invalid row field collation presence")),
                    }
                } else {
                    None
                };
                fields.push_copy(uqa_core::RecordFieldType {
                    oid,
                    type_modifier,
                    collation_oid,
                })?;
            }
            Some(fields.finish()?)
        };
        let values = self.read_values("row length", depth, control)?;
        let row = match fields {
            Some(fields) => uqa_core::RowValue::typed_with_control(values, fields, control)
                .map_err(|error| match error {
                    uqa_core::ValueRetentionError::Malformed { .. } => {
                        spill_error(format!("invalid row descriptor: {error}"))
                    }
                    error => error.into(),
                })?,
            None => uqa_core::RowValue::new_with_control(values, control)?,
        };
        let (row, memory) = row.into_parts();
        Ok(control.finish(Value::Row(row), memory)?)
    }

    fn read_record_value(
        &mut self,
        tag: u8,
        depth: usize,
        control: &ProductionControl<'_>,
    ) -> ExecResult<Produced<Value>> {
        let oid = if tag == 23 {
            Some(self.read_oid("record type OID")?)
        } else {
            None
        };
        let count = self.read_count("record length", 9)?;
        let mut fields = ProductionVec::new(*control);
        fields.reserve(count)?;
        for _ in 0..count {
            let name = self.read_text_with_control("record field name", control)?;
            let value = self.read_value_with_control(depth + 1, control)?;
            let (name, name_memory) = name.into_parts();
            let (value, memory) = value.into_parts();
            fields.push_produced(
                control.finish((name, value), control.combine(name_memory, memory))?,
            )?;
        }
        let (record, memory) =
            uqa_core::RecordValue::with_control(fields.finish()?, oid, control)?.into_parts();
        Ok(control.finish(Value::Record(record), memory)?)
    }
}
