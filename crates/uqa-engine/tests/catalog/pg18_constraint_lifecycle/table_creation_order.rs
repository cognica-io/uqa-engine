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
