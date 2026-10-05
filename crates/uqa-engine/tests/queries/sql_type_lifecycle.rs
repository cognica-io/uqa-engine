//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Live `PostgreSQL` 18.4 evidence for the lifecycle of user-defined types: `ALTER TYPE | DOMAIN ... RENAME TO | SET SCHEMA | OWNER TO`, `GRANT | REVOKE ... ON TYPE | DOMAIN` with `typacl` and `has_type_privilege`, `USAGE` enforcement, stored references that follow renames, dependency-aware drops and search-path visibility of type names.

use uqa_engine::Engine;

fn verify_type_lifecycle(engine: &Engine) {
    crate::pg18_oracle::verify(
        engine,
        include_str!("../../../../tests/parity/pg18/type_lifecycle_oracle.expected.json"),
    );
}

#[test]
fn type_lifecycle_matches_postgresql_memory() {
    verify_type_lifecycle(&Engine::new());
}

#[test]
fn type_lifecycle_matches_postgresql_sqlite() {
    let directory = tempfile::tempdir().unwrap();
    verify_type_lifecycle(&Engine::open(&directory.path().join("types.db")).unwrap());
}
