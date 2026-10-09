//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Live `PostgreSQL` 18.4 evidence for `pg_depend`, `pg_shdepend` and `pg_describe_object`: the dependencies each kind of object records, the details `DROP TYPE | DOMAIN` and `DROP ROLE` report from them, and what cascading drops remove.

use uqa_engine::Engine;

#[path = "catalog_dependencies/retention.rs"]
mod retention;

fn verify_catalog_dependencies(engine: &Engine) {
    crate::pg18_oracle::verify(
        engine,
        include_str!("../../../../tests/parity/pg18/catalog_dependencies_oracle.expected.json"),
    );
}

#[test]
fn catalog_dependencies_match_postgresql_memory() {
    verify_catalog_dependencies(&Engine::new());
}

#[test]
fn catalog_dependencies_match_postgresql_sqlite() {
    let directory = tempfile::tempdir().unwrap();
    verify_catalog_dependencies(&Engine::open(&directory.path().join("dependencies.db")).unwrap());
}
