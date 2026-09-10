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

use std::collections::BTreeMap;
use std::sync::Arc;

use uqa_core::{DocId, Value};
use uqa_sql::ast::{
    AlterTableAction, AlterTableStmt, ColumnType, CreateTable, DropKind, DropStmt, Statement,
};
#[cfg(test)]
use uqa_sql::compile;
use uqa_sql::{SQLError, SQLParam, SQLResult};
use uqa_storage::document_store::Document;

use crate::Engine;

mod age_cypher;
mod aggregates;
mod catalog;
pub(crate) use catalog::snapshot_table_relation_oid;
pub(crate) use catalog::{rename_view_column_query, view_query_references_column};
mod api;
mod catalog_statement_routines;
mod completion;
mod copy;
mod correlation;
mod cte_validation;
mod cursor;
mod ddl;
pub(crate) mod dml;
mod domains;
mod driver;
mod from_rows;
mod generated;
mod hierarchy;
mod mutability;
pub(crate) mod plan_executor;
pub use uqa_sql::result::format_postgres_text;
mod planning;
mod plpgsql_exec;
mod prepared;
pub(crate) use prepared::{
    analyze_prepared_plan, infer_prepared_parameter_types, prepared_result_schema_matches,
};
mod read_only;
mod regrole_dependencies;
mod row_functions;
pub(crate) mod scalar;
mod select;
pub(crate) mod session_portal_worker;
mod triggers;

pub(crate) fn active_trigger_transition_relation_names() -> std::collections::BTreeSet<String> {
    triggers::current_transition_relation_names()
}
mod vacuum;
mod volatility;
mod window;

pub use catalog::{postgres_result_type, SQLTypeMetadata};
pub(crate) use catalog_statement_routines::{
    bind_catalog_statement_routines, collect_expression_routine_references,
    mark_catalog_statement_relations_bound,
};
pub use cursor::{SQLCursor, SQLCursorSummary};
pub(crate) use domains::{cast_domain_value, resolve_declared_column_type};
pub(crate) use driver::{execute, execute_nested};
use mutability::{
    is_transaction_control, query_may_mutate_engine, query_requires_statement_transaction,
};
#[cfg(test)]
use planning::compile_logical_plans;
use planning::lower_statement;
pub(super) use planning::{
    estimate_engine_plan, execute_compiled_statement,
    execute_compiled_statement_with_privilege_subject, optimize_engine_plan, optimize_engine_query,
    plan_for_execution,
};
pub(crate) use plpgsql_exec::{call_bound_user_scalar_function, call_user_scalar_function};
use select::query_has_row_locks;
pub(crate) use select::{execute_query_plan, RowLockRetryCache};
pub(crate) use triggers::fire_statement_triggers;
pub(crate) use triggers::{fire_deferred_constraint_trigger_event, DeferredConstraintTriggerEvent};

pub(crate) use catalog::query_source_column_names;
pub(crate) use catalog::{
    foreign_table_relation_oid, plpgsql_catalog, resolve_age_label_relation_name,
    resolve_bound_regclass_oid, resolve_catalog_column_type, resolve_catalog_column_type_name,
    resolve_catalog_domain_type_by_oid, resolve_regclass_kind_by_oid, resolve_regclass_oid,
    resolve_regnamespace_oid, resolve_regobject_oid, resolve_regprocedure_oid, resolve_regrole_oid,
    resolve_regtype_oid, resolve_regtype_output, runtime_constraints, schema_object_oid,
    sequence_relation_oid, view_relation_oid,
};
use ddl::{
    column_type_name, json_table_arg, json_table_value_to_text, json_to_core_value,
    run_alter_sequence, run_alter_table, run_create_index, run_create_sequence, run_create_table,
    run_create_table_as, run_create_table_if_not_exists, run_drop, CreateTableAsExecution,
};
pub(crate) use ddl::{
    convert_value_to_column_type, drop_column_cascade, drop_constraint_dependency,
    drop_index_dependency, validate_check_expression, validate_default_expression,
    validate_postgres_column_name, validate_postgres_relation_column_type,
    validate_vector_dimensions,
};
use dml::{index_vectors_for_type, run_delete, run_insert, run_merge, run_update};
use from_rows::engine_func_intercept;
pub(crate) use generated::{prepare_generated_columns, refresh_stored_generated_columns};
pub(in crate::sql) use hierarchy::{
    prospective_partition_bound_accepts_document, validate_new_partition_bound,
};
use plan_executor::UnifiedPlanExecutor;
pub(crate) use regrole_dependencies::{
    reject_stored_plan_regrole_constants, reject_stored_query_regrole_constants,
    reject_stored_regrole_constants, StoredRegroleConstants,
};
use row_functions::{
    execute_tree_entries, expect_column_name, expect_optional_graph_value,
    graph_betweenness_entries, graph_hits_entries, graph_pagerank_entries,
    run_age_alter_graph_with_evaluator, run_age_create_elabel_with_evaluator,
    run_age_create_graph_with_evaluator, run_age_create_vlabel_with_evaluator,
    run_age_drop_graph_with_evaluator, run_age_drop_label_with_evaluator,
    run_age_graph_exists_with_evaluator, run_graph_create_with_evaluator,
    run_graph_drop_with_evaluator,
};
pub(crate) use row_functions::{
    run_bayesian_match_with_prior_in_execution, run_bayesian_match_with_prior_public,
    run_calibrated_vector_match_public, run_multi_field_match_in_execution,
    run_multi_field_match_public,
};
use vacuum::run_vacuum;

