//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Live `PostgreSQL` 18.4 evidence for `coerce_to_boolean`: the conditions of `WHERE`, `HAVING`, `JOIN ... ON`, searched `CASE`, `AND`, `OR`, `NOT` and `FILTER` in queries, updates and deletes must be boolean, and an `unknown` literal in one is read as boolean input.

use uqa_engine::Engine;

fn verify_boolean_conditions(engine: &Engine) {
    crate::pg18_oracle::verify(
        engine,
        include_str!("../../../../tests/parity/pg18/boolean_conditions_oracle.expected.json"),
    );
}

#[test]
fn boolean_conditions_match_postgresql_memory() {
    verify_boolean_conditions(&Engine::new());
}

#[test]
fn boolean_conditions_match_postgresql_sqlite() {
    let directory = tempfile::tempdir().unwrap();
    verify_boolean_conditions(
        &Engine::open(&directory.path().join("boolean-conditions.db")).unwrap(),
    );
}
