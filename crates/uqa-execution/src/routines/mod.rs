//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! PL/pgSQL activation records, control flow, cursor loops, and diagnostics.

use diagnostics::{
    arm_matches, catchable, format_raise_message, looks_like_sqlstate, result_row_count,
    result_row_values, return_query_context_error, routine_message, strict_into_check,
    to_i64_value,
};
use std::collections::{BTreeSet, HashMap};
use transaction::DirectRoutineCommandGuard;
use uqa_core::{ArrayValue, Value};
use uqa_sql::assignment::routines::{coerce_routine_value, coerce_routine_value_from};
use uqa_sql::ast::{
    ColumnType, CreateFunction, CursorDirection, Expr, FetchCursorStmt, FunctionReturns, Statement,
};
use uqa_sql::expr::{cast_value_from, coercion_type_name, value_type_name};
use uqa_sql::plpgsql::runtime_diagnostics as diagnostics;
use uqa_sql::plpgsql::{
    bind_expr, bind_statement, condition_sqlstate, IntoTarget, PLpgSQLBlock, PLpgSQLCursorArgument,
    PLpgSQLCursorCount, PLpgSQLCursorOpen, PLpgSQLDatum, PLpgSQLFunction, PLpgSQLReturnValue,
    PLpgSQLRowField, PLpgSQLStmt, RaiseLevel, ResolvedVariable, VariableResolver,
};
use uqa_sql::type_resolution::canonical_routine_type_name;
use uqa_sql::{compile, SQLError, SQLParam, SQLResult};
pub mod context;
pub mod transaction;
pub use context::RoutineContext;
mod blocks;
mod control_flow;
mod cursors;
mod datum;
mod records;
mod resolver;
mod sql_runtime;
mod state;
mod statements;
mod transaction_control;
pub use records::shape_trigger_outcome;

pub struct TriggerRoutineContext {
    pub column_types: Vec<Option<uqa_sql::ast::ColumnType>>,
    pub old: Value,
    pub new: Value,
    pub name: String,
    pub when: String,
    pub level: String,
    pub operation: String,
    pub relation_oid: i64,
    pub table_name: String,
    pub table_schema: String,
    pub arguments: Vec<String>,
}
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
pub struct RoutineOutcome {
    pub value: Value,
    pub out_values: Vec<Value>,
    pub set_rows: Vec<Vec<Value>>,
    pub anonymous_record_column_types: Option<Vec<Option<ColumnType>>>,
}

/// Mutable activation record for one PL/pgSQL invocation.
pub struct Interpreter<'a> {
    services: RoutineContext<'a>,
    def: &'a CreateFunction,
    datums: &'a [PLpgSQLDatum],
    values: Vec<Value>,
    record_types: HashMap<usize, Vec<Option<ColumnType>>>,
    bindings: HashMap<String, Vec<usize>>,
    err_stack: Vec<(String, String)>,
    set_rows: Vec<Vec<Value>>,
    ret: Value,
    ret_record_types: Option<Vec<Option<ColumnType>>>,
    out_datums: Vec<usize>,
    found: Option<usize>,
    last_row_count: i64,
    is_set: bool,
}

/// Maps variable names and positional parameters onto an activation record.
struct DatumResolver<'a> {
    services: RoutineContext<'a>,
    datums: &'a [PLpgSQLDatum],
    values: &'a [Value],
    record_types: &'a HashMap<usize, Vec<Option<ColumnType>>>,
    bindings: &'a HashMap<String, Vec<usize>>,
    error: Option<&'a (String, String)>,
    param_count: usize,
}

pub mod arguments;
pub mod sql_body;
