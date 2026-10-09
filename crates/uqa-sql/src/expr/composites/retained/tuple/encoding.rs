//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Heap tuple data and header construction, using SQL's existing scalar Datum codecs.

use super::{
    align, datum, pg_type_align, pg_type_len, pg_type_storage, width, ColumnType,
    CompositeTypeDescriptor, Value,
};

mod arrays;

pub(super) fn encode(
    fields: &[(String, Value)],
    descriptor: &CompositeTypeDescriptor,
) -> Option<(Vec<u8>, Vec<Option<usize>>, usize)> {
    let count = descriptor
        .attributes
        .iter()
        .map(|a| a.number)
        .chain(descriptor.dropped.iter().map(|a| a.number))
        .max()
        .unwrap_or(0) as usize;
    let has_null =
        !descriptor.dropped.is_empty() || fields.iter().any(|(_, v)| matches!(v, Value::Null));
    let start = align(23 + if has_null { count.div_ceil(8) } else { 0 }, b'd')?;
    let mut bytes = vec![0; start];
    bytes[4..8].copy_from_slice(&(-1_i32).to_le_bytes());
    bytes[8..12].copy_from_slice(&descriptor.type_oid.to_le_bytes());
    bytes[12..16].fill(0xff);
    bytes[18..20].copy_from_slice(&u16::try_from(count).ok()?.to_le_bytes());
    bytes[22] = u8::try_from(start).ok()?;
    let mut mask = u16::from(has_null);
    let mut positions = Vec::with_capacity(descriptor.attributes.len());
    for attribute in &descriptor.attributes {
        let (_, value) = fields.iter().find(|(name, _)| *name == attribute.name)?;
        if matches!(value, Value::Null) {
            positions.push(None);
            continue;
        }
        if has_null {
            let bit = usize::try_from(attribute.number - 1).ok()?;
            bytes[23 + bit / 8] |= 1 << (bit % 8);
        }
        let length = pg_type_len(&attribute.ty);
        if length == -1 {
            mask |= 2;
            let payload = payload(value, &attribute.ty)?;
            let short = payload.len() < 127 && pg_type_storage(&attribute.ty) != "p";
            if !short {
                bytes.resize(
                    align(bytes.len(), pg_type_align(&attribute.ty).as_bytes()[0])?,
                    0,
                );
            }
            positions.push(Some(bytes.len()));
            if short {
                bytes.push(((payload.len() as u8 + 1) << 1) | 1);
            } else {
                bytes.extend_from_slice(
                    &(u32::try_from(payload.len().checked_add(4)?).ok()? << 2).to_le_bytes(),
                );
            }
            bytes.extend_from_slice(&payload);
        } else {
            bytes.resize(
                align(bytes.len(), pg_type_align(&attribute.ty).as_bytes()[0])?,
                0,
            );
            positions.push(Some(bytes.len()));
            if let Some(length) = width(length) {
                bytes.extend_from_slice(
                    &datum::encode_bits(value, &attribute.ty)?.to_le_bytes()[..length],
                );
            } else {
                let encoded = fixed(value, &attribute.ty)?;
                if encoded.len() != usize::try_from(length).ok()? {
                    return None;
                }
                bytes.extend_from_slice(&encoded);
            }
        }
    }
    bytes[20..22].copy_from_slice(&mask.to_le_bytes());
    let total = u32::try_from(bytes.len()).ok()?.checked_mul(4)?;
    bytes[..4].copy_from_slice(&total.to_le_bytes());
    Some((bytes, positions, start))
}

fn fixed(value: &Value, ty: &ColumnType) -> Option<Vec<u8>> {
    if let ColumnType::Domain { base, .. } = ty {
        return fixed(value, base);
    }
    match (value, ty) {
        (Value::Str(text), ColumnType::Name) if text.len() < 64 => {
            let mut bytes = text.as_bytes().to_vec();
            bytes.resize(64, 0);
            Some(bytes)
        }
        (Value::Str(text), ColumnType::Uuid) => {
            Some(crate::expr::uuid::parse_uuid_bytes(text).ok()?.to_vec())
        }
        (Value::Temporal(value), _) if matches!(pg_type_len(ty), 12 | 16) => {
            crate::catalog::node_tree::encode_temporal_datum(
                value,
                crate::catalog::type_metadata::pg_type_oid(ty),
            )
            .ok()
        }
        _ => None,
    }
}

fn payload(value: &Value, ty: &ColumnType) -> Option<Vec<u8>> {
    if let ColumnType::Domain { base, .. } = ty {
        return payload(value, base);
    }
    match (value, ty) {
        (
            Value::Str(text) | Value::FixedChar(text),
            ColumnType::Text
            | ColumnType::Varchar(_)
            | ColumnType::Character(_)
            | ColumnType::Bpchar
            | ColumnType::RefCursor
            | ColumnType::PgNodeTree,
        ) => Some(text.as_bytes().to_vec()),
        (Value::Json(text), ColumnType::Json) => Some(text.as_bytes().to_vec()),
        (Value::JsonB(text), ColumnType::JsonB) => crate::expr::json::encode_jsonb_datum(text).ok(),
        (Value::Bytes(bytes), ColumnType::Bytea) => Some(bytes.clone()),
        (Value::Decimal(number), ColumnType::Numeric { .. }) => {
            crate::catalog::node_tree::encode_numeric_datum(number).ok()
        }
        (Value::Array(array), ColumnType::Array(element)) => arrays::payload(array, element),
        _ => None,
    }
}
