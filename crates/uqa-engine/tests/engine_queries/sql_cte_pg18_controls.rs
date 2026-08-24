//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` 18.4 recursive CTE controls, materialization policy, and CTE row-lock interaction.

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use uqa_core::Value;
use uqa_engine::{Engine, SQLFunctionOptions, SQLFunctionVolatility, SQLScalarFunction};
use uqa_sql::SQLError;

struct CountCalls(Arc<AtomicUsize>);

impl SQLScalarFunction for CountCalls {
    fn call(&self, args: &[Value]) -> Result<Value, SQLError> {
        assert!(args.is_empty());
        Ok(Value::Int(self.0.fetch_add(1, Ordering::SeqCst) as i64 + 1))
    }
}

fn ints(result: &uqa_engine::SQLResult, column: &str) -> Vec<i64> {
    result
        .rows
        .iter()
        .map(|row| match row.get(column) {
            Some(Value::Int(value)) => *value,
            other => panic!("expected integer {column}, got {other:?}"),
        })
        .collect()
}

fn graph_engine() -> Engine {
    let engine = Engine::new();
    engine
        .sql("CREATE TABLE edges(src integer, dst integer)", &[])
        .unwrap();
    engine
        .sql(
            "INSERT INTO edges VALUES (1,2),(1,3),(2,4),(3,4),(4,2),(3,5)",
            &[],
        )
        .unwrap();
    engine
}

#[test]
fn recursive_search_depth_and_breadth_match_postgresql_18() {
    let engine = graph_engine();
    let depth = engine
        .sql(
            "WITH RECURSIVE walk(node, depth) AS (
                 VALUES (1,0)
                 UNION ALL
                 SELECT e.dst, w.depth+1 FROM walk w JOIN edges e ON e.src=w.node WHERE w.depth < 3
             ) SEARCH DEPTH FIRST BY node SET ord
             SELECT node, depth, pg_typeof(ord) AS ord_type FROM walk ORDER BY ord",
            &[],
        )
        .unwrap();
    assert_eq!(ints(&depth, "node"), [1, 2, 4, 2, 3, 4, 2, 5]);
    assert!(depth
        .rows
        .iter()
        .all(|row| row["ord_type"] == Value::Str("record[]".into())));

    let breadth = engine
        .sql(
            "WITH RECURSIVE walk(node, depth) AS (
                 VALUES (1,0)
                 UNION ALL
                 SELECT e.dst, w.depth+1 FROM walk w JOIN edges e ON e.src=w.node WHERE w.depth < 2
             ) SEARCH BREADTH FIRST BY depth, node SET ord
             SELECT node, pg_typeof(ord) AS ord_type FROM walk ORDER BY ord",
            &[],
        )
        .unwrap();
    assert_eq!(ints(&breadth, "node"), [1, 2, 3, 4, 4, 5]);
    assert!(breadth
        .rows
        .iter()
        .all(|row| row["ord_type"] == Value::Str("record".into())));
}

#[test]
fn recursive_cycle_emits_cycle_rows_without_expanding_them() {
    let engine = graph_engine();
    let result = engine
        .sql(
            "WITH RECURSIVE walk(node, depth) AS (
                 VALUES (1,0)
                 UNION ALL
                 SELECT e.dst, w.depth+1 FROM walk w JOIN edges e ON e.src=w.node
             ) CYCLE node SET cyc USING path
             SELECT node, depth, cyc, pg_typeof(cyc) AS mark_type, pg_typeof(path) AS path_type
             FROM walk ORDER BY path",
            &[],
        )
        .unwrap();
    assert_eq!(ints(&result, "node"), [1, 2, 4, 2, 3, 4, 2, 4, 5]);
    assert_eq!(
        result
            .rows
            .iter()
            .filter(|row| row["cyc"] == Value::Bool(true))
            .count(),
        2
    );
    assert!(result.rows.iter().all(|row| {
        row["mark_type"] == Value::Str("boolean".into())
            && row["path_type"] == Value::Str("record[]".into())
    }));

    let custom = engine
        .sql(
            "WITH RECURSIVE t(a,b) AS (
                 VALUES (1,'x'::text)
                 UNION ALL SELECT a+1,b FROM t WHERE a<2
             ) CYCLE a,b SET cyc TO 7 DEFAULT 9 USING path
             SELECT a, cyc, pg_typeof(cyc) AS mark_type FROM t ORDER BY path",
            &[],
        )
        .unwrap();
    assert_eq!(ints(&custom, "a"), [1, 2]);
    assert_eq!(ints(&custom, "cyc"), [9, 9]);
    assert!(custom
        .rows
        .iter()
        .all(|row| row["mark_type"] == Value::Str("integer".into())));
}

