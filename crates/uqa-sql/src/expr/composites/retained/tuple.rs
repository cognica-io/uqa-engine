//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Form the original tuple once and deform it under the current descriptor. Variable-width fields retain byte views until an operation reads them; NULL tests inspect only the tuple's null bitmap.

use super::super::{datum, CompositeTypeDescriptor};
use crate::catalog::type_metadata::{pg_type_align, pg_type_len, pg_type_oid, pg_type_storage};
use crate::ColumnType;
use uqa_core::{DatumValue, Value};

mod encoding;

pub(super) fn project(
    fields: &[(String, Value)],
    before: &CompositeTypeDescriptor,
    after: &CompositeTypeDescriptor,
    descriptors: &super::Descriptors,
) -> Option<Value> {
    let (bytes, positions, start) = encoding::encode(fields, before, descriptors)?;
    let backing = DatumValue::new(0, 0, bytes);
    let bytes = backing.bytes();
    let mut output = after
        .attributes
        .iter()
        .map(|attribute| (attribute.name.clone(), Value::Null))
        .collect::<Vec<_>>();
    let mut offset = start;
    for (original, position) in before.attributes.iter().zip(positions) {
        let Some(position) = position else {
            continue;
        };
        let (_, value) = fields.iter().find(|(name, _)| *name == original.name)?;
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
            let dropped = after
                .dropped
                .iter()
                .find(|attribute| attribute.number == original.number)?;
            (dropped.length, dropped.alignment)
        };
        // att_align_pointer leaves packed varlena values unaligned. Zero is the pad byte preceding a four-byte header.
        if length != -1 || bytes.get(offset) == Some(&0) {
            offset = align(offset, alignment)?;
        }
        if let Some((index, attribute)) = current {
            // Array operations dispatch on the admitted header's element OID. An unchanged byte position can retain its validated elements even when the declared array type changes.
            let retained_array = (position == offset && original.ty != attribute.ty)
                .then(|| retain_array_identity(value, &original.ty, &attribute.ty))
                .flatten();
            output[index].1 = if position == offset && original.ty == attribute.ty {
                value.clone()
            } else if let Some(array) = retained_array {
                array
            } else if length == -1 || !matches!(length, 1 | 2 | 4 | 8) {
                Value::Datum(backing.field(base_oid(&attribute.ty)?, u32::try_from(offset).ok()?))
            } else {
                let length = width(length)?;
                let end = offset.checked_add(length)?;
                let mut bits = [0; 8];
                bits[..length].copy_from_slice(bytes.get(offset..end)?);
                datum::decode_bits(u64::from_le_bytes(bits), &attribute.ty)?
            };
        }
        offset = offset.checked_add(if length == -1 {
            bytes.get(offset..).and_then(variable_length).unwrap_or(0)
        } else {
            usize::try_from(length).ok()?
        })?;
    }
    Some(Value::Record(uqa_core::RecordValue::from_parts(
        output,
        Some(before.type_oid),
    )))
}

fn retain_array_identity(value: &Value, before: &ColumnType, after: &ColumnType) -> Option<Value> {
    fn element_oid(ty: &ColumnType) -> Option<u32> {
        match ty {
            ColumnType::Domain { base, .. } => element_oid(base),
            ColumnType::Array(element) => {
                let mut leaf = element.as_ref();
                while let ColumnType::Array(element) = leaf {
                    leaf = element;
                }
                Some(pg_type_oid(leaf) as u32)
            }
            _ => None,
        }
    }
    element_oid(after)?;
    let Value::Array(array) = value else {
        return None;
    };
    let oid = array.element_type_oid().or(element_oid(before));
    Some(Value::Array(array.clone().with_element_type_oid(oid)))
}

fn base_oid(ty: &ColumnType) -> Option<u32> {
    match ty {
        ColumnType::Domain { base, .. } => base_oid(base),
        _ => u32::try_from(pg_type_oid(ty)).ok(),
    }
}

fn variable_length(bytes: &[u8]) -> Option<usize> {
    let first = *bytes.first()?;
    if first == 1 {
        // External on-disk pointers occupy eighteen bytes; indirect and expanded pointers occupy ten.
        return Some(match bytes.get(1) {
            Some(1..=3) => 10,
            Some(18) => 18,
            _ => 2,
        });
    }
    if first & 1 != 0 {
        Some(usize::from(first >> 1))
    } else {
        Some((u32::from_le_bytes(bytes.get(..4)?.try_into().ok()?) >> 2) as usize)
    }
}

fn width(length: i64) -> Option<usize> {
    matches!(length, 1 | 2 | 4 | 8).then_some(length as usize)
}

fn align(offset: usize, alignment: u8) -> Option<usize> {
    let mask = match alignment {
        b'c' => 0,
        b's' => 1,
        b'i' => 3,
        b'd' => 7,
        _ => return None,
    };
    Some(offset.checked_add(mask)? & !mask)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{expr::composites::CompositeAttribute, ColumnType};

    #[test]
    fn width_changes_reposition_following_values_without_rewriting_the_original_tuple() {
        let types = [
            ColumnType::BigInteger,
            ColumnType::Integer,
            ColumnType::SmallInteger,
            ColumnType::Boolean,
            ColumnType::Integer,
        ];
        let before = CompositeTypeDescriptor {
            type_oid: 20_000,
            relation_oid: 20_001,
            dropped: Vec::new(),
            attributes: types
                .into_iter()
                .enumerate()
                .map(|(index, ty)| CompositeAttribute {
                    name: char::from(b'a' + index as u8).to_string(),
                    number: index as i16 + 1,
                    ty,
                })
                .collect(),
        };
        let values = [
            Value::Int(72_623_859_790_382_856),
            Value::Int(287_454_020),
            Value::Int(21_862),
            Value::Bool(true),
            Value::Int(2_005_440_938),
        ];
        let fields = before
            .attributes
            .iter()
            .zip(values)
            .map(|(attribute, value)| (attribute.name.clone(), value))
            .collect::<Vec<_>>();
        let mut after = before.clone();
        after.attributes[0].ty = ColumnType::Integer;
        let expected = [
            Value::Int(84_281_096),
            Value::Int(16_909_060),
            Value::Int(13_124),
            Value::Bool(true),
            Value::Int(87_398),
        ];
        assert_eq!(
            project(&fields, &before, &after, &super::super::Descriptors::new()),
            Some(Value::Record(
                before
                    .attributes
                    .iter()
                    .zip(expected)
                    .map(|(attribute, value)| (attribute.name.clone(), value))
                    .collect()
            ))
        );
        assert_eq!(
            project(&fields, &before, &before, &super::super::Descriptors::new()),
            Some(Value::Record(fields.into()))
        );
    }

    #[test]
    fn tuple_reads_never_extend_past_the_admitted_bytes() {
        let before = CompositeTypeDescriptor {
            type_oid: 20_000,
            relation_oid: 20_001,
            dropped: Vec::new(),
            attributes: vec![CompositeAttribute {
                name: "a".into(),
                number: 1,
                ty: ColumnType::Integer,
            }],
        };
        let mut after = before.clone();
        after.attributes[0].ty = ColumnType::BigInteger;
        assert!(project(
            &[("a".into(), Value::Int(4))],
            &before,
            &after,
            &super::super::Descriptors::new()
        )
        .is_none());
    }
}
