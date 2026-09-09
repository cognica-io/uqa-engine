//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Static cast compatibility that depends on declared SQL type identity.

use uqa_sql::ast::ColumnType;
use uqa_sql::SQLError;

use super::common::base_type;

/// Whether `PostgreSQL` assignment coercion accepts the declared source and target.
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

pub(super) fn validate_void_cast(
    source: Option<&ColumnType>,
    target: &ColumnType,
) -> Result<(), SQLError> {
    let target = base_type(target);
    if matches!(target, ColumnType::Void) {
        if source.is_none_or(|source| {
            let source = base_type(source);
            matches!(source, ColumnType::Void) || is_string_io_type(source)
        }) {
            return Ok(());
        }
        return Err(undefined_cast(
            source.expect("known non-string source checked above"),
            target,
        ));
    }
    if source.is_some_and(|source| matches!(base_type(source), ColumnType::Void))
        && !is_string_io_type(target)
    {
        return Err(undefined_cast(
            source.expect("void source checked above"),
            target,
        ));
    }
    Ok(())
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
