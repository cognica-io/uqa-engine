//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Mutation row identities retain selected relation metadata without recapturing the whole catalog.

use super::{Engine, Value, CATALOG_CAPTURES};
use std::sync::{atomic::Ordering, Arc};

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_kv(2)]
#[case::redb(3)]
fn mutation_row_identity_catalog_captures_do_not_grow_with_rows(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("row-identities.db");
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
    engine.sql("CREATE TABLE items(id int PRIMARY KEY, v int NOT NULL); INSERT INTO items SELECT i,0 FROM generate_series(1,9) AS g(i)", &[]).unwrap();
    let expected_oid = engine
        .sql("SELECT 'items'::regclass::oid", &[])
        .unwrap()
        .value_at(0, 0)
        .cloned()
        .unwrap();
    for returning in [false, true] {
        let captures = [1, 9].map(|rows| {
            let suffix = if returning { " RETURNING tableoid" } else { "" };
            CATALOG_CAPTURES.set(0);
            let result = engine
                .sql(
                    &format!("UPDATE items SET v=v+1 WHERE id <= $1{suffix}"),
                    &[crate::SQLParam::scalar(Value::Int(rows))],
                )
                .unwrap();
            let captures = CATALOG_CAPTURES.get();
            if returning {
                assert_eq!(result.rows.len(), usize::try_from(rows).unwrap());
                for index in 0..result.rows.len() {
                    assert_eq!(result.value_at(index, 0), Some(&expected_oid));
                }
            }
            captures
        });
        assert!(
            captures[1] <= captures[0],
            "catalog captures grow with rows: {captures:?}, returning={returning}"
        );
    }
    assert_eq!(
        engine
            .sql("SELECT sum(v) FROM items", &[])
            .unwrap()
            .value_at(0, 0),
        Some(&Value::Int(20))
    );
}

#[test]
fn mutation_identity_uses_bound_ordinary_foreign_and_quoted_names() {
    use uqa_execution::mutation::rows::context::MutationTupleMetadata;
    let mut engine = Engine::new();
    engine.sql("CREATE TABLE items(id int); CREATE SCHEMA shadow; CREATE TABLE shadow.items(id int); CREATE SCHEMA \"odd.schema\"; CREATE TABLE \"odd.schema\".\"odd.table\"(id int); CREATE FOREIGN DATA WRAPPER row_metadata; CREATE SERVER row_metadata FOREIGN DATA WRAPPER row_metadata; CREATE FOREIGN TABLE public.foreign_items(id int) SERVER row_metadata; SET search_path TO shadow, public", &[]).unwrap();
    for name in [
        "public.items",
        "shadow.items",
        "\"odd.schema\".\"odd.table\"",
        "public.foreign_items",
    ] {
        let expected = engine
            .sql(&format!("SELECT '{name}'::regclass::oid"), &[])
            .unwrap()
            .value_at(0, 0)
            .cloned()
            .unwrap();
        CATALOG_CAPTURES.set(0);
        assert_eq!(
            Value::Int(MutationTupleMetadata::table_relation_oid(&engine, name).unwrap()),
            expected
        );
        assert_eq!(CATALOG_CAPTURES.get(), 0);
    }
    let original = MutationTupleMetadata::table_relation_oid(&engine, "public.items").unwrap();
    let retained = Arc::new(crate::session::RetainedCatalogSnapshot {
        durable: engine.durable.snapshot(),
        read_view: engine.catalog_read_view(),
    });
    engine
        .sql(
            "DROP TABLE public.items; CREATE TABLE public.items(id int)",
            &[],
        )
        .unwrap();
    assert_ne!(
        MutationTupleMetadata::table_relation_oid(&engine, "public.items").unwrap(),
        original
    );
    engine.query_catalog_snapshot = Some(retained);
    CATALOG_CAPTURES.set(0);
    assert_eq!(
        MutationTupleMetadata::table_relation_oid(&engine, "public.items").unwrap(),
        original
    );
    assert_eq!(CATALOG_CAPTURES.get(), 0);
    assert!(MutationTupleMetadata::table_relation_oid(&engine, "items").is_err());
    assert!(MutationTupleMetadata::table_relation_oid(&engine, "absent.items").is_err());
}
