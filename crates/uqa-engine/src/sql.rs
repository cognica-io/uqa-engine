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
    clippy::unnested_or_patterns,
    clippy::too_many_lines
)]

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use uqa_core::{DecimalValue, DocId, TemporalValue, Value};
use uqa_sql::ast::{
    AlterTableAction, AlterTableStmt, BinaryOp, ColumnType, CreateIndex, CreateTable, DropKind,
    DropStmt, ForeignKey, ForeignKeyAction, ForeignKeyMatch, SetOpKind, Statement,
};
use uqa_sql::expr::{value_to_tensor, value_to_vector};
use uqa_sql::{compile, ResultRow, SQLError, SQLParam, SQLResult};
use uqa_storage::document_store::Document;

use crate::{Engine, HNSWIndexParams, IVFIndexParams, ScoredEntry, VectorIndexSpec};

mod age_cypher;
mod aggregates;
mod catalog;
mod correlation;
mod cursor;
mod ddl;
pub(crate) mod dml;
mod driver;
mod engine_api;
mod from_rows;
mod generated;
mod mutability;
mod plan_executor;
mod planning;
mod plpgsql_exec;
mod row_functions;
mod scalar;
mod select;
mod volatility;
mod where_eval;
mod window;

pub use cursor::{SQLCursor, SQLCursorSummary};
pub(crate) use driver::execute;
use mutability::{is_transaction_control, query_may_mutate_engine};
#[cfg(test)]
use planning::compile_logical_plans;
use planning::lower_statement;
pub(super) use planning::{execute_compiled_statement, optimize_engine_plan};
pub(crate) use plpgsql_exec::{call_bound_user_scalar_function, call_user_scalar_function};
use select::query_has_row_locks;
pub(crate) use select::RowLockRetryCache;

