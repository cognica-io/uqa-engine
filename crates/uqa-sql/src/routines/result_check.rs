//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The final statement of a SQL function body checked against the declared result, as `PostgreSQL`'s `check_sql_fn_retval` checks it at creation and before execution.

use super::declaration::RoutineTypeCatalog;
use crate::{
    ast::{ColumnType, CreateFunction, FunctionReturns},
    binding::statements::AnalyzedResult,
    expr::composites,
    type_resolution::canonical_routine_type_name,
    SQLError,
};

mod anonymous;
pub use anonymous::validate_anonymous_record_result;

/// Whether the routine returns nothing, one output value, or the complete output tuple.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SQLFunctionResultKind {
    Void,
    Value,
    Tuple,
}

/// A live attribute in the declared function result, in physical attribute order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SQLFunctionResultColumn {
    pub name: String,
    pub ty: ColumnType,
}

/// The SQL-owned result decision. A lone row value is distinct from a tuple whose individual columns need assignment coercion. Domains remain scalar, even when their base type is composite.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SQLFunctionResultLayout {
    pub kind: SQLFunctionResultKind,
    pub declared_type: ColumnType,
    /// Descriptor of a lone returned row, captured before the statement executes.
    pub source_record: Option<Vec<Option<ColumnType>>>,
    /// Named composite attributes or OUT parameters; absent for scalar and unspecified record results.
    pub columns: Option<Vec<SQLFunctionResultColumn>>,
}

/// Check the final statement without exposing its execution layout to callers that only validate a definition.
pub fn check_sql_function_result(
    types: &dyn RoutineTypeCatalog,
    def: &CreateFunction,
    last: Option<&AnalyzedResult>,
) -> Result<(), SQLError> {
    sql_function_result_layout(types, def, last).map(|_| ())
}

/// Decide whether the final statement returns a single value or its complete row, using the same assignment rules at creation and invocation.
pub fn sql_function_result_layout(
    types: &dyn RoutineTypeCatalog,
    def: &CreateFunction,
    last: Option<&AnalyzedResult>,
) -> Result<SQLFunctionResultLayout, SQLError> {
    let mut layout = declared_sql_function_result(types, def)?;
    if layout.kind == SQLFunctionResultKind::Void {
        return Ok(layout);
    }
    let mismatch = |detail: String| -> Result<SQLError, SQLError> {
        Ok(SQLError::Diagnostic {
            sqlstate: "42P13".into(),
            message: format!(
                "return type mismatch in function declared to return {}",
                types.format_type(&layout.declared_type)?
            ),
            detail: Some(detail),
            hint: None,
        })
    };
    let Some(columns) = last.and_then(AnalyzedResult::column_types) else {
        return Err(mismatch(
            "Function's final statement must be SELECT or INSERT/UPDATE/DELETE/MERGE RETURNING."
                .into(),
        )?);
    };
    if let [actual] = columns {
        layout.source_record = last
            .and_then(|result| result.record_fields(0))
            .map(|fields| fields.to_vec())
            .or(sql_function_composite_columns(types, actual.as_ref())?);
    }
    if layout.kind == SQLFunctionResultKind::Value {
        let [actual] = columns else {
            return Err(mismatch(
                "Final statement must return exactly one column.".into(),
            )?);
        };
        if let Some(actual) = actual {
            if !crate::assignment_type_compatible(actual, &layout.declared_type) {
                return Err(mismatch(format!(
                    "Actual return type is {}.",
                    types.format_type(actual)?
                ))?);
            }
        }
        return Ok(layout);
    }
    if !def.is_procedure {
        if let [Some(actual)] = columns {
            if crate::assignment_type_compatible(actual, &layout.declared_type) {
                layout.kind = SQLFunctionResultKind::Value;
                if matches!(actual, ColumnType::Record)
                    && matches!(layout.declared_type, ColumnType::Composite(_))
                {
                    if let Some(source) = &layout.source_record {
                        check_composite_assignment(types, source, &layout)?;
                    }
                }
                return Ok(layout);
            }
        }
    }
    if let Some(expected) = &layout.columns {
        for (position, actual) in columns.iter().enumerate() {
            let Some(expected) = expected.get(position) else {
                return Err(mismatch(
                    "Final statement returns too many columns.".into(),
                )?);
            };
            if let Some(actual) = actual {
                if !crate::assignment_type_compatible(actual, &expected.ty) {
                    return Err(mismatch(format!(
                        "Final statement returns {} instead of {} at column {}.",
                        types.format_type(actual)?,
                        types.format_type(&expected.ty)?,
                        position + 1
                    ))?);
                }
            }
        }
        if columns.len() < expected.len() {
            return Err(mismatch("Final statement returns too few columns.".into())?);
        }
    }
    Ok(layout)
}

