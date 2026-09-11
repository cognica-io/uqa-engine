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
            runtime_record_column_type, validate_anonymous_record_column_types,
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
    let outcome = execute_routine(context, &function, bound, &invocation, false)?;
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
                .collect(),
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
    let columns = if let Some((columns, _)) = record_definition {
        columns.to_vec()
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
    let outcome = execute_routine(context, &function, bound, &invocation, false)?;
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
        );
    }
    let rows = if function.def.returns_set() {
        outcome.set_rows
    } else if out_params.is_empty() {
        vec![vec![outcome.value]]
    } else {
        vec![outcome.out_values]
    };
    Ok(SQLTableFunctionResult::new(columns, rows))
}

fn shape_anonymous_record_outcome(
    context: &RoutineInvocationContext<'_>,
    outcome: crate::routines::RoutineOutcome,
    returns_set: bool,
    columns: &[String],
    types: &[String],
) -> Result<SQLTableFunctionResult, SQLError> {
    if columns.len() != types.len() {
        return Err(SQLError::Internal(format!(
            "anonymous record definition has {} columns but {} types",
            columns.len(),
            types.len()
        )));
    }
    if let Some(source_types) = outcome.anonymous_record_column_types.as_deref() {
        validate_anonymous_record_column_types(source_types, types)?;
    }
    let source_rows = if returns_set {
        outcome.set_rows
    } else {
        vec![vec![outcome.value]]
    };
    let mut rows = Vec::with_capacity(source_rows.len());
    for row in source_rows {
        let mut values = match row.as_slice() {
            [Value::Record(fields)] => fields.iter().map(|(_, value)| value.clone()).collect(),
            [Value::Row(values)] => values.clone(),
            [Value::Null] => vec![Value::Null; columns.len()],
            _ if row.len() == columns.len() => row,
            _ => return Err(anonymous_record_shape_error()),
        };
        if values.len() != columns.len() {
            return Err(anonymous_record_shape_error());
        }
        if outcome.anonymous_record_column_types.is_none() {
            let source_types = values
                .iter()
                .map(runtime_record_column_type)
                .collect::<Vec<_>>();
            validate_anonymous_record_column_types(&source_types, types)?;
        }
        for (value, type_name) in values.iter_mut().zip(types) {
            *value = coerce_anonymous_record_value(context.runtime.expressions, value, type_name)?;
        }
        rows.push(values);
    }
    Ok(SQLTableFunctionResult::new(columns.iter().cloned(), rows))
}
