//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Live `PostgreSQL` 18.4 evidence for `unknown` literals taking the types their operators and functions select: comparisons, IN, BETWEEN, ANY and ALL, NULLIF, IS DISTINCT FROM and `anycompatible` array functions; together with `format()`, explicit `VARIADIC` arrays for `VARIADIC "any"` built-ins, JSON object builder diagnostics, window output names and whole-row values.

use uqa_engine::Engine;

fn verify_coercion(engine: &Engine) {
    crate::pg18_oracle::verify(
        engine,
        include_str!("../../../../tests/parity/pg18/literal_coercion_oracle.expected.json"),
    );
}

#[test]
fn literal_coercion_matches_postgresql_memory() {
    verify_coercion(&Engine::new());
}

#[test]
fn literal_coercion_matches_postgresql_sqlite() {
    let directory = tempfile::tempdir().unwrap();
    verify_coercion(&Engine::open(&directory.path().join("coercion.db")).unwrap());
}
