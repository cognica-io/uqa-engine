//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use serde_json::{json, Value as Json};

mod locking;
mod metadata;

fn fixture(engine: &Engine) {
    sql(engine, "CREATE TABLE diskann_docs(id int, embedding tensor(2)); INSERT INTO diskann_docs VALUES (1,ARRAY[ARRAY[1.0,0.0],ARRAY[0.0,1.0]]),(2,ARRAY[ARRAY[0.0,0.0]]); CREATE INDEX diskann_idx ON diskann_docs USING diskann(embedding) WITH(max_degree=2,search_list_size=4,beam_width=2,pq_bytes=1)");
}

fn explain(engine: &Engine, query: &str) -> Json {
    let result = sql(engine, &format!("EXPLAIN (FORMAT JSON) {query}"));
    let Value::Str(value) = &result.rows[0]["plan"] else {
        panic!("EXPLAIN must return text")
    };
    serde_json::from_str(value).unwrap()
}

fn nodes(plan: &Json) -> &[Json] {
    plan.get("Physical Plans")
        .and_then(Json::as_array)
        .map_or(&[], Vec::as_slice)
}

#[test]
fn diskann_explain_memory_populations_follow_private_writes_and_savepoint_undo() {
    population_undo(&Engine::new());
}

#[test]
fn diskann_explain_persistent_populations_follow_private_writes_and_savepoint_undo() {
    for provider in 0..3 {
        let (_directory, engine, _peer) = sessions(provider);
        population_undo(&engine);
    }
}

fn population_undo(engine: &Engine) {
    fixture(engine);
    let check = |current: u64, changed: u64| {
        let plan = explain(
            engine,
            "SELECT id FROM diskann_docs WHERE knn_match(embedding,ARRAY[1.0,0.0],2)",
        );
        assert_eq!(nodes(&plan)[0]["Population"]["Current Vectors"], current);
        assert_eq!(nodes(&plan)[0]["Population"]["Changed Vectors"], changed);
    };
    check(3, 0);
    sql(engine, "BEGIN; UPDATE diskann_docs SET embedding=ARRAY[ARRAY[1.0,0.0]] WHERE id=1; SAVEPOINT counted");
    check(2, 1);
    sql(
        engine,
        "INSERT INTO diskann_docs VALUES(3,ARRAY[ARRAY[1.0,0.0],ARRAY[0.0,1.0]])",
    );
    check(4, 3);
    sql(engine, "ROLLBACK TO counted");
    check(2, 1);
    sql(engine, "ROLLBACK");
    check(3, 0);
}

#[test]
fn diskann_explain_reports_the_selected_field_configuration_and_numeric_route() {
    let engine = Engine::new();
    fixture(&engine);
    let query =
        "SELECT id FROM diskann_docs d WHERE knn_match(d.embedding,ARRAY[1.0,0.0],2) AND id > 1";
    let plan = explain(&engine, query);
    assert!(plan["Plan"]
        .as_array()
        .unwrap()
        .iter()
        .all(|line| { !line.as_str().unwrap().contains("DiskANN Search") }));
    let [node] = nodes(&plan) else {
        panic!("expected one physical vector leaf: {plan}")
    };
    assert_eq!(node["Alias"], "d");
    assert_eq!(node["Field"], "embedding");
    assert_eq!(node["Route"], "approximate");
    assert_eq!(node["Candidate K"], 2);
    assert_eq!(node["Configuration"]["Logical Beam Width"], 2);
    assert_eq!(node["Configuration"]["Search List Size"], 4);
    assert_eq!(node["Configuration"]["PQ Bytes"], 1);
    assert_eq!(node["Population"]["Base Vectors"], 3);
    assert_eq!(node["Population"]["Side Vectors"], 1);
    assert_eq!(node["Candidate Refill After Residual Filter"], false);
    assert!(
        node["Estimated Work"]["Logical Page Requests"]
            .as_f64()
            .unwrap()
            > 0.0
    );
    let exact = explain(
        &engine,
        "SELECT id FROM diskann_docs WHERE knn_match(embedding,ARRAY[0.0,0.0],2)",
    );
    assert_eq!(nodes(&exact)[0]["Route"], "exact zero norm");
    assert_eq!(
        nodes(&exact)[0]["Estimated Work"]["Logical Page Requests"],
        0.0
    );
    let text = sql(&engine, &format!("EXPLAIN {query}"));
    assert!(text
        .rows
        .iter()
        .any(|row| matches!(&row["plan"], Value::Str(line) if line.contains("DiskANN Search"))));
    assert_eq!(sql(&engine, query).rows.len(), 1);
}

