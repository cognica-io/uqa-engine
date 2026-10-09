//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Interpret retained fixed-width fields using the current tuple descriptor.

use crate::{
    catalog::{
        node_tree::{decode_temporal_datum, encode_temporal_datum},
        type_metadata::{pg_type_len, pg_type_oid},
    },
    ColumnType,
};
use uqa_core::Value;

/// A descriptor change does not cast a constant already read by `PostgreSQL`. For represented equal-width by-value fields, recover the original datum bits and interpret those bits under the new type. The source constant remains immutable for subsequent changes and rollback.
pub(crate) fn reinterpret(value: &Value, before: &ColumnType, after: &ColumnType) -> Option<Value> {
    let before = base(before);
    let after = base(after);
    if before == after || pg_type_len(before) != pg_type_len(after) {
        return None;
    }
    decode_bits(encode_bits(value, before)?, after)
}

pub(crate) fn encode_bits(value: &Value, ty: &ColumnType) -> Option<u64> {
    Some(match (base(ty), value) {
        (ColumnType::Enum(_), Value::Enum(value)) => u64::from(value.label_oid()?),
        (ColumnType::Boolean, Value::Bool(value)) => u64::from(*value),
        (ColumnType::InternalChar, Value::Str(value)) => {
            u64::from(value.as_bytes().first().copied().unwrap_or(0))
        }
        (ColumnType::SmallInteger, Value::Int(value)) => u64::from(*value as u16),
        (ColumnType::Integer, Value::Int(value)) => u64::from(*value as u32),
        (ColumnType::BigInteger, Value::Int(value)) => *value as u64,
        (ty, Value::Int(value)) if oid(ty) => u64::from(*value as u32),
        (ColumnType::Real, Value::Float(value)) => u64::from(real_bits(*value)),
        (ColumnType::DoublePrecision, Value::Float(value)) => value.to_bits(),
        (ty, Value::Temporal(value)) if temporal(ty) => {
            let bytes = encode_temporal_datum(value, pg_type_oid(ty)).ok()?;
            u64::from_le_bytes(bytes.try_into().ok()?)
        }
        _ => return None,
    })
}

pub(crate) fn decode_bits(bits: u64, ty: &ColumnType) -> Option<Value> {
    match base(ty) {
        ColumnType::Boolean => Some(Value::Bool(bits as u8 != 0)),
        ColumnType::InternalChar => Some(Value::Str(if bits == 0 {
            String::new()
        } else {
            char::from(bits as u8).to_string()
        })),
        ColumnType::SmallInteger => Some(Value::Int(i64::from(bits as i16))),
        ColumnType::Integer => Some(Value::Int(i64::from(bits as i32))),
        ColumnType::BigInteger => Some(Value::Int(bits as i64)),
        ty if oid(ty) => Some(Value::Int(i64::from(bits as u32))),
        ColumnType::Real => Some(Value::Float(real_value(bits as u32))),
        ColumnType::DoublePrecision => Some(Value::Float(f64::from_bits(bits))),
        ty if temporal(ty) => decode_temporal_datum(&bits.to_le_bytes(), pg_type_oid(ty))
            .ok()
            .map(Value::Temporal),
        _ => None,
    }
}

fn temporal(ty: &ColumnType) -> bool {
    matches!(
        ty.without_temporal_modifiers(),
        ColumnType::Date | ColumnType::Time | ColumnType::Timestamp | ColumnType::TimestampTz
    )
}

// Hardware widening and narrowing can quiet a signaling NaN. Descriptor projection must keep those bits, including across serialization, so changing the descriptor back recovers the original integer datum.
fn real_value(bits: u32) -> f64 {
    if bits & 0x7f80_0000 == 0x7f80_0000 && bits & 0x007f_ffff != 0 {
        f64::from_bits(
            (u64::from(bits & 0x8000_0000) << 32)
                | 0x7ff0_0000_0000_0000
                | (u64::from(bits & 0x007f_ffff) << 29),
        )
    } else {
        f64::from(f32::from_bits(bits))
    }
}

fn real_bits(value: f64) -> u32 {
    if value.is_nan() {
        let bits = value.to_bits();
        let fraction = ((bits >> 29) as u32) & 0x007f_ffff;
        (((bits >> 32) as u32) & 0x8000_0000)
            | 0x7f80_0000
            | if fraction == 0 { 0x0040_0000 } else { fraction }
    } else {
        (value as f32).to_bits()
    }
}

fn base(mut ty: &ColumnType) -> &ColumnType {
    while let ColumnType::Domain { base, .. } = ty {
        ty = base;
    }
    ty
}

fn oid(ty: &ColumnType) -> bool {
    matches!(
        ty,
        ColumnType::Oid
            | ColumnType::Xid
            | ColumnType::Regproc
            | ColumnType::Regprocedure
            | ColumnType::Regclass
            | ColumnType::Regcollation
            | ColumnType::Regnamespace
            | ColumnType::Regrole
            | ColumnType::Regtype
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_descriptor_projection_preserves_all_nan_bits_through_catalog_json() {
        for bits in [0x7f80_0001_u32, 0x7fc0_0001, 0xff80_0001, 0xffff_ffff] {
            let input = Value::Int(i64::from(bits as i32));
            let projected = reinterpret(&input, &ColumnType::Integer, &ColumnType::Real).unwrap();
            let encoded = serde_json::to_string(&projected).unwrap();
            let retained = serde_json::from_str(&encoded).unwrap();
            assert_eq!(
                reinterpret(&retained, &ColumnType::Real, &ColumnType::Integer),
                Some(input)
            );
        }
    }
}
