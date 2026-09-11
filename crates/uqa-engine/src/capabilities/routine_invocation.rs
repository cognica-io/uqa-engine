//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Compose routine invocation with the active interpreter, catalog, and retained session state.
use crate::{Engine, SQLTableFunctionResult};
use uqa_core::Value;
use uqa_execution::routines::{
    invocation::{
        self,
        context::{
            AnonymousBlockContext, RoutineInvocationContext, RoutineInvocationSession,
            RoutineInvocationState,
        },
    },
    TriggerRoutineContext,
};
use uqa_sql::{
    ast::FunctionBinding,
    routines::{resolution::RoutineOverloadContext, SQLUserFunction},
    SQLError,
};
impl RoutineInvocationState for crate::roles::RoutineSessionStateGuard<'_> {
    fn preserve_current_user(&mut self) {
        self.preserve_current_user();
    }
}
impl RoutineInvocationSession for Engine {
    fn depth_limit(&self) -> usize {
        self.sql_function_depth_limit()
    }
    fn state_guard(&self) -> Box<dyn RoutineInvocationState + '_> {
        Box::new(self.routine_session_state_guard())
    }
    fn set_current_user(&self, user: &str) {
        self.session.state.write().current_user = user.to_string();
    }
    fn set_variable(&self, name: &str, value: &str) -> Result<(), SQLError> {
        Engine::set_variable(self, name, value)
    }
}
impl Engine {
    pub(crate) fn routine_invocation_context(&self) -> RoutineInvocationContext<'_> {
        RoutineInvocationContext {
            runtime: self.routine_execution_context(),
            session: self,
            lookup: self,
            overloads: RoutineOverloadContext { catalog: self },
            types: self,
            authority: self,
        }
    }
    pub(crate) fn anonymous_block_context(&self) -> AnonymousBlockContext<'_> {
        AnonymousBlockContext {
            runtime: self.routine_execution_context(),
            session: self,
            types: self,
            parsers: self,
        }
    }
}
type AnonymousRecordDefinition<'a> = (&'a [String], &'a [String]);
pub(crate) fn call_user_scalar_function(
    engine: &Engine,
    name: &str,
    args: &[(Option<String>, Value)],
) -> Option<Result<Value, SQLError>> {
    invocation::call_user_scalar_function(&engine.routine_invocation_context(), name, args)
}
pub(crate) fn call_bound_user_scalar_function(
    engine: &Engine,
    binding: &FunctionBinding,
    args: &[(Option<String>, Value)],
) -> Option<Result<Value, SQLError>> {
    invocation::call_bound_user_scalar_function(&engine.routine_invocation_context(), binding, args)
}
pub(crate) fn resolved_user_function_returns_set(
    engine: &Engine,
    name: &str,
    args: &[(Option<String>, Value)],
) -> Option<Result<bool, SQLError>> {
    invocation::resolved_user_function_returns_set(&engine.routine_invocation_context(), name, args)
}
pub(crate) fn resolved_bound_user_function_returns_set(
    engine: &Engine,
    binding: &FunctionBinding,
    args: &[(Option<String>, Value)],
) -> Option<Result<bool, SQLError>> {
    invocation::resolved_bound_user_function_returns_set(
        &engine.routine_invocation_context(),
        binding,
        args,
    )
}
pub(crate) fn call_user_table_function(
    engine: &Engine,
    name: &str,
    args: &[(Option<String>, Value)],
    record_definition: Option<AnonymousRecordDefinition<'_>>,
) -> Option<Result<SQLTableFunctionResult, SQLError>> {
    invocation::call_user_table_function(
        &engine.routine_invocation_context(),
        name,
        args,
        record_definition,
    )
}
pub(crate) fn call_bound_user_table_function(
    engine: &Engine,
    binding: &FunctionBinding,
    args: &[(Option<String>, Value)],
    record_definition: Option<AnonymousRecordDefinition<'_>>,
) -> Option<Result<SQLTableFunctionResult, SQLError>> {
    invocation::call_bound_user_table_function(
        &engine.routine_invocation_context(),
        binding,
        args,
        record_definition,
    )
}
pub(crate) fn execute_trigger_routine(
    engine: &Engine,
    function: &SQLUserFunction,
    context: &TriggerRoutineContext,
) -> Result<Value, SQLError> {
    invocation::execute_trigger_routine(&engine.routine_invocation_context(), function, context)
}

pub(crate) fn analyze_call_result_schema(
    engine: &Engine,
    name: &str,
    arguments: &[uqa_sql::plan::ExpressionPlan],
    params: &[uqa_sql::SQLParam],
) -> Result<Option<uqa_sql::RowSchema>, SQLError> {
    let analysis = uqa_sql::routines::call::ProcedureCallAnalysis::new(arguments)?;
    let scope = crate::capabilities::query_scope::new_for_current_routine(engine);
    analysis.result_schema(
        name,
        &RoutineOverloadContext { catalog: engine },
        engine,
        &mut |argument| {
            uqa_execution::query::binding::bind_expression_plan_type(
                engine, argument, params, &scope,
            )
        },
    )
}
