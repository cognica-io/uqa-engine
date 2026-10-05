//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Live `PostgreSQL` 18.4 evidence for `pg_get_function_sqlbody`: `RETURN` bodies and `BEGIN ATOMIC` bodies whose `SELECT`, `INSERT`, `UPDATE`, `DELETE`, `MERGE` and `WITH` statements print as `get_query_def` prints them, with parameters qualified by the routine's name once a statement has a range table.

use uqa_engine::Engine;

fn verify_function_sqlbody(engine: &Engine) {
    crate::pg18_oracle::verify(
        engine,
        include_str!("../../../../tests/parity/pg18/function_sqlbody_oracle.expected.json"),
    );
}

#[test]
fn function_sqlbody_matches_postgresql_memory() {
    verify_function_sqlbody(&Engine::new());
}

#[test]
fn function_sqlbody_matches_postgresql_sqlite() {
    let directory = tempfile::tempdir().unwrap();
    verify_function_sqlbody(&Engine::open(&directory.path().join("sqlbody.db")).unwrap());
}
