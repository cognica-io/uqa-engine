//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Invoke scalar, table, and procedure routines and materialize their physical results.
use super::{
    context::RoutineInvocationContext,
    execution::execute_routine,
    resolution::{resolve_bound_routine, resolve_routine, ResolvedRoutine},
};
use crate::{
    functions::SQLTableFunctionResult, routines::transaction::nonatomic_routine_entry_allowed,
};
use uqa_core::Value;
use uqa_sql::{
    ast::FunctionBinding,
    routines::{
        invocation::{
            anonymous_record_shape_error, call_output_schema, call_signature,
            coerce_anonymous_record_value, output_column_names, routine_resolution_error,
            runtime_record_column_type,
        },
        result_check::{
            validate_anonymous_record_result, SQLFunctionResultColumn, SQLFunctionResultKind,
        },
        routine_local_name,
    },
    ResultRow, SQLError, SQLResult,
};
pub fn run_call(
    context: &RoutineInvocationContext<'_>,
    name: &str,
    call_args: &[(Option<String>, Value)],
    argument_types: &[Option<uqa_sql::ast::ColumnType>],
    explicit_variadic: bool,
    nested_statement: bool,
) -> Result<SQLResult, SQLError> {
    let function = match resolve_routine(
        context,
        name,
        call_args,
        Some(argument_types),
        "procedure",
        explicit_variadic,
    )? {
        Some(resolved) => resolved,
        None => {
            return Err(routine_resolution_error(
                "procedure",
                name,
                call_args,
                "does not exist",
            ));
        }
    };
    let ResolvedRoutine {
        function,
        bound,
        invocation,
    } = function;
    if !function.def.is_procedure {
        return Err(SQLError::Routine {
            sqlstate: "42809".into(),
            message: format!("{} is not a procedure", call_signature(name, call_args)),
        });
    }
    let outcome = execute_routine(
        context,
        &function,
        bound,
        &invocation,
        nonatomic_routine_entry_allowed(context.runtime.session, nested_statement),
        None,
    )?;
    let Some(schema) =
        call_output_schema(context.types, &function.def, &invocation.parameter_types)?
    else {
        return Ok(SQLResult::empty());
    };
    let columns = schema.columns().to_vec();
    let column_types = schema.column_types().to_vec();
    let mut row = ResultRow::new();
    for (column, value) in columns.iter().zip(outcome.out_values.iter()) {
        row.insert(column.clone(), value.clone());
    }
    Ok(SQLResult {
        kind: uqa_sql::SQLResultKind::Rows,
        command_tag: None,
        column_types,
        columns,
        rows: vec![row],
        positional_rows: None,
        affected_rows: 0,
    })
}

/// Scalar-context invocation used by the expression evaluator's
/// context hook. `None` means no routine with this name exists.
pub fn call_user_scalar_function(
    context: &RoutineInvocationContext<'_>,
    name: &str,
    args: &[(Option<String>, Value)],
) -> Option<Result<Value, SQLError>> {
    let resolved = match resolve_routine(context, name, args, None, "function", false) {
        Ok(Some(resolved)) => resolved,
        Ok(None) => return None,
        Err(e) => return Some(Err(e)),
    };
    Some(execute_resolved_scalar_function(
        context, name, args, resolved,
    ))
}

pub fn call_bound_user_scalar_function(
    context: &RoutineInvocationContext<'_>,
    binding: &FunctionBinding,
    args: &[(Option<String>, Value)],
) -> Option<Result<Value, SQLError>> {
    let resolved = match resolve_bound_routine(context, binding, args) {
        Ok(Some(resolved)) => resolved,
        Ok(None) => return None,
        Err(error) => return Some(Err(error)),
    };
    Some(execute_resolved_scalar_function(
        context,
        &binding.name,
        args,
        resolved,
    ))
}

fn execute_resolved_scalar_function(
    context: &RoutineInvocationContext<'_>,
    name: &str,
    args: &[(Option<String>, Value)],
    resolved: ResolvedRoutine,
) -> Result<Value, SQLError> {
    let ResolvedRoutine {
        function,
        bound,
        invocation,
    } = resolved;
    if function.def.is_procedure {
        return Err(SQLError::Routine {
            sqlstate: "42809".into(),
            message: format!("{} is a procedure", call_signature(name, args)),
        });
    }
    if function.def.returns_set() {
        return Err(SQLError::Routine {
            sqlstate: "0A000".into(),
            message: "set-valued function called in context that cannot accept a set".into(),
        });
    }
    if function.def.strict && bound.iter().any(|v| matches!(v, Value::Null)) {
        uqa_sql::routines::security::ensure_routine_execute_privilege(
            context.authority,
            &function.def,
        )?;
        return Ok(Value::Null);
    }
    let outcome = execute_routine(context, &function, bound, &invocation, false, None)?;
    let out_params = function.def.output_params();
    if outcome.out_values.len() != out_params.len() {
        return Err(SQLError::Internal(format!(
            "routine `{}` produced {} OUT values for {} OUT parameters",
            function.def.name,
            outcome.out_values.len(),
            out_params.len()
        )));
    }
    let value = match out_params.len() {
        0 => outcome.value,
        1 => match outcome.out_values.into_iter().next() {
            Some(value) => value,
            None => {
                return Err(SQLError::Internal(format!(
                    "routine `{}` lost its validated OUT value",
                    function.def.name
                )));
            }
        },
        _ => Value::Record(
            output_column_names(&function.def)
                .into_iter()
                .zip(outcome.out_values)
                .collect::<Vec<_>>()
                .into(),
        ),
    };
    Ok(value)
}

