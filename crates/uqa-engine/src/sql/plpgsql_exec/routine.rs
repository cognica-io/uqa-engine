//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Routine execution, recursion limits, and `LANGUAGE sql` result shaping.

use super::{
    coerce_routine_value, result_row_values, Cell, CompiledFunctionBody, CreateFunction, Engine,
    FunctionReturns, Interpreter, PLpgSQLDatum, RoutineOutcome, SQLError, SQLParam, SQLResult,
    SQLUserFunction, UnifiedPlanExecutor, Value,
};
use crate::engine_user_functions::routine_returns_anonymous_record;
use uqa_sql::ast::RoutineInvocationBinding;

thread_local! {
    static CALL_DEPTH: Cell<usize> = const { Cell::new(0) };
    static STACK_BASE: Cell<usize> = const { Cell::new(0) };
}

/// Native stack budget for nested routine calls, measured from the
/// outermost routine entry. The `PostgreSQL` `max_stack_depth`
/// setting plays the same role (default 2MB there); this budget is
/// sized so the guard
/// always fires before a 2MB thread stack (the Rust test-runner
/// default) is exhausted, even in debug builds.
const STACK_BYTE_BUDGET: usize = 1_000_000;

/// Approximate current stack position.
#[inline(never)]
fn stack_marker() -> usize {
    let marker = 0u8;
    std::ptr::from_ref(&marker) as usize
}

fn stack_depth_error() -> SQLError {
    SQLError::Routine {
        sqlstate: "54001".into(),
        message: "stack depth limit exceeded".into(),
    }
}

/// RAII guard for the user-routine nesting caps: a configurable
/// frame-count limit plus a native stack-byte budget.
pub(super) struct DepthGuard;

impl DepthGuard {
    pub(super) fn enter(engine: &Engine) -> Result<Self, SQLError> {
        let depth = CALL_DEPTH.get();
        if depth == 0 {
            STACK_BASE.set(stack_marker());
        } else if STACK_BASE.get().abs_diff(stack_marker()) > STACK_BYTE_BUDGET {
            return Err(stack_depth_error());
        }
        if depth >= engine.sql_function_depth_limit() {
            return Err(stack_depth_error());
        }
        CALL_DEPTH.set(depth + 1);
        Ok(Self)
    }
}

impl Drop for DepthGuard {
    fn drop(&mut self) {
        CALL_DEPTH.set(CALL_DEPTH.get().saturating_sub(1));
    }
}

pub(super) fn execute_routine(
    engine: &Engine,
    function: &SQLUserFunction,
    bound: Vec<Value>,
    invocation: &RoutineInvocationBinding,
) -> Result<RoutineOutcome, SQLError> {
    let _guard = DepthGuard::enter(engine)?;
    let definition = specialized_definition(&function.def, invocation)?;
    match &function.compiled {
        CompiledFunctionBody::PLpgSQL(parsed) => {
            let mut parsed = parsed.clone();
            for (index, parameter) in definition.params.iter().enumerate() {
                if let Some(PLpgSQLDatum::Var(variable)) = parsed.datums.get_mut(index) {
                    variable.type_name.clone_from(&parameter.type_name);
                }
            }
            let mut interpreter = Interpreter::new(engine, &definition, &parsed, bound)?;
            interpreter.run(&parsed.action)?;
            Ok(interpreter.into_outcome())
        }
        CompiledFunctionBody::SQL(statements) => {
            execute_sql_language(engine, &definition, statements, &bound)
        }
    }
}

fn specialized_definition(
    definition: &CreateFunction,
    invocation: &RoutineInvocationBinding,
) -> Result<CreateFunction, SQLError> {
    if invocation.parameter_types.len() != definition.params.len() {
        return Err(SQLError::Internal(format!(
            "routine `{}` has {} concrete parameter types for {} parameters",
            definition.name,
            invocation.parameter_types.len(),
            definition.params.len()
        )));
    }
    let mut specialized = definition.clone();
    for (parameter, type_name) in specialized
        .params
        .iter_mut()
        .zip(&invocation.parameter_types)
    {
        parameter.type_name.clone_from(type_name);
    }
    if let Some(return_type) = &invocation.return_type {
        match &mut specialized.returns {
            FunctionReturns::Scalar { type_name } | FunctionReturns::SetOf { type_name } => {
                type_name.clone_from(return_type);
            }
            FunctionReturns::None | FunctionReturns::Table => {}
        }
    }
    Ok(specialized)
}

