//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Full TPC-H query-set compatibility coverage.

#[path = "support/tpch_fixture.rs"]
mod tpch_fixture;

#[test]
fn all_tpch_queries_match_postgresql_18() {
    let engine = tpch_fixture::load_engine();
    assert_queries_match(&engine, &tpch_fixture::load_queries());
}

#[test]
fn spilled_tpch_results_match_postgresql_18() {
    let engine = tpch_fixture::load_engine();
    engine.sql("SET work_mem TO '64kB'", &[]).unwrap();
    let queries = tpch_fixture::load_queries();
    assert_queries_match(&engine, &queries);

    // Q18 groups line items by order. Verify the limited run actually wrote and read temporary blocks.
    let explain = format!(
        "EXPLAIN (ANALYZE, BUFFERS, TIMING FALSE, FORMAT JSON) {}",
        queries[17]
    );
    let result = engine.sql(&explain, &[]).unwrap();
    let Some(uqa_core::Value::Json(text)) = result.value_at(0, 0) else {
        panic!("expected EXPLAIN JSON");
    };
    let document: serde_json::Value = serde_json::from_str(text).unwrap();
    let plan = &document[0]["Plan"];
    for field in ["Temp Written Blocks", "Temp Read Blocks"] {
        assert!(
            plan[field].as_u64().unwrap() > 0,
            "Q18 {field}: {}",
            plan[field]
        );
    }
}

fn assert_queries_match(engine: &uqa_engine::Engine, queries: &[String]) {
    let expected = tpch_fixture::load_expected_results();
    assert_eq!(queries.len(), expected.len());
    for (index, (query, expected)) in queries.iter().zip(expected).enumerate() {
        eprintln!("running TPC-H Q{:02}", index + 1);
        let actual = engine
            .sql(query, &[])
            .unwrap_or_else(|error| panic!("TPC-H Q{:02} failed: {error}", index + 1));
        assert_eq!(
            tpch_fixture::canonical_result(&actual),
            expected,
            "TPC-H Q{:02} differs from PostgreSQL 18",
            index + 1
        );
    }
}
