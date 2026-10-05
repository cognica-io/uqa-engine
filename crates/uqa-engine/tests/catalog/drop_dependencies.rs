//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Live `PostgreSQL` 18.4 evidence for what each `DROP` removes and reports: the error, detail and hint of a restricted drop, the notice of a cascading one, and the objects that remain, for tables, views, materialized views, sequences, routines, indexes, schemas, types, domains, columns, constraints, defaults, triggers and rules.

use uqa_engine::Engine;

fn verify_drop_dependencies(engine: &Engine) {
    crate::pg18_oracle::verify(
        engine,
        include_str!("../../../../tests/parity/pg18/drop_dependencies_oracle.expected.json"),
    );
}

#[test]
fn drop_dependencies_match_postgresql_memory() {
    verify_drop_dependencies(&Engine::new());
}

#[test]
fn drop_dependencies_match_postgresql_sqlite() {
    let directory = tempfile::tempdir().unwrap();
    verify_drop_dependencies(&Engine::open(&directory.path().join("drops.db")).unwrap());
}