pub(crate) fn map_physical_exec_error(error: uqa_execution::ExecError) -> SQLError {
    select::physical_exec_error(error)
}

pub(crate) fn execute_nested_optimized_command(
    engine: &Engine,
    command: &uqa_planner::CommandPlan,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    UnifiedPlanExecutor::new_nested(engine, params).execute(&uqa_planner::UnifiedPlan::Command(
        Box::new(command.clone()),
    ))
}

pub(in crate::sql) fn analyze_call_result_schema(
    engine: &Engine,
    name: &str,
    arguments: &[uqa_planner::ExpressionPlan],
    params: &[SQLParam],
) -> Result<Option<uqa_execution::RowSchema>, SQLError> {
    plan_executor::analyze_call_result_schema(engine, name, arguments, params)
}

pub(crate) fn call_bound_engine_builtin(
    engine: &Engine,
    binding: &uqa_sql::ast::FunctionBinding,
    arguments: &[(Option<String>, Value)],
) -> Option<Result<Value, SQLError>> {
    if !binding.builtin {
        return None;
    }
    let values = arguments
        .iter()
        .map(|(_, value)| value.clone())
        .collect::<Vec<_>>();
    from_rows::engine_catalog_scalar_value(engine, &binding.name, &values)
}
use select::run_explain;
pub(crate) use select::CteScope;
pub(crate) use session_portal_worker::start_session_portal_worker;
pub(crate) use uqa_sql::semantics::expr_is_null_free as expr_is_null_free_public;

type RowUpdateVectors = BTreeMap<String, Vec<Vec<f32>>>;

/// Analyze the declared RETURNING row type of a rewrite-rule action without
/// executing the action.
pub(crate) fn analyze_rule_action_returning_schema(
    engine: &Engine,
    statement: Statement,
) -> Result<Option<uqa_execution::RowSchema>, SQLError> {
    dml::dml_statement_returning_schema(engine, statement)
}

/// Bind every catalog-owned scalar and table-function call to an exact routine identity before the query plan is serialized.
pub(crate) fn bind_catalog_query_routines(
    engine: &Engine,
    query: &mut uqa_planner::QueryPlan,
    params: &[SQLParam],
) -> Result<uqa_execution::RowSchema, SQLError> {
    let ctes = crate::capabilities::query_scope::new_for_catalog_binding(engine);
    select::bind_query_plan_routines_for_storage(engine, query, params, &ctes, None)
}

/// Bind a catalog-owned query whose expressions may reference a statically typed routine parameter scope.
pub(crate) fn bind_catalog_query_routines_with_outer(
    engine: &Engine,
    query: &mut uqa_planner::QueryPlan,
    params: &[SQLParam],
    outer: &uqa_execution::RowSchema,
) -> Result<uqa_execution::RowSchema, SQLError> {
    let ctes = crate::capabilities::query_scope::new_for_catalog_binding(engine);
    select::bind_query_plan_routines_for_storage(engine, query, params, &ctes, Some(outer))
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

pub(crate) fn validate_stored_view_check_option(
    engine: &Engine,
    name: &str,
    view: &crate::StoredView,
) -> Result<(), SQLError> {
    dml::view_automatic::validate_view_definition_check_option(engine, name, view)
}

pub(crate) use uqa_sql::semantics::XMIN_COLUMN;

pub(crate) use uqa_execution::query::document_projection::{
    project_stored_document_column, projection_uses_tuple_xmin, projections_use_tuple_xmin,
};

pub(crate) use uqa_sql::semantics::builtin_function_dispatch_name;

use uqa_sql::semantics::doc_id_value;

#[cfg(test)]
#[path = "sql/tests.rs"]
mod tests;

pub(crate) use uqa_sql::semantics::merge_action_attribute;

pub(crate) use triggers::current_transition_relations;

pub(crate) use select::{attach_lock_rows, prepare_correlated_exists_predicate, ScopedEngineHook};

pub(crate) use plpgsql_exec::execute_trigger_routine;
