//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Form the original tuple once and deform it under the current descriptor. Variable-width fields retain byte views until an operation reads them; NULL tests inspect only the tuple's null bitmap.

use super::super::CompositeTypeDescriptor;
use crate::catalog::type_metadata::{pg_type_align, pg_type_len, pg_type_oid, pg_type_storage};
use crate::ColumnType;
use uqa_core::Value;

mod encoding;
mod production;
pub(crate) use encoding::{array_payload, array_payload_with_control};
pub(super) use production::project_with_control;

pub(crate) fn record_bytes_with_control(
    fields: &uqa_core::RecordValue,
    descriptor: &CompositeTypeDescriptor,
    catalog: Option<&dyn super::super::CompositeTypeCatalog>,
    control: &uqa_core::memory::ProductionControl<'_>,
) -> Result<uqa_core::memory::Produced<Vec<u8>>, crate::SQLError> {
    Ok(encoding::encode_with_control(fields, descriptor, catalog, control)?.bytes)
}

pub(crate) fn record_bytes(
    fields: &uqa_core::RecordValue,
    type_oid: u32,
    descriptors: &super::Descriptors,
) -> Option<Vec<u8>> {
    encoding::encode(fields, descriptors.get(&type_oid)?, descriptors).map(|(bytes, _, _)| bytes)
}

#[cfg(test)]
pub(super) fn project(
    fields: &uqa_core::RecordValue,
    before: &CompositeTypeDescriptor,
    after: &CompositeTypeDescriptor,
    descriptors: &super::Descriptors,
) -> Option<Value> {
    project_with_control(
        fields,
        before,
        after,
        descriptors,
        &uqa_core::memory::ProductionControl::uncontrolled(),
    )
    .ok()?
    .map(|value| {
        value
            .into_uncontrolled()
            .expect("ordinary tuple projection")
    })
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
                    collation_oid: None,
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
            project(
                &fields.clone().into(),
                &before,
                &after,
                &super::super::Descriptors::new()
            ),
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
            project(
                &fields.clone().into(),
                &before,
                &before,
                &super::super::Descriptors::new()
            ),
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
                collation_oid: None,
                name: "a".into(),
                number: 1,
                ty: ColumnType::Integer,
            }],
        };
        let mut after = before.clone();
        after.attributes[0].ty = ColumnType::BigInteger;
        let Value::Record(fields) = project(
            &vec![("a".into(), Value::Int(4))].into(),
            &before,
            &after,
            &super::super::Descriptors::new(),
        )
        .unwrap() else {
            panic!("retained record")
        };
        let Value::Datum(field) = &fields.values()[0] else {
            panic!("unread fixed field")
        };
        assert_eq!(field.bytes().len(), 28);
        assert_eq!(field.offset(), 24);
        assert_eq!(field.type_oid(), 20);
        let error = crate::expr::datums::read(field).unwrap_err();
        assert_eq!(error.sqlstate(), Some("XX001"));
        assert_eq!(error.to_string(), "invalid datum length");
    }
}