#[test]
fn recursive_cycle_columns_are_bindable_but_hidden_from_the_recursive_wildcard() {
    let engine = Engine::new();
    let wildcard = engine
        .sql(
            "WITH RECURSIVE a AS (
                 SELECT 1 AS b
                 UNION ALL
                 SELECT * FROM a
             ) CYCLE b SET c USING p
             SELECT b, c, pg_typeof(p) AS path_type FROM a ORDER BY cardinality(p)",
            &[],
        )
        .unwrap();
    assert_eq!(ints(&wildcard, "b"), [1, 1]);
    assert_eq!(
        wildcard
            .rows
            .iter()
            .map(|row| row["c"].clone())
            .collect::<Vec<_>>(),
        [Value::Bool(false), Value::Bool(true)]
    );
    assert!(wildcard
        .rows
        .iter()
        .all(|row| row["path_type"] == Value::Str("record[]".into())));

    let explicit = engine
        .sql(
            "WITH RECURSIVE test AS (
                 SELECT 0 AS x
                 UNION ALL
                 SELECT (x+1)%3 FROM test WHERE NOT test.is_cycle
             ) CYCLE x SET is_cycle USING path
             SELECT x, is_cycle, cardinality(path) AS path_length
             FROM test ORDER BY cardinality(path)",
            &[],
        )
        .unwrap();
    assert_eq!(ints(&explicit, "x"), [0, 1, 2, 0]);
    assert_eq!(ints(&explicit, "path_length"), [1, 2, 3, 4]);
    assert_eq!(explicit.rows[3]["is_cycle"], Value::Bool(true));
}

#[test]
fn recursive_union_distinct_includes_generated_paths_in_row_identity() {
    let engine = Engine::new();
    let result = engine
        .sql(
            "WITH RECURSIVE t(n) AS (
                 VALUES (1)
                 UNION
                 SELECT n+1 FROM t WHERE n<3
             ) SEARCH DEPTH FIRST BY n SET ord
             SELECT n, cardinality(ord) AS path_length FROM t ORDER BY n",
            &[],
        )
        .unwrap();
    assert_eq!(ints(&result, "n"), [1, 2, 3]);
    assert_eq!(ints(&result, "path_length"), [1, 2, 3]);

    let graph = Engine::new();
    let paths = graph
        .sql(
            "WITH RECURSIVE
                 edges(src,dst) AS (VALUES (1,2),(1,3),(2,4),(3,4)),
                 walk(node,depth) AS (
                     VALUES(1,0)
                     UNION
                     SELECT e.dst,w.depth+1 FROM walk w JOIN edges e ON e.src=w.node WHERE w.depth<2
                 ) SEARCH DEPTH FIRST BY node SET ord
             SELECT node FROM walk ORDER BY ord",
            &[],
        )
        .unwrap();
    assert_eq!(ints(&paths, "node"), [1, 2, 4, 3, 4]);
}

#[test]
fn recursive_control_step_runs_once_per_working_table() {
    let engine = Engine::new();
    let result = engine
        .sql(
            "WITH RECURSIVE t(n) AS (
                 VALUES (1),(2)
                 UNION ALL
                 (SELECT n+10 FROM t WHERE n<15 LIMIT 1)
             ) SEARCH DEPTH FIRST BY n SET ord
             SELECT n FROM t ORDER BY n",
            &[],
        )
        .unwrap();
    assert_eq!(ints(&result, "n"), [1, 2, 11, 21]);
}

