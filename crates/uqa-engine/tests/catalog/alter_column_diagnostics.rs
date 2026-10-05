//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Live `PostgreSQL` 18.4 evidence for the checks `ALTER TABLE ... ALTER COLUMN` makes before it changes a default, a generation expression or a `NOT NULL` constraint: missing, system, identity and generated columns, and columns that a partition or an inheritance child takes from its parent.

use uqa_engine::Engine;

fn verify_alter_column_diagnostics(engine: &Engine) {
    crate::pg18_oracle::verify(
        engine,
        include_str!("../../../../tests/parity/pg18/alter_column_diagnostics_oracle.expected.json"),
    );
}

#[test]
fn alter_column_diagnostics_match_postgresql_memory() {
    verify_alter_column_diagnostics(&Engine::new());
}

#[test]
fn alter_column_diagnostics_match_postgresql_sqlite() {
    let directory = tempfile::tempdir().unwrap();
    verify_alter_column_diagnostics(
        &Engine::open(&directory.path().join("alter-column.db")).unwrap(),
    );
}
