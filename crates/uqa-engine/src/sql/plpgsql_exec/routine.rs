//! Routine execution, recursion limits, and `LANGUAGE sql` result shaping.

use super::{
    best_effort_cast, row_value, Cell, CompiledFunctionBody, CreateFunction, Engine,
    FunctionReturns, Interpreter, ResultRow, RoutineOutcome, SQLError, SQLParam, SQLResult,
    SQLUserFunction, UnifiedPlanExecutor, Value,
};

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
) -> Result<RoutineOutcome, SQLError> {
    let _guard = DepthGuard::enter(engine)?;
    match &function.compiled {
        CompiledFunctionBody::PLpgSQL(parsed) => {
            let mut interpreter = Interpreter::new(engine, &function.def, parsed, bound)?;
            interpreter.run(&parsed.action)?;
            Ok(interpreter.into_outcome())
        }
        CompiledFunctionBody::SQL(statements) => {
            execute_sql_language(engine, &function.def, statements, &bound)
        }
    }
}

/// `LANGUAGE sql` body: run every statement; the last statement's
/// result shapes the routine output.
fn execute_sql_language(
    engine: &Engine,
    def: &CreateFunction,
    plans: &[uqa_planner::UnifiedPlan],
    bound: &[Value],
) -> Result<RoutineOutcome, SQLError> {
    let params: Vec<SQLParam> = bound.iter().cloned().map(SQLParam::Scalar).collect();
    let mut last = SQLResult::empty();
    for plan in plans {
        last = UnifiedPlanExecutor::new(engine, &params).execute(plan)?;
    }
    let out_params = def.output_params();
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
    let shape_checked = !returns_void && (!def.is_procedure || !out_params.is_empty());
    if shape_checked && last.columns.len() != expected {
        return Err(sql_body_shape_error(def));
    }
    let row_values = |row: &ResultRow| -> Vec<Value> {
        last.columns
            .iter()
            .map(|column| row_value(row, column))
            .collect()
    };
    if def.returns_set() {
        let mut set_rows = Vec::with_capacity(last.rows.len());
        for row in &last.rows {
            let mut values = row_values(row);
            if values.len() != expected {
                return Err(sql_body_shape_error(def));
            }
            if out_params.is_empty() {
                if let FunctionReturns::SetOf { type_name } = &def.returns {
                    values[0] = best_effort_cast(&values[0], type_name)?;
                }
            }
            set_rows.push(values);
        }
        return Ok(RoutineOutcome {
            value: Value::Null,
            out_values: vec![Value::Null; out_params.len()],
            set_rows,
        });
    }
    let first = last.rows.first().map(row_values);
    if !out_params.is_empty() {
        let mut out_values = vec![Value::Null; out_params.len()];
        if let Some(values) = first {
            for (idx, value) in values.into_iter().take(out_values.len()).enumerate() {
                out_values[idx] = value;
            }
        }
        return Ok(RoutineOutcome {
            value: Value::Null,
            out_values,
            set_rows: Vec::new(),
        });
    }
    let value = match first {
        Some(_) if returns_void => Value::Null,
        Some(mut values) => {
            if values.is_empty() {
                Value::Null
            } else {
                let value = values.remove(0);
                match &def.returns {
                    FunctionReturns::Scalar { type_name } => best_effort_cast(&value, type_name)?,
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
    })
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
