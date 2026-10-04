//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The final statement of a SQL function body checked against the declared result, as `PostgreSQL`'s `check_sql_fn_retval` checks it whenever it analyzes the body: at creation under `check_function_bodies`, and before the final statement runs when the function is called.

use super::declaration::RoutineTypeCatalog;
use crate::{
    ast::{ColumnType, CreateFunction, FunctionReturns},
    binding::statements::AnalyzedResult,
    type_resolution::canonical_routine_type_name,
    SQLError,
};

/// The result a SQL function's final statement must produce.
enum DeclaredResult {
    /// `void`, or a procedure without output parameters: the body may end with any statement.
    Void,
    /// One value of a base, domain, enum, range or array type.
    Scalar(ColumnType),
    /// A row: the types of the output parameters, or `None` for `record` without them, whose columns the call defines.
    Row(Option<Vec<ColumnType>>),
}

/// Check what the analysis of the final statement of `def`'s body derives, or `None` for an empty body, against the declared result.
pub fn check_sql_function_result(
    types: &dyn RoutineTypeCatalog,
    def: &CreateFunction,
    last: Option<&AnalyzedResult>,
) -> Result<(), SQLError> {
    let (declared, declared_type) = declared_result(types, def)?;
    if matches!(declared, DeclaredResult::Void) {
        return Ok(());
    }
    let mismatch = |detail: String| -> Result<SQLError, SQLError> {
        Ok(SQLError::Diagnostic {
            sqlstate: "42P13".into(),
            message: format!(
                "return type mismatch in function declared to return {}",
                types.format_type(&declared_type)?
            ),
            detail: Some(detail),
            hint: None,
        })
    };
    let Some(AnalyzedResult::Rows(columns)) = last else {
        return Err(mismatch(
            "Function's final statement must be SELECT or INSERT/UPDATE/DELETE/MERGE RETURNING."
                .into(),
        )?);
    };
    match declared {
        DeclaredResult::Void => Ok(()),
        DeclaredResult::Scalar(expected) => {
            let [actual] = columns.as_slice() else {
                return Err(mismatch(
                    "Final statement must return exactly one column.".into(),
                )?);
            };
            match actual {
                Some(actual) if !crate::assignment_type_compatible(actual, &expected) => {
                    Err(mismatch(format!(
                        "Actual return type is {}.",
                        types.format_type(actual)?
                    ))?)
                }
                _ => Ok(()),
            }
        }
        DeclaredResult::Row(expected) => {
            // A lone column that is itself a row is the whole result; a procedure's output parameters are always assigned column by column.
            if let [Some(ColumnType::Record)] = columns.as_slice() {
                if !def.is_procedure {
                    return Ok(());
                }
            }
            let Some(expected) = expected else {
                return Ok(());
            };
            for (position, actual) in columns.iter().enumerate() {
                let Some(expected) = expected.get(position) else {
                    return Err(mismatch(
                        "Final statement returns too many columns.".into(),
                    )?);
                };
                if let Some(actual) = actual {
                    if !crate::assignment_type_compatible(actual, expected) {
                        return Err(mismatch(format!(
                            "Final statement returns {} instead of {} at column {}.",
                            types.format_type(actual)?,
                            types.format_type(expected)?,
                            position + 1
                        ))?);
                    }
                }
            }
            if columns.len() < expected.len() {
                return Err(mismatch("Final statement returns too few columns.".into())?);
            }
            Ok(())
        }
    }
}

/// The declared result of `def` and the type its messages name: the type of a lone output parameter, `record` for several, or the `RETURNS` type.
fn declared_result(
    types: &dyn RoutineTypeCatalog,
    def: &CreateFunction,
) -> Result<(DeclaredResult, ColumnType), SQLError> {
    let outputs = def.output_params();
    if def.is_procedure || outputs.len() > 1 {
        if outputs.is_empty() {
            return Ok((DeclaredResult::Void, ColumnType::Void));
        }
        let columns = outputs
            .iter()
            .map(|parameter| types.resolve_catalog_column_type_name(&parameter.type_name))
            .collect::<Result<Vec<_>, _>>()?;
        return Ok((DeclaredResult::Row(Some(columns)), ColumnType::Record));
    }
    let type_name = match (outputs.first(), &def.returns) {
        (Some(parameter), _) => parameter.type_name.as_str(),
        (None, FunctionReturns::Scalar { type_name } | FunctionReturns::SetOf { type_name }) => {
            type_name.as_str()
        }
        (None, FunctionReturns::None | FunctionReturns::Table) => {
            return Ok((DeclaredResult::Void, ColumnType::Void));
        }
    };
    match canonical_routine_type_name(type_name).as_str() {
        "void" => Ok((DeclaredResult::Void, ColumnType::Void)),
        "record" => Ok((DeclaredResult::Row(None), ColumnType::Record)),
        _ => {
            let declared = types.resolve_catalog_column_type_name(type_name)?;
            Ok((DeclaredResult::Scalar(declared.clone()), declared))
        }
    }
}

#[cfg(test)]
mod tests;
