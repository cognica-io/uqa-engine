//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Invocation services for compiled SQL routines.

use crate::functions::SQLTableFunctionResult;
use uqa_core::Value;
use uqa_sql::{ast::FunctionBinding, SQLError};

/// Execute a routine through the caller's transaction and session context.
pub trait SQLRoutineInvoker {
    fn has_user_function(
        &self,
        name: &str,
        binding: Option<&FunctionBinding>,
    ) -> Result<bool, SQLError>;
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
}