/// Resolve a user function call far enough for the projection planner to
/// choose scalar or set execution. `None` means no routine with this name
/// exists; argument-resolution errors remain observable at execution time.
pub fn resolved_user_function_returns_set(
    context: &RoutineInvocationContext<'_>,
    name: &str,
    args: &[(Option<String>, Value)],
) -> Option<Result<bool, SQLError>> {
    let resolved = match resolve_routine(context, name, args, None, "function", false) {
        Ok(Some(resolved)) => resolved,
        Ok(None) => return None,
        Err(error) => return Some(Err(error)),
    };
    let function = resolved.function;
    if function.def.is_procedure {
        return Some(Err(SQLError::Routine {
            sqlstate: "42809".into(),
            message: format!("{} is a procedure", call_signature(name, args)),
        }));
    }
    Some(Ok(function.def.returns_set()))
}

pub fn resolved_bound_user_function_returns_set(
    context: &RoutineInvocationContext<'_>,
    binding: &FunctionBinding,
    args: &[(Option<String>, Value)],
) -> Option<Result<bool, SQLError>> {
    let resolved = match resolve_bound_routine(context, binding, args) {
        Ok(Some(resolved)) => resolved,
        Ok(None) => return None,
        Err(error) => return Some(Err(error)),
    };
    let function = resolved.function;
    if function.def.is_procedure {
        return Some(Err(SQLError::Routine {
            sqlstate: "42809".into(),
            message: format!("{} is a procedure", call_signature(&binding.name, args)),
        }));
    }
    Some(Ok(function.def.returns_set()))
}

/// FROM-clause invocation: any user routine is callable as a table
/// source (`SELECT * FROM f(...)`); scalar functions produce a single
/// row. `None` means no routine with this name exists.
pub fn call_user_table_function(
    context: &RoutineInvocationContext<'_>,
    name: &str,
    args: &[(Option<String>, Value)],
    record_definition: Option<AnonymousRecordDefinition<'_>>,
) -> Option<Result<SQLTableFunctionResult, SQLError>> {
    let resolved = match resolve_routine(context, name, args, None, "function", false) {
        Ok(Some(resolved)) => resolved,
        Ok(None) => return None,
        Err(e) => return Some(Err(e)),
    };
    Some(execute_resolved_table_function(
        context,
        name,
        args,
        resolved,
        record_definition,
    ))
}

type AnonymousRecordDefinition<'a> = (&'a [String], &'a [String]);

pub fn call_bound_user_table_function(
    context: &RoutineInvocationContext<'_>,
    binding: &FunctionBinding,
    args: &[(Option<String>, Value)],
    record_definition: Option<AnonymousRecordDefinition<'_>>,
) -> Option<Result<SQLTableFunctionResult, SQLError>> {
    let resolved = match resolve_bound_routine(context, binding, args) {
        Ok(Some(resolved)) => resolved,
        Ok(None) => return None,
        Err(error) => return Some(Err(error)),
    };
    Some(execute_resolved_table_function(
        context,
        &binding.name,
        args,
        resolved,
        record_definition,
    ))
}

