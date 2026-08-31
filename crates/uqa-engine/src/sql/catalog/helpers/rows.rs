//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog value and row construction.

use uqa_core::{ArrayValue, Value};
use uqa_sql::{ResultRow, SQLError};

pub(in crate::sql::catalog) fn catalog_name() -> Value {
    Value::Str("uqa".into())
}

pub(in crate::sql::catalog) fn catalog_usize(value: usize, label: &str) -> Result<i64, SQLError> {
    i64::try_from(value).map_err(|_| {
        SQLError::Internal(format!(
            "{label} exceeds the SQL catalog BIGINT representation"
        ))
    })
}

pub(in crate::sql::catalog) fn catalog_ordinal(index: usize, label: &str) -> Result<i64, SQLError> {
    let ordinal = index
        .checked_add(1)
        .ok_or_else(|| SQLError::Internal(format!("{label} ordinal overflow")))?;
    catalog_usize(ordinal, label)
}

pub(in crate::sql::catalog) fn str_value(value: impl Into<String>) -> Value {
    Value::Str(value.into())
}

pub(in crate::sql::catalog) fn int_value(value: i64) -> Value {
    Value::Int(value)
}

pub(in crate::sql::catalog) fn bool_value(value: bool) -> Value {
    Value::Bool(value)
}

pub(in crate::sql::catalog) fn list_int(values: &[i64]) -> Value {
    Value::List(values.iter().copied().map(Value::Int).collect())
}

pub(in crate::sql::catalog) fn catalog_array(
    values: Vec<Value>,
    label: &str,
) -> Result<Value, SQLError> {
    ArrayValue::try_new(values)
        .map(Value::Array)
        .ok_or_else(|| SQLError::Internal(format!("{label} has non-rectangular dimensions")))
}

pub(in crate::sql::catalog) fn row(
    entries: impl IntoIterator<Item = (&'static str, Value)>,
) -> ResultRow {
    let mut out = ResultRow::new();
    for (key, value) in entries {
        out.insert(key.to_string(), value);
    }
    out
}
