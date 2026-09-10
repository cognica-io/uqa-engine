//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Routine execution, recursion limits, and `LANGUAGE sql` result shaping.

use super::{
    Cell, CompiledFunctionBody, CreateFunction, Engine, FunctionReturns, Interpreter, PLpgSQLDatum,
    RoutineOutcome, SQLError, SQLUserFunction, Value,
};
use crate::user_functions::canonical_routine_type_name;
use uqa_sql::ast::RoutineInvocationBinding;

pub(crate) use uqa_execution::routines::TriggerRoutineContext;

thread_local! {
    static CALL_DEPTH: Cell<usize> = const { Cell::new(0) };
    static STACK_BASE: Cell<usize> = const { Cell::new(0) };
}
pub(super) use uqa_execution::routines::transaction::RoutineTransactionGuard;
pub(super) fn nonatomic_routine_entry_allowed(engine: &Engine, nested_statement: bool) -> bool {
    uqa_execution::routines::transaction::nonatomic_routine_entry_allowed(
        engine.routine_session_id(),
        nested_statement,
    )
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
    allow_nonatomic: bool,
) -> Result<RoutineOutcome, SQLError> {
    if matches!(
        &function.def.returns,
        FunctionReturns::Scalar { type_name }
            if canonical_routine_type_name(type_name) == "trigger"
    ) {
        return Err(SQLError::Routine {
            sqlstate: "0A000".into(),
            message: "trigger functions can only be called as triggers".into(),
        });
    }
    let _guard = DepthGuard::enter(engine)?;
    let _transition_scope = crate::sql::triggers::enter_empty_transition_relation_scope();
    let specialized = specialized_definition(&function.def, invocation)?;
    let definition = specialized.as_ref().unwrap_or(&function.def);
    let nonatomic = allow_nonatomic
        && definition.is_procedure
        && !definition.security.security_definer
        && definition.config.is_empty();
    let _transaction_context =
        RoutineTransactionGuard::enter(engine.routine_session_id(), nonatomic);
    engine.ensure_routine_execute_privilege(definition)?;
    engine.with_routine_context(definition, || match &function.compiled {
        CompiledFunctionBody::PLpgSQL(parsed) => {
            if specialized.is_some() {
                let mut parsed = parsed.clone();
                for (index, parameter) in definition.params.iter().enumerate() {
                    if let Some(PLpgSQLDatum::Var(variable)) = parsed.datums.get_mut(index) {
                        variable.type_name.clone_from(&parameter.type_name);
                    }
                }
                execute_plpgsql_language(engine, definition, &parsed, bound)
            } else {
                execute_plpgsql_language(engine, definition, parsed, bound)
            }
        }
        CompiledFunctionBody::SQL(statements) => {
            execute_sql_language(engine, definition, statements, &bound)
        }
    })
}

pub(crate) fn execute_trigger_routine(
    engine: &Engine,
    function: &SQLUserFunction,
    context: &TriggerRoutineContext,
) -> Result<Value, SQLError> {
    let _guard = DepthGuard::enter(engine)?;
    let _transaction_context = RoutineTransactionGuard::enter(engine.routine_session_id(), false);
    engine.with_routine_context(&function.def, || {
        let CompiledFunctionBody::PLpgSQL(parsed) = &function.compiled else {
            return Err(SQLError::Unsupported(
                "only LANGUAGE plpgsql trigger functions are executable".into(),
            ));
        };
        let mut interpreter = Interpreter::new(
            engine.routine_execution_context(),
            &function.def,
            parsed,
            Vec::new(),
        )?;
        interpreter.initialize_trigger_context(parsed, context)?;
        interpreter.run(&parsed.action)?;
        uqa_execution::routines::shape_trigger_outcome(interpreter.into_outcome(), context)
    })
}

fn execute_plpgsql_language(
    engine: &Engine,
    definition: &CreateFunction,
    parsed: &uqa_sql::plpgsql::PLpgSQLFunction,
    bound: Vec<Value>,
) -> Result<RoutineOutcome, SQLError> {
    let mut interpreter = Interpreter::new(
        engine.routine_execution_context(),
        definition,
        parsed,
        bound,
    )?;
    interpreter.run(&parsed.action)?;
    Ok(interpreter.into_outcome())
}

fn specialized_definition(
    definition: &CreateFunction,
    invocation: &RoutineInvocationBinding,
) -> Result<Option<CreateFunction>, SQLError> {
    if invocation.parameter_types.len() != definition.params.len() {
        return Err(SQLError::Internal(format!(
            "routine `{}` has {} concrete parameter types for {} parameters",
            definition.name,
            invocation.parameter_types.len(),
            definition.params.len()
        )));
    }
    let parameters_match = definition
        .params
        .iter()
        .zip(&invocation.parameter_types)
        .all(|(parameter, type_name)| parameter.type_name == *type_name);
    let return_type_matches = match (&invocation.return_type, &definition.returns) {
        (Some(concrete), FunctionReturns::Scalar { type_name })
        | (Some(concrete), FunctionReturns::SetOf { type_name }) => concrete == type_name,
        (None, _) | (Some(_), FunctionReturns::None | FunctionReturns::Table) => true,
    };
    if parameters_match && return_type_matches {
        return Ok(None);
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
    Ok(Some(specialized))
}

fn execute_sql_language(
    engine: &Engine,
    definition: &CreateFunction,
    plans: &[uqa_planner::UnifiedPlan],
    bound: &[Value],
) -> Result<RoutineOutcome, SQLError> {
    uqa_execution::routines::sql_body::execute_sql_language(
        engine.routine_execution_context(),
        definition,
        plans,
        bound,
    )
}
