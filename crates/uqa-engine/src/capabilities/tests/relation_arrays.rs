//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Creation names read current array identities without capturing unrelated metadata.

use super::{Engine, CATALOG_CAPTURES};
use std::sync::{atomic::Ordering, Arc};
use uqa_core::RelationIdentity;
use uqa_execution::schema::namespaces::relations::RelationCreationRuntime;
use uqa_sql::catalog::resolution::creation::CreationRelationGuards;

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_kv(2)]
#[case::redb(3)]
fn relation_array_lookups_avoid_unrelated_catalog_captures(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("array-names.db");
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
    engine.sql("CREATE TABLE items(id int PRIMARY KEY); CREATE VIEW shown AS SELECT id FROM items; CREATE FOREIGN DATA WRAPPER array_names; CREATE SERVER array_names FOREIGN DATA WRAPPER array_names; CREATE FOREIGN TABLE foreign_items(id int) SERVER array_names", &[]).unwrap();
    let catalog = engine.catalog_read_view();
    let snapshot = catalog.snapshot();
    let names =
        ["items", "shown", "foreign_items"].map(|name| RelationIdentity::new("public", name));
    let oids = [
        snapshot.tables[&names[0]].catalog_oids.array_type.unwrap(),
        snapshot.definitions.views[&names[1]]
            .relation_oids()
            .array_type
            .unwrap(),
        snapshot.definitions.foreign_tables[&names[2]]
            .relation_oids()
            .array_type
            .unwrap(),
    ];
    for fillers in [0, 60] {
        for index in 0..fillers {
            engine.sql(&format!("CREATE TABLE filler_{index}(id int PRIMARY KEY, body text CHECK(length(body)>0))"), &[]).unwrap();
        }
        CATALOG_CAPTURES.set(0);
        for (name, oid) in names.iter().zip(oids) {
            let expected = format!("_{}", name.name);
            for _ in 0..8 {
                assert!(engine.named_type_exists(&RelationIdentity::new("public", &expected)));
                assert_eq!(
                    engine.relation_array_name(name, oid),
                    Some(expected.clone())
                );
                assert_eq!(engine.relation_array_name(name, 0), None);
            }
        }
        assert!(!engine.named_type_exists(&RelationIdentity::new("other", "_items")));
        assert!(!engine.named_type_exists(&RelationIdentity::new("public", "absent")));
        assert_eq!(
            engine.relation_array_name(&RelationIdentity::new("public", "absent"), oids[0]),
            None
        );
        assert_eq!(
            CATALOG_CAPTURES.get(),
            0,
            "array-name reads captured unrelated metadata, fillers={fillers}"
        );
    }
}

#[test]
fn relation_array_names_use_current_definitions_with_a_retained_query_view() {
    let mut engine = Engine::new();
    engine.sql("CREATE TABLE items(id int)", &[]).unwrap();
    let relation = RelationIdentity::new("public", "items");
    let retained = Arc::new(crate::session::RetainedCatalogSnapshot {
        durable: engine.durable.snapshot(),
        read_view: engine.catalog_read_view(),
    });
    let retained_table = &retained.read_view.snapshot().tables[&relation];
    let oid = retained_table.catalog_oids.array_type.unwrap();
    assert_eq!(
        retained_table.row_type_array_name.as_deref(),
        Some("_items")
    );
    engine
        .sql("CREATE TYPE _items AS ENUM ('occupied')", &[])
        .unwrap();
    engine.query_catalog_snapshot = Some(retained);
    CATALOG_CAPTURES.set(0);
    assert!(engine.named_type_exists(&RelationIdentity::new("public", "__items")));
    assert_eq!(
        engine.relation_array_name(&relation, oid),
        Some("__items".into())
    );
    assert_eq!(CATALOG_CAPTURES.get(), 0);
    assert_eq!(
        engine.catalog_read_view().snapshot().tables[&relation]
            .row_type_array_name
            .as_deref(),
        Some("_items")
    );
}