#[test]
fn recursive_control_validation_matches_postgresql_18_ordering() {
    let engine = Engine::new();
    for (sql, state, message) in [
        (
            "WITH RECURSIVE t(n) AS (VALUES(1) UNION ALL SELECT n+1 FROM t WHERE n<2) SEARCH DEPTH FIRST BY n,n SET ord SELECT * FROM t",
            "42601",
            "search column \"n\" specified more than once",
        ),
        (
            "WITH RECURSIVE t(n) AS (VALUES(1) UNION ALL SELECT n+1 FROM t WHERE n<2) CYCLE n,n SET c USING p SELECT * FROM t",
            "42601",
            "cycle column \"n\" specified more than once",
        ),
        (
            "WITH RECURSIVE t(n) AS (VALUES(1) UNION ALL SELECT n+1 FROM t WHERE n<2) CYCLE n SET c USING c SELECT * FROM t",
            "42601",
            "cycle mark column name and cycle path column name are the same",
        ),
        (
            "WITH RECURSIVE t(n) AS (VALUES(1) UNION ALL SELECT n+1 FROM t WHERE n<2) SEARCH DEPTH FIRST BY missing SET ord SELECT * FROM t",
            "42601",
            "search column \"missing\" not in WITH query column list",
        ),
        (
            "WITH RECURSIVE t(n) AS (VALUES(1) UNION ALL SELECT 1 FROM t WHERE false) CYCLE n SET c USING n SELECT * FROM t",
            "42601",
            "cycle path column name \"n\" already used in WITH query column list",
        ),
        (
            "WITH RECURSIVE t(n) AS (VALUES(1) UNION ALL SELECT n+1 FROM t WHERE n<2) SEARCH DEPTH FIRST BY n SET n SELECT * FROM t",
            "42702",
            "ambiguous",
        ),
        (
            "WITH RECURSIVE t(n) AS (VALUES(1) UNION ALL SELECT 1 FROM t WHERE false) SEARCH DEPTH FIRST BY n SET n SELECT * FROM t",
            "42601",
            "search sequence column name \"n\" already used",
        ),
        (
            "WITH RECURSIVE t(n) AS (VALUES(1) UNION ALL SELECT n+1 FROM t WHERE n<2) CYCLE n SET c TO true DEFAULT 55 USING p SELECT * FROM t",
            "42804",
            "types",
        ),
        (
            "WITH RECURSIVE t(n,j) AS (VALUES(1,json '{}') UNION ALL SELECT n+1,j FROM t WHERE n<2) CYCLE j SET c USING p SELECT * FROM t",
            "42883",
            "json",
        ),
        (
            "WITH RECURSIVE t(n) AS (VALUES(1)) SEARCH DEPTH FIRST BY n SET ord SELECT * FROM t",
            "42601",
            "WITH query is not recursive",
        ),
    ] {
        let error = engine.sql(sql, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some(state), "{sql}: {error}");
        assert!(error.to_string().contains(message), "{sql}: {error}");
    }
}

