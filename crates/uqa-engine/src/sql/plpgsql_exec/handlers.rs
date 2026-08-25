//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL statement handlers and scalar/table routine entry points.

use super::{
    call_signature, execute_routine, output_column_names, resolve_bound_routine, resolve_routine,
    routine_local_name, routine_resolution_error, CreateFunction, DepthGuard, DropFunctionStmt,
    Engine, FunctionBinding, FunctionReturns, Interpreter, ResolvedRoutine, ResultRow, SQLError,
    SQLResult, SQLTableFunctionResult, Value,
};

pub(in crate::sql) fn run_create_function(
    engine: &Engine,
    def: CreateFunction,
) -> Result<SQLResult, SQLError> {
    engine.register_sql_function(def)?;
    Ok(SQLResult::empty())
}

pub(in crate::sql) fn run_drop_function(
    engine: &Engine,
    stmt: &DropFunctionStmt,
) -> Result<SQLResult, SQLError> {
    engine.drop_sql_functions(stmt)?;
    Ok(SQLResult::empty())
}

pub(in crate::sql) fn run_do_block(
    engine: &Engine,
    language: &str,
    body: &str,
) -> Result<SQLResult, SQLError> {
    if language != "plpgsql" {
        return Err(SQLError::Routine {
            sqlstate: "42704".into(),
            message: format!("language \"{language}\" does not exist"),
        });
    }
    let parsed = uqa_sql::plpgsql::parse_do_block(body)?;
    let def = CreateFunction {
        name: "inline_code_block".into(),
        or_replace: false,
        is_procedure: false,
        params: Vec::new(),
        returns: FunctionReturns::Scalar {
            type_name: "void".into(),
        },
        return_type_reference: None,
        language: "plpgsql".into(),
        body: uqa_sql::ast::FunctionBody::Source(body.to_string()),
        creation_search_path: Vec::new(),
        volatility: uqa_sql::ast::FunctionVolatility::Volatile,
        strict: false,
        owner: String::new(),
        security: uqa_sql::ast::RoutineSecurityAttributes::default(),
        parallel: uqa_sql::ast::FunctionParallel::Unsafe,
        support: None,
        config: Vec::new(),
        config_actions: Vec::new(),
        execute_acl: None,
    };
    let _guard = DepthGuard::enter(engine)?;
    let mut interpreter = Interpreter::new(engine, &def, &parsed, Vec::new())?;
    interpreter.run(&parsed.action)?;
    Ok(SQLResult::empty())
}

