//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `check_sql_stmt_retval` for SQL-standard bodies: when the routine is defined, its final statement must return what the routine declares, each column coercible to its declared type by assignment.

use crate::ast::{ColumnType, CreateFunction, FunctionParamMode, FunctionReturns};
use crate::type_resolution::assignment_type_compatible;
use crate::{RowSchema, SQLError};

use super::declaration::RoutineTypeCatalog;

/// What a routine returns, as `get_func_result_type` describes it.
enum DeclaredResult {
    Void,
    /// One value of a base, domain, enum or range type, including a lone output parameter; the name is the type's as `format_type_be` prints it.
    Scalar(Option<ColumnType>, String),
    /// A row of the output parameters' types, or of any shape for `record`.
    Row(Option<Vec<Option<ColumnType>>>),
}

/// Check the columns the final statement returns (`None` when it returns none) against the routine's declared result.
pub(super) fn check_final_statement_result(
    types: &dyn RoutineTypeCatalog,
    def: &CreateFunction,
    output: Option<&RowSchema>,
) -> Result<(), SQLError> {
    let declared = declared_result(types, def);
    let declared_name = match &declared {
        DeclaredResult::Void => return Ok(()),
        DeclaredResult::Scalar(_, name) => name.clone(),
        DeclaredResult::Row(_) => "record".into(),
    };
    let mismatch = |detail: String| SQLError::Diagnostic {
        sqlstate: "42P13".into(),
        message: format!("return type mismatch in function declared to return {declared_name}"),
        detail: Some(detail),
        hint: None,
    };
    let Some(output) = output else {
        return Err(mismatch(
            "Function's final statement must be SELECT or INSERT/UPDATE/DELETE/MERGE RETURNING."
                .into(),
        ));
    };
    let returned = output.column_types();
    match declared {
        DeclaredResult::Void | DeclaredResult::Row(None) => Ok(()),
        DeclaredResult::Scalar(expected, _) => {
            let [actual] = returned else {
                return Err(mismatch(
                    "Final statement must return exactly one column.".into(),
                ));
            };
            match (actual, expected) {
                (Some(actual), Some(expected))
                    if !assignment_type_compatible(actual, &expected) =>
                {
                    Err(mismatch(format!(
                        "Actual return type is {}.",
                        actual.regtype_name()
                    )))
                }
                _ => Ok(()),
            }
        }
        DeclaredResult::Row(Some(expected)) => {
            // One column that is itself a row can be the whole result, except for a procedure, whose CALL reads its output parameters.
            if let [Some(ColumnType::Record)] = returned {
                if !def.is_procedure {
                    return Ok(());
                }
            }
            for (index, actual) in returned.iter().enumerate() {
                let Some(expected) = expected.get(index) else {
                    return Err(mismatch("Final statement returns too many columns.".into()));
                };
                if let (Some(actual), Some(expected)) = (actual, expected) {
                    if !assignment_type_compatible(actual, expected) {
                        return Err(mismatch(format!(
                            "Final statement returns {} instead of {} at column {}.",
                            actual.regtype_name(),
                            expected.regtype_name(),
                            index + 1
                        )));
                    }
                }
            }
            if returned.len() < expected.len() {
                return Err(mismatch("Final statement returns too few columns.".into()));
            }
            Ok(())
        }
    }
}

fn declared_result(types: &dyn RoutineTypeCatalog, def: &CreateFunction) -> DeclaredResult {
    let resolve = |name: &str| {
        types
            .resolve_catalog_column_type(name)
            .or_else(|| ColumnType::from_sql_name(name).ok())
    };
    let outputs = def
        .params
        .iter()
        .filter(|parameter| {
            matches!(
                parameter.mode,
                FunctionParamMode::Out | FunctionParamMode::InOut | FunctionParamMode::Table
            )
        })
        .collect::<Vec<_>>();
    // Procedures with output parameters always return a row; a function with one returns its type.
    match (def.is_procedure, outputs.as_slice()) {
        (true, []) => return DeclaredResult::Void,
        (false, [output]) => {
            let ty = resolve(&output.type_name);
            let name = ty
                .as_ref()
                .map_or_else(|| output.type_name.clone(), ColumnType::regtype_name);
            return DeclaredResult::Scalar(ty, name);
        }
        (_, []) => {}
        (_, outputs) => {
            return DeclaredResult::Row(Some(
                outputs
                    .iter()
                    .map(|output| resolve(&output.type_name))
                    .collect(),
            ))
        }
    }
    let (FunctionReturns::Scalar { type_name } | FunctionReturns::SetOf { type_name }) =
        &def.returns
    else {
        return DeclaredResult::Row(None);
    };
    match resolve(type_name) {
        Some(ColumnType::Void) => DeclaredResult::Void,
        Some(ColumnType::Record) => DeclaredResult::Row(None),
        ty => {
            let name = ty
                .as_ref()
                .map_or_else(|| type_name.clone(), ColumnType::regtype_name);
            DeclaredResult::Scalar(ty, name)
        }
    }
}
