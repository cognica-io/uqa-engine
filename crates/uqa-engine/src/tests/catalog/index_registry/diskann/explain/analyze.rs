//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

const QUERY: &str = "SELECT id FROM diskann_docs WHERE knn_match(embedding,ARRAY[1.0,0.0],2)";

fn analyze(engine: &Engine, query: &str) -> Json {
    let result = sql(engine, &format!("EXPLAIN (ANALYZE, FORMAT JSON) {query}"));
    let Value::Str(value) = &result.rows[0]["plan"] else {
        panic!("expected text");
    };
    serde_json::from_str(value).unwrap()
}

fn searches(plan: &Json) -> &[Json] {
    plan["Vector Searches"].as_array().unwrap()
}

fn verify(engine: &Engine) {
    fixture(engine);
    let ordinary = explain(engine, QUERY);
    assert!(ordinary.get("Vector Searches").is_none());
    let measured = analyze(engine, QUERY);
    let [search] = searches(&measured) else {
        panic!("one physical invocation: {measured}");
    };
    assert_eq!(search["Relation"], "public.diskann_docs");
    assert_eq!(search["Generation"], nodes(&ordinary)[0]["Generation"]);
    assert_eq!(search["Field"], "embedding");
    assert_eq!(search["Operation"], "knn");
    assert_eq!(search["Requested K"], 2);
    assert_eq!(search["Returned Documents"], 2);
    assert_eq!(search["Route"], "approximate");
    assert_eq!(search["Scoring"]["Reranked"]["Vectors"], 3);
    assert_eq!(search["Scoring"]["Exact"]["Vectors"], 0);
    assert!(search["Traversal"]["PQ Estimates"].as_u64().unwrap() > 0);
    let exact = analyze(engine, &QUERY.replace("ARRAY[1.0,0.0]", "ARRAY[0.0,0.0]"));
    assert_eq!(searches(&exact)[0]["Route"], "exact zero norm");
    assert_eq!(searches(&exact)[0]["Scoring"]["Exact"]["Vectors"], 3);
    assert_eq!(searches(&exact)[0]["Traversal"]["PQ Estimates"], 0);
    let calibrated = analyze(
        engine,
        &QUERY.replace("knn_match", "calibrated_vector_match"),
    );
    assert_eq!(searches(&calibrated).len(), 1);
    assert_eq!(searches(&calibrated)[0]["Requested K"], 2);
    let text = sql(engine, &format!("EXPLAIN ANALYZE {QUERY}"));
    assert!(text
        .rows
        .iter()
        .any(|row| matches!(&row["plan"], Value::Str(line) if line.contains("Vector Search:"))));
}

#[test]
fn diskann_analyze_records_actual_memory_searches_in_text_and_json() {
    verify(&Engine::new());
}

#[test]
fn diskann_analyze_records_actual_provider_searches_in_text_and_json() {
    for provider in 0..3 {
        let (_directory, engine, _peer) = sessions(provider);
        verify(&engine);
    }
}

#[test]
fn diskann_analyze_preserves_dml_execution_and_transaction_undo() {
    verify_dml(&Engine::new());
    for provider in 0..3 {
        let (_directory, engine, _peer) = sessions(provider);
        verify_dml(&engine);
    }
}

fn verify_dml(engine: &Engine) {
    fixture(engine);
    sql(engine, "CREATE TABLE picked(id int)");
    sql(engine, "BEGIN");
    let plan = analyze(engine, &format!("INSERT INTO picked {QUERY}"));
    assert_eq!(plan["Affected Rows"], 2);
    assert_eq!(searches(&plan).len(), 1);
    assert_eq!(sql(engine, "SELECT * FROM picked").rows.len(), 2);
    sql(engine, "ROLLBACK");
    assert_eq!(sql(engine, "SELECT * FROM picked").rows.len(), 0);
    assert_eq!(
        engine
            .runtime
            .sql_execution_depth
            .load(std::sync::atomic::Ordering::Relaxed),
        0
    );
    assert!(engine.runtime.diagnostics.capture().is_none());
}

