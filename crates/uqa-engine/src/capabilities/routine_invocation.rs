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
    ast::{CreateFunction, DropFunctionStmt, FunctionBinding},
    routines::{resolution::RoutineOverloadContext, SQLUserFunction},
    SQLError, SQLResult,
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
    fn routine_invocation_context(&self) -> RoutineInvocationContext<'_> {
        RoutineInvocationContext {
            runtime: self.routine_execution_context(),
            session: self,
            lookup: self,
            overloads: RoutineOverloadContext { catalog: self },
            types: self,
            authority: self,
        }
    }
    fn anonymous_block_context(&self) -> AnonymousBlockContext<'_> {
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
pub(crate) fn run_call(
    engine: &Engine,
    name: &str,
    call_args: &[(Option<String>, Value)],
    argument_types: &[Option<uqa_sql::ast::ColumnType>],
    explicit_variadic: bool,
    nested_statement: bool,
) -> Result<SQLResult, SQLError> {
    invocation::run_call(
        &engine.routine_invocation_context(),
        name,
        call_args,
        argument_types,
        explicit_variadic,
        nested_statement,
    )
}
pub(crate) fn execute_trigger_routine(
    engine: &Engine,
    function: &SQLUserFunction,
    context: &TriggerRoutineContext,
) -> Result<Value, SQLError> {
    invocation::execute_trigger_routine(&engine.routine_invocation_context(), function, context)
}
pub(crate) fn run_do_block(
    engine: &Engine,
    language: &str,
    body: &str,
    nested_statement: bool,
) -> Result<SQLResult, SQLError> {
    invocation::run_do_block(
        &engine.anonymous_block_context(),
        language,
        body,
        nested_statement,
    )
}
pub(crate) fn run_create_function(
    engine: &Engine,
    def: CreateFunction,
) -> Result<SQLResult, SQLError> {
    engine.register_sql_function(def)?;
    Ok(SQLResult::empty())
}

pub(crate) fn run_drop_function(
    engine: &Engine,
    stmt: &DropFunctionStmt,
) -> Result<SQLResult, SQLError> {
    engine.drop_sql_functions(stmt)?;
    Ok(SQLResult::empty())
}
