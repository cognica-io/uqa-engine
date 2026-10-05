//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Live `PostgreSQL` 18.4 evidence for `ALTER SCHEMA ... RENAME TO`: the namespace keeps its OID, privileges and `regnamespace` constants under the new name, the tables, views, sequences, functions, types and indexes it holds follow (`pg_get_viewdef`, `pg_get_expr` and the `reg*` output print the new name), the old name no longer resolves (`42P01`, and a `search_path` entry naming it is skipped), and the diagnostics are `RenameSchema`'s: `3F000` for a missing schema, `42P06` for a taken name including the schema's own, `42939` with `PostgreSQL`'s DETAIL for a `pg_` prefix, and `42501` for a user who does not own the schema.

use tempfile::TempDir;
use uqa_engine::Engine;

fn verify_schema_rename(engine: &Engine) {
    crate::pg18_oracle::verify(
        engine,
        include_str!("../../../../tests/parity/pg18/schema_rename_oracle.expected.json"),
    );
}

#[test]
fn schema_rename_matches_postgresql_memory() {
    verify_schema_rename(&Engine::new());
}

#[test]
fn schema_rename_matches_postgresql_sqlite() {
    let directory = TempDir::new().unwrap();
    verify_schema_rename(&Engine::open(&directory.path().join("schema-rename.db")).unwrap());
}
