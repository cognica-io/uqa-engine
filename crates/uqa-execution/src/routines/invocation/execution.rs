//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Execute concrete routine bodies under the caller's transaction and state guards.
use super::{context::RoutineInvocationContext, depth::DepthGuard};
use crate::routines::{
    transaction::RoutineTransactionGuard, CreateFunction, FunctionReturns, Interpreter,
    PLpgSQLDatum, RoutineOutcome, TriggerRoutineContext,
};
use std::sync::Arc;
use uqa_core::Value;
use uqa_sql::{
    ast::RoutineInvocationBinding,
    routines::{
        compilation::compile_function_body, invocation::specialized_definition,
        CompiledFunctionBody, SQLUserFunction,
    },
    type_resolution::canonical_routine_type_name,
    SQLError,
};

/// The body a session compiles when it first calls a routine whose body `CREATE FUNCTION` left unexamined, under the routine's own settings, which the caller has applied; the session keeps it for later calls of the same definition.
fn compile_deferred_body(
    context: &RoutineInvocationContext<'_>,
    function: &Arc<SQLUserFunction>,
) -> Result<Arc<CompiledFunctionBody>, SQLError> {
    if let Some(body) = context.session.compiled_routine_body(function) {
        return Ok(body);
    }
    let body = Arc::new(compile_function_body(&context.compilation, &function.def)?);
    context
        .session
        .retain_compiled_routine_body(function, Arc::clone(&body));
    Ok(body)
}

pub(super) fn execute_routine(
    context: &RoutineInvocationContext<'_>,
    function: &Arc<SQLUserFunction>,
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
    let _guard = DepthGuard::enter(context.session)?;
    let _transition_scope = crate::mutation::triggers::enter_empty_transition_relation_scope();
    let specialized = specialized_definition(&function.def, invocation)?;
    let definition = specialized.as_ref().unwrap_or(&function.def);
    let nonatomic = allow_nonatomic
        && definition.is_procedure
        && !definition.security.security_definer
        && definition.config.is_empty();
    let _transaction_context = RoutineTransactionGuard::enter(context.runtime.session, nonatomic);
    uqa_sql::routines::security::ensure_routine_execute_privilege(context.authority, definition)?;
    super::scopes::with_routine_context(context.session, definition, || {
        let deferred;
        let compiled = if matches!(function.compiled, CompiledFunctionBody::Deferred) {
            deferred = compile_deferred_body(context, function)?;
            deferred.as_ref()
        } else {
            &function.compiled
        };
        execute_compiled_body(context, definition, specialized.is_some(), compiled, bound)
    })
}

fn execute_compiled_body(
    context: &RoutineInvocationContext<'_>,
    definition: &CreateFunction,
    specialized: bool,
    compiled: &CompiledFunctionBody,
    bound: Vec<Value>,
) -> Result<RoutineOutcome, SQLError> {
    match compiled {
        CompiledFunctionBody::PLpgSQL(parsed) => {
            if specialized {
                let mut parsed = parsed.clone();
                for (index, parameter) in definition.params.iter().enumerate() {
                    if let Some(PLpgSQLDatum::Var(variable)) = parsed.datums.get_mut(index) {
                        variable.type_name.clone_from(&parameter.type_name);
                    }
                }
                execute_plpgsql_language(context, definition, &parsed, bound)
            } else {
                execute_plpgsql_language(context, definition, parsed, bound)
            }
        }
        CompiledFunctionBody::SQL(statements) => {
            execute_sql_language(context, definition, statements, &bound)
        }
        CompiledFunctionBody::Deferred => Err(SQLError::Internal(format!(
            "routine `{}` reached execution without compiling its body",
            definition.name
        ))),
    }
}

pub fn execute_trigger_routine(
    context: &RoutineInvocationContext<'_>,
    function: &Arc<SQLUserFunction>,
    trigger: &TriggerRoutineContext,
) -> Result<Value, SQLError> {
    let _guard = DepthGuard::enter(context.session)?;
    let _transaction_context = RoutineTransactionGuard::enter(context.runtime.session, false);
    super::scopes::with_routine_context(context.session, &function.def, || {
        let deferred;
        let compiled = if matches!(function.compiled, CompiledFunctionBody::Deferred) {
            deferred = compile_deferred_body(context, function)?;
            deferred.as_ref()
        } else {
            &function.compiled
        };
        let CompiledFunctionBody::PLpgSQL(parsed) = compiled else {
            return Err(SQLError::Unsupported(
                "only LANGUAGE plpgsql trigger functions are executable".into(),
            ));
        };
        let mut interpreter = Interpreter::new(context.runtime, &function.def, parsed, Vec::new())?;
        interpreter.initialize_trigger_context(parsed, trigger)?;
        interpreter.run(&parsed.action)?;
        crate::routines::shape_trigger_outcome(interpreter.into_outcome(), trigger)
    })
}

fn execute_plpgsql_language(
    context: &RoutineInvocationContext<'_>,
    definition: &CreateFunction,
    parsed: &uqa_sql::plpgsql::PLpgSQLFunction,
    bound: Vec<Value>,
) -> Result<RoutineOutcome, SQLError> {
    let mut interpreter = Interpreter::new(context.runtime, definition, parsed, bound)?;
    interpreter.run(&parsed.action)?;
    Ok(interpreter.into_outcome())
}

fn execute_sql_language(
    context: &RoutineInvocationContext<'_>,
    definition: &CreateFunction,
    plans: &[uqa_sql::plan::UnifiedPlan],
    bound: &[Value],
) -> Result<RoutineOutcome, SQLError> {
    crate::routines::sql_body::execute_sql_language(
        context.runtime,
        &context.compilation,
        &context.overloads,
        definition,
        plans,
        bound,
    )
}
