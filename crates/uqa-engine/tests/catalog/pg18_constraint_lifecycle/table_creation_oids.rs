//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Live `PostgreSQL` 18.4 evidence for the OIDs `CREATE TABLE` allocates: the relation, its array type and its row type as `heap_create_with_catalog` allocates them, the inherited expressions it stores, then each default, CHECK, NOT NULL, key index and constraint and foreign key as `DefineRelation` and the commands it queues define them, so that every object's OID stands at `PostgreSQL`'s offset from its relation's and a failing statement uses the OIDs `PostgreSQL` uses.

use tempfile::TempDir;
use uqa_engine::Engine;

fn verify_table_creation_oids(engine: &Engine) {
    crate::pg18_oracle::verify(
        engine,
        include_str!("../../../../../tests/parity/pg18/table_creation_oids_oracle.expected.json"),
    );
}

#[test]
fn table_creation_oids_match_postgresql_memory() {
    verify_table_creation_oids(&Engine::new());
}

#[test]
fn table_creation_oids_match_postgresql_sqlite() {
    let directory = TempDir::new().unwrap();
    verify_table_creation_oids(
        &Engine::open(&directory.path().join("table-creation-oids.db")).unwrap(),
    );
}
