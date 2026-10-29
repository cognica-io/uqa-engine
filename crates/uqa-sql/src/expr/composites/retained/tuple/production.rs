//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Controlled tuple deformation keeps lazy field views within the original admitted backing allocation.

use super::{
    align, base_oid, pg_type_align, pg_type_len, variable_length, width, CompositeTypeDescriptor,
};
use crate::{expr::composites::retained::owners::DescriptorLookup, ColumnType, SQLError};
use uqa_core::{
    memory::{MemoryReservation, Produced, ProductionControl, ProductionVec},
    DatumValue, RecordValue, Value,
};

pub(in crate::expr::composites::retained) fn project_with_control(
    fields: &RecordValue,
    before: &CompositeTypeDescriptor,
    after: &CompositeTypeDescriptor,
    descriptors: &dyn DescriptorLookup,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<Value>>, SQLError> {
    let encoded =
        match super::encoding::encode_with_control(fields, before, Some(descriptors), control) {
            Ok(encoded) => encoded,
            Err(error) if matches!(error.sqlstate(), Some("53200" | "57014")) => return Err(error),
            Err(_) => return Ok(None),
        };
    let backing = DatumValue::with_control(0, 0, encoded.bytes, control)?;
    let bytes = backing.bytes();
    let mut output = Fields::new(after.attributes.len(), control)?;
    let mut uses_backing = false;
    let mut offset = encoded.start;
    for (original, position) in before.attributes.iter().zip(encoded.positions.iter()) {
        control.check()?;
        let Some(position) = position else {
            continue;
        };
        let Some((_, value)) = fields
            .iter()
            .find(|(name, _)| name.as_str() == original.name)
        else {
            return Ok(None);
        };
        let current = after
            .attributes
            .iter()
            .enumerate()
            .find(|(_, attribute)| attribute.number == original.number);
        let (length, alignment) = if let Some((_, attribute)) = current {
            (
                pg_type_len(&attribute.ty),
                pg_type_align(&attribute.ty).as_bytes()[0],
            )
        } else {
            let Some(dropped) = after
                .dropped
                .iter()
                .find(|attribute| attribute.number == original.number)
            else {
                return Ok(None);
            };
            (dropped.length, dropped.alignment)
        };
        if length != -1 || bytes.get(offset) == Some(&0) {
            let Some(aligned) = align(offset, alignment) else {
                return Ok(None);
            };
            offset = aligned;
        }
        if let Some((index, attribute)) = current {
            let value = if *position == offset && original.ty == attribute.ty {
                Some(control.copy_value(value)?)
            } else if *position == offset {
                retain_array_identity(value, &original.ty, &attribute.ty, control)?
            } else {
                None
            };
            let value = if let Some(value) = value {
                value
            } else {
                let Some(value) = read_field(
                    &backing,
                    offset,
                    length,
                    &attribute.ty,
                    &mut uses_backing,
                    control,
                )?
                else {
                    return Ok(None);
                };
                value
            };
            output.values[index] = value;
        }
        let advance = if length == -1 {
            bytes.get(offset..).and_then(variable_length).unwrap_or(0)
        } else {
            let Ok(length) = usize::try_from(length) else {
                return Ok(None);
            };
            length
        };
        let Some(next) = offset.checked_add(advance) else {
            return Ok(None);
        };
        offset = next;
    }
    let result = output.finish(after, before.type_oid, control)?;
    if uses_backing {
        let (backing, backing_memory) = backing.into_parts();
        drop(backing);
        let (value, memory) = result.into_parts();
        Ok(Some(
            control.finish(value, control.combine(memory, backing_memory))?,
        ))
    } else {
        Ok(Some(result))
    }
}

fn read_field(
    backing: &DatumValue,
    offset: usize,
    length: i64,
    ty: &ColumnType,
    uses_backing: &mut bool,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<Value>>, SQLError> {
    if length != -1 && matches!(length, 1 | 2 | 4 | 8) {
        let Some(length) = width(length) else {
            return Ok(None);
        };
        let Some(end) = offset.checked_add(length) else {
            return Ok(None);
        };
        if let Some(bytes) = backing.bytes().get(offset..end) {
            let mut bits = [0; 8];
            bits[..length].copy_from_slice(bytes);
            return super::super::super::datum::decode_bits_with_control(
                u64::from_le_bytes(bits),
                ty,
                control,
            );
        }
    }
    let (Some(oid), Ok(offset)) = (base_oid(ty), u32::try_from(offset)) else {
        return Ok(None);
    };
    *uses_backing = true;
    Ok(Some(control.finish(
        Value::Datum(backing.field(oid, offset)),
        control.empty_reservation(),
    )?))
}

fn retain_array_identity(
    value: &Value,
    before: &ColumnType,
    after: &ColumnType,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<Value>>, SQLError> {
    fn element_oid(ty: &ColumnType) -> Option<u32> {
        match ty {
            ColumnType::Domain { base, .. } => element_oid(base),
            ColumnType::Array(element) => {
                let mut leaf = element.as_ref();
                while let ColumnType::Array(element) = leaf {
                    leaf = element;
                }
                Some(crate::catalog::type_metadata::pg_type_oid(leaf) as u32)
            }
            _ => None,
        }
    }
    if element_oid(after).is_none() {
        return Ok(None);
    }
    let Value::Array(array) = value else {
        return Ok(None);
    };
    let oid = array.element_type_oid().or(element_oid(before));
    let (value, memory) = control.copy_value(value)?.into_parts();
    let Value::Array(array) = value else {
        unreachable!();
    };
    Ok(Some(control.finish(
        Value::Array(array.with_element_type_oid(oid)),
        memory,
    )?))
}

struct Fields {
    values: Vec<Produced<Value>>,
    memory: Option<MemoryReservation>,
}
impl Fields {
    fn new(width: usize, control: &ProductionControl<'_>) -> Result<Self, SQLError> {
        let mut values = ProductionVec::new(*control);
        values.reserve(width)?;
        for _ in 0..width {
            let value = control.finish(Value::Null, control.empty_reservation())?;
            values.push_produced(control.finish(value, control.empty_reservation())?)?;
        }
        let (values, memory) = values.finish()?.into_parts();
        Ok(Self { values, memory })
    }
    fn finish(
        self,
        descriptor: &CompositeTypeDescriptor,
        oid: u32,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Value>, SQLError> {
        let Self { values, memory } = self;
        let mut fields = ProductionVec::new(*control);
        for (attribute, value) in descriptor.attributes.iter().zip(values) {
            super::super::production::push_field(&mut fields, &attribute.name, value, control)?;
        }
        drop(memory);
        super::super::production::record(fields.finish()?, Some(oid), control)
    }
}