#[test]
fn materialization_controls_choose_the_postgresql_18_execution_policy() {
    let engine = Engine::new();
    engine
        .sql("CREATE TABLE items(id integer primary key)", &[])
        .unwrap();
    engine.sql("INSERT INTO items VALUES (1),(2)", &[]).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    engine
        .register_scalar_function_with_options(
            "stable_counter",
            SQLFunctionOptions::read_only(SQLFunctionVolatility::Stable),
            CountCalls(Arc::clone(&calls)),
        )
        .unwrap();

    engine
        .sql(
            "WITH c AS (SELECT id, stable_counter() AS marker FROM items)
             SELECT marker FROM c LIMIT 1",
            &[],
        )
        .unwrap();
    assert_eq!(calls.swap(0, Ordering::SeqCst), 2);

    engine
        .sql(
            "WITH c AS MATERIALIZED (SELECT id, stable_counter() AS marker FROM items)
             SELECT marker FROM c LIMIT 1",
            &[],
        )
        .unwrap();
    assert_eq!(calls.swap(0, Ordering::SeqCst), 2);

    engine
        .sql(
            "WITH c AS (SELECT id, stable_counter() AS marker FROM items)
             SELECT a.marker AS left_marker, b.marker AS right_marker
             FROM c a JOIN c b ON a.id=b.id ORDER BY a.id",
            &[],
        )
        .unwrap();
    assert_eq!(calls.swap(0, Ordering::SeqCst), 2);

    engine
        .sql(
            "WITH c AS NOT MATERIALIZED (SELECT id, stable_counter() AS marker FROM items)
             SELECT a.marker AS left_marker, b.marker AS right_marker
             FROM c a JOIN c b ON a.id=b.id ORDER BY a.id",
            &[],
        )
        .unwrap();
    assert_eq!(calls.swap(0, Ordering::SeqCst), 4);

    engine
        .register_scalar_function_with_options(
            "volatile_counter",
            SQLFunctionOptions::read_only(SQLFunctionVolatility::Volatile),
            CountCalls(Arc::clone(&calls)),
        )
        .unwrap();
    engine
        .sql(
            "WITH c AS NOT MATERIALIZED (SELECT volatile_counter() AS marker)
             SELECT a.marker, b.marker FROM c a CROSS JOIN c b",
            &[],
        )
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn row_lock_errors_follow_shape_clause_and_cte_ordering() {
    let engine = graph_engine();
    let shape = engine
        .sql("SELECT DISTINCT src FROM edges FOR UPDATE OF missing", &[])
        .unwrap_err();
    assert_eq!(shape.sqlstate(), Some("0A000"));
    assert!(shape.to_string().contains("DISTINCT"));

    let cte_first = engine
        .sql(
            "WITH c AS (SELECT * FROM edges) SELECT * FROM c FOR UPDATE OF c FOR SHARE OF missing",
            &[],
        )
        .unwrap_err();
    assert_eq!(cte_first.sqlstate(), Some("0A000"));
    assert!(cte_first.to_string().contains("WITH query"));

    let missing_first = engine
        .sql(
            "WITH c AS (SELECT * FROM edges) SELECT * FROM c FOR SHARE OF missing FOR UPDATE OF c",
            &[],
        )
        .unwrap_err();
    assert_eq!(missing_first.sqlstate(), Some("42P01"));

    let nested = engine
        .sql(
            "WITH c AS (SELECT * FROM edges) SELECT * FROM (SELECT * FROM c FOR UPDATE OF c) s",
            &[],
        )
        .unwrap_err();
    assert_eq!(nested.sqlstate(), Some("0A000"));
    assert!(nested.to_string().contains("WITH query"));

    let catalog = engine
        .sql(
            "SELECT typname FROM pg_catalog.pg_type ORDER BY oid LIMIT 1 FOR UPDATE OF pg_type",
            &[],
        )
        .unwrap();
    assert_eq!(catalog.rows.len(), 1);

    engine
        .sql("SELECT * FROM ag_catalog.ag_graph FOR UPDATE", &[])
        .unwrap();
    let catalog_view = engine
        .sql("SELECT * FROM pg_catalog.pg_tables FOR UPDATE", &[])
        .unwrap_err();
    assert_eq!(catalog_view.sqlstate(), Some("0A000"));
}

#[test]
fn stored_views_retain_recursive_controls_and_materialization_policy_after_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("cte-controls.db");
    {
        let engine = Engine::open(&path).unwrap();
        engine
            .sql("CREATE TABLE edges(src integer, dst integer)", &[])
            .unwrap();
        engine
            .sql("INSERT INTO edges VALUES (1,2),(2,1),(2,3)", &[])
            .unwrap();
        engine
            .sql(
                "CREATE VIEW walked AS
                 WITH RECURSIVE walk(node, depth) AS NOT MATERIALIZED (
                     VALUES (1,0)
                     UNION ALL SELECT e.dst, w.depth+1 FROM walk w JOIN edges e ON e.src=w.node
                 ) SEARCH DEPTH FIRST BY node SET ord CYCLE node SET cyc USING path
                 SELECT node, depth, cyc, ord, path FROM walk",
                &[],
            )
            .unwrap();
    }
    let reopened = Engine::open(&path).unwrap();
    let result = reopened
        .sql(
            "SELECT node, depth, cyc, pg_typeof(ord) AS ord_type, pg_typeof(path) AS path_type FROM walked ORDER BY ord",
            &[],
        )
        .unwrap();
    assert_eq!(ints(&result, "node"), [1, 2, 1, 3]);
    assert_eq!(
        result
            .rows
            .iter()
            .filter(|row| row["cyc"] == Value::Bool(true))
            .count(),
        1
    );
    assert!(result.rows.iter().all(|row| {
        row["ord_type"] == Value::Str("record[]".into())
            && row["path_type"] == Value::Str("record[]".into())
    }));
}