fn execute_resolved_table_function(
    context: &RoutineInvocationContext<'_>,
    name: &str,
    args: &[(Option<String>, Value)],
    resolved: ResolvedRoutine,
    record_definition: Option<AnonymousRecordDefinition<'_>>,
) -> Result<SQLTableFunctionResult, SQLError> {
    let ResolvedRoutine {
        function,
        bound,
        invocation,
    } = resolved;
    if function.def.is_procedure {
        return Err(SQLError::Routine {
            sqlstate: "42809".into(),
            message: format!("{} is a procedure", call_signature(name, args)),
        });
    }
    let out_params = function.def.output_params();
    let specialized =
        uqa_sql::routines::invocation::specialized_definition(&function.def, &invocation)?;
    let declared = uqa_sql::routines::result_check::declared_sql_function_result(
        context.types,
        specialized.as_ref().unwrap_or(&function.def),
    )?;
    let composite_columns = matches!(declared.declared_type, uqa_sql::ColumnType::Composite(_))
        .then_some(declared.columns)
        .flatten();
    let columns = if let Some((columns, _)) = record_definition {
        columns.to_vec()
    } else if let Some(columns) = &composite_columns {
        columns.iter().map(|column| column.name.clone()).collect()
    } else if out_params.is_empty() {
        vec![routine_local_name(&function.def.name)?]
    } else {
        output_column_names(&function.def)
    };
    if function.def.strict && bound.iter().any(|v| matches!(v, Value::Null)) {
        uqa_sql::routines::security::ensure_routine_execute_privilege(
            context.authority,
            &function.def,
        )?;
        let rows = if function.def.returns_set() {
            Vec::new()
        } else {
            vec![vec![Value::Null; columns.len()]]
        };
        return Ok(SQLTableFunctionResult::new(columns, rows));
    }
    let record_target = record_definition
        .map(|(columns, types)| anonymous_record_target(context, columns, types))
        .transpose()?;
    let outcome = execute_routine(
        context,
        &function,
        bound,
        &invocation,
        false,
        record_target.as_deref(),
    )?;
    if let Some((columns, types)) = record_definition {
        if !uqa_sql::routines::routine_returns_anonymous_record(&function.def) {
            return Err(SQLError::Internal(format!(
                "non-anonymous routine `{}` reached record-definition shaping",
                function.def.name
            )));
        }
        return shape_anonymous_record_outcome(
            context,
            outcome,
            function.def.returns_set(),
            columns,
            types,
            record_target.as_deref().unwrap_or_default(),
        );
    }
    let mut rows = if function.def.returns_set() {
        outcome.set_rows
    } else if out_params.is_empty() {
        vec![vec![outcome.value]]
    } else {
        vec![outcome.out_values]
    };
    if let Some(columns) = &composite_columns {
        rows = rows
            .into_iter()
            .map(|mut row| {
                super::super::sql_body::row_fields(row.pop().unwrap_or(Value::Null), columns.len())
            })
            .collect::<Result<Vec<_>, _>>()?;
    }
    Ok(SQLTableFunctionResult::new(columns, rows))
}

fn shape_anonymous_record_outcome(
    context: &RoutineInvocationContext<'_>,
    outcome: crate::routines::RoutineOutcome,
    returns_set: bool,
    columns: &[String],
    types: &[String],
    target: &[SQLFunctionResultColumn],
) -> Result<SQLTableFunctionResult, SQLError> {
    let sql_kind = outcome.sql_result_kind;
    let tuple = sql_kind == Some(SQLFunctionResultKind::Tuple);
    let validate_types = |source: &[Option<uqa_sql::ColumnType>]| {
        validate_anonymous_record_result(context.types, source, target, sql_kind)
    };
    if tuple {
        if let Some(source_types) = outcome.anonymous_record_column_types.as_deref() {
            validate_types(source_types)?;
        }
    }
    let source_rows = if returns_set {
        outcome.set_rows
    } else {
        vec![vec![outcome.value]]
    };
    let mut rows = Vec::with_capacity(source_rows.len());
    for row in source_rows {
        if !tuple && matches!(row.as_slice(), [Value::Null]) {
            rows.push(vec![Value::Null; columns.len()]);
            continue;
        }
        let runtime_descriptor = match row.as_slice() {
            [Value::Row(row)] if sql_kind == Some(SQLFunctionResultKind::Value) => {
                row.field_types()
            }
            _ => None,
        };
        if let Some(source) = runtime_descriptor {
            uqa_sql::routines::result_check::validate_sql_function_record_identity(
                context.types,
                source,
                &target
                    .iter()
                    .map(|column| column.ty.clone())
                    .collect::<Vec<_>>(),
            )?;
        } else if !tuple {
            if let Some(source_types) = outcome.anonymous_record_column_types.as_deref() {
                validate_types(source_types)?;
            }
        }
        let has_runtime_descriptor = runtime_descriptor.is_some();
        let mut values = match row.as_slice() {
            [Value::Record(fields)] => fields.iter().map(|(_, value)| value.clone()).collect(),
            [Value::Row(values)] => values.values().to_vec(),
            [Value::Null] => vec![Value::Null; columns.len()],
            _ if row.len() == columns.len() => row,
            _ => return Err(anonymous_record_shape_error()),
        };
        if values.len() != columns.len() {
            return Err(anonymous_record_shape_error());
        }
        if outcome.anonymous_record_column_types.is_none() && !has_runtime_descriptor {
            let source_types = values
                .iter()
                .map(runtime_record_column_type)
                .collect::<Vec<_>>();
            validate_types(&source_types)?;
        }
        for (value, type_name) in values.iter_mut().zip(types) {
            *value = coerce_anonymous_record_value(context.runtime.expressions, value, type_name)?;
        }
        rows.push(values);
    }
    Ok(SQLTableFunctionResult::new(columns.iter().cloned(), rows))
}

fn anonymous_record_target(
    context: &RoutineInvocationContext<'_>,
    columns: &[String],
    types: &[String],
) -> Result<Vec<SQLFunctionResultColumn>, SQLError> {
    if columns.len() != types.len() {
        return Err(SQLError::Internal(format!(
            "anonymous record definition has {} columns but {} types",
            columns.len(),
            types.len()
        )));
    }
    columns
        .iter()
        .zip(types)
        .map(|(name, ty)| {
            Ok(SQLFunctionResultColumn {
                name: name.clone(),
                ty: context.types.resolve_catalog_column_type_name(ty)?,
            })
        })
        .collect()
}
