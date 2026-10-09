//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Inline array datums keep their physical element identity, bounds and null bitmap.

use super::{align, datum, fixed, pg_type_align, pg_type_len, width, ColumnType, Value};
use crate::catalog::type_metadata::{builtin_scalar_type, pg_type_oid};
use uqa_core::{memory::ProductionControl, ArrayValue};

pub(super) fn payload(array: &ArrayValue, mut element: &ColumnType) -> Option<Vec<u8>> {
    while let ColumnType::Array(inner) = element {
        element = inner;
    }
    let oid = array
        .element_type_oid()
        .unwrap_or(pg_type_oid(element) as u32);
    let element = builtin_scalar_type(oid)?;
    let dimensions = array.dimensions();
    if dimensions.len() > 6 {
        return None;
    }
    let control = ProductionControl::uncontrolled();
    let mut cursor = array.elements_with_control(&control).ok()?;
    let mut has_null = false;
    let mut count = 0_usize;
    while let Some(value) = cursor.next_element().ok()? {
        has_null |= matches!(value, Value::Null);
        count = count.checked_add(1)?;
    }
    let bitmap = 16_usize.checked_add(dimensions.len().checked_mul(8)?)?;
    let start = align(
        bitmap.checked_add(if has_null { count.div_ceil(8) } else { 0 })?,
        b'd',
    )?;
    let mut bytes = vec![0; start];
    bytes[4..8].copy_from_slice(&(dimensions.len() as i32).to_le_bytes());
    if has_null {
        bytes[8..12].copy_from_slice(&i32::try_from(start).ok()?.to_le_bytes());
    }
    bytes[12..16].copy_from_slice(&oid.to_le_bytes());
    for (index, dimension) in dimensions.iter().enumerate() {
        let offset = 16 + index * 4;
        bytes[offset..offset + 4].copy_from_slice(&i32::try_from(*dimension).ok()?.to_le_bytes());
    }
    for (index, bound) in array.lower_bounds().iter().enumerate() {
        let offset = 16 + dimensions.len() * 4 + index * 4;
        bytes[offset..offset + 4].copy_from_slice(&bound.to_le_bytes());
    }
    let mut cursor = array.elements_with_control(&control).ok()?;
    let mut index = 0;
    while let Some(value) = cursor.next_element().ok()? {
        if !matches!(value, Value::Null) {
            if has_null {
                bytes[bitmap + index / 8] |= 1 << (index % 8);
            }
            bytes.resize(align(bytes.len(), pg_type_align(element).as_bytes()[0])?, 0);
            let length = pg_type_len(element);
            if length == -1 {
                let payload = super::payload(value, element)?;
                let header = u32::try_from(payload.len().checked_add(4)?)
                    .ok()?
                    .checked_mul(4)?;
                bytes.extend_from_slice(&header.to_le_bytes());
                bytes.extend_from_slice(&payload);
            } else if let Some(length) = width(length) {
                bytes.extend_from_slice(
                    &datum::encode_bits(value, element)?.to_le_bytes()[..length],
                );
            } else {
                bytes.extend_from_slice(&fixed(value, element)?);
            }
            bytes.resize(align(bytes.len(), pg_type_align(element).as_bytes()[0])?, 0);
        }
        index += 1;
    }
    Some(bytes.split_off(4))
}