#[test]
fn diskann_analyze_nested_host_queries_execute_once_and_restore_the_outer_scope() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let engine = Arc::new(Engine::new());
    fixture(&engine);
    let weak = Arc::downgrade(&engine);
    let calls = Arc::new(AtomicUsize::new(0));
    let called = Arc::clone(&calls);
    engine
        .register_scalar_function("nested_probe", move |_args: &[Value]| {
            called.fetch_add(1, Ordering::Relaxed);
            let engine = weak.upgrade().unwrap();
            let nested = analyze(&engine, QUERY);
            assert_eq!(searches(&nested).len(), 1);
            Ok(Value::Int(2))
        })
        .unwrap();
    let query = QUERY.replace(",2)", ",nested_probe())");
    let parent = analyze(&engine, &query);
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    assert_eq!(searches(&parent).len(), 2);
    assert!(engine.runtime.diagnostics.capture().is_none());
    assert_eq!(searches(&analyze(&engine, QUERY)).len(), 1);
}

#[test]
fn diskann_analyze_keeps_independent_host_sessions_out_of_the_outer_report() {
    let engine = Engine::new();
    fixture(&engine);
    let independent = Engine::new();
    fixture(&independent);
    engine
        .register_scalar_function("independent_probe", move |_args: &[Value]| {
            assert_eq!(searches(&analyze(&independent, QUERY)).len(), 1);
            Ok(Value::Int(2))
        })
        .unwrap();
    let query = QUERY.replace(",2)", ",independent_probe())");
    assert_eq!(searches(&analyze(&engine, &query)).len(), 1);
}

#[test]
fn diskann_analyze_reports_parallel_fields_as_separate_actual_invocations() {
    let engine = Engine::new();
    fixture(&engine);
    sql(&engine, "ALTER TABLE diskann_docs ADD COLUMN other tensor(2); UPDATE diskann_docs SET other=embedding; CREATE INDEX other_idx ON diskann_docs USING diskann(other) WITH(max_degree=2,search_list_size=4,beam_width=2,pq_bytes=1)");
    let plan = analyze(&engine, "SELECT id FROM diskann_docs WHERE knn_match(embedding,ARRAY[1.0,0.0],2) OR knn_match(other,ARRAY[0.0,1.0],1)");
    let mut actual = searches(&plan)
        .iter()
        .map(|search| {
            (
                search["Field"].as_str().unwrap(),
                search["Requested K"].as_u64().unwrap(),
            )
        })
        .collect::<Vec<_>>();
    actual.sort_unstable();
    assert_eq!(actual, [("embedding", 2), ("other", 1)]);
    assert_ne!(
        searches(&plan)[0]["Generation"],
        searches(&plan)[1]["Generation"]
    );
}

#[test]
fn diskann_analyze_includes_host_threshold_calls_from_the_original_index_invocation() {
    let engine = Arc::new(Engine::new());
    fixture(&engine);
    let weak = Arc::downgrade(&engine);
    engine
        .register_scalar_function("threshold_probe", move |_args: &[Value]| {
            let rows = weak.upgrade().unwrap().vector_similarity_search(
                "diskann_docs",
                "embedding",
                vec![1.0, 0.0],
                0.5,
            )?;
            assert_eq!(rows.len(), 1);
            Ok(Value::Int(2))
        })
        .unwrap();
    let plan = analyze(&engine, &QUERY.replace(",2)", ",threshold_probe())"));
    assert_eq!(searches(&plan).len(), 2);
    let threshold = searches(&plan)
        .iter()
        .find(|search| search["Operation"] == "threshold")
        .unwrap();
    assert_eq!(threshold["Route"], "exact threshold");
    assert_eq!(threshold["Threshold"], 0.5);
    assert_eq!(threshold["Returned Documents"], 1);
    assert_eq!(threshold["Scoring"]["Exact"]["Vectors"], 3);
}

#[test]
fn diskann_analyze_keeps_the_executed_retained_generation_across_replacement() {
    for provider in 0..3 {
        let (_directory, engine, _peer) = sessions(provider);
        fixture(&engine);
        let before = analyze(&engine, QUERY);
        let fixed = engine.capture_statement_read_snapshot().unwrap();
        let reader = engine.statement_read_snapshot_engine(&fixed);
        sql(&engine, "DROP INDEX diskann_idx; CREATE INDEX diskann_idx ON diskann_docs USING diskann(embedding) WITH(search_list_size=16,beam_width=1,pq_bytes=2)");
        let retained = analyze(&reader, QUERY);
        let current = analyze(&engine, QUERY);
        assert_eq!(
            searches(&before)[0]["Generation"],
            searches(&retained)[0]["Generation"]
        );
        assert_ne!(
            searches(&before)[0]["Generation"],
            searches(&current)[0]["Generation"]
        );
    }
}