#[test]
fn diskann_explain_defers_volatile_arguments_and_analyze_executes_once() {
    let engine = Engine::new();
    fixture(&engine);
    sql(&engine, "CREATE SEQUENCE explain_calls");
    let query = "SELECT id FROM diskann_docs WHERE knn_match(embedding,ARRAY[1.0,0.0],nextval('explain_calls'))";
    let plan = explain(&engine, query);
    assert_eq!(nodes(&plan)[0]["Argument Status"], "deferred");
    assert_eq!(nodes(&plan)[0]["Estimated Work"], Json::Null);
    let analysis = sql(&engine, &format!("EXPLAIN (ANALYZE, FORMAT JSON) {query}"));
    let Value::Str(value) = &analysis.rows[0]["plan"] else {
        panic!("expected EXPLAIN text")
    };
    let analysis: Json = serde_json::from_str(value).unwrap();
    assert_eq!(analysis["Actual Rows"], 1);
    assert_eq!(
        sql(&engine, "SELECT nextval('explain_calls') AS n").rows[0]["n"],
        Value::Int(2)
    );
    let result = engine
        .sql(
            "EXPLAIN (FORMAT JSON) SELECT id FROM diskann_docs WHERE knn_match(embedding,$1,$2)",
            &[
                uqa_sql::SQLParam::Vector(vec![1.0, 0.0]),
                uqa_sql::SQLParam::Scalar(Value::Int(2)),
            ],
        )
        .unwrap();
    let Value::Str(value) = &result.rows[0]["plan"] else {
        panic!("expected EXPLAIN text")
    };
    assert_eq!(
        nodes(&serde_json::from_str::<Json>(value).unwrap())[0]["Candidate K"],
        2
    );
}

#[test]
fn diskann_explain_respects_cte_shadowing_and_reachable_relational_children() {
    let engine = Engine::new();
    fixture(&engine);
    let shadow = explain(&engine, "WITH diskann_docs AS (SELECT 1 AS id, ARRAY[1.0,0.0] AS embedding) SELECT id FROM diskann_docs WHERE knn_match(embedding,ARRAY[1.0,0.0],1)");
    assert_eq!(nodes(&shadow).len(), 0);
    let unused = explain(&engine, "WITH unused AS (SELECT id FROM diskann_docs WHERE knn_match(embedding,ARRAY[1.0,0.0],1)) SELECT 1");
    assert_eq!(nodes(&unused).len(), 0);
    let derived = explain(&engine, "WITH picked AS (SELECT id FROM diskann_docs WHERE knn_match(embedding,ARRAY[1.0,0.0],1)) SELECT id FROM picked UNION ALL SELECT id FROM (SELECT id FROM diskann_docs WHERE knn_match(embedding,ARRAY[0.0,0.0],1)) q");
    assert_eq!(
        nodes(&derived)
            .iter()
            .map(|node| node["Route"].clone())
            .collect::<Vec<_>>(),
        vec![json!("approximate"), json!("exact zero norm")]
    );
    let alias = explain(
        &engine,
        "SELECT n FROM diskann_docs AS d(n,v) WHERE knn_match(v,ARRAY[1.0,0.0],1)",
    );
    assert_eq!(nodes(&alias)[0]["Field"], "embedding");
}

#[test]
fn diskann_explain_retains_generations_across_persistent_index_replacement() {
    for provider in 0..3 {
        let (_directory, engine, _peer) = sessions(provider);
        fixture(&engine);
        let query = "SELECT id FROM diskann_docs WHERE knn_match(embedding,ARRAY[1.0,0.0],2)";
        let before = explain(&engine, query);
        let fixed = engine.capture_statement_read_snapshot().unwrap();
        let reader = engine.statement_read_snapshot_engine(&fixed);
        sql(&engine, "DROP INDEX diskann_idx; CREATE INDEX diskann_idx ON diskann_docs USING diskann(embedding) WITH(search_list_size=16,beam_width=1,pq_bytes=2)");
        let retained = explain(&reader, query);
        let live = explain(&engine, query);
        assert_eq!(
            nodes(&before)[0]["Generation"],
            nodes(&retained)[0]["Generation"]
        );
        assert_ne!(
            nodes(&before)[0]["Generation"],
            nodes(&live)[0]["Generation"]
        );
        assert_eq!(nodes(&retained)[0]["Configuration"]["PQ Bytes"], 1);
        assert_eq!(nodes(&live)[0]["Configuration"]["PQ Bytes"], 2);
    }
}

