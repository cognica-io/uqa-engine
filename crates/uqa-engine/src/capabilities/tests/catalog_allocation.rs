//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! OID allocation retains the current catalog while observing private definition changes and rollback.

use super::{Engine, Value, CATALOG_CAPTURES};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use uqa_sql::{
    ast::ConstraintCatalogIdentity,
    schema::constraint_metadata::{CatalogObjectAllocator, CatalogOidClass},
};

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_kv(2)]
#[case::redb(3)]
fn catalog_allocation_reuses_metadata_until_ddl_or_rollback(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("allocation.db");
    let engine = match provider {
        0 => Engine::new(),
        1 => Engine::open(&path).unwrap(),
        2 => Engine::from_persistent_provider(Arc::new(
            uqa_storage_sqlite::SQLiteKeyValueStorage::open(&path).unwrap(),
        ))
        .unwrap(),
        3 => Engine::from_persistent_provider(Arc::new(
            uqa_storage_redb::RedbStorage::open(&path).unwrap(),
        ))
        .unwrap(),
        _ => unreachable!(),
    };
    engine.release_automatic_statistics_client();
    engine
        .session
        .statistics_worker
        .store(true, Ordering::Release);
    verify_catalog_allocation(&engine, false);
    verify_catalog_allocation(&engine, true);
}

fn verify_catalog_allocation(engine: &Engine, temporary: bool) {
    let persistence = if temporary { "TEMP" } else { "" };
    engine
        .sql(
            &format!("CREATE {persistence} TABLE items(id int PRIMARY KEY); BEGIN"),
            &[],
        )
        .unwrap();
    let owner = uqa_core::RelationIdentity::from_legacy_name(
        &engine.try_resolve_table_name("items").unwrap().unwrap(),
    )
    .unwrap();
    let mut allocator = engine
        .catalog_identity_reservation_context()
        .allocator(uqa_execution::catalog::identity::allocate_catalog_object_id);
    CATALOG_CAPTURES.set(0);
    let mut assigned = std::collections::BTreeSet::new();
    for _ in 0..20 {
        let oid = allocator
            .allocate_catalog_oid(CatalogOidClass::Relation, &[7; 16])
            .unwrap();
        assert!(assigned.insert(oid));
    }
    assert_eq!(CATALOG_CAPTURES.get(), 1);

    engine
        .sql(
            "SAVEPOINT before_check; ALTER TABLE items ADD CONSTRAINT allocated_check CHECK(id>0)",
            &[],
        )
        .unwrap();
    let result = engine
        .sql(
            "SELECT oid FROM pg_constraint WHERE conname='allocated_check'",
            &[],
        )
        .unwrap();
    let Some(Value::Int(oid)) = result.value_at(0, 0) else {
        panic!("constraint OID")
    };
    let identity = ConstraintCatalogIdentity {
        object_id: [9; 16],
        oid: *oid,
    };
    CATALOG_CAPTURES.set(0);
    assert!(allocator
        .include_catalog_identity(&owner, CatalogOidClass::Constraint, identity)
        .is_err());
    assert_eq!(CATALOG_CAPTURES.get(), 1);

    engine.sql("ROLLBACK TO before_check", &[]).unwrap();
    CATALOG_CAPTURES.set(0);
    allocator
        .include_catalog_identity(&owner, CatalogOidClass::Constraint, identity)
        .unwrap();
    assert_eq!(CATALOG_CAPTURES.get(), 1);
    engine.sql("ROLLBACK", &[]).unwrap();
    assert_eq!(
        engine
            .sql(
                "SELECT count(*) FROM pg_constraint WHERE conname='allocated_check'",
                &[]
            )
            .unwrap()
            .value_at(0, 0),
        Some(&Value::Int(0))
    );
}
