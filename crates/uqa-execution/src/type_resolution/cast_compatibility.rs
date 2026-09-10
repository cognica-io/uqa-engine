//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Static cast compatibility that depends on declared SQL type identity.

mod catalog;

use uqa_sql::ast::ColumnType;
use uqa_sql::SQLError;

use super::common::base_type;

/// Whether an explicit SQL cast has a `PostgreSQL` coercion path, independently of its value. NULL input does not make an otherwise missing cast valid.
#[must_use]
pub fn explicit_type_compatible(source: &ColumnType, target: &ColumnType) -> bool {
    let source = base_type(source).without_type_modifiers();
    let target = base_type(target).without_type_modifiers();
    if source == target {
        return true;
    }
    if catalog::context(&cast_catalog_name(&source), &cast_catalog_name(&target)).is_some() {
        return true;
    }
    if let ColumnType::Array(target) = &target {
        let source = match &source {
            ColumnType::Array(source) => Some(source.as_ref()),
            ColumnType::Int2Vector => Some(&ColumnType::SmallInteger),
            ColumnType::OidVector => Some(&ColumnType::Oid),
            _ => None,
        };
        if let Some(source) = source {
            return explicit_type_compatible(source, target);
        }
    }
    is_string_io_type(&source) || is_string_io_type(&target)
}

fn cast_catalog_name(ty: &ColumnType) -> String {
    match ty {
        ColumnType::InternalChar => "char".into(),
        _ => super::canonical_column_type_name(ty),
    }
}

pub(super) fn validate_explicit_cast(
    source: Option<&ColumnType>,
    target: &ColumnType,
) -> Result<(), SQLError> {
    if let Some(source) = source {
        if !explicit_type_compatible(source, target) {
            return Err(undefined_cast(source, target));
        }
    }
    Ok(())
}

/// Whether ``PostgreSQL`` assignment coercion accepts the declared source and target.
/// Domains use their base type for cast selection and retain constraint checking
/// at the value conversion boundary; array coercion applies element by element.
#[must_use]
pub fn assignment_type_compatible(source: &ColumnType, target: &ColumnType) -> bool {
    let source = base_type(source).without_type_modifiers();
    let target = base_type(target).without_type_modifiers();
    if source == target || is_string_io_type(&target) {
        return true;
    }
    if let (ColumnType::Array(source), ColumnType::Array(target)) = (&source, &target) {
        return assignment_type_compatible(source, target);
    }
    let source = super::canonical_column_type_name(&source);
    let target = super::canonical_column_type_name(&target);
    if super::routine_type_accepts_implicit_cast(&source, &target) {
        return true;
    }
    let numeric = |name: &str| {
        matches!(
            name,
            "int2" | "int4" | "int8" | "float4" | "float8" | "numeric"
        )
    };
    (numeric(&source) && numeric(&target))
        || matches!(
            (source.as_str(), target.as_str()),
            ("timestamp", "date" | "time")
                | ("timestamptz", "date" | "time" | "timetz" | "timestamp")
                | ("timetz" | "interval", "time")
                | ("json", "jsonb")
                | ("jsonb", "json")
                | ("bit", "varbit")
                | ("varbit", "bit")
                | (
                    "oid" | "regclass" | "regnamespace" | "regproc" | "regrole" | "regtype",
                    "int4" | "int8"
                )
        )
}

fn is_string_io_type(ty: &ColumnType) -> bool {
    matches!(
        ty,
        ColumnType::Text
            | ColumnType::Name
            | ColumnType::Varchar(_)
            | ColumnType::Bpchar
            | ColumnType::Character(_)
    )
}

fn undefined_cast(source: &ColumnType, target: &ColumnType) -> SQLError {
    SQLError::Routine {
        sqlstate: "42846".into(),
        message: format!(
            "cannot cast type {} to {}",
            source.sql_name(),
            target.sql_name()
        ),
    }
}
