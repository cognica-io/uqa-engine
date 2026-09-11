//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `Engine::sql` driver: parse SQL via `uqa_sql::compile`, lower each
//! statement onto the engine's mutation / search APIs, and roll the
//! result rows into a [`SQLResult`].
//!
//! The SQL surface covers table DDL/DML, indexes, joins, CTEs, windows,
//! aggregates, graph functions, retrieval functions, and engine-registered
//! Rust functions. Unsupported statements return
//! [`uqa_sql::SQLError::Unsupported`] cleanly instead of silently falling
//! through.

#![allow(
    clippy::useless_format,
    clippy::manual_let_else,
    clippy::needless_pass_by_value,
    clippy::unnecessary_wraps,
    clippy::items_after_statements,
    clippy::unnecessary_map_or,
    clippy::match_same_arms,
    clippy::unnested_or_patterns
)]

#[cfg(test)]
use std::sync::Arc;

use uqa_sql::ast::Statement;
#[cfg(test)]
use uqa_sql::compile;
use uqa_sql::{SQLError, SQLParam, SQLResult};

use crate::Engine;

mod api;
mod catalog;
mod catalog_statement_routines;
mod from_rows;
mod generated;
#[cfg(test)]
mod mutability;
pub use uqa_sql::result::format_postgres_text;
mod planning;
mod regrole_dependencies;
pub(crate) mod scalar;
mod select;
pub(crate) mod session_portal_worker;
mod triggers;

pub(crate) fn active_trigger_transition_relation_names() -> std::collections::BTreeSet<String> {
    triggers::current_transition_relation_names()
}
mod window;

pub(crate) use crate::capabilities::routine_invocation::call_user_scalar_function;
pub use catalog::{postgres_result_type, SQLTypeMetadata};
pub(crate) use catalog_statement_routines::{
    bind_catalog_statement_routines, collect_expression_routine_references,
};
#[cfg(test)]
use mutability::{query_may_mutate_engine, query_requires_statement_transaction};
#[cfg(test)]
use planning::compile_logical_plans;
#[cfg(test)]
use planning::lower_statement;
pub(super) use planning::{
    estimate_engine_plan, execute_compiled_statement,
    execute_compiled_statement_with_privilege_subject, optimize_engine_plan, optimize_engine_query,
    plan_for_execution,
};
use select::query_has_row_locks;
pub(crate) use select::RowLockRetryCache;
pub(crate) use triggers::{fire_deferred_constraint_trigger_event, DeferredConstraintTriggerEvent};
pub use uqa_execution::query::cursor::{SQLCursor, SQLCursorSummary};

pub(crate) use catalog::{
    resolve_age_label_relation_name, resolve_catalog_column_type, resolve_catalog_column_type_name,
    resolve_regclass_oid, resolve_regnamespace_oid, resolve_regobject_oid,
    resolve_regprocedure_oid, resolve_regrole_oid, resolve_regtype_oid, resolve_regtype_output,
    runtime_constraints, sequence_relation_oid,
};
pub(crate) use generated::{prepare_generated_columns, refresh_stored_generated_columns};
pub(crate) type UnifiedPlanExecutor<'engine, 'params> =
    uqa_execution::statement::plan_executor::UnifiedPlanExecutor<
        'engine,
        'params,
        crate::session::StatementReadSnapshot,
    >;
pub(crate) use regrole_dependencies::{
    reject_stored_plan_regrole_constants, reject_stored_regrole_constants,
};
pub(crate) use uqa_sql::assignment::conversion::{
    convert_value_to_column_type, validate_vector_dimensions,
};
pub(crate) use uqa_sql::schema::columns::{
    validate_postgres_column_name, validate_postgres_relation_column_type,
};

pub(crate) fn map_physical_exec_error(error: uqa_execution::ExecError) -> SQLError {
    select::physical_exec_error(error)
}

pub(crate) fn execute_nested_optimized_command(
    engine: &Engine,
    command: &uqa_planner::CommandPlan,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    UnifiedPlanExecutor::new_nested(engine.statement_execution_context(), params).execute(
        &uqa_planner::UnifiedPlan::Command(Box::new(command.clone())),
    )
}

pub(in crate::sql) use crate::capabilities::routine_invocation::analyze_call_result_schema;

pub(crate) use select::CteScope;
pub(crate) use session_portal_worker::start_session_portal_worker;

/// Analyze the declared RETURNING row type of a rewrite-rule action without executing the action.
pub(crate) fn analyze_rule_action_returning_schema(
    engine: &Engine,
    statement: Statement,
) -> Result<Option<uqa_execution::RowSchema>, SQLError> {
    uqa_sql::semantics::returning::dml_statement_returning_schema(
        engine.returning_analysis_context(),
        statement,
    )
}

/// Bind a catalog-owned scalar expression, including all nested query plans, against a statically typed outer row.
pub(crate) fn bind_catalog_expression_routines_with_outer(
    engine: &Engine,
    expression: &mut uqa_planner::ExpressionPlan,
    params: &[SQLParam],
    outer: &uqa_execution::RowSchema,
) -> Result<Option<uqa_sql::ast::ColumnType>, SQLError> {
    let ctes = crate::capabilities::query_scope::new_for_catalog_binding(engine);
    select::bind_expression_plan_routines_for_storage(engine, expression, params, &ctes, outer)
}

pub(crate) use uqa_sql::semantics::XMIN_COLUMN;

pub(crate) use uqa_execution::query::document_projection::{
    project_stored_document_column, projection_uses_tuple_xmin, projections_use_tuple_xmin,
};

pub(crate) use uqa_sql::semantics::builtin_function_dispatch_name;

#[cfg(test)]
#[path = "sql/tests.rs"]
mod tests;

pub(crate) use triggers::current_transition_relations;

pub(crate) use select::{attach_lock_rows, ScopedEngineHook};

pub(crate) use crate::capabilities::routine_invocation::execute_trigger_routine;
