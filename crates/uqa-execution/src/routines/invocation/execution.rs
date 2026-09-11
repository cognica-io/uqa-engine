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
use uqa_core::Value;
use uqa_sql::{
    ast::RoutineInvocationBinding,
    routines::{invocation::specialized_definition, CompiledFunctionBody, SQLUserFunction},
    type_resolution::canonical_routine_type_name,
    SQLError,
};
pub(super) fn execute_routine(
    context: &RoutineInvocationContext<'_>,
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
    super::scopes::with_routine_context(context.session, definition, || match &function.compiled {
        CompiledFunctionBody::PLpgSQL(parsed) => {
            if specialized.is_some() {
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
    })
}

pub fn execute_trigger_routine(
    context: &RoutineInvocationContext<'_>,
    function: &SQLUserFunction,
    trigger: &TriggerRoutineContext,
) -> Result<Value, SQLError> {
    let _guard = DepthGuard::enter(context.session)?;
    let _transaction_context = RoutineTransactionGuard::enter(context.runtime.session, false);
    super::scopes::with_routine_context(context.session, &function.def, || {
        let CompiledFunctionBody::PLpgSQL(parsed) = &function.compiled else {
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
    crate::routines::sql_body::execute_sql_language(context.runtime, definition, plans, bound)
}
