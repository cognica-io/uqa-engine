//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Route physical function invocation through the engine's active routine context.

use super::callbacks::ScopedEngineHook;
use uqa_core::Value;
use uqa_execution::functions::SQLTableFunctionResult;
use uqa_execution::query::table_functions::{TableFunctionCall, TableFunctionRows};
use uqa_sql::{ast::FunctionBinding, SQLError, SQLParam};

impl uqa_execution::query::routine_invocation::SQLRoutineInvoker for ScopedEngineHook<'_> {
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
        let context = crate::sql::from_rows::SourceEvalContext::new(
            self.engine,
            params,
            self,
            self,
            &self.ctes.scalar_subqueries,
        );
        crate::sql::from_rows::build_table_function_row_stream_with_row(&context, call, row)
    }
}