pub(in crate::sql) fn run_call(
    engine: &Engine,
    name: &str,
    call_args: &[(Option<String>, Value)],
    argument_types: &[Option<uqa_sql::ast::ColumnType>],
    explicit_variadic: bool,
) -> Result<SQLResult, SQLError> {
    let function = match resolve_routine(
        engine,
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
    let outcome = execute_routine(engine, &function, bound, &invocation)?;
    let out_params = function.def.output_params();
    if out_params.is_empty() {
        return Ok(SQLResult::empty());
    }
    let columns = output_column_names(&function.def);
    let output_indices = function
        .def
        .params
        .iter()
        .enumerate()
        .filter_map(|(index, parameter)| {
            matches!(
                parameter.mode,
                uqa_sql::ast::FunctionParamMode::Out
                    | uqa_sql::ast::FunctionParamMode::InOut
                    | uqa_sql::ast::FunctionParamMode::Table
            )
            .then_some(index)
        });
    let column_types = output_indices
        .map(|index| {
            crate::sql::resolve_catalog_column_type(engine, &invocation.parameter_types[index])
                .or_else(|| {
                    uqa_sql::ast::ColumnType::from_sql_name(&invocation.parameter_types[index]).ok()
                })
                .map(Some)
                .ok_or_else(|| {
                    SQLError::TypeMismatch(format!(
                        "unknown type `{}`",
                        invocation.parameter_types[index]
                    ))
                })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut row = ResultRow::new();
    for (column, value) in columns.iter().zip(outcome.out_values.iter()) {
        row.insert(column.clone(), value.clone());
    }
    Ok(SQLResult {
        column_types,
        columns,
        rows: vec![row],
        positional_rows: None,
        affected_rows: 0,
    })
}

/// Scalar-context invocation used by the expression evaluator's
/// engine hook. `None` means no routine with this name exists.
pub(crate) fn call_user_scalar_function(
    engine: &Engine,
    name: &str,
    args: &[(Option<String>, Value)],
) -> Option<Result<Value, SQLError>> {
    let resolved = match resolve_routine(engine, name, args, None, "function", false) {
        Ok(Some(resolved)) => resolved,
        Ok(None) => return None,
        Err(e) => return Some(Err(e)),
    };
    Some(execute_resolved_scalar_function(
        engine, name, args, resolved,
    ))
}

pub(crate) fn call_bound_user_scalar_function(
    engine: &Engine,
    binding: &FunctionBinding,
    args: &[(Option<String>, Value)],
) -> Option<Result<Value, SQLError>> {
    let resolved = match resolve_bound_routine(engine, binding, args) {
        Ok(Some(resolved)) => resolved,
        Ok(None) => return None,
        Err(error) => return Some(Err(error)),
    };
    Some(execute_resolved_scalar_function(
        engine,
        &binding.name,
        args,
        resolved,
    ))
}

fn execute_resolved_scalar_function(
    engine: &Engine,
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
        engine.ensure_routine_execute_privilege(&function.def)?;
        return Ok(Value::Null);
    }
    let outcome = execute_routine(engine, &function, bound, &invocation)?;
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
pub(crate) fn resolved_user_function_returns_set(
    engine: &Engine,
    name: &str,
    args: &[(Option<String>, Value)],
) -> Option<Result<bool, SQLError>> {
    let resolved = match resolve_routine(engine, name, args, None, "function", false) {
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

pub(crate) fn resolved_bound_user_function_returns_set(
    engine: &Engine,
    binding: &FunctionBinding,
    args: &[(Option<String>, Value)],
) -> Option<Result<bool, SQLError>> {
    let resolved = match resolve_bound_routine(engine, binding, args) {
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
pub(crate) fn call_user_table_function(
    engine: &Engine,
    name: &str,
    args: &[(Option<String>, Value)],
    record_definition: Option<AnonymousRecordDefinition<'_>>,
) -> Option<Result<SQLTableFunctionResult, SQLError>> {
    let resolved = match resolve_routine(engine, name, args, None, "function", false) {
        Ok(Some(resolved)) => resolved,
        Ok(None) => return None,
        Err(e) => return Some(Err(e)),
    };
    Some(execute_resolved_table_function(
        engine,
        name,
        args,
        resolved,
        record_definition,
    ))
}

type AnonymousRecordDefinition<'a> = (&'a [String], &'a [String]);

pub(crate) fn call_bound_user_table_function(
    engine: &Engine,
    binding: &FunctionBinding,
    args: &[(Option<String>, Value)],
    record_definition: Option<AnonymousRecordDefinition<'_>>,
) -> Option<Result<SQLTableFunctionResult, SQLError>> {
    let resolved = match resolve_bound_routine(engine, binding, args) {
        Ok(Some(resolved)) => resolved,
        Ok(None) => return None,
        Err(error) => return Some(Err(error)),
    };
    Some(execute_resolved_table_function(
        engine,
        &binding.name,
        args,
        resolved,
        record_definition,
    ))
}

fn execute_resolved_table_function(
    engine: &Engine,
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
        engine.ensure_routine_execute_privilege(&function.def)?;
        let rows = if function.def.returns_set() {
            Vec::new()
        } else {
            vec![vec![Value::Null; columns.len()]]
        };
        return Ok(SQLTableFunctionResult::new(columns, rows));
    }
    let outcome = execute_routine(engine, &function, bound, &invocation)?;
    if let Some((columns, types)) = record_definition {
        if !crate::engine_user_functions::routine_returns_anonymous_record(&function.def) {
            return Err(SQLError::Internal(format!(
                "non-anonymous routine `{}` reached record-definition shaping",
                function.def.name
            )));
        }
        return shape_anonymous_record_outcome(
            engine,
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
    engine: &Engine,
    outcome: super::RoutineOutcome,
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
            *value = coerce_anonymous_record_value(engine, value, type_name)?;
        }
        rows.push(values);
    }
    Ok(SQLTableFunctionResult::new(columns.iter().cloned(), rows))
}

fn validate_anonymous_record_column_types(
    source_types: &[Option<uqa_sql::ast::ColumnType>],
    target_types: &[String],
) -> Result<(), SQLError> {
    if source_types.len() != target_types.len() {
        return Err(anonymous_record_shape_error());
    }
    for (source, target) in source_types.iter().zip(target_types) {
        let Some(source) = source else {
            continue;
        };
        let source = uqa_execution::canonical_column_type_name(source);
        let target = crate::engine_user_functions::canonical_routine_type_name(target);
        if !uqa_execution::routine_type_accepts_implicit_cast(&source, &target) {
            return Err(anonymous_record_shape_error());
        }
    }
    Ok(())
}

fn runtime_record_column_type(value: &Value) -> Option<uqa_sql::ast::ColumnType> {
    if matches!(value, Value::Null) {
        return None;
    }
    uqa_sql::ast::ColumnType::from_sql_name(uqa_sql::expr::value_type_name(value)).ok()
}

fn coerce_anonymous_record_value(
    engine: &Engine,
    value: &Value,
    type_name: &str,
) -> Result<Value, SQLError> {
    let target = crate::sql::resolve_catalog_column_type(engine, type_name)
        .or_else(|| uqa_sql::ast::ColumnType::from_sql_name(type_name).ok());
    let Some(target) = target else {
        return super::coerce_routine_value(engine, value, type_name);
    };
    crate::sql::ddl::convert_value_to_column_type(value.clone(), &target).map_err(|error| {
        match error {
            SQLError::TypeMismatch(message) if message.starts_with("value too long for type ") => {
                SQLError::Routine {
                    sqlstate: "22001".into(),
                    message,
                }
            }
            other => other,
        }
    })
}

fn anonymous_record_shape_error() -> SQLError {
    SQLError::Routine {
        sqlstate: "42P13".into(),
        message: "return type mismatch in function declared to return record".into(),
    }
}
