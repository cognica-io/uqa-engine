//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Heap tuple headers and fields use the shared admitted SQL Datum codec.

use super::super::Descriptors;
use super::{align, pg_type_align, pg_type_len, pg_type_storage, CompositeTypeDescriptor, Value};
use crate::{
    expr::{
        composites::CompositeTypeCatalog,
        datums::encoding::{append, encode as encode_value, header, invalid, pad_to},
    },
    SQLError,
};
use uqa_core::memory::{Produced, ProductionControl, ProductionVec};

mod arrays;
pub(crate) use arrays::{
    payload as array_payload, payload_with_control as array_payload_with_control,
};

pub(super) struct EncodedTuple {
    pub bytes: Produced<Vec<u8>>,
    pub positions: Produced<Vec<Option<usize>>>,
    pub start: usize,
}

pub(super) fn encode(
    fields: &[(String, Value)],
    descriptor: &CompositeTypeDescriptor,
    descriptors: &Descriptors,
) -> Option<(Vec<u8>, Vec<Option<usize>>, usize)> {
    let output = encode_with_control(
        fields,
        descriptor,
        Some(descriptors),
        &ProductionControl::uncontrolled(),
    )
    .ok()?;
    Some((
        output
            .bytes
            .into_uncontrolled()
            .expect("ordinary tuple encoding"),
        output
            .positions
            .into_uncontrolled()
            .expect("ordinary tuple positions"),
        output.start,
    ))
}

pub(super) fn encode_with_control(
    fields: &[(String, Value)],
    descriptor: &CompositeTypeDescriptor,
    catalog: Option<&dyn CompositeTypeCatalog>,
    control: &ProductionControl<'_>,
) -> Result<EncodedTuple, SQLError> {
    control.check()?;
    let count = usize::try_from(descriptor.physical_width()).map_err(|_| invalid())?;
    let has_null = !descriptor.dropped.is_empty()
        || fields.iter().any(|(_, value)| matches!(value, Value::Null));
    let start =
        align(23 + if has_null { count.div_ceil(8) } else { 0 }, b'd').ok_or_else(invalid)?;
    let mut bytes = ProductionVec::new(*control);
    pad_to(&mut bytes, start)?;
    bytes.as_mut_slice()[4..8].copy_from_slice(&(-1_i32).to_le_bytes());
    bytes.as_mut_slice()[8..12].copy_from_slice(&descriptor.type_oid.to_le_bytes());
    bytes.as_mut_slice()[12..16].fill(0xff);
    bytes.as_mut_slice()[18..20]
        .copy_from_slice(&u16::try_from(count).map_err(|_| invalid())?.to_le_bytes());
    bytes.as_mut_slice()[22] = u8::try_from(start).map_err(|_| invalid())?;
    let mut mask = u16::from(has_null);
    let mut positions = ProductionVec::new(*control);
    positions.reserve(descriptor.attributes.len())?;
    for attribute in &descriptor.attributes {
        control.check()?;
        let (_, value) = fields
            .iter()
            .find(|(name, _)| *name == attribute.name)
            .ok_or_else(invalid)?;
        if matches!(value, Value::Null) {
            positions.push_copy(None)?;
            continue;
        }
        if has_null {
            let bit = usize::try_from(attribute.number - 1).map_err(|_| invalid())?;
            *bytes
                .as_mut_slice()
                .get_mut(23 + bit / 8)
                .ok_or_else(invalid)? |= 1 << (bit % 8);
        }
        let length = pg_type_len(&attribute.ty);
        let payload = encode_value(value, &attribute.ty, catalog, control)?;
        if length == -1 {
            mask |= 2;
            let short = payload.len() < 127 && pg_type_storage(&attribute.ty) != "p";
            if !short {
                let aligned = align(bytes.len(), pg_type_align(&attribute.ty).as_bytes()[0])
                    .ok_or_else(invalid)?;
                pad_to(&mut bytes, aligned)?;
            }
            positions.push_copy(Some(bytes.len()))?;
            if short {
                bytes.push_copy(((payload.len() as u8 + 1) << 1) | 1)?;
            } else {
                append(&mut bytes, &header(payload.len())?)?;
            }
        } else {
            if payload.len() != usize::try_from(length).map_err(|_| invalid())? {
                return Err(invalid());
            }
            let aligned = align(bytes.len(), pg_type_align(&attribute.ty).as_bytes()[0])
                .ok_or_else(invalid)?;
            pad_to(&mut bytes, aligned)?;
            positions.push_copy(Some(bytes.len()))?;
        }
        append(&mut bytes, &payload)?;
    }
    bytes.as_mut_slice()[20..22].copy_from_slice(&mask.to_le_bytes());
    let total = header(bytes.len().checked_sub(4).ok_or_else(invalid)?)?;
    bytes.as_mut_slice()[..4].copy_from_slice(&total);
    Ok(EncodedTuple {
        bytes: bytes.finish()?,
        positions: positions.finish()?,
        start,
    })
}
