//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Execution of user-defined routines.
//!
//! Statement entry points, overload resolution, routine execution, interpreter
//! state, control flow, dynamic SQL, and diagnostics live in focused modules.

use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use uqa_core::Value;
use uqa_sql::ast::{
    CreateFunction, DropFunctionStmt, Expr, FunctionBinding, FunctionReturns, Statement,
};
use uqa_sql::expr::{cast_value, truthy, value_type_name};
use uqa_sql::plpgsql::{
    bind_expr, bind_statement, condition_sqlstate, condition_sqlstates, IntoTarget, PLpgSQLBlock,
    PLpgSQLCursorArgument, PLpgSQLDatum, PLpgSQLFunction, PLpgSQLReturnValue, PLpgSQLRowField,
    PLpgSQLStmt, RaiseLevel, VariableResolver,
};
use uqa_sql::{ResultRow, SQLError, SQLParam, SQLResult};

use crate::engine_user_functions::{
    canonical_routine_type_name, routine_local_name, routine_signature_types, CompiledFunctionBody,
    SQLUserFunction,
};
use crate::{Engine, SQLTableFunctionResult};

use super::execute_compiled_statement;
use super::plan_executor::UnifiedPlanExecutor;
use super::scalar::eval_lowered_expression;

mod blocks;
mod control_flow;
mod cursors;
mod datum;
mod diagnostics;
mod handlers;
mod resolution;
mod resolver;
mod routine;
mod sql_runtime;
mod state;
mod statements;

pub(crate) use handlers::{
    call_bound_user_scalar_function, call_user_scalar_function, call_user_table_function,
    resolved_user_function_returns_set,
};
pub(super) use handlers::{run_call, run_create_function, run_do_block, run_drop_function};

use diagnostics::{
    arm_matches, catchable, format_raise_message, looks_like_sqlstate, result_row_count,
    return_query_context_error, routine_message, row_value, strict_into_check, to_i64_value,
};
use resolution::{
    best_effort_cast, call_signature, output_column_names, resolve_bound_routine, resolve_routine,
    routine_resolution_error,
};
use routine::{execute_routine, DepthGuard};

/// Control-flow signal propagated by statement execution.
enum Flow {
    Normal,
    Exit(Option<String>),
    Continue(Option<String>),
    Return,
}

/// Flow classification of one loop iteration.
enum LoopSignal {
    Continue,
    Break,
    Propagate(Flow),
}

/// Result of one routine execution before caller-context shaping.
struct RoutineOutcome {
    value: Value,
    out_values: Vec<Value>,
    set_rows: Vec<Vec<Value>>,
}

#[derive(Clone)]
struct PLpgSQLCursorState {
    result: SQLResult,
    position: usize,
}

/// Mutable activation record for one PL/pgSQL invocation.
struct Interpreter<'a> {
    engine: &'a Engine,
    def: &'a CreateFunction,
    datums: &'a [PLpgSQLDatum],
    values: Vec<Value>,
    bindings: HashMap<String, Vec<usize>>,
    err_stack: Vec<(String, String)>,
    set_rows: Vec<Vec<Value>>,
    ret: Value,
    out_datums: Vec<usize>,
    found: Option<usize>,
    last_row_count: i64,
    is_set: bool,
    cursors: HashMap<String, PLpgSQLCursorState>,
    next_cursor_id: usize,
}

/// Maps variable names and positional parameters onto an activation record.
struct DatumResolver<'a> {
    datums: &'a [PLpgSQLDatum],
    values: &'a [Value],
    bindings: &'a HashMap<String, Vec<usize>>,
    error: Option<&'a (String, String)>,
    param_count: usize,
}

#[cfg(test)]
mod tests;
