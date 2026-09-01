//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    builtin_function_dispatch_name, compile, lower_statement, query_may_mutate_engine,
    query_requires_statement_transaction, volatility::function_volatility, Engine,
};
use crate::{SQLAggregateState, SQLFunctionOptions, SQLFunctionVolatility, SQLTableFunctionResult};
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

fn query_requires_transaction(engine: &Engine, sql: &str) -> bool {
    let mut statements = compile(sql).expect("compile transactional callback query");
    assert_eq!(statements.len(), 1);
    let plan = lower_statement(engine, statements.remove(0));
    let UnifiedPlan::Query(query) = plan else {
        panic!("callback SELECT did not lower to a query plan");
    };
    query_requires_statement_transaction(engine, &query)
        .expect("classify callback statement transaction")
}

#[test]
fn operator_join_estimate_uses_each_relation_cardinality() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE estimate_left (id INTEGER PRIMARY KEY, embedding VECTOR(2)); \
             CREATE TABLE estimate_right (id INTEGER PRIMARY KEY, archived_embedding VECTOR(2)); \
             INSERT INTO estimate_left VALUES (1, ARRAY[1.0, 0.0]); \
             INSERT INTO estimate_right VALUES \
                 (101, ARRAY[1.0, 0.0]), \
                 (102, ARRAY[1.0, 0.0]), \
                 (103, ARRAY[1.0, 0.0]), \
                 (104, ARRAY[1.0, 0.0]), \
                 (105, ARRAY[1.0, 0.0])",
            &[],
        )
        .unwrap();
    let mut statements = compile(
        "SELECT * FROM vector_similarity_join(\
             estimate_left,\
             knn_match(embedding, ARRAY[1.0, 0.0], 10),\
             estimate_right,\
             knn_match(archived_embedding, ARRAY[1.0, 0.0], 10),\
             -1.0\
         )",
    )
    .unwrap();
    let UnifiedPlan::Query(query) = lower_statement(&engine, statements.remove(0)) else {
        panic!("operator join did not lower to a query");
    };
    let uqa_planner::RelationalPlan::QueryBlock(block) = query.root else {
        panic!("operator join query did not lower to a query block");
    };
    let Some(uqa_planner::SourcePlan::Function {
        name,
        relations,
        args,
        ..
    }) = block.from
    else {
        panic!("operator join did not lower to a function source");
    };
    let estimate = crate::operator_tree_bridge::estimate_operator_join_table_function(
        &engine,
        &name,
        relations.as_ref(),
        &args,
        &[],
    )
    .unwrap();
    assert_eq!(estimate.output_rows, 5.0);
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
fn sql_routine_body_effects_drive_writer_classification() {
    let engine = Engine::new();
    engine
            .sql(
                "CREATE TABLE routine_mutations (id INTEGER); \
                 CREATE SEQUENCE routine_sequence; \
                 CREATE FUNCTION routine_reader() RETURNS INTEGER LANGUAGE SQL AS 'SELECT 1'; \
                 CREATE FUNCTION nested_routine_reader() RETURNS INTEGER LANGUAGE SQL AS 'SELECT routine_reader()'; \
                 CREATE FUNCTION routine_writer() RETURNS INTEGER LANGUAGE SQL AS 'INSERT INTO routine_mutations VALUES (1) RETURNING id' VOLATILE; \
                 CREATE FUNCTION nested_routine_writer() RETURNS INTEGER LANGUAGE SQL AS 'SELECT routine_writer()' VOLATILE; \
                 CREATE FUNCTION routine_sequence_writer() RETURNS BIGINT LANGUAGE SQL AS 'SELECT nextval(''routine_sequence'')' VOLATILE; \
                 CREATE FUNCTION plpgsql_routine_reader() RETURNS INTEGER LANGUAGE plpgsql AS 'BEGIN RETURN routine_reader(); END'; \
                 CREATE FUNCTION plpgsql_exception_reader() RETURNS INTEGER LANGUAGE plpgsql AS 'BEGIN BEGIN RAISE EXCEPTION ''handled''; EXCEPTION WHEN OTHERS THEN NULL; END; RETURN 1; END'; \
                 CREATE FUNCTION plpgsql_routine_writer() RETURNS INTEGER LANGUAGE plpgsql AS 'BEGIN INSERT INTO routine_mutations VALUES (2); RETURN 2; END' VOLATILE",
                &[],
            )
            .unwrap();

    assert!(!query_is_writer(&engine, "SELECT routine_reader()"));
    assert!(!query_is_writer(&engine, "SELECT nested_routine_reader()"));
    assert!(query_is_writer(&engine, "SELECT routine_writer()"));
    assert!(query_is_writer(&engine, "SELECT nested_routine_writer()"));
    assert!(query_is_writer(&engine, "SELECT routine_sequence_writer()"));
    assert!(!query_is_writer(&engine, "SELECT plpgsql_routine_reader()"));
    assert!(!query_is_writer(
        &engine,
        "SELECT plpgsql_exception_reader()"
    ));
    assert!(query_requires_transaction(
        &engine,
        "SELECT plpgsql_exception_reader()"
    ));
    assert!(!query_requires_transaction(
        &engine,
        "SELECT plpgsql_routine_reader()"
    ));
    assert!(query_is_writer(&engine, "SELECT plpgsql_routine_writer()"));
    for _ in 0..2 {
        let result = engine
            .sql("SELECT plpgsql_exception_reader() AS value", &[])
            .unwrap();
        assert_eq!(result.rows[0]["value"], Value::Int(1));
    }
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

#[cfg(test)]
mod unified_plan_tests {
    use uqa_planner::{CommandPlan, ComputePlan, RelationalPlan, SourcePlan, UnifiedPlan};

    use super::super::{compile_logical_plans, doc_id_value, optimize_engine_plan, Engine};

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
