//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Live `PostgreSQL` 18.4 evidence for `transformColumnDefinition`: an array of SERIAL, constraint attributes placed after a constraint that cannot take them or repeated, and a column's clauses checked against one another in written order (repeated defaults, identities and generation expressions, a default with an identity or generation expression, NULL against NOT NULL, PRIMARY KEY, SERIAL and identity, NO INHERIT conflicts, and NOT NULL names), each once `CREATE TABLE` or `ALTER TABLE ... ADD COLUMN` has found its relation and the column's type.

use tempfile::TempDir;
use uqa_engine::Engine;

fn verify_column_declarations(engine: &Engine) {
    crate::pg18_oracle::verify(
        engine,
        include_str!("../../../../../tests/parity/pg18/column_declaration_oracle.expected.json"),
    );
}

#[test]
fn column_declarations_match_postgresql_memory() {
    verify_column_declarations(&Engine::new());
}

#[test]
fn column_declarations_match_postgresql_sqlite() {
    let directory = TempDir::new().unwrap();
    verify_column_declarations(
        &Engine::open(&directory.path().join("column-declarations.db")).unwrap(),
    );
}
