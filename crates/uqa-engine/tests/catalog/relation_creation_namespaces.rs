//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Live `PostgreSQL` 18.4 evidence for where a new relation goes: `RangeVarGetCreationNamespace` sends a temporary relation, one qualified with `pg_temp` and one created while `pg_temp` leads the search path to the session's temporary namespace, `RangeVarAdjustRelationPersistence` makes a relation there temporary and rejects a temporary relation elsewhere and an unlogged one there, a view over a temporary relation becomes temporary with a notice, and `heap_create` refuses `pg_catalog` after the relation takes its OID.

use uqa_engine::Engine;

#[path = "relation_creation_namespaces/builtin_storage.rs"]
mod builtin_storage;

fn verify_relation_creation_namespaces(engine: &Engine) {
    crate::pg18_oracle::verify(
        engine,
        include_str!(
            "../../../../tests/parity/pg18/relation_creation_namespace_oracle.expected.json"
        ),
    );
}

#[test]
fn relation_creation_namespaces_match_postgresql_memory() {
    verify_relation_creation_namespaces(&Engine::new());
}

#[test]
fn relation_creation_namespaces_match_postgresql_sqlite() {
    let directory = tempfile::tempdir().unwrap();
    verify_relation_creation_namespaces(
        &Engine::open(&directory.path().join("relation_namespaces.db")).unwrap(),
    );
}
