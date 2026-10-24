//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Invocation services for compiled SQL routines.

use crate::functions::SQLTableFunctionResult;
use crate::routines::invocation::RoutineProjectionResult;
use uqa_core::Value;
use uqa_sql::{ast::FunctionBinding, routines::SQLUserFunction, SQLError};

/// Execute a routine through the caller's transaction and session context.
pub trait SQLRoutineInvoker {
    fn has_user_function(
        &self,
        name: &str,
        binding: Option<&FunctionBinding>,
    ) -> Result<bool, SQLError>;
    /// Reuse successful metadata only when the adapter validates its current scope.
    fn has_user_function_with_descriptions(
        &self,
        name: &str,
        binding: Option<&FunctionBinding>,
        _descriptions: &uqa_sql::routines::invocation::descriptions::RoutineCallDescriptions,
    ) -> Result<bool, SQLError> {
        self.has_user_function(name, binding)
    }
    fn user_function_returns_set(
        &self,
        name: &str,
        binding: Option<&FunctionBinding>,
        arguments: &[(Option<String>, Value)],
    ) -> Option<Result<bool, SQLError>>;
    fn call_user_scalar_function(
        &self,
        name: &str,
        binding: Option<&FunctionBinding>,
        arguments: &[(Option<String>, Value)],
    ) -> Option<Result<Value, SQLError>>;
    fn call_user_table_function(
        &self,
        name: &str,
        binding: Option<&FunctionBinding>,
        arguments: &[(Option<String>, Value)],
    ) -> Option<Result<SQLTableFunctionResult, SQLError>>;
    /// Native adapters can resolve and execute one invocation. External adapters
    /// retain their existing classification, callbacks and disappearing-call errors.
    fn invoke_projection_function(
        &self,
        name: &str,
        binding: Option<&FunctionBinding>,
        arguments: &[(Option<String>, Value)],
        _descriptions: &uqa_sql::routines::invocation::descriptions::RoutineCallDescriptions,
    ) -> crate::ExecResult<RoutineProjectionResult> {
        let missing = |kind: &str| {
            crate::ExecError::Other(format!(
                "user function `{name}` disappeared during {kind}projection"
            ))
        };
        let returns_set = self
            .user_function_returns_set(name, binding, arguments)
            .ok_or_else(|| missing(""))??;
        if returns_set {
            self.call_user_table_function(name, binding, arguments)
                .ok_or_else(|| missing("set "))?
                .map(RoutineProjectionResult::Table)
                .map_err(Into::into)
        } else {
            self.call_user_scalar_function(name, binding, arguments)
                .ok_or_else(|| missing("scalar "))?
                .map(RoutineProjectionResult::Scalar)
                .map_err(Into::into)
        }
    }
    /// Enforce EXECUTE on a user routine that a FROM item calls, as `PostgreSQL` checks it when it initializes the scan, before the routine runs for any row.
    fn require_user_function_execute(&self, function: &SQLUserFunction) -> Result<(), SQLError>;
}
