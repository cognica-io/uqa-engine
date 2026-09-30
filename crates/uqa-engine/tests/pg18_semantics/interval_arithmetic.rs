//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Live `PostgreSQL` 18.4 evidence for interval arithmetic: `interval_mul` and `interval_div` cascade fractional months and days downward, `sum(interval)` and `avg(interval)` add field by field and divide by the count, the time types ignore an interval's calendar fields, and every date, time, timestamp and interval overflow is SQLSTATE 22008.

use uqa_engine::Engine;

fn verify_interval_arithmetic(engine: &Engine) {
    crate::pg18_oracle::verify(
        engine,
        include_str!("../../../../tests/parity/pg18/interval_arithmetic_oracle.expected.json"),
    );
}

#[test]
fn interval_arithmetic_matches_postgresql_memory() {
    verify_interval_arithmetic(&Engine::new());
}

#[test]
fn interval_arithmetic_matches_postgresql_sqlite() {
    let directory = tempfile::tempdir().unwrap();
    verify_interval_arithmetic(
        &Engine::open(&directory.path().join("interval-arithmetic.db")).unwrap(),
    );
}
