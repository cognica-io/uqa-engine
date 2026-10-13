//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Live `PostgreSQL` 18.4 evidence for the order in which `CREATE TABLE` analyzes a table: the sequences of its columns before `MergeAttributes`, the defaults and generation expressions in column order before the partition bound and key, the keys a partition clones before its CHECK constraints, and the CHECKs in written order, each named, merged or rejected as `AddRelationNewConstraints` does.

use tempfile::TempDir;
use uqa_engine::Engine;

fn verify_table_creation_order(engine: &Engine) {
    crate::pg18_oracle::verify(
        engine,
        include_str!("../../../../../tests/parity/pg18/create_table_order_oracle.expected.json"),
    );
}

#[test]
fn table_creation_order_matches_postgresql_memory() {
    verify_table_creation_order(&Engine::new());
}

#[test]
fn table_creation_order_matches_postgresql_sqlite() {
    let directory = TempDir::new().unwrap();
    verify_table_creation_order(
        &Engine::open(&directory.path().join("table-creation-order.db")).unwrap(),
    );
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn partition_column_options_inherit_type_and_collation(#[case] provider: usize) {
    let directory = TempDir::new().unwrap();
    let engine = scope_engine(provider, &directory.path().join("partition-options.db"));
    crate::pg18_oracle::verify(&engine, include_str!("partition_column_options.json"));
}

fn scope_engine(provider: usize, path: &std::path::Path) -> Engine {
    match provider {
        0 => Engine::new(),
        1 => Engine::open(path).unwrap(),
        2 => Engine::from_persistent_provider(std::sync::Arc::new(
            uqa_storage_sqlite::SQLiteKeyValueStorage::open(path).unwrap(),
        ))
        .unwrap(),
        3 => Engine::from_persistent_provider(std::sync::Arc::new(
            uqa_storage_redb::RedbStorage::open(path).unwrap(),
        ))
        .unwrap(),
        _ => unreachable!(),
    }
}
