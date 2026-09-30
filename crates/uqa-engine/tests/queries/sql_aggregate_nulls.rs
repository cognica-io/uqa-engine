//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Live `PostgreSQL` 18.4 evidence for aggregates over NULL inputs and enum values: `array_agg`, `json_agg`, `jsonb_agg` and `string_agg` with `ORDER BY`, `DISTINCT` and `FILTER`, array accumulation and its errors, domain element types, and `min`, `max`, `count`, ordered-set aggregates, `enum_range` and window aggregates over an enum whose labels were inserted out of order.

use uqa_engine::Engine;

fn verify_aggregate_nulls(engine: &Engine) {
    crate::pg18_oracle::verify(
        engine,
        include_str!("../../../../tests/parity/pg18/aggregate_nulls_oracle.expected.json"),
    );
}

#[test]
fn aggregate_nulls_match_postgresql_memory() {
    verify_aggregate_nulls(&Engine::new());
}

#[test]
fn aggregate_nulls_match_postgresql_sqlite() {
    let directory = tempfile::tempdir().unwrap();
    verify_aggregate_nulls(&Engine::open(&directory.path().join("aggregate-nulls.db")).unwrap());
}
