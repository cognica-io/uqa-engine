//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Execution of user-defined routines.
//!
//! Statement entry points, overload resolution, routine execution, interpreter
//! state, control flow, dynamic SQL, and diagnostics live in focused modules.

use crate::user_functions::{routine_local_name, CompiledFunctionBody, SQLUserFunction};
use crate::{Engine, SQLTableFunctionResult};
use std::cell::Cell;
use std::sync::Arc;
use uqa_core::Value;
use uqa_sql::ast::{CreateFunction, DropFunctionStmt, FunctionBinding, FunctionReturns};
use uqa_sql::expr::value_type_name;
use uqa_sql::plpgsql::PLpgSQLDatum;
use uqa_sql::{ResultRow, SQLError, SQLResult};

mod handlers;
mod resolution;
mod routine;

pub(crate) use handlers::{
    call_bound_user_scalar_function, call_bound_user_table_function, call_user_scalar_function,
    call_user_table_function, resolved_bound_user_function_returns_set,
    resolved_user_function_returns_set,
};
pub(super) use handlers::{run_call, run_create_function, run_do_block, run_drop_function};
pub(crate) use routine::execute_trigger_routine;

use resolution::{
    call_signature, coerce_routine_value, output_column_names, resolve_bound_routine,
    resolve_routine, routine_resolution_error, ResolvedRoutine,
};
use routine::{execute_routine, DepthGuard, RoutineTransactionGuard};

pub(super) use uqa_execution::routines::{Interpreter, RoutineOutcome};