use aggregates::{
    aggregate_value, contains_aggregate, has_aggregate, projection_label_at, AggregateAccumulator,
    PhysicalAggregateExecutor,
};
use catalog::build_info_schema_rows;
pub(in crate::sql) use catalog::virtual_relation_schema;
use ddl::{
    coerce_to_column_type, column_type_name, core_value_to_json, json_table_arg,
    json_table_value_to_text, json_to_core_value, run_alter_sequence, run_alter_table,
    run_create_index, run_create_sequence, run_create_table, run_create_table_as, run_drop,
    value_to_text,
};
pub(crate) use ddl::{convert_value_to_column_type, validate_vector_dimensions};
use dml::{index_vectors_for_type, run_delete, run_insert, run_merge, run_update};
use from_rows::{build_join_spill_with_ctes, engine_func_intercept, ColumnPrune, QualifierFilters};
pub(crate) use generated::refresh_stored_generated_columns;
use plan_executor::UnifiedPlanExecutor;
use row_functions::{
    execute_function, execute_function_with_top_k, execute_tree_entries, expect_column_name,
    expect_optional_graph_value, graph_betweenness_entries, graph_hits_entries,
    graph_pagerank_entries, run_age_alter_graph_with_evaluator,
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
pub(crate) use select::CteScope;
use select::{
    bind_projection_output_schema, build_projection_physical_row_with_ctes, projection_columns,
    run_explain, ScopedEngineHook,
};
use where_eval::execute_mixed_where;
pub(crate) use where_eval::expr_is_null_free as expr_is_null_free_public;
use window::{has_window, prepare_window_plan, PhysicalWindowExecutor};

type RowUpdateValues = BTreeMap<String, Value>;
type RowUpdateVectors = BTreeMap<String, Vec<Vec<f32>>>;
type RowIndependentUpdateValues = (RowUpdateValues, RowUpdateVectors);

/// Analyze a catalog-owned query without executing it or sampling rows.
pub(crate) fn analyze_catalog_query_schema(
    engine: &Engine,
    query: &uqa_planner::QueryPlan,
    params: &[SQLParam],
) -> Result<uqa_execution::RowSchema, SQLError> {
    select::analyze_query_plan_schema(engine, query, params, &CteScope::default(), None)
}

const SCORE_COLUMN: &str = "_score";
pub(in crate::sql) const DOC_ID_COLUMN: &str = "_doc_id";
const MERGE_ACTION_COLUMN: &str = "_merge_action";
// NUL cannot occur in a SQL identifier, so this row-carried field cannot
// collide with a user column. Its value is the score emitted by an executed
// retrieval access path; `Null` explicitly marks an ordinary scan.
const SCORE_PROVENANCE_COLUMN: &str = "\0uqa.score_bearing";

fn is_score_provenance_column(column: &str) -> bool {
    column == SCORE_PROVENANCE_COLUMN
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
                        | "setval"
                        | "current_schema"
                        | "current_schemas"
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
mod mutability_classifier_tests {
    use super::{
        builtin_function_dispatch_name, compile, lower_statement, query_may_mutate_engine,
        volatility::function_volatility, Engine,
    };
    use crate::{
        SQLAggregateState, SQLFunctionOptions, SQLFunctionVolatility, SQLTableFunctionResult,
    };
    use uqa_core::Value;
    use uqa_planner::UnifiedPlan;
    use uqa_sql::SQLError;

    #[derive(Default)]
    struct NullAggregate;

    impl SQLAggregateState for NullAggregate {
        fn observe(&mut self, _args: &[Value]) -> Result<(), SQLError> {
            Ok(())
        }

        fn finish(&self) -> Result<Value, SQLError> {
            Ok(Value::Null)
        }
    }

    fn query_is_writer(engine: &Engine, sql: &str) -> bool {
        let mut statements = compile(sql).expect("compile callback query");
        assert_eq!(statements.len(), 1);
        let plan = lower_statement(engine, statements.remove(0));
        let UnifiedPlan::Query(query) = plan else {
            panic!("callback SELECT did not lower to a query plan");
        };
        query_may_mutate_engine(engine, &query).expect("classify callback query")
    }

    #[test]
    fn runtime_registered_callbacks_are_writer_classified() {
        let engine = Engine::new();
        engine
            .register_scalar_function("runtime_scalar", |_args: &[Value]| Ok(Value::Null))
            .unwrap();
        engine
            .register_table_function("runtime_table", |_args: &[Value]| {
                Ok(SQLTableFunctionResult::new(
                    ["value"],
                    Vec::<Vec<Value>>::new(),
                ))
            })
            .unwrap();
        engine
            .register_aggregate_function("runtime_aggregate", NullAggregate::default)
            .unwrap();

        assert!(query_is_writer(&engine, "SELECT runtime_scalar() AS value"));
        assert!(query_is_writer(
            &engine,
            "SELECT value FROM runtime_table() AS rows(value)"
        ));
        assert!(query_is_writer(
            &engine,
            "SELECT runtime_aggregate(1) AS value"
        ));

        engine
            .sql(
                "CREATE VIEW runtime_callback_inner AS \
                 SELECT runtime_scalar() AS value",
                &[],
            )
            .unwrap();
        engine
            .sql(
                "CREATE VIEW runtime_callback_outer AS \
                 SELECT value FROM runtime_callback_inner",
                &[],
            )
            .unwrap();
        assert!(query_is_writer(
            &engine,
            "SELECT value FROM runtime_callback_outer"
        ));

        engine
            .sql("CREATE SEQUENCE nested_view_sequence START 1", &[])
            .unwrap();
        engine
            .sql(
                "CREATE VIEW sequence_inner AS \
                 SELECT nextval('nested_view_sequence') AS value",
                &[],
            )
            .unwrap();
        engine
            .sql(
                "CREATE VIEW sequence_outer AS SELECT value FROM sequence_inner",
                &[],
            )
            .unwrap();
        assert!(query_is_writer(&engine, "SELECT value FROM sequence_outer"));
    }

    #[test]
    fn explicit_runtime_callback_properties_drive_transactions_and_optimization() {
        let engine = Engine::new();
        let immutable_reader = SQLFunctionOptions::read_only(SQLFunctionVolatility::Immutable);
        engine
            .register_scalar_function_with_options(
                "runtime_scalar_reader",
                immutable_reader,
                |_args: &[Value]| Ok(Value::Null),
            )
            .unwrap();
        engine
            .register_table_function_with_options(
                "runtime_table_reader",
                immutable_reader,
                |_args: &[Value]| {
                    Ok(SQLTableFunctionResult::new(
                        ["value"],
                        Vec::<Vec<Value>>::new(),
                    ))
                },
            )
            .unwrap();
        engine
            .register_aggregate_function_with_options(
                "runtime_aggregate_reader",
                immutable_reader,
                NullAggregate::default,
            )
            .unwrap();

        assert!(!query_is_writer(
            &engine,
            "SELECT runtime_scalar_reader() AS value"
        ));
        assert!(!query_is_writer(
            &engine,
            "SELECT value FROM runtime_table_reader() AS rows(value)"
        ));
        assert!(!query_is_writer(
            &engine,
            "SELECT runtime_aggregate_reader(1) AS value"
        ));
        assert_eq!(
            function_volatility(&engine, "runtime_scalar_reader", 0),
            SQLFunctionVolatility::Immutable
        );

        let invalid = SQLFunctionOptions::new(SQLFunctionVolatility::Stable, true);
        let error = engine
            .register_scalar_function_with_options(
                "invalid_mutating_stable",
                invalid,
                |_args: &[Value]| Ok(Value::Null),
            )
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("may mutate engine state must be VOLATILE"));
    }

    #[test]
    fn implicit_hybrid_fusion_is_classified_as_a_writer_for_calibration() {
        let engine = Engine::new();
        assert!(!query_is_writer(
            &engine,
            "SELECT id FROM docs WHERE text_match(body, 'rust')"
        ));
        assert!(query_is_writer(
            &engine,
            "SELECT id FROM docs \
             WHERE text_match(body, 'rust') \
               AND knn_match(embedding, ARRAY[1.0, 0.0], 10)"
        ));
        assert!(query_is_writer(
            &engine,
            "SELECT id FROM docs \
             WHERE text_match(body, 'rust') \
               AND (knn_match(embedding, ARRAY[1.0, 0.0], 10) AND kind = 'article')"
        ));
        assert!(!query_is_writer(
            &engine,
            "SELECT d.id FROM docs d JOIN vectors v ON d.id = v.id \
             WHERE text_match(body, 'rust') \
               AND knn_match(embedding, ARRAY[1.0, 0.0], 10)"
        ));
        assert!(query_is_writer(
            &engine,
            "SELECT d.id FROM docs d JOIN metadata m ON d.id = m.id \
             WHERE text_match(d.body, 'rust') \
               AND knn_match(d.embedding, ARRAY[1.0, 0.0], 10)"
        ));
    }

    #[test]
    fn reserved_catalog_aliases_resolve_only_existing_builtins() {
        assert_eq!(
            builtin_function_dispatch_name("ag_catalog.cypher"),
            "cypher"
        );
        assert_eq!(
            builtin_function_dispatch_name("pg_catalog.generate_series"),
            "generate_series"
        );
        assert_eq!(
            builtin_function_dispatch_name("pg_catalog.reverse"),
            "reverse"
        );
        for function in ["crc32", "crc32c", "gamma", "lgamma", "md5"] {
            assert_eq!(
                builtin_function_dispatch_name(&format!("pg_catalog.{function}")),
                function
            );
        }
        for function in [
            "bit_length",
            "char_length",
            "character_length",
            "length",
            "octet_length",
        ] {
            assert_eq!(
                builtin_function_dispatch_name(&format!("pg_catalog.{function}")),
                function
            );
        }
        assert_eq!(
            builtin_function_dispatch_name("ag_catalog.generate_series"),
            "ag_catalog.generate_series"
        );
        assert_eq!(
            builtin_function_dispatch_name("application.cypher"),
            "application.cypher"
        );

        let engine = Engine::new();
        assert!(query_is_writer(
            &engine,
            "SELECT * FROM ag_catalog.cypher('g', $$CREATE (n)$$) AS (v agtype)"
        ));
        assert_eq!(
            function_volatility(&engine, "ag_catalog.cypher", 2),
            SQLFunctionVolatility::Volatile
        );
    }
}

#[cfg(test)]
mod unified_plan_tests {
    use uqa_planner::{CommandPlan, ComputePlan, RelationalPlan, SourcePlan, UnifiedPlan};

    use super::{compile_logical_plans, doc_id_value, optimize_engine_plan, Engine};

    #[test]
    fn document_ids_outside_bigint_are_rejected_at_the_sql_boundary() {
        assert!(doc_id_value(i64::MAX as u64).is_ok());
        assert!(doc_id_value(i64::MAX as u64 + 1).is_err());
    }

    fn one(engine: &Engine, sql: &str) -> UnifiedPlan {
        let mut plans = compile_logical_plans(engine, sql).expect("statement plans");
        assert_eq!(plans.len(), 1);
        optimize_engine_plan(engine, plans.remove(0)).expect("optimized statement plan")
    }

    #[test]
    fn sql_boundaries_are_cached_as_structural_unified_plans() {
        let engine = Engine::new();

        let arithmetic = one(&engine, "SELECT amount * 2 + 1 AS adjusted FROM ledger");
        let UnifiedPlan::Query(query) = arithmetic else {
            panic!("arithmetic SELECT must be a QueryPlan");
        };
        let RelationalPlan::QueryBlock(block) = &query.root else {
            panic!("expected query block");
        };
        assert!(matches!(block.compute, ComputePlan::Project));

        let window = one(
            &engine,
            "SELECT row_number() OVER (PARTITION BY account ORDER BY amount) FROM ledger",
        );
        let UnifiedPlan::Query(query) = window else {
            panic!("window SELECT must be a QueryPlan");
        };
        let RelationalPlan::QueryBlock(block) = &query.root else {
            panic!("expected query block");
        };
        assert!(matches!(block.compute, ComputePlan::Window));

        let subquery = one(
            &engine,
            "SELECT q.total FROM (SELECT sum(amount) AS total FROM ledger) AS q",
        );
        let UnifiedPlan::Query(query) = subquery else {
            panic!("subquery SELECT must be a QueryPlan");
        };
        let RelationalPlan::QueryBlock(block) = &query.root else {
            panic!("expected query block");
        };
        assert!(matches!(block.from, Some(SourcePlan::Subquery { .. })));

        let mutation = one(
            &engine,
            "UPDATE ledger SET amount = amount + 1 WHERE amount > 0",
        );
        assert!(matches!(
            mutation,
            UnifiedPlan::Command(command) if matches!(*command, CommandPlan::Update(_))
        ));

        // The cache retains the structural IR alongside the parsed statement;
        // execution still enters exclusively through UnifiedPlanExecutor.
        assert!(engine
            .cached_sql_plans("SELECT amount * 2 + 1 AS adjusted FROM ledger")
            .is_some_and(|plans| matches!(plans.as_slice(), [UnifiedPlan::Query(_)])));
    }

    #[test]
    fn memory_read_only_statements_reuse_optimized_plan_until_invalidation() {
        let engine = Engine::new();
        engine
            .sql("CREATE TABLE items (id INTEGER PRIMARY KEY)", &[])
            .expect("create table");
        engine
            .sql("INSERT INTO items (id) VALUES (1), (2)", &[])
            .expect("seed rows");
        let query = "SELECT id FROM items WHERE id > 0 ORDER BY id";

        engine.sql(query, &[]).expect("warm statement cache");
        let first = engine
            .cached_sql_statement(query)
            .and_then(|cached| cached.optimized_plan)
            .expect("memory read caches its optimized plan");

        engine.sql(query, &[]).expect("reuse statement cache");
        let second = engine
            .cached_sql_statement(query)
            .and_then(|cached| cached.optimized_plan)
            .expect("optimized plan remains cached");
        assert!(std::sync::Arc::ptr_eq(&first, &second));

        engine
            .sql("INSERT INTO items (id) VALUES (3)", &[])
            .expect("mutate table");
        assert!(
            engine.cached_sql_statement(query).is_none(),
            "a committed data change must invalidate the optimized plan"
        );
    }

    #[test]
    fn cached_memory_read_plan_is_not_reused_inside_explicit_transaction() {
        let engine = Engine::new();
        engine
            .sql("CREATE TABLE items (id INTEGER PRIMARY KEY)", &[])
            .expect("create table");
        engine
            .sql("INSERT INTO items (id) VALUES (1), (2)", &[])
            .expect("seed rows");
        let query = "SELECT id FROM items ORDER BY id";

        engine.sql(query, &[]).expect("warm statement cache");
        assert!(
            engine
                .cached_sql_statement(query)
                .is_some_and(|cached| cached.optimized_plan.is_some()),
            "memory read caches its optimized plan"
        );

        engine.begin().expect("begin explicit transaction");
        engine.sql(query, &[]).expect("execute transactional read");
        assert!(
            engine
                .cached_sql_statement(query)
                .is_some_and(|cached| cached.optimized_plan.is_none()),
            "the regular SQL entry point must replan inside an explicit transaction"
        );
        engine.rollback().expect("rollback explicit transaction");

        engine.begin().expect("begin second explicit transaction");
        let cursor = engine
            .sql_cursor(query, &[])
            .expect("execute transactional cursor read");
        assert_eq!(cursor.row_count(), 2);
        assert!(
            engine
                .cached_sql_statement(query)
                .is_some_and(|cached| cached.optimized_plan.is_none()),
            "the cursor entry point must replan inside an explicit transaction"
        );
        drop(cursor);
        engine.rollback().expect("rollback explicit transaction");
        assert!(
            engine
                .cached_sql_statement(query)
                .is_some_and(|cached| cached.optimized_plan.is_some()),
            "rollback restores the optimized plan cached before the transaction"
        );
    }

    #[test]
    fn snapshot_scoped_statements_do_not_cache_optimized_plans() {
        let dir = tempfile::tempdir().expect("temporary database directory");
        let persistent =
            Engine::open(&dir.path().join("statement-optimized-cache.db")).expect("open engine");
        persistent
            .sql("CREATE TABLE items (id INTEGER PRIMARY KEY)", &[])
            .expect("create table");
        persistent
            .sql("INSERT INTO items (id) VALUES (1), (2)", &[])
            .expect("seed rows");
        let persistent_query = "SELECT id FROM items WHERE id > 0 ORDER BY id";
        persistent
            .sql(persistent_query, &[])
            .expect("run persistent read");
        assert!(
            persistent
                .cached_sql_statement(persistent_query)
                .is_some_and(|cached| cached.optimized_plan.is_none()),
            "persistent reads must optimize inside each storage snapshot"
        );

        let memory = Engine::new();
        memory
            .sql("CREATE TABLE items (id INTEGER PRIMARY KEY)", &[])
            .expect("create memory table");
        memory.begin().expect("begin explicit transaction");
        let transactional_query = "SELECT id FROM items ORDER BY id";
        memory
            .sql(transactional_query, &[])
            .expect("run explicit-transaction read");
        assert!(
            memory
                .cached_sql_statement(transactional_query)
                .is_some_and(|cached| cached.optimized_plan.is_none()),
            "explicit transactions must optimize against their current state"
        );
        memory.rollback().expect("rollback explicit transaction");
    }

    #[test]
    fn views_retain_their_compiled_query_plan() {
        let engine = Engine::new();
        engine
            .sql("CREATE TABLE ledger (amount INTEGER)", &[])
            .expect("view source table");
        engine
            .sql(
                "CREATE VIEW ledger_totals AS SELECT sum(amount) AS total FROM ledger",
                &[],
            )
            .expect("view definition");

        let plan = engine
            .view_plan("ledger_totals")
            .expect("view catalog read")
            .expect("compiled view plan");
        let RelationalPlan::QueryBlock(block) = plan.root else {
            panic!("view must retain a query block");
        };
        assert!(matches!(block.compute, ComputePlan::Aggregate));
    }

    #[test]
    fn committed_table_data_invalidates_sibling_statement_plans_but_rollback_does_not() {
        let dir = tempfile::tempdir().expect("temporary database directory");
        let root = Engine::open(&dir.path().join("statement-data-epoch.db"))
            .expect("open persistent engine");
        root.sql("CREATE TABLE items (id INTEGER PRIMARY KEY)", &[])
            .expect("create table");
        let writer = root.new_session().expect("writer session");
        let observer = root.new_session().expect("observer session");
        let query = "SELECT id FROM items WHERE id = 1";

        let version_before_query = observer
            .storage
            .backend
            .as_ref()
            .expect("persistent observer")
            .change_version()
            .expect("read storage change version");
        observer.sql(query, &[]).expect("warm statement plan");
        assert_eq!(
            observer
                .storage
                .backend
                .as_ref()
                .expect("persistent observer")
                .change_version()
                .expect("read storage change version"),
            version_before_query,
            "a read-only statement persisted an alias-scoped value index"
        );
        assert!(observer
            .storage
            .backend
            .as_ref()
            .expect("persistent observer")
            .btree_index_fields("items")
            .expect("read unqualified value-index fields")
            .is_empty());
        assert_eq!(
            observer
                .storage
                .backend
                .as_ref()
                .expect("persistent observer")
                .btree_index_fields("public.items")
                .expect("read canonical value-index fields"),
            vec!["id"]
        );
        assert!(observer.cached_sql_plans(query).is_some());
        assert_eq!(
            observer
                .epochs
                .table_data
                .seen
                .load(std::sync::atomic::Ordering::Acquire),
            observer
                .epochs
                .table_data
                .published
                .load(std::sync::atomic::Ordering::Acquire),
            "the completed read statement left its in-process data generation stale"
        );
        assert_eq!(
            Some(
                observer
                    .epochs
                    .seen_storage_change_version
                    .load(std::sync::atomic::Ordering::Acquire)
            ),
            observer
                .storage
                .backend
                .as_ref()
                .expect("persistent observer")
                .change_version()
                .expect("read storage change version"),
            "the completed read statement left its SQLite data version stale"
        );
        assert_eq!(observer.table_doc_count("items").expect("warm count"), 0);
        assert!(
            observer.cached_sql_plans(query).is_some(),
            "reading the observer's table count cleared its statement cache"
        );
        let epoch_before_rollback = observer
            .epochs
            .table_data
            .published
            .load(std::sync::atomic::Ordering::Acquire);
        let data_version_before_rollback = observer
            .storage
            .backend
            .as_ref()
            .expect("persistent observer")
            .change_version()
            .expect("read storage change version");
        assert_eq!(
            observer
                .epochs
                .table_data
                .seen
                .load(std::sync::atomic::Ordering::Acquire),
            epoch_before_rollback,
            "the warmed observer did not consume the current in-process data generation"
        );
        assert_eq!(
            Some(
                observer
                    .epochs
                    .seen_storage_change_version
                    .load(std::sync::atomic::Ordering::Acquire)
            ),
            data_version_before_rollback,
            "the warmed observer did not consume the current SQLite data version"
        );

        writer.begin().expect("begin rolled-back write");
        assert!(
            observer.cached_sql_plans(query).is_some(),
            "starting a sibling transaction cleared the observer statement cache"
        );
        writer
            .sql("INSERT INTO items (id) VALUES (1)", &[])
            .expect("insert rolled-back row");
        assert!(
            observer.cached_sql_plans(query).is_some(),
            "an uncommitted sibling write cleared the observer statement cache"
        );
        writer.rollback().expect("rollback write");
        assert_eq!(
            observer
                .epochs
                .table_data
                .published
                .load(std::sync::atomic::Ordering::Acquire),
            epoch_before_rollback,
            "a rolled-back mutation published an in-process data generation"
        );
        assert_eq!(
            observer
                .storage
                .backend
                .as_ref()
                .expect("persistent observer")
                .change_version()
                .expect("read storage change version"),
            data_version_before_rollback,
            "a rolled-back mutation changed SQLite's committed data version"
        );
        assert_eq!(
            observer
                .epochs
                .table_data
                .seen
                .load(std::sync::atomic::Ordering::Acquire),
            epoch_before_rollback,
            "a rolled-back mutation changed the observer's consumed data generation"
        );
        assert_eq!(
            Some(
                observer
                    .epochs
                    .seen_storage_change_version
                    .load(std::sync::atomic::Ordering::Acquire)
            ),
            data_version_before_rollback,
            "a rolled-back mutation changed the observer's consumed SQLite data version"
        );
        assert!(
            observer.cached_sql_plans(query).is_some(),
            "the writer cleared a sibling statement cache before synchronization"
        );
        observer
            .synchronize_table_data()
            .expect("check unchanged generation");
        assert!(
            observer.cached_sql_plans(query).is_some(),
            "a rolled-back mutation must not publish a cache generation"
        );
        assert_eq!(observer.table_doc_count("items").expect("cached count"), 0);

        writer.begin().expect("begin committed write");
        writer
            .sql("INSERT INTO items (id) VALUES (1)", &[])
            .expect("insert committed row");
        writer.commit().expect("commit write");
        assert!(observer.cached_sql_plans(query).is_some());
        observer
            .synchronize_table_data()
            .expect("refresh committed generation");
        assert!(
            observer.cached_sql_plans(query).is_none(),
            "a sibling commit must invalidate optimized statement plans"
        );
        assert_eq!(
            observer.table_doc_count("items").expect("refreshed count"),
            1
        );
    }

    #[test]
    fn sibling_catalog_commit_invalidates_cached_logical_statements() {
        let dir = tempfile::tempdir().expect("temporary database directory");
        let root = Engine::open(&dir.path().join("statement-catalog-epoch.db"))
            .expect("open persistent engine");
        root.sql("CREATE TABLE items (id INTEGER PRIMARY KEY)", &[])
            .expect("create table");
        let writer = root.new_session().expect("writer session");
        let observer = root.new_session().expect("observer session");
        let query = "SELECT id FROM items WHERE id = 1";

        observer.sql(query, &[]).expect("warm logical statement");
        assert!(observer.cached_sql_plans(query).is_some());
        writer
            .sql("ALTER TABLE items ADD COLUMN label TEXT", &[])
            .expect("commit sibling DDL");
        observer
            .synchronize_table_catalog()
            .expect("refresh sibling table catalog");
        assert!(observer.cached_sql_plans(query).is_none());
        assert!(observer.table_has_column("items", "label").unwrap());
    }

    #[test]
    fn multi_statement_batch_lowers_each_statement_after_prior_catalog_changes() {
        let engine = Engine::new();
        let result = engine
            .sql(
                "CREATE SCHEMA batch_ns; \
                 CREATE TABLE batch_ns.items (id INTEGER PRIMARY KEY); \
                 SET search_path TO batch_ns; \
                 INSERT INTO items (id) VALUES (7); \
                 SELECT id FROM items",
                &[],
            )
            .expect("execute dependent statements in one parsed batch");
        assert_eq!(result.rows.len(), 1);
        assert_eq!(result.rows[0]["id"], uqa_core::Value::Int(7));
    }

    #[test]
    fn multi_statement_batch_observes_function_create_and_drop() {
        let engine = Engine::new();
        let created = engine
            .sql(
                "CREATE FUNCTION batch_inc(a INTEGER) RETURNS INTEGER RETURN a + 1; \
                 SELECT batch_inc(4) AS value",
                &[],
            )
            .expect("call function created by the preceding statement");
        assert_eq!(created.rows[0]["value"], uqa_core::Value::Int(5));

        let error = engine
            .sql(
                "DROP FUNCTION batch_inc(INTEGER); SELECT batch_inc(4) AS value",
                &[],
            )
            .expect_err("dropped function must not survive as a stale lowered plan");
        assert!(error.to_string().contains("batch_inc"));
    }
}
