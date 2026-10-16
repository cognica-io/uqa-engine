//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Inline array datums keep their physical element identity, bounds and null bitmap.

use super::{align, pg_type_align, pg_type_len, Descriptors, Value};
use crate::catalog::type_metadata::{builtin_scalar_type, pg_type_oid};
use crate::{
    expr::{
        composites::CompositeTypeCatalog,
        datums::encoding::{append, encode, header, invalid, pad_to},
    },
    ColumnType, SQLError,
};
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionVec},
    ArrayValue,
};

pub(crate) fn payload(
    array: &ArrayValue,
    element: &ColumnType,
    descriptors: &Descriptors,
) -> Option<Vec<u8>> {
    payload_with_control(
        array,
        element,
        Some(descriptors),
        &ProductionControl::uncontrolled(),
    )
    .ok()?
    .into_uncontrolled()
    .ok()
}

pub(crate) fn payload_with_control(
    array: &ArrayValue,
    mut element: &ColumnType,
    catalog: Option<&dyn CompositeTypeCatalog>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<u8>>, SQLError> {
    control.check()?;
    while let ColumnType::Array(inner) = element {
        element = inner;
    }
    let oid = array
        .element_type_oid()
        .unwrap_or(pg_type_oid(element) as u32);
    let element = builtin_scalar_type(oid)
        .or_else(|| (pg_type_oid(element) == i64::from(oid)).then_some(element))
        .ok_or_else(invalid)?;
    let dimensions = array.dimensions();
    if dimensions.len() > 6 {
        return Err(invalid());
    }
    let mut cursor = array.elements_with_control(control)?;
    let mut has_null = false;
    let mut count = 0_usize;
    while let Some(value) = cursor.next_element()? {
        has_null |= matches!(value, Value::Null);
        count = count.checked_add(1).ok_or_else(invalid)?;
    }
    drop(cursor);
    let bitmap = 16 + dimensions.len() * 8;
    let start = align(
        bitmap
            .checked_add(if has_null { count.div_ceil(8) } else { 0 })
            .ok_or_else(invalid)?,
        b'd',
    )
    .ok_or_else(invalid)?;
    let mut bytes = ProductionVec::new(*control);
    pad_to(&mut bytes, start)?;
    bytes.as_mut_slice()[4..8].copy_from_slice(&(dimensions.len() as i32).to_le_bytes());
    if has_null {
        bytes.as_mut_slice()[8..12]
            .copy_from_slice(&i32::try_from(start).map_err(|_| invalid())?.to_le_bytes());
    }
    bytes.as_mut_slice()[12..16].copy_from_slice(&oid.to_le_bytes());
    for (index, dimension) in dimensions.iter().enumerate() {
        let offset = 16 + index * 4;
        bytes.as_mut_slice()[offset..offset + 4].copy_from_slice(
            &i32::try_from(*dimension)
                .map_err(|_| invalid())?
                .to_le_bytes(),
        );
    }
    for (index, bound) in array.lower_bounds().iter().enumerate() {
        let offset = 16 + dimensions.len() * 4 + index * 4;
        bytes.as_mut_slice()[offset..offset + 4].copy_from_slice(&bound.to_le_bytes());
    }
    let mut cursor = array.elements_with_control(control)?;
    let mut index = 0;
    while let Some(value) = cursor.next_element()? {
        if !matches!(value, Value::Null) {
            if has_null {
                bytes.as_mut_slice()[bitmap + index / 8] |= 1 << (index % 8);
            }
            let aligned =
                align(bytes.len(), pg_type_align(element).as_bytes()[0]).ok_or_else(invalid)?;
            pad_to(&mut bytes, aligned)?;
            let length = pg_type_len(element);
            let payload = encode(value, element, catalog, control)?;
            if length == -1 {
                append(&mut bytes, &header(payload.len())?)?;
            } else if payload.len() != usize::try_from(length).map_err(|_| invalid())? {
                return Err(invalid());
            }
            append(&mut bytes, &payload)?;
            let aligned =
                align(bytes.len(), pg_type_align(element).as_bytes()[0]).ok_or_else(invalid)?;
            pad_to(&mut bytes, aligned)?;
        }
        index += 1;
    }
    let (mut bytes, memory) = bytes.finish()?.into_parts();
    bytes.drain(..4);
    Ok(control.finish(bytes, memory)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domain_elements_keep_catalog_identity_and_base_type_layout() {
        for (base, values, reference) in [
            (
                ColumnType::Integer,
                vec![Value::Int(1), Value::Null, Value::Int(3)],
                "010000002000000074400000030000000100000005000000000000000100000003000000",
            ),
            (
                ColumnType::Real,
                vec![Value::Float(f64::from(0.1_f32)), Value::Float(-0.0), Value::Float(1.25)],
                "0100000000000000744000000300000001000000cdcccc3d000000800000a03f",
            ),
            (
                ColumnType::Text,
                vec![Value::Str("a".into()), Value::Null, Value::Str("bc".into())],
                "0100000020000000744000000300000001000000050000000000000014000000610000001800000062630000",
            ),
        ] {
            let ty = ColumnType::Domain {
                collation_oid: None,
                schema: "public".into(),
                name: "array_element".into(),
                oid: 16_500,
                array_oid: Some(16_501),
                base: Box::new(base),
            };
            let array = ArrayValue::try_new(values)
                .unwrap()
                .with_element_type_oid(Some(16_500));
            // PostgreSQL's captured payload, with only the database-local OID replaced.
            let expected = (0..reference.len())
                .step_by(2)
                .map(|index| u8::from_str_radix(&reference[index..index + 2], 16).unwrap())
                .collect::<Vec<_>>();
            assert_eq!(payload(&array, &ty, &Descriptors::new()).unwrap(), expected);
        }
    }
}
