//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Source qualifier filters and checked SQL integer carriers.

use crate::{plan::source_projection::QualifierFilters, SQLError, ScalarExpr};
use uqa_core::Value;

pub fn checked_integer_value<T>(value: T, label: &str) -> Result<Value, SQLError>
where
    T: Copy + std::fmt::Display,
    i64: TryFrom<T>,
{
    i64::try_from(value).map(Value::Int).map_err(|_| {
        SQLError::TypeMismatch(format!("{label} {value} exceeds the SQL BIGINT range"))
    })
}

pub fn qualifier_for(qualifier: &str, alias: Option<&str>) -> String {
    alias.unwrap_or(qualifier).to_string()
}

pub fn has_filters_for_qualifier(filters: Option<&QualifierFilters>, qual: &str) -> bool {
    filters
        .and_then(|filters| filters.get(qual))
        .is_some_and(|filters| !filters.is_empty())
}

pub fn combine_filters(filters: impl IntoIterator<Item = ScalarExpr>) -> Option<ScalarExpr> {
    let mut filters: Vec<ScalarExpr> = filters.into_iter().collect();
    if filters.len() == 1 {
        filters.pop()
    } else if filters.is_empty() {
        None
    } else {
        Some(ScalarExpr::And(filters))
    }
}

pub fn qualifier_filter(filters: Option<&QualifierFilters>, qualifier: &str) -> Option<ScalarExpr> {
    filters
        .and_then(|filters| filters.get(qualifier))
        .filter(|filters| !filters.is_empty())
        .and_then(|filters| combine_filters(filters.iter().cloned()))
}
