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

#[derive(Clone, Copy, PartialEq, Eq)]
enum RecordResultContext {
    SQLTuple,
    Procedural,
    Trigger,
}

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
    validate_record_fields(
        types,
        source,
        target,
        if sql_kind == Some(SQLFunctionResultKind::Tuple) {
            RecordResultContext::SQLTuple
        } else {
            RecordResultContext::Procedural
        },
    )
}

/// A trigger returns its relation's exact descriptor; validating the fields does not coerce their values again.
pub fn validate_trigger_record_result(
    types: &dyn RoutineTypeCatalog,
    source: &[Option<ColumnType>],
    target: &[SQLFunctionResultColumn],
) -> Result<(), SQLError> {
    validate_record_fields(types, source, target, RecordResultContext::Trigger)
}

/// An anonymous procedural row carries OIDs and typmods even when its descriptor was selected at runtime.
pub fn validate_procedural_record_identity(
    types: &dyn RoutineTypeCatalog,
    source: &[uqa_core::RecordFieldType],
    target: &[SQLFunctionResultColumn],
) -> Result<(), SQLError> {
    let context = RecordResultContext::Procedural;
    validate_width(source.len(), target.len(), context)?;
    for (index, (source, target)) in source.iter().zip(target).enumerate() {
        use crate::catalog::type_metadata::{pg_type_modifier, pg_type_oid};
        if i64::from(source.oid) == pg_type_oid(&target.ty)
            && (pg_type_modifier(&target.ty) < 0
                || i64::from(source.type_modifier) == pg_type_modifier(&target.ty))
        {
            continue;
        }
        let source = types.format_type_oid(source.oid)?;
        let expected = types.format_type(&target.ty)?;
        return Err(mismatch(
            context,
            format!(
                "Returned type {source} does not match expected type {expected} in column \"{}\" (position {}).",
                target.name,
                index + 1
            ),
        ));
    }
    Ok(())
}

fn validate_width(
    source: usize,
    target: usize,
    context: RecordResultContext,
) -> Result<(), SQLError> {
    if source == target {
        return Ok(());
    }
    let detail = if context == RecordResultContext::SQLTuple {
        format!(
            "Final statement returns too {} columns.",
            if source < target { "few" } else { "many" }
        )
    } else {
        format!(
            "Number of returned columns ({source}) does not match expected column count ({target})."
        )
    };
    Err(mismatch(context, detail))
}

fn validate_record_fields(
    types: &dyn RoutineTypeCatalog,
    source: &[Option<ColumnType>],
    target: &[SQLFunctionResultColumn],
    context: RecordResultContext,
) -> Result<(), SQLError> {
    let tuple = context == RecordResultContext::SQLTuple;
    validate_width(source.len(), target.len(), context)?;
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
            format!(
                "Returned type {source} does not match expected type {expected} in column \"{}\" (position {}).",
                target.name,
                index + 1
            )
        };
        return Err(mismatch(context, detail));
    }
    Ok(())
}

fn same_identity(source: &ColumnType, target: &ColumnType) -> bool {
    use crate::catalog::type_metadata::{pg_type_modifier, pg_type_oid};
    pg_type_oid(source) == pg_type_oid(target)
        && (pg_type_modifier(target) < 0 || pg_type_modifier(source) == pg_type_modifier(target))
}

fn mismatch(context: RecordResultContext, detail: String) -> SQLError {
    SQLError::Diagnostic {
        sqlstate: if context == RecordResultContext::SQLTuple {
            "42P13"
        } else {
            "42804"
        }
        .into(),
        message: match context {
            RecordResultContext::SQLTuple => {
                "return type mismatch in function declared to return record"
            }
            RecordResultContext::Procedural => {
                "returned record type does not match expected record type"
            }
            RecordResultContext::Trigger => {
                "returned row structure does not match the structure of the triggering table"
            }
        }
        .into(),
        detail: Some(detail),
        hint: None,
    }
}
