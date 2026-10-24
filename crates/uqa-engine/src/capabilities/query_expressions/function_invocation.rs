//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Route physical function invocation through the engine's active routine context.

use super::ScopedEngineHook;
use uqa_core::Value;
use uqa_execution::functions::SQLTableFunctionResult;
use uqa_execution::query::table_functions::{TableFunctionCall, TableFunctionRows};
use uqa_execution::routines::invocation::{self, context::RoutineCallRetention};
use uqa_sql::{ast::FunctionBinding, SQLError, SQLParam};

impl uqa_execution::query::routine_invocation::SQLRoutineInvoker for ScopedEngineHook<'_> {
    fn has_user_function_with_descriptions(
        &self,
        name: &str,
        binding: Option<&FunctionBinding>,
        descriptions: &uqa_sql::routines::invocation::descriptions::RoutineCallDescriptions,
    ) -> Result<bool, SQLError> {
        let scope = || self.routine_description_scope();
        invocation::has_described_user_function(
            self.engine,
            name,
            binding,
            RoutineCallRetention {
                descriptions,
                scope: &scope,
                control: uqa_core::memory::ProductionControl::uncontrolled(),
            },
        )
    }

    fn invoke_projection_function(
        &self,
        name: &str,
        binding: Option<&FunctionBinding>,
        arguments: &[(Option<String>, Value)],
        descriptions: &uqa_sql::routines::invocation::descriptions::RoutineCallDescriptions,
    ) -> uqa_execution::ExecResult<invocation::RoutineProjectionResult> {
        let scope = || self.routine_description_scope();
        invocation::call_user_projection_function(
            &self.engine.routine_invocation_context(),
            name,
            binding,
            arguments,
            Some(RoutineCallRetention {
                descriptions,
                scope: &scope,
                control: uqa_core::memory::ProductionControl::uncontrolled(),
            }),
        )
        .ok_or_else(|| {
            uqa_execution::ExecError::Other(format!(
                "user function `{name}` disappeared during projection"
            ))
        })?
        .map_err(Into::into)
    }

    fn has_user_function(
        &self,
        name: &str,
        binding: Option<&FunctionBinding>,
    ) -> Result<bool, SQLError> {
        Ok(match binding {
            Some(binding) if binding.builtin => false,
            Some(binding) => self
                .engine
                .lookup_bound_sql_functions_by_binding(binding)
                .is_some(),
            None => self.engine.lookup_visible_sql_functions(name)?.is_some(),
        })
    }
    fn user_function_returns_set(
        &self,
        name: &str,
        binding: Option<&FunctionBinding>,
        arguments: &[(Option<String>, Value)],
    ) -> Option<Result<bool, SQLError>> {
        match binding {
            Some(binding) => {
                crate::capabilities::routine_invocation::resolved_bound_user_function_returns_set(
                    self.engine,
                    binding,
                    arguments,
                )
            }
            None => crate::capabilities::routine_invocation::resolved_user_function_returns_set(
                self.engine,
                name,
                arguments,
            ),
        }
    }
    fn call_user_scalar_function(
        &self,
        name: &str,
        binding: Option<&FunctionBinding>,
        arguments: &[(Option<String>, Value)],
    ) -> Option<Result<Value, SQLError>> {
        match binding {
            Some(binding) => {
                crate::capabilities::routine_invocation::call_bound_user_scalar_function(
                    self.engine,
                    binding,
                    arguments,
                )
            }
            None => crate::capabilities::routine_invocation::call_user_scalar_function(
                self.engine,
                name,
                arguments,
            ),
        }
    }
    fn call_user_table_function(
        &self,
        name: &str,
        binding: Option<&FunctionBinding>,
        arguments: &[(Option<String>, Value)],
    ) -> Option<Result<SQLTableFunctionResult, SQLError>> {
        match binding {
            Some(binding) => {
                crate::capabilities::routine_invocation::call_bound_user_table_function(
                    self.engine,
                    binding,
                    arguments,
                    None,
                )
            }
            None => crate::capabilities::routine_invocation::call_user_table_function(
                self.engine,
                name,
                arguments,
                None,
            ),
        }
    }
    fn require_user_function_execute(
        &self,
        function: &uqa_sql::routines::SQLUserFunction,
    ) -> Result<(), SQLError> {
        uqa_sql::routines::security::ensure_routine_execute_privilege(self.engine, &function.def)
    }
}

impl uqa_execution::query::set_projection::SetFunctionRuntime for ScopedEngineHook<'_> {
    fn has_registered_table_function(&self, name: &str) -> bool {
        self.engine.has_registered_table_function(name)
    }

    fn table_function_rows(
        &self,
        call: TableFunctionCall<'_>,
        params: &[SQLParam],
        row: Option<&uqa_execution::OwnedPhysicalRow>,
    ) -> Result<TableFunctionRows, SQLError> {
        let context =
            self.engine
                .table_function_context(params, self, self, &self.ctes.scalar_subqueries);
        uqa_execution::query::table_functions::build_table_function_row_stream_with_row(
            &context, call, row,
        )
    }
}
