//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Typed native expectations for `PostgreSQL` catalog-vector columns.

use uqa_core::{LegacyVectorKind, LegacyVectorValue, Value};

pub(crate) fn int2vector(values: Vec<Value>) -> Value {
    Value::LegacyVector(LegacyVectorValue::try_new(LegacyVectorKind::SmallInteger, values).unwrap())
}

pub(crate) fn oidvector(values: Vec<Value>) -> Value {
    Value::LegacyVector(LegacyVectorValue::try_new(LegacyVectorKind::Oid, values).unwrap())
}
