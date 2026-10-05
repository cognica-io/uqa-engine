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

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn add_column_checks_match_postgresql(#[case] provider: usize) {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("add-column-checks.db");
    let engine = match provider {
        0 => Engine::new(),
        1 => Engine::open(&path).unwrap(),
        2 => Engine::from_persistent_provider(std::sync::Arc::new(
            uqa_storage_sqlite::SQLiteKeyValueStorage::open(&path).unwrap(),
        ))
        .unwrap(),
        3 => Engine::from_persistent_provider(std::sync::Arc::new(
            uqa_storage_redb::RedbStorage::open(&path).unwrap(),
        ))
        .unwrap(),
        _ => unreachable!(),
    };
    crate::pg18_oracle::verify(
        &engine,
        include_str!("../../../../../tests/parity/pg18/add_column_checks_oracle.expected.json"),
    );
}
