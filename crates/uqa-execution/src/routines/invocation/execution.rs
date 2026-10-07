//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Execute concrete routine bodies under the caller's transaction and state guards.
use super::{context::RoutineInvocationContext, depth::DepthGuard};
use crate::routines::{
    transaction::RoutineTransactionGuard, CreateFunction, FunctionReturns, Interpreter,
    RoutineOutcome, TriggerRoutineContext,
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
    record_target: Option<&[uqa_sql::routines::result_check::SQLFunctionResultColumn]>,
) -> Result<RoutineOutcome, SQLError> {
    execute_entry(
        context,
        function,
        bound,
        invocation,
        RoutineEntry::Invocation {
            allow_nonatomic,
            record_target,
        },
    )
}

enum RoutineEntry<'a> {
    Invocation {
        allow_nonatomic: bool,
        record_target: Option<&'a [uqa_sql::routines::result_check::SQLFunctionResultColumn]>,
    },
    CatalogCallback,
}

pub(super) fn execute_catalog_routine(
    context: &RoutineInvocationContext<'_>,
    function: &SQLUserFunction,
    bound: Vec<Value>,
    invocation: &RoutineInvocationBinding,
) -> Result<RoutineOutcome, SQLError> {
    execute_entry(
        context,
        function,
        bound,
        invocation,
        RoutineEntry::CatalogCallback,
    )
}

fn execute_entry(
    context: &RoutineInvocationContext<'_>,
    function: &SQLUserFunction,
    bound: Vec<Value>,
    invocation: &RoutineInvocationBinding,
    entry: RoutineEntry<'_>,
) -> Result<RoutineOutcome, SQLError> {
    let (allow_nonatomic, record_target, require_execute) = match entry {
        RoutineEntry::Invocation {
            allow_nonatomic,
            record_target,
        } => (allow_nonatomic, record_target, true),
        RoutineEntry::CatalogCallback => (false, None, false),
    };
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
    if require_execute {
        uqa_sql::routines::security::ensure_routine_execute_privilege(
            context.authority,
            definition,
        )?;
    }
    super::scopes::with_routine_context(context.session, definition, || {
        let body = if definition.language == "plpgsql" {
            context.session.plpgsql_body(function, definition, None)?
        } else {
            context.lookup.routine_body(function)?
        };
        execute_compiled_body(context, function, definition, &body, bound, record_target)
    })
}

fn execute_compiled_body(
    context: &RoutineInvocationContext<'_>,
    function: &SQLUserFunction,
    definition: &CreateFunction,
    compiled: &CompiledFunctionBody,
    bound: Vec<Value>,
    record_target: Option<&[uqa_sql::routines::result_check::SQLFunctionResultColumn]>,
) -> Result<RoutineOutcome, SQLError> {
    match compiled {
        CompiledFunctionBody::PLpgSQL(parsed) => {
            execute_plpgsql_language(context, definition, parsed, bound)
        }
        CompiledFunctionBody::SQL(statements) => execute_sql_language(
            context,
            function,
            definition,
            statements,
            &bound,
            record_target,
        ),
    }
}

pub fn execute_trigger_routine(
    context: &RoutineInvocationContext<'_>,
    function: &SQLUserFunction,
    trigger: &TriggerRoutineContext,
) -> Result<Value, SQLError> {
    let _guard = DepthGuard::enter(context.session)?;
    let _transaction_context = RoutineTransactionGuard::enter(context.runtime.session, false);
    super::scopes::with_routine_context(context.session, &function.def, || {
        let body =
            context
                .session
                .plpgsql_body(function, &function.def, Some(trigger.relation_oid))?;
        let CompiledFunctionBody::PLpgSQL(parsed) = &*body else {
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
    function: &SQLUserFunction,
    definition: &CreateFunction,
    plans: &[uqa_sql::plan::UnifiedPlan],
    bound: &[Value],
    record_target: Option<&[uqa_sql::routines::result_check::SQLFunctionResultColumn]>,
) -> Result<RoutineOutcome, SQLError> {
    crate::routines::sql_body::execute_sql_language(
        context.runtime,
        context.types,
        &context.overloads,
        crate::routines::sql_body::SQLBody {
            function,
            definition,
            plans,
        },
        bound,
        record_target,
    )
}
