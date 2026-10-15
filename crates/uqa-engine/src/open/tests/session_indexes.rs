//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable physical index preparation is shared while session state stays independent.

use super::{Arc, Engine, Value};
use std::sync::atomic::Ordering;

fn engine(provider: usize, path: &std::path::Path) -> Engine {
    let engine = match provider {
        0 => Engine::open(path).unwrap(),
        1 => Engine::from_persistent_provider(Arc::new(
            uqa_storage_sqlite::SQLiteKeyValueStorage::open(path).unwrap(),
        ))
        .unwrap(),
        2 => Engine::from_persistent_provider(Arc::new(
            uqa_storage_redb::RedbStorage::open(path).unwrap(),
        ))
        .unwrap(),
        _ => unreachable!(),
    };
    engine.release_automatic_statistics_client();
    engine
        .session
        .statistics_worker
        .store(true, Ordering::Release);
    engine
}

#[rstest::rstest]
#[case::sqlite(0)]
#[case::sqlite_kv(1)]
#[case::redb(2)]
fn independent_sessions_reuse_physical_index_preparation(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let engine = engine(provider, &directory.path().join("indexes.db"));
    engine.sql("CREATE TABLE items(id int PRIMARY KEY,label text); INSERT INTO items VALUES(1,'ready')", &[]).unwrap();
    for fillers in [0, 60] {
        for index in 0..fillers {
            engine.sql(&format!("CREATE TABLE filler_{index}(id int PRIMARY KEY,body text); CREATE INDEX filler_idx_{index} ON filler_{index}(body)"), &[]).unwrap();
        }
        let first = engine.new_session().unwrap();
        let original = first.physical_index_definitions().unwrap();
        for _ in 0..4 {
            let next = engine.new_session().unwrap();
            assert!(Arc::ptr_eq(
                &first.durable.catalog_indexes.snapshot(),
                &next.durable.catalog_indexes.snapshot()
            ));
            assert!(
                Arc::ptr_eq(&original, &next.physical_index_definitions().unwrap()),
                "an unchanged registry was parsed again: provider={provider}, fillers={fillers}"
            );
            assert_eq!(
                next.sql("SELECT label FROM items WHERE id=1", &[])
                    .unwrap()
                    .value_at(0, 0),
                Some(&Value::Str("ready".into()))
            );
            assert!(!Arc::ptr_eq(
                &first.try_table("items").unwrap().unwrap(),
                &next.try_table("items").unwrap().unwrap()
            ));
        }
        assert!(Arc::ptr_eq(
            &original,
            &engine.physical_index_definitions().unwrap()
        ));
    }
}

#[rstest::rstest]
#[case::sqlite(0)]
#[case::sqlite_kv(1)]
#[case::redb(2)]
fn shared_index_preparation_preserves_private_and_committed_definitions(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let engine = engine(provider, &directory.path().join("catalog.db"));
    engine.sql("CREATE TABLE items(id int PRIMARY KEY,label text); INSERT INTO items VALUES(1,'ready')", &[]).unwrap();
    let table = engine.try_table("items").unwrap().unwrap();
    let columns = table.columns.snapshot();
    let constraints = table.key_constraints.snapshot();
    let fields =
        |definitions: &uqa_execution::catalog::index::physical::PhysicalIndexDefinitions| {
            definitions
                .indexable_fields("public.items", &columns, &constraints)
                .unwrap()
        };
    let retained = engine.physical_index_definitions().unwrap();
    let original = fields(&retained);
    let writer = engine.new_session().unwrap();
    writer
        .sql("BEGIN; CREATE INDEX private_label ON items(label)", &[])
        .unwrap();
    let private = writer.physical_index_definitions().unwrap();
    assert!(fields(&private).len() > original.len());
    assert_eq!(
        fields(&engine.physical_index_definitions().unwrap()),
        original
    );
    assert_eq!(fields(&retained), original);
    writer.sql("ROLLBACK", &[]).unwrap();
    assert_eq!(
        fields(&writer.physical_index_definitions().unwrap()),
        original
    );
    writer
        .sql("CREATE INDEX committed_label ON items(label)", &[])
        .unwrap();
    assert_eq!(
        engine
            .sql("SELECT id FROM items WHERE label='ready'", &[])
            .unwrap()
            .value_at(0, 0),
        Some(&Value::Int(1))
    );
    let committed = engine.physical_index_definitions().unwrap();
    assert!(fields(&committed).len() > original.len());
    assert_eq!(fields(&retained), original);
    assert!(fields(&private).len() > original.len());
    assert_eq!(
        writer
            .sql(
                "SELECT count(*) FROM pg_indexes WHERE tablename='items'",
                &[]
            )
            .unwrap()
            .value_at(0, 0),
        Some(&Value::Int(2))
    );
    let sibling = engine.new_session().unwrap();
    writer.cancel();
    assert_eq!(
        sibling
            .sql("SELECT label FROM items WHERE id=1", &[])
            .unwrap()
            .value_at(0, 0),
        Some(&Value::Str("ready".into()))
    );
}
