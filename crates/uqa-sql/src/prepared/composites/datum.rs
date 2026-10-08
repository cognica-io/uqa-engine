//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Interpret retained fixed-width fields using the current tuple descriptor.

use crate::{catalog::type_metadata::pg_type_len, ColumnType};
use uqa_core::Value;

/// A descriptor change does not cast a constant already read by `PostgreSQL`. For equal-width numeric fields, recover the original datum bits and interpret those bits under the new type. The source constant remains immutable for subsequent changes and rollback.
pub(super) fn reinterpret(value: &Value, before: &ColumnType, after: &ColumnType) -> Option<Value> {
    let before = base(before);
    let after = base(after);
    if before == after || pg_type_len(before) != pg_type_len(after) {
        return None;
    }
    let bits = match (before, value) {
        (ColumnType::Integer, Value::Int(value)) => u64::from(*value as u32),
        (ColumnType::BigInteger, Value::Int(value)) => *value as u64,
        (ty, Value::Int(value)) if oid(ty) => u64::from(*value as u32),
        (ColumnType::Real, Value::Float(value)) => u64::from((*value as f32).to_bits()),
        (ColumnType::DoublePrecision, Value::Float(value)) => value.to_bits(),
        _ => return None,
    };
    match after {
        ColumnType::Integer => Some(Value::Int(i64::from(bits as i32))),
        ColumnType::BigInteger => Some(Value::Int(bits as i64)),
        ty if oid(ty) => Some(Value::Int(i64::from(bits as u32))),
        ColumnType::Real => Some(Value::Float(f64::from(f32::from_bits(bits as u32)))),
        ColumnType::DoublePrecision => Some(Value::Float(f64::from_bits(bits))),
        _ => None,
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
