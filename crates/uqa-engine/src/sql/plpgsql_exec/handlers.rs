//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL statement handlers and scalar/table routine entry points.

use super::{
    call_signature, execute_routine, output_column_names, resolve_bound_routine, resolve_routine,
    routine_local_name, routine_resolution_error, BTreeMap, CreateFunction, DepthGuard,
    DropFunctionStmt, Engine, FunctionBinding, FunctionReturns, Interpreter, ResultRow, SQLError,
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
        language: "plpgsql".into(),
        body: uqa_sql::ast::FunctionBody::Source(body.to_string()),
        volatility: uqa_sql::ast::FunctionVolatility::Volatile,
        strict: false,
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
) -> Result<SQLResult, SQLError> {
    let function = match resolve_routine(engine, name, call_args, "procedure")? {
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
    let (function, bound) = function;
    if !function.def.is_procedure {
        return Err(SQLError::Routine {
            sqlstate: "42809".into(),
            message: format!("{} is not a procedure", call_signature(name, call_args)),
        });
    }
    let outcome = execute_routine(engine, &function, bound)?;
    let out_params = function.def.output_params();
    if out_params.is_empty() {
        return Ok(SQLResult::empty());
    }
    let columns = output_column_names(&function.def);
    let mut row = ResultRow::new();
    for (column, value) in columns.iter().zip(outcome.out_values.iter()) {
        row.insert(column.clone(), value.clone());
    }
    Ok(SQLResult {
        columns,
        rows: vec![row],
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
    let resolved = match resolve_routine(engine, name, args, "function") {
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
    resolved: (
        std::sync::Arc<crate::engine_user_functions::SQLUserFunction>,
        Vec<Value>,
    ),
) -> Result<Value, SQLError> {
    let (function, bound) = resolved;
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
        return Ok(Value::Null);
    }
    let outcome = execute_routine(engine, &function, bound)?;
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
        _ => {
            let mut record = BTreeMap::new();
            for (column, value) in output_column_names(&function.def)
                .into_iter()
                .zip(outcome.out_values)
            {
                record.insert(column, value);
            }
            Value::Map(record)
        }
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
    let resolved = match resolve_routine(engine, name, args, "function") {
        Ok(Some(resolved)) => resolved,
        Ok(None) => return None,
        Err(error) => return Some(Err(error)),
    };
    let (function, _) = resolved;
    if function.def.is_procedure {
        return Some(Err(SQLError::Routine {
            sqlstate: "42809".into(),
            message: format!("{} is a procedure", call_signature(name, args)),
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
) -> Option<Result<SQLTableFunctionResult, SQLError>> {
    let resolved = match resolve_routine(engine, name, args, "function") {
        Ok(Some(resolved)) => resolved,
        Ok(None) => return None,
        Err(e) => return Some(Err(e)),
    };
    let (function, bound) = resolved;
    if function.def.is_procedure {
        return Some(Err(SQLError::Routine {
            sqlstate: "42809".into(),
            message: format!("{} is a procedure", call_signature(name, args)),
        }));
    }
    let out_params = function.def.output_params();
    let columns = if out_params.is_empty() {
        match routine_local_name(&function.def.name) {
            Ok(name) => vec![name],
            Err(error) => return Some(Err(error)),
        }
    } else {
        output_column_names(&function.def)
    };
    if function.def.strict && bound.iter().any(|v| matches!(v, Value::Null)) {
        let rows = if function.def.returns_set() {
            Vec::new()
        } else {
            vec![vec![Value::Null; columns.len()]]
        };
        return Some(Ok(SQLTableFunctionResult::new(columns, rows)));
    }
    let outcome = match execute_routine(engine, &function, bound) {
        Ok(outcome) => outcome,
        Err(e) => return Some(Err(e)),
    };
    let rows = if function.def.returns_set() {
        outcome.set_rows
    } else if out_params.is_empty() {
        vec![vec![outcome.value]]
    } else {
        vec![outcome.out_values]
    };
    Some(Ok(SQLTableFunctionResult::new(columns, rows)))
}
