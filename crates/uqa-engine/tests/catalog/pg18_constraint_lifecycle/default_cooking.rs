//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Live `PostgreSQL` 18.4 evidence for `cookDefault`: defaults and generation expressions of `CREATE TABLE`, `ADD COLUMN`, `SET DEFAULT` and `CREATE DOMAIN` read `unknown` literals with the type's input function and store the constant, need an assignment cast for a typed expression, store no default for a NULL constant, reject subqueries, aggregates, window and set-returning functions and references to generated columns with `PostgreSQL`'s SQLSTATEs, and print their constants as `get_const_expr` prints them.

use tempfile::TempDir;
use uqa_engine::Engine;

fn verify_default_cooking(engine: &Engine) {
    crate::pg18_oracle::verify(
        engine,
        include_str!("../../../../../tests/parity/pg18/default_cooking_oracle.expected.json"),
    );
}

#[test]
fn default_cooking_matches_postgresql_memory() {
    verify_default_cooking(&Engine::new());
}

#[test]
fn default_cooking_matches_postgresql_sqlite() {
    let directory = TempDir::new().unwrap();
    verify_default_cooking(&Engine::open(&directory.path().join("default-cooking.db")).unwrap());
}
