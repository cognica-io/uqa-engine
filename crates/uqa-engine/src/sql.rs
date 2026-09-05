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

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, LazyLock};

use uqa_core::{DecimalValue, DocId, TemporalValue, Value};
use uqa_sql::ast::{
    AlterTableAction, AlterTableStmt, BinaryOp, ColumnType, CreateIndex, CreateTable, DropKind,
    DropStmt, ForeignKey, ForeignKeyAction, ForeignKeyMatch, SetOpKind, Statement,
};
use uqa_sql::expr::{value_to_tensor, value_to_vector};
use uqa_sql::{compile, ResultRow, SQLError, SQLParam, SQLResult};
use uqa_storage::document_store::{Document, DocumentMetadata, StoredDocument};

use crate::{Engine, HNSWIndexParams, IVFIndexParams, ScoredEntry, VectorIndexSpec};

mod age_cypher;
mod aggregates;
mod catalog;
pub(crate) use catalog::snapshot_table_relation_oid;
mod catalog_statement_routines;
mod copy;
mod correlation;
mod cursor;
mod ddl;
pub(crate) mod dml;
mod driver;
mod engine_api;
mod from_rows;
mod generated;
mod hierarchy;
mod mutability;
mod plan_executor;
mod planning;
mod plpgsql_exec;
mod read_only;
mod regrole_dependencies;
mod row_functions;
mod rules;
mod scalar;
mod select;
mod session_portal_worker;
mod triggers;

pub(crate) fn active_trigger_transition_relation_names() -> std::collections::BTreeSet<String> {
    triggers::current_transition_relation_names()
}
mod vacuum;
mod volatility;
mod where_eval;
mod window;

pub(crate) use catalog_statement_routines::{
    bind_catalog_statement_routines, collect_expression_routine_references,
    mark_catalog_statement_relations_bound, BoundRoutineReference,
};
pub use cursor::{SQLCursor, SQLCursorSummary};
pub(crate) use driver::{execute, execute_nested};
use mutability::{
    is_transaction_control, query_may_mutate_engine, query_requires_statement_transaction,
};
#[cfg(test)]
use planning::compile_logical_plans;
use planning::lower_statement;
pub(super) use planning::{
    execute_compiled_statement, execute_compiled_statement_with_privilege_subject,
    optimize_engine_plan,
};
pub(crate) use plpgsql_exec::{call_bound_user_scalar_function, call_user_scalar_function};
use select::query_has_row_locks;
pub(crate) use select::{execute_query_plan, RowLockRetryCache};
pub(crate) use triggers::fire_statement_triggers;
pub(crate) use triggers::{fire_deferred_constraint_trigger_event, DeferredConstraintTriggerEvent};