#[test]
fn diskann_explain_does_not_substitute_builtins_for_host_callbacks() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let engine = Engine::new();
    fixture(&engine);
    let calls = Arc::new(AtomicUsize::new(0));
    let called = calls.clone();
    engine
        .register_scalar_function("length", move |_args: &[Value]| {
            called.fetch_add(1, Ordering::Relaxed);
            Ok(Value::Int(2))
        })
        .unwrap();
    let query = "SELECT id FROM diskann_docs WHERE knn_match(embedding,ARRAY[1.0,0.0],length('x'))";
    let plan = explain(&engine, query);
    assert_eq!(calls.load(Ordering::Relaxed), 0);
    assert_eq!(nodes(&plan)[0]["Candidate K"], Json::Null);
    sql(&engine, &format!("EXPLAIN ANALYZE {query}"));
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    engine
        .register_scalar_function_with_options(
            "length",
            crate::SQLFunctionOptions::read_only(crate::SQLFunctionVolatility::Immutable),
            |_args: &[Value]| Ok(Value::Int(2)),
        )
        .unwrap();
    let plan = explain(&engine, query);
    assert_eq!(
        nodes(&plan)[0]["Candidate K"],
        Json::Null,
        "a shadowed builtin is not the selected implementation"
    );
}

#[test]
fn diskann_explain_follows_join_operands_views_and_mutation_inputs() {
    let engine = Engine::new();
    fixture(&engine);
    sql(&engine, "CREATE TABLE other_docs(id int, embedding vector(2)); INSERT INTO other_docs VALUES(3,ARRAY[0.0,1.0]); CREATE INDEX other_diskann ON other_docs USING diskann(embedding) WITH(pq_bytes=2); CREATE VIEW picked AS SELECT id, embedding FROM diskann_docs");
    let joined = explain(&engine, "SELECT d.id FROM diskann_docs d JOIN other_docs o ON d.id=o.id WHERE knn_match(d.embedding,ARRAY[1.0,0.0],1) AND knn_match(o.embedding,ARRAY[0.0,1.0],1)");
    assert_eq!(nodes(&joined).len(), 2);
    let mut widths = nodes(&joined)
        .iter()
        .map(|node| node["Configuration"]["PQ Bytes"].as_u64().unwrap())
        .collect::<Vec<_>>();
    widths.sort_unstable();
    assert_eq!(widths, vec![1, 2]);
    let operands = explain(&engine, "SELECT left_doc_id,right_doc_id FROM vector_similarity_join(diskann_docs,knn_match(embedding,ARRAY[1.0,0.0],1),other_docs,knn_match(embedding,ARRAY[0.0,1.0],1),0.0)");
    assert_eq!(nodes(&operands).len(), 2);
    assert_eq!(nodes(&operands)[0]["Configuration"]["PQ Bytes"], 1);
    assert_eq!(nodes(&operands)[1]["Configuration"]["PQ Bytes"], 2);
    let view = explain(
        &engine,
        "SELECT id FROM picked WHERE knn_match(embedding,ARRAY[1.0,0.0],1)",
    );
    assert_eq!(nodes(&view).len(), 1);
    let shadowed_query = "WITH diskann_docs AS MATERIALIZED (SELECT -1 AS id, ARRAY[0.0,1.0] AS embedding) SELECT picked.id FROM picked CROSS JOIN diskann_docs d WHERE knn_match(picked.embedding,ARRAY[1.0,0.0],1)";
    let shadowed = explain(&engine, shadowed_query);
    assert_eq!(nodes(&shadowed).len(), 1);
    assert_eq!(
        nodes(&shadowed)[0]["Generation"],
        nodes(&view)[0]["Generation"]
    );
    assert_eq!(sql(&engine, shadowed_query).rows[0]["id"], Value::Int(1));
    let update = explain(
        &engine,
        "UPDATE diskann_docs SET id=id+10 WHERE knn_match(embedding,ARRAY[1.0,0.0],1)",
    );
    assert_eq!(nodes(&update).len(), 1);
    assert_eq!(
        sql(&engine, "SELECT id FROM diskann_docs ORDER BY id").rows[0]["id"],
        Value::Int(1)
    );
    let deletion = explain(
        &engine,
        "DELETE FROM diskann_docs WHERE knn_match(embedding,ARRAY[1.0,0.0],1)",
    );
    assert_eq!(nodes(&deletion).len(), 1);
}