/// Resolve the result declaration independently of a final statement.
pub fn declared_sql_function_result(
    types: &dyn RoutineTypeCatalog,
    def: &CreateFunction,
) -> Result<SQLFunctionResultLayout, SQLError> {
    let outputs = def.output_params();
    if def.is_procedure || outputs.len() > 1 {
        let columns = outputs
            .iter()
            .map(|parameter| {
                Ok(SQLFunctionResultColumn {
                    name: parameter.name.clone(),
                    ty: types.resolve_catalog_column_type_name(&parameter.type_name)?,
                })
            })
            .collect::<Result<Vec<_>, SQLError>>()?;
        return Ok(SQLFunctionResultLayout {
            kind: if columns.is_empty() {
                SQLFunctionResultKind::Void
            } else {
                SQLFunctionResultKind::Tuple
            },
            declared_type: ColumnType::Record,
            source_record: None,
            columns: Some(columns),
        });
    }
    let type_name = match (outputs.first(), &def.returns) {
        (Some(parameter), _) => parameter.type_name.as_str(),
        (None, FunctionReturns::Scalar { type_name } | FunctionReturns::SetOf { type_name }) => {
            type_name.as_str()
        }
        (None, FunctionReturns::None | FunctionReturns::Table) => "void",
    };
    let declared_type = match canonical_routine_type_name(type_name).as_str() {
        "void" => ColumnType::Void,
        "record" => ColumnType::Record,
        _ => types.resolve_catalog_column_type_name(type_name)?,
    };
    let (kind, columns) = match &declared_type {
        ColumnType::Void => (SQLFunctionResultKind::Void, None),
        ColumnType::Record => (SQLFunctionResultKind::Tuple, None),
        ColumnType::Composite(reference) => {
            let descriptor = composites::descriptor(types.composite_types(), reference.oid)?;
            let columns = descriptor
                .attributes
                .iter()
                .map(|attribute| SQLFunctionResultColumn {
                    name: attribute.name.clone(),
                    ty: attribute.ty.clone(),
                })
                .collect();
            (SQLFunctionResultKind::Tuple, Some(columns))
        }
        _ => (SQLFunctionResultKind::Value, None),
    };
    Ok(SQLFunctionResultLayout {
        kind,
        declared_type,
        source_record: None,
        columns,
    })
}

fn check_composite_assignment(
    types: &dyn RoutineTypeCatalog,
    source: &[Option<ColumnType>],
    layout: &SQLFunctionResultLayout,
) -> Result<(), SQLError> {
    let target = layout.columns.as_deref().unwrap_or_default();
    let detail = match source.len().cmp(&target.len()) {
        std::cmp::Ordering::Less => Some("Input has too few columns.".into()),
        std::cmp::Ordering::Greater => Some("Input has too many columns.".into()),
        std::cmp::Ordering::Equal => source
            .iter()
            .zip(target)
            .enumerate()
            .find_map(|(index, (source, target))| {
                source
                    .as_ref()
                    .filter(|source| !crate::assignment_type_compatible(source, &target.ty))
                    .map(|source| {
                        Ok::<_, SQLError>(format!(
                            "Cannot cast type {} to {} in column {}.",
                            types.format_type(source)?,
                            types.format_type(&target.ty)?,
                            index + 1
                        ))
                    })
            })
            .transpose()?,
    };
    if let Some(detail) = detail {
        return Err(SQLError::Diagnostic {
            sqlstate: "42846".into(),
            message: format!(
                "cannot cast type record to {}",
                types.format_type(&layout.declared_type)?
            ),
            detail: Some(detail),
            hint: None,
        });
    }
    Ok(())
}

