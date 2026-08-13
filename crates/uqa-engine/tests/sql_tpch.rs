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
    let queries = tpch_fixture::load_queries();
    let expected = tpch_fixture::load_expected_results();
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
