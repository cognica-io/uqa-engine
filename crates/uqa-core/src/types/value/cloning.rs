//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Scalar copies do not enter the recursive payload-cloning frame.

use super::Value;

impl Clone for Value {
    #[inline]
    fn clone(&self) -> Self {
        match self {
            Self::Null => Self::Null,
            Self::Void => Self::Void,
            Self::Bool(value) => Self::Bool(*value),
            Self::Int(value) => Self::Int(*value),
            Self::Float(value) => Self::Float(*value),
            Self::Temporal(value) => Self::Temporal(value.clone()),
            _ => clone_payload(self),
        }
    }
}

// Keep fieldwise payload cloning out of scalar callers. The exhaustive fallback also remains valid for scalar values.
#[inline(never)]
fn clone_payload(value: &Value) -> Value {
    match value {
        Value::Null => Value::Null,
        Value::Void => Value::Void,
        Value::Bool(value) => Value::Bool(*value),
        Value::Int(value) => Value::Int(*value),
        Value::Float(value) => Value::Float(*value),
        Value::Temporal(value) => Value::Temporal(value.clone()),
        Value::Str(value) => Value::Str(value.clone()),
        Value::FixedChar(value) => Value::FixedChar(value.clone()),
        Value::Bytes(value) => Value::Bytes(value.clone()),
        Value::Decimal(value) => Value::Decimal(value.clone()),
        Value::Json(value) => Value::Json(value.clone()),
        Value::JsonB(value) => Value::JsonB(value.clone()),
        Value::Array(value) => Value::Array(value.clone()),
        Value::LegacyVector(value) => Value::LegacyVector(value.clone()),
        Value::List(value) => Value::List(value.clone()),
        Value::Row(value) => Value::Row(value.clone()),
        Value::Record(value) => Value::Record(value.clone()),
        Value::Map(value) => Value::Map(value.clone()),
        Value::Enum(value) => Value::Enum(value.clone()),
        Value::Datum(value) => Value::Datum(value.clone()),
        Value::BitString(value) => Value::BitString(value.clone()),
    }
}

#[cfg(test)]
mod tests;
