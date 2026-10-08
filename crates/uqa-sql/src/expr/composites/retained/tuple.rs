//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Fixed-width tuple data is laid out once under its original descriptor and read under the current descriptor. NULL slots take no bytes; dropped non-NULL slots retain their physical width and alignment.

use super::super::{datum, CompositeTypeDescriptor};
use crate::catalog::type_metadata::{pg_type_align, pg_type_len};
use uqa_core::Value;

pub(super) fn project(
    fields: &[(String, Value)],
    before: &CompositeTypeDescriptor,
    after: &CompositeTypeDescriptor,
) -> Option<Value> {
    let mut bytes = Vec::new();
    for attribute in &before.attributes {
        let value = fields.iter().find(|(name, _)| *name == attribute.name)?;
        if matches!(value.1, Value::Null) {
            continue;
        }
        let length = width(pg_type_len(&attribute.ty))?;
        let offset = align(bytes.len(), pg_type_align(&attribute.ty).as_bytes()[0])?;
        bytes.resize(offset, 0);
        bytes.extend_from_slice(
            &datum::encode_bits(&value.1, &attribute.ty)?.to_le_bytes()[..length],
        );
    }

    let mut output = after
        .attributes
        .iter()
        .map(|attribute| (attribute.name.clone(), Value::Null))
        .collect::<Vec<_>>();
    let mut offset = 0;
    for original in &before.attributes {
        let (_, value) = fields.iter().find(|(name, _)| *name == original.name)?;
        if matches!(value, Value::Null) {
            continue;
        }
        if let Some((index, attribute)) = after
            .attributes
            .iter()
            .enumerate()
            .find(|(_, attribute)| attribute.number == original.number)
        {
            let length = width(pg_type_len(&attribute.ty))?;
            offset = align(offset, pg_type_align(&attribute.ty).as_bytes()[0])?;
            let end = offset.checked_add(length)?;
            let mut bits = [0; 8];
            bits[..length].copy_from_slice(bytes.get(offset..end)?);
            output[index].1 = datum::decode_bits(u64::from_le_bytes(bits), &attribute.ty)?;
            offset = end;
        } else {
            let dropped = after
                .dropped
                .iter()
                .find(|attribute| attribute.number == original.number)?;
            offset = align(offset, dropped.alignment)?.checked_add(width(dropped.length)?)?;
        }
    }
    Some(Value::Record(output))
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
            project(&fields, &before, &after),
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
            project(&fields, &before, &before),
            Some(Value::Record(fields))
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
        assert!(project(&[("a".into(), Value::Int(4))], &before, &after).is_none());
    }
}