/// Read the live descriptor of a named row value. An anonymous row's descriptor must come from expression analysis, rather than inspecting its values.
pub fn sql_function_composite_columns(
    types: &dyn RoutineTypeCatalog,
    source: Option<&ColumnType>,
) -> Result<Option<Vec<Option<ColumnType>>>, SQLError> {
    match source {
        Some(ColumnType::Composite(reference)) => {
            let descriptor = composites::descriptor(types.composite_types(), reference.oid)?;
            Ok(Some(
                descriptor
                    .attributes
                    .iter()
                    .map(|attribute| Some(attribute.ty.clone()))
                    .collect(),
            ))
        }
        Some(ColumnType::Domain { base, .. }) => sql_function_composite_columns(types, Some(base)),
        _ => Ok(None),
    }
}

/// A whole returned record must match the caller's tuple descriptor; its fields are not assignment-coerced as independently selected columns are.
pub fn validate_sql_function_record(
    types: &dyn RoutineTypeCatalog,
    source: &[Option<ColumnType>],
    target: &[ColumnType],
) -> Result<(), SQLError> {
    if source.len() != target.len() {
        return Err(record_mismatch(format!(
            "Returned row contains {} attribute{}, but query expects {}.",
            source.len(),
            if source.len() == 1 { "" } else { "s" },
            target.len()
        )));
    }
    for (index, (source, target)) in source.iter().zip(target).enumerate() {
        let Some(source) = source else {
            return Err(record_mismatch(format!(
                "Returned type unknown at ordinal position {}, but query expects {}.",
                index + 1,
                types.format_type(target)?
            )));
        };
        let source_oid = crate::catalog::type_metadata::pg_type_oid(source);
        let target_oid = crate::catalog::type_metadata::pg_type_oid(target);
        let source_modifier = crate::catalog::type_metadata::pg_type_modifier(source);
        let target_modifier = crate::catalog::type_metadata::pg_type_modifier(target);
        if source_oid != target_oid || (target_modifier >= 0 && source_modifier != target_modifier)
        {
            return Err(record_mismatch(format!(
                "Returned type {} at ordinal position {}, but query expects {}.",
                types.format_type(source)?,
                index + 1,
                types.format_type(target)?
            )));
        }
    }
    Ok(())
}

fn record_mismatch(detail: String) -> SQLError {
    SQLError::Diagnostic {
        sqlstate: "42804".into(),
        message: "function return row and query-specified return row do not match".into(),
        detail: Some(detail),
        hint: None,
    }
}

/// A SQL set-returning function materializes one record descriptor for all non-null rows before the caller checks the declared result. Inspect borrowed values without copying their fields.
pub fn validate_sql_function_record_rows(result: &crate::SQLResult) -> Result<(), SQLError> {
    let mut descriptor = None;
    for index in 0..result.rows.len() {
        if let Some(uqa_core::Value::Row(row)) = result.value_at(index, 0) {
            if let Some(fields) = row.field_types() {
                if descriptor.is_some_and(|previous| previous != fields) {
                    return Err(SQLError::Routine {
                        sqlstate: "42804".into(),
                        message: "rows returned by function are not all of the same row type"
                            .into(),
                    });
                }
                descriptor = Some(fields);
            }
        }
    }
    Ok(())
}

/// Validate the descriptor carried by the selected runtime row, including CASE branches and materialized intermediate rows whose record shapes differ.
pub fn validate_sql_function_record_identity(
    types: &dyn RoutineTypeCatalog,
    source: &[uqa_core::RecordFieldType],
    target: &[ColumnType],
) -> Result<(), SQLError> {
    if source.len() != target.len() {
        return Err(record_mismatch(format!(
            "Returned row contains {} attribute{}, but query expects {}.",
            source.len(),
            if source.len() == 1 { "" } else { "s" },
            target.len(),
        )));
    }
    for (index, (source, target)) in source.iter().zip(target).enumerate() {
        let target_modifier = crate::catalog::type_metadata::pg_type_modifier(target);
        if i64::from(source.oid) == crate::catalog::type_metadata::pg_type_oid(target)
            && (target_modifier < 0 || i64::from(source.type_modifier) == target_modifier)
        {
            continue;
        }
        let source_name = if source.oid == 705 {
            "unknown".into()
        } else {
            types.format_type_oid(source.oid)?
        };
        return Err(record_mismatch(format!(
            "Returned type {source_name} at ordinal position {}, but query expects {}.",
            index + 1,
            types.format_type(target)?,
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
