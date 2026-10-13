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
fn declaration_expressions_see_their_own_relation_and_type(#[case] provider: usize) {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("declaration-scope.db");
    let open = || scope_engine(provider, &path);
    let engine = open();
    crate::pg18_oracle::verify(&engine, include_str!("declaration_scope.json"));
    drop(engine);
    if provider != 0 {
        crate::pg18_oracle::verify(&open(), include_str!("declaration_scope_reopen.json"));
    }
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

#[rstest::rstest]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn declared_relation_visibility_follows_transaction_commit(#[case] provider: usize) {
    let directory = TempDir::new().unwrap();
    let writer = scope_engine(provider, &directory.path().join("declaration-peers.db"));
    let reader = writer.new_session().unwrap();
    let reference: serde_json::Value =
        serde_json::from_str(include_str!("declaration_peers.json")).unwrap();
    for case in reference["cases"].as_array().unwrap() {
        let engine = match case["session"].as_u64().unwrap() {
            0 => &writer,
            1 => &reader,
            _ => unreachable!(),
        };
        let transcript = serde_json::json!({
            "postgresql_version": reference["postgresql_version"],
            "cases": [case],
        });
        crate::pg18_oracle::verify(engine, &transcript.to_string());
    }
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