/// `LANGUAGE sql` body: run every statement; the last statement's
/// result shapes the routine output.
fn execute_sql_language(
    engine: &Engine,
    def: &CreateFunction,
    plans: &[uqa_planner::UnifiedPlan],
    bound: &[Value],
) -> Result<RoutineOutcome, SQLError> {
    let call_params = def.call_params();
    if call_params.len() != bound.len() {
        return Err(SQLError::Internal(format!(
            "routine `{}` received {} values for {} concrete call parameters",
            def.name,
            bound.len(),
            call_params.len()
        )));
    }
    let params = bound
        .iter()
        .cloned()
        .zip(call_params)
        .map(|(value, parameter)| {
            let ty = crate::sql::resolve_catalog_column_type(engine, &parameter.type_name)
                .or_else(|| uqa_sql::ast::ColumnType::from_sql_name(&parameter.type_name).ok())
                .ok_or_else(|| {
                    SQLError::TypeMismatch(format!("unknown type `{}`", parameter.type_name))
                })?;
            Ok(SQLParam::typed_scalar(value, ty))
        })
        .collect::<Result<Vec<_>, SQLError>>()?;
    let mut last = SQLResult::empty();
    for plan in plans {
        last = UnifiedPlanExecutor::new(engine, &params).execute(plan)?;
    }
    let out_params = def.output_params();
    let returns_anonymous_record = routine_returns_anonymous_record(def);
    let returns_void = matches!(
        &def.returns,
        FunctionReturns::Scalar { type_name } if type_name == "void"
    );
    let expected = if out_params.is_empty() {
        1
    } else {
        out_params.len()
    };
    // PostgreSQL enforces the final statement's column shape at
    // CREATE time; the engine has no schema binding there, so the
    // same 42P13 error surfaces on the first call instead.
    let shape_checked =
        !returns_void && !returns_anonymous_record && (!def.is_procedure || !out_params.is_empty());
    if shape_checked && last.columns.len() != expected {
        return Err(sql_body_shape_error(def));
    }
    if def.returns_set() {
        let mut set_rows = Vec::with_capacity(last.rows.len());
        for row_index in 0..last.rows.len() {
            let mut values = result_row_values(&last, row_index).unwrap_or_default();
            if !returns_anonymous_record && values.len() != expected {
                return Err(sql_body_shape_error(def));
            }
            if returns_anonymous_record {
                values = vec![anonymous_record_value(&last.columns, values)];
            } else if out_params.is_empty() {
                if let FunctionReturns::SetOf { type_name } = &def.returns {
                    values[0] = coerce_routine_value(engine, &values[0], type_name)?;
                }
            } else {
                for (value, parameter) in values.iter_mut().zip(&out_params) {
                    *value = coerce_routine_value(engine, value, &parameter.type_name)?;
                }
            }
            set_rows.push(values);
        }
        return Ok(RoutineOutcome {
            value: Value::Null,
            out_values: vec![Value::Null; out_params.len()],
            set_rows,
            anonymous_record_column_types: returns_anonymous_record
                .then(|| last.column_types.clone()),
        });
    }
    let first = result_row_values(&last, 0);
    if !out_params.is_empty() {
        let mut out_values = vec![Value::Null; out_params.len()];
        if let Some(values) = first {
            for (idx, value) in values.into_iter().take(out_values.len()).enumerate() {
                out_values[idx] = coerce_routine_value(engine, &value, &out_params[idx].type_name)?;
            }
        }
        return Ok(RoutineOutcome {
            value: Value::Null,
            out_values,
            set_rows: Vec::new(),
            anonymous_record_column_types: None,
        });
    }
    let value = match first {
        Some(_) if returns_void => Value::Null,
        Some(values) if returns_anonymous_record => anonymous_record_value(&last.columns, values),
        Some(mut values) => {
            if values.is_empty() {
                Value::Null
            } else {
                let value = values.remove(0);
                match &def.returns {
                    FunctionReturns::Scalar { type_name } => {
                        coerce_routine_value(engine, &value, type_name)?
                    }
                    _ => value,
                }
            }
        }
        None => Value::Null,
    };
    Ok(RoutineOutcome {
        value,
        out_values: Vec::new(),
        set_rows: Vec::new(),
        anonymous_record_column_types: returns_anonymous_record.then(|| last.column_types.clone()),
    })
}

fn anonymous_record_value(columns: &[String], values: Vec<Value>) -> Value {
    Value::Record(columns.iter().cloned().zip(values).collect())
}

fn sql_body_shape_error(def: &CreateFunction) -> SQLError {
    let declared = match &def.returns {
        FunctionReturns::Scalar { type_name } | FunctionReturns::SetOf { type_name } => {
            type_name.clone()
        }
        FunctionReturns::Table => "record".into(),
        FunctionReturns::None => "record".into(),
    };
    SQLError::Routine {
        sqlstate: "42P13".into(),
        message: format!("return type mismatch in function declared to return {declared}"),
    }
}