use aggregates::{
    aggregate_value, contains_aggregate, has_aggregate, projection_label_at, AggregateAccumulator,
    PhysicalAggregateExecutor,
};
use catalog::build_info_schema_rows;
pub(crate) use catalog::query_source_column_names;
pub(crate) use catalog::{
    foreign_table_relation_oid, resolve_age_label_relation_name, resolve_catalog_column_type,
    resolve_catalog_column_type_name, resolve_regclass_kind_by_oid, resolve_regclass_oid,
    resolve_regnamespace_oid, resolve_regobject_oid, resolve_regprocedure_oid, resolve_regrole_oid,
    resolve_regtype_output, runtime_constraints, schema_object_oid, sequence_relation_oid,
    view_relation_oid, RegtypeOutputCatalog,
};
pub(in crate::sql) use catalog::{virtual_relation_accepts_row_lock, virtual_relation_schema};
pub(crate) use ddl::{
    bind_stored_check_expression_routines, bind_stored_schema_expression_routines,
    convert_value_to_column_type, convert_value_to_column_type_with_engine,
    drop_constraint_dependency, validate_check_expression, validate_default_expression,
    validate_postgres_column_name, validate_postgres_relation_column_type,
    validate_vector_dimensions,
};
use ddl::{
    coerce_to_column_type, column_type_name, core_value_to_json, json_table_arg,
    json_table_value_to_text, json_to_core_value, run_alter_sequence, run_alter_table,
    run_create_index, run_create_sequence, run_create_table, run_create_table_as, run_drop,
    value_to_text, CreateTableAsExecution,
};
use dml::{index_vectors_for_type, run_delete, run_insert, run_merge, run_update};
use from_rows::{build_join_spill_with_ctes, engine_func_intercept, ColumnPrune, QualifierFilters};
pub(crate) use generated::{prepare_generated_columns, refresh_stored_generated_columns};
pub(in crate::sql) use hierarchy::{
    partition_constraint_accepts_document, partition_insert_target,
    prospective_partition_bound_accepts_document, validate_hash_partition_spec,
    validate_new_partition_bound,
};
use plan_executor::UnifiedPlanExecutor;
pub(crate) use regrole_dependencies::{
    reject_stored_plan_regrole_constants, reject_stored_query_regrole_constants,
    reject_stored_regrole_constants, StoredRegroleConstants,
};
use row_functions::{
    execute_function, execute_function_with_top_k, execute_tree_entries, expect_column_name,
    expect_optional_graph_value, graph_betweenness_entries, graph_hits_entries,
    graph_pagerank_entries, is_semantic_field_argument, run_age_alter_graph_with_evaluator,
    run_age_create_elabel_with_evaluator, run_age_create_graph_with_evaluator,
    run_age_create_vlabel_with_evaluator, run_age_drop_graph_with_evaluator,
    run_age_drop_label_with_evaluator, run_age_graph_exists_with_evaluator,
    run_graph_create_with_evaluator, run_graph_drop_with_evaluator,
    validate_expr_text_match_fields, validate_joined_expr_text_match_fields,
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
pub(crate) use select::CteScope;
use select::{
    bind_projection_output_schema, build_projection_physical_row_with_ctes, projection_columns,
    run_explain, ScopedEngineHook,
};
pub(crate) use session_portal_worker::start_session_portal_worker;
use where_eval::execute_mixed_where;
pub(crate) use where_eval::expr_is_null_free as expr_is_null_free_public;
use window::{has_window, prepare_window_plan, PhysicalWindowExecutor};

type RowUpdateValues = BTreeMap<String, Value>;
type RowUpdateVectors = BTreeMap<String, Vec<Vec<f32>>>;
type RowIndependentUpdateValues = (RowUpdateValues, RowUpdateVectors);

pub(crate) fn analyze_query_schema_with_catalog(
    engine: &Engine,
    query: &uqa_planner::QueryPlan,
    params: &[SQLParam],
    catalog: crate::engine_capabilities::CatalogReadView,
    resolution: crate::engine_capabilities::RelationNameResolution,
) -> Result<uqa_execution::RowSchema, SQLError> {
    select::analyze_query_plan_schema_with_catalog(engine, query, params, catalog, resolution)
}

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
    let ctes = CteScope::new_for_catalog_binding(engine);
    select::bind_query_plan_routines_for_storage(engine, query, params, &ctes, None)
}

/// Bind a catalog-owned query whose expressions may reference a statically typed routine parameter scope.
pub(crate) fn bind_catalog_query_routines_with_outer(
    engine: &Engine,
    query: &mut uqa_planner::QueryPlan,
    params: &[SQLParam],
    outer: &uqa_execution::RowSchema,
) -> Result<uqa_execution::RowSchema, SQLError> {
    let ctes = CteScope::new_for_catalog_binding(engine);
    select::bind_query_plan_routines_for_storage(engine, query, params, &ctes, Some(outer))
}

/// Bind a catalog-owned scalar expression, including all nested query plans, against a statically typed outer row.
pub(crate) fn bind_catalog_expression_routines_with_outer(
    engine: &Engine,
    expression: &mut uqa_planner::ExpressionPlan,
    params: &[SQLParam],
    outer: &uqa_execution::RowSchema,
) -> Result<Option<uqa_sql::ast::ColumnType>, SQLError> {
    let ctes = CteScope::new_for_catalog_binding(engine);
    select::bind_expression_plan_routines_for_storage(engine, expression, params, &ctes, outer)
}

pub(crate) fn validate_stored_view_check_option(
    engine: &Engine,
    name: &str,
    view: &crate::StoredView,
) -> Result<(), SQLError> {
    dml::view_automatic::validate_view_definition_check_option(engine, name, view)
}

const SCORE_COLUMN: &str = "_score";
pub(in crate::sql) const DOC_ID_COLUMN: &str = "_doc_id";
pub(in crate::sql) const TABLE_OID_COLUMN: &str = "tableoid";
pub(crate) const XMIN_COLUMN: &str = "xmin";

pub(crate) fn projection_uses_tuple_xmin(
    column: &str,
    definitions: &[uqa_sql::ast::ColumnDef],
) -> bool {
    column == XMIN_COLUMN
        && !definitions
            .iter()
            .any(|definition| definition.name == XMIN_COLUMN)
}

