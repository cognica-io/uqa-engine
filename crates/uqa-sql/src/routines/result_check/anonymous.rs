//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Caller-defined record columns retain the routine's original result semantics.

use super::{
    validate_sql_function_record, ColumnType, RoutineTypeCatalog, SQLError,
    SQLFunctionResultColumn, SQLFunctionResultKind,
};

/// SQL target lists assign each column; a whole SQL or procedural record keeps its producer's field identities.
pub fn validate_anonymous_record_result(
    types: &dyn RoutineTypeCatalog,
    source: &[Option<ColumnType>],
    target: &[SQLFunctionResultColumn],
    sql_kind: Option<SQLFunctionResultKind>,
) -> Result<(), SQLError> {
    if sql_kind == Some(SQLFunctionResultKind::Value) {
        return validate_sql_function_record(
            types,
            source,
            &target
                .iter()
                .map(|column| column.ty.clone())
                .collect::<Vec<_>>(),
        );
    }
    let tuple = sql_kind == Some(SQLFunctionResultKind::Tuple);
    if source.len() != target.len() {
        let detail = if tuple {
            format!(
                "Final statement returns too {} columns.",
                if source.len() < target.len() {
                    "few"
                } else {
                    "many"
                }
            )
        } else {
            format!(
                "Number of returned columns ({}) does not match expected column count ({}).",
                source.len(),
                target.len()
            )
        };
        return Err(mismatch(tuple, detail));
    }
    for (index, (source, target)) in source.iter().zip(target).enumerate() {
        if tuple
            && source
                .as_ref()
                .is_none_or(|source| crate::assignment_type_compatible(source, &target.ty))
        {
            continue;
        }
        if !tuple
            && source
                .as_ref()
                .is_some_and(|source| same_identity(source, &target.ty))
        {
            continue;
        }
        let source = source
            .as_ref()
            .map_or_else(|| Ok("unknown".into()), |source| types.format_type(source))?;
        let expected = types.format_type(&target.ty)?;
        let detail = if tuple {
            format!(
                "Final statement returns {source} instead of {expected} at column {}.",
                index + 1
            )
        } else {
            format!("Returned type {source} does not match expected type {expected} in column \"{}\" (position {}).", target.name, index + 1)
        };
        return Err(mismatch(tuple, detail));
    }
    Ok(())
}

fn same_identity(source: &ColumnType, target: &ColumnType) -> bool {
    use crate::catalog::type_metadata::{pg_type_modifier, pg_type_oid};
    pg_type_oid(source) == pg_type_oid(target)
        && (pg_type_modifier(target) < 0 || pg_type_modifier(source) == pg_type_modifier(target))
}

fn mismatch(tuple: bool, detail: String) -> SQLError {
    SQLError::Diagnostic {
        sqlstate: if tuple { "42P13" } else { "42804" }.into(),
        message: if tuple {
            "return type mismatch in function declared to return record"
        } else {
            "returned record type does not match expected record type"
        }
        .into(),
        detail: Some(detail),
        hint: None,
    }
}