#[test]
fn diskann_explain_uses_each_inheritance_member_and_honors_only() {
    let engine = Engine::new();
    fixture(&engine);
    sql(&engine, "CREATE TABLE child_docs(extra int) INHERITS(diskann_docs); CREATE INDEX child_diskann ON child_docs USING diskann(embedding) WITH(pq_bytes=2)");
    let inherited = explain(
        &engine,
        "SELECT id FROM diskann_docs WHERE knn_match(embedding,ARRAY[1.0,0.0],1)",
    );
    assert_eq!(nodes(&inherited).len(), 2);
    let only = explain(
        &engine,
        "SELECT id FROM ONLY diskann_docs WHERE knn_match(embedding,ARRAY[1.0,0.0],1)",
    );
    assert_eq!(nodes(&only).len(), 1);
    assert_eq!(nodes(&only)[0]["Configuration"]["PQ Bytes"], 1);
    sql(&engine, "CREATE TABLE partitioned_docs(id int, embedding vector(2)) PARTITION BY RANGE(id); CREATE TABLE low_docs PARTITION OF partitioned_docs FOR VALUES FROM(0) TO(10); CREATE TABLE high_docs PARTITION OF partitioned_docs FOR VALUES FROM(10) TO(20); CREATE INDEX low_diskann ON low_docs USING diskann(embedding) WITH(pq_bytes=1); CREATE INDEX high_diskann ON high_docs USING diskann(embedding) WITH(pq_bytes=2)");
    let partitions = explain(
        &engine,
        "SELECT id FROM partitioned_docs WHERE knn_match(embedding,ARRAY[1.0,0.0],1)",
    );
    assert_eq!(nodes(&partitions).len(), 2);
    assert_ne!(
        nodes(&partitions)[0]["Generation"],
        nodes(&partitions)[1]["Generation"]
    );
}

#[test]
fn diskann_explain_analyze_preserves_reached_argument_diagnostics() {
    let engine = Engine::new();
    fixture(&engine);
    for vector in ["ARRAY[1.0]", "ARRAY['NaN'::real,0.0]", "ARRAY[]::real[]"] {
        let query = format!("SELECT id FROM diskann_docs WHERE knn_match(embedding,{vector},1)");
        let expected = engine.sql(&query, &[]).unwrap_err();
        let found = engine
            .sql(&format!("EXPLAIN ANALYZE {query}"), &[])
            .unwrap_err();
        assert_eq!(found.sqlstate(), expected.sqlstate());
        assert_eq!(found.to_string(), expected.to_string());
    }
}

#[test]
fn diskann_explain_survives_reopen_and_private_definition_rollback() {
    for provider in 0..3 {
        let (_directory, engine, peer) = sessions(provider);
        fixture(&engine);
        let query = "SELECT id FROM diskann_docs WHERE knn_match(embedding,ARRAY[1.0,0.0],1)";
        let base = explain(&engine, query);
        let factory = engine.storage.provider.as_ref().unwrap().clone();
        drop((engine, peer));
        let reopened = Engine::from_persistent_provider(factory).unwrap();
        assert_eq!(
            nodes(&explain(&reopened, query))[0]["Generation"],
            nodes(&base)[0]["Generation"]
        );
        sql(&reopened, "BEGIN; SAVEPOINT kept; DROP INDEX diskann_idx; CREATE INDEX diskann_idx ON diskann_docs USING diskann(embedding) WITH(pq_bytes=2)");
        let private = explain(&reopened, query);
        assert_ne!(
            nodes(&private)[0]["Generation"],
            nodes(&base)[0]["Generation"]
        );
        assert_eq!(nodes(&private)[0]["Configuration"]["PQ Bytes"], 2);
        sql(&reopened, "ROLLBACK TO kept; COMMIT");
        assert_eq!(
            nodes(&explain(&reopened, query))[0]["Generation"],
            nodes(&base)[0]["Generation"]
        );
    }
}

#[test]
fn diskann_explain_preserves_recursive_and_shared_cte_placement() {
    let engine = Engine::new();
    fixture(&engine);
    let recursive = explain(&engine, "WITH RECURSIVE walk(n) AS (SELECT id FROM diskann_docs WHERE knn_match(embedding,ARRAY[1.0,0.0],1) UNION ALL SELECT n+1 FROM walk WHERE n<2) SELECT n FROM walk WHERE n<3");
    assert_eq!(nodes(&recursive).len(), 1);
    let shared = explain(&engine, "WITH picked AS MATERIALIZED (SELECT id FROM diskann_docs WHERE knn_match(embedding,ARRAY[1.0,0.0],1)) SELECT a.id FROM picked a JOIN picked b ON a.id=b.id");
    assert_eq!(nodes(&shared).len(), 1);
    let reused = explain(&engine, "WITH picked AS NOT MATERIALIZED (SELECT id FROM diskann_docs WHERE knn_match(embedding,ARRAY[1.0,0.0],1)) SELECT a.id FROM picked a JOIN picked b ON a.id=b.id");
    assert_eq!(nodes(&reused).len(), 2);
    let scalar = explain(
        &engine,
        "SELECT (SELECT id FROM diskann_docs WHERE knn_match(embedding,ARRAY[1.0,0.0],1)) AS id",
    );
    assert_eq!(nodes(&scalar).len(), 1);
}

mod analyze;