pub(crate) fn projections_use_tuple_xmin(
    columns: &[String],
    definitions: &[uqa_sql::ast::ColumnDef],
) -> bool {
    columns
        .iter()
        .any(|column| projection_uses_tuple_xmin(column, definitions))
}

pub(crate) fn project_document_column(
    document: &Document,
    metadata: DocumentMetadata,
    column: &str,
    definitions: &[uqa_sql::ast::ColumnDef],
) -> Value {
    if !projection_uses_tuple_xmin(column, definitions) {
        return document.get(column).cloned().unwrap_or(Value::Null);
    }
    if definitions.is_empty() {
        if let Some(value) = document.get(column) {
            return value.clone();
        }
    }
    metadata
        .tuple_xmin()
        .map_or(Value::Null, |xmin| Value::Int(i64::from(xmin)))
}

pub(crate) fn project_stored_document_column(
    document: &StoredDocument,
    column: &str,
    definitions: &[uqa_sql::ast::ColumnDef],
) -> Value {
    project_document_column(document.fields(), document.metadata(), column, definitions)
}

pub(in crate::sql) const META_QUALIFIER: &str = "_meta";
pub(in crate::sql) const META_DOC_ID_COLUMN: &str = "doc_id";
pub(in crate::sql) const META_SCORE_COLUMN: &str = "score";

/// Executor-only carrier for `PostgreSQL` 18's `merge_action()` value. The attribute has no SQL name and therefore cannot collide with a target or source column named `_merge_action`.
pub(in crate::sql) fn merge_action_attribute() -> uqa_sql::ast::InternalColumnRef {
    static ATTRIBUTE: LazyLock<uqa_sql::ast::InternalColumnRef> =
        LazyLock::new(|| uqa_sql::ast::InternalRelationId::allocate().column(0));
    *ATTRIBUTE
}
/// Resolve reserved system-schema aliases only when the local name belongs to
/// that schema's built-in surface. Ordinary qualified names stay intact for
/// runtime callbacks and user-defined routine lookup.
pub(crate) fn builtin_function_dispatch_name(name: &str) -> String {
    let lower = name.to_ascii_lowercase();
    let Some((schema, local)) = lower.split_once('.') else {
        return lower;
    };
    let is_builtin = match schema {
        "ag_catalog" => matches!(
            local,
            "cypher"
                | "create_graph"
                | "drop_graph"
                | "graph_exists"
                | "create_vlabel"
                | "create_elabel"
                | "drop_label"
                | "alter_graph"
        ),
        "pg_catalog" => {
            uqa_sql::registry::is_registered(local)
                || matches!(
                    local,
                    "generate_series"
                        | "unnest"
                        | "regexp_split_to_table"
                        | "string_to_table"
                        | "json_array_elements"
                        | "jsonb_array_elements"
                        | "json_array_elements_text"
                        | "jsonb_array_elements_text"
                        | "json_each"
                        | "jsonb_each"
                        | "json_each_text"
                        | "jsonb_each_text"
                        | "json_object_keys"
                        | "jsonb_object_keys"
                        | "bit_length"
                        | "char_length"
                        | "character_length"
                        | "crc32"
                        | "crc32c"
                        | "gamma"
                        | "json_strip_nulls"
                        | "jsonb_strip_nulls"
                        | "length"
                        | "lgamma"
                        | "md5"
                        | "octet_length"
                        | "reverse"
                        | "random"
                        | "setseed"
                        | "nextval"
                        | "currval"
                        | "lastval"
                        | "setval"
                        | "current_schema"
                        | "current_schemas"
                        | "pg_backend_pid"
                        | "pg_listening_channels"
                        | "pg_notify"
                        | "pg_notification_queue_usage"
                        | "pg_get_expr"
                        | "pg_get_partkeydef"
                        | "pg_get_serial_sequence"
                        | "pg_get_triggerdef"
                        | "pg_get_ruledef"
                        | "pg_has_role"
                        | "has_table_privilege"
                        | "has_column_privilege"
                        | "has_database_privilege"
                        | "has_schema_privilege"
                        | "has_sequence_privilege"
                )
        }
        _ => false,
    };
    if is_builtin {
        local.to_string()
    } else {
        lower
    }
}

fn doc_id_value(doc_id: DocId) -> Result<Value, SQLError> {
    i64::try_from(doc_id).map(Value::Int).map_err(|_| {
        SQLError::TypeMismatch(format!("document id {doc_id} exceeds the SQL BIGINT range"))
    })
}

#[cfg(test)]
#[path = "sql/tests.rs"]
mod tests;