#[test]
fn diskann_analyze_distinguishes_non_finite_norm_and_unexecuted_or_unsupported_leaves() {
    let engine = Engine::new();
    fixture(&engine);
    let overflow = analyze(
        &engine,
        &QUERY.replace("ARRAY[1.0,0.0]", "ARRAY[3e38,3e38]"),
    );
    assert_eq!(searches(&overflow)[0]["Route"], "exact non-finite norm");
    assert_eq!(searches(&overflow)[0]["Scoring"]["Exact"]["Vectors"], 3);
    let zero = engine
        .sql(
            &format!("EXPLAIN ANALYZE {}", QUERY.replace(",2)", ",0)")),
            &[],
        )
        .unwrap_err();
    assert!(zero.to_string().contains("knn_match.k must be positive"));
    assert!(engine.runtime.diagnostics.capture().is_none());
    let empty = analyze(&engine, &QUERY.replace("WHERE ", "WHERE FALSE AND "));
    assert_eq!(searches(&empty).len(), 0);
    assert_eq!(empty["Actual Rows"], 0);
    sql(
        &engine,
        "DROP INDEX diskann_idx; CREATE INDEX hnsw_idx ON diskann_docs USING hnsw(embedding)",
    );
    assert_eq!(searches(&analyze(&engine, QUERY)).len(), 0);
}

#[test]
fn diskann_analyze_reports_deferred_cursor_searches_in_the_fetching_invocation() {
    for scroll in ["NO SCROLL", "SCROLL"] {
        verify_cursor_invocations(scroll);
    }
}

fn verify_cursor_invocations(scroll: &str) {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let engine = Arc::new(Engine::new());
    fixture(&engine);
    let calls = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&calls);
    engine
        .register_scalar_function("cursor_pool", move |_args: &[Value]| {
            counted.fetch_add(1, Ordering::Relaxed);
            Ok(Value::Int(2))
        })
        .unwrap();
    let query = QUERY.replace(",2)", ",cursor_pool())");
    sql(&engine, "BEGIN");
    let declaration = format!("DECLARE pending_vectors {scroll} CURSOR FOR SELECT id FROM (VALUES(10),(11)) AS warm(id) UNION ALL {query} UNION ALL {query}");
    sql(&engine, &declaration);
    assert_eq!(sql(&engine, "FETCH 2 FROM pending_vectors").rows.len(), 2);
    assert_eq!(calls.load(Ordering::Relaxed), 0);
    let ordinary_counts = std::array::from_fn::<_, 2, _>(|_| {
        assert_eq!(sql(&engine, "FETCH 2 FROM pending_vectors").rows.len(), 2);
        calls.load(Ordering::Relaxed)
    });
    sql(&engine, "CLOSE pending_vectors");
    calls.store(0, Ordering::Relaxed);
    sql(&engine, &declaration);
    assert_eq!(sql(&engine, "FETCH 2 FROM pending_vectors").rows.len(), 2);
    assert_eq!(calls.load(Ordering::Relaxed), 0);
    let weak = Arc::downgrade(&engine);
    engine
        .register_scalar_function("fetch_vectors", move |_args: &[Value]| {
            let engine = weak.upgrade().unwrap();
            let remaining = engine.sql("FETCH 2 FROM pending_vectors", &[])?;
            assert_eq!(remaining.rows.len(), 2);
            Ok(Value::Int(2))
        })
        .unwrap();
    let weak = Arc::downgrade(&engine);
    engine
        .register_scalar_function("analyze_fetch", move |_args: &[Value]| {
            let nested = analyze(&weak.upgrade().unwrap(), "SELECT fetch_vectors()");
            assert_eq!(searches(&nested).len(), 1, "{nested}");
            Ok(Value::Int(2))
        })
        .unwrap();
    for expected in ordinary_counts {
        let plan = analyze(&engine, "SELECT analyze_fetch()");
        assert_eq!(searches(&plan).len(), 1, "{plan}");
        assert_eq!(calls.load(Ordering::Relaxed), expected, "{scroll}");
    }
    sql(&engine, "CLOSE pending_vectors; ROLLBACK");
}
