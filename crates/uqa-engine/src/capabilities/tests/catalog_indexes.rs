//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Index name adapters read existing registries without capturing unrelated table metadata.

use super::{Engine, CATALOG_CAPTURES};
use std::sync::{atomic::Ordering, Arc};
use uqa_core::RelationIdentity;

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_kv(2)]
#[case::redb(3)]
fn catalog_index_lookup_avoids_table_metadata_captures(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("index-names.db");
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
    engine.sql("CREATE TABLE items(id int PRIMARY KEY, v int); CREATE INDEX items_v ON items(v); CREATE SCHEMA shadow; CREATE TABLE shadow.items(id int PRIMARY KEY); CREATE SCHEMA \"odd.schema\"; CREATE TABLE \"odd.schema\".\"odd.table\"(id int PRIMARY KEY)", &[]).unwrap();
    engine
        .sql("SET search_path TO shadow, public", &[])
        .unwrap();
    for fillers in [0, 32] {
        for number in 0..fillers {
            engine
                .sql(
                    &format!("CREATE TABLE filler_{number}(id int PRIMARY KEY)"),
                    &[],
                )
                .unwrap();
        }
        CATALOG_CAPTURES.set(0);
        for (name, relation) in [
            ("items_pkey", RelationIdentity::new("shadow", "items_pkey")),
            (
                "public.items_pkey",
                RelationIdentity::new("public", "items_pkey"),
            ),
            ("items_v", RelationIdentity::new("public", "items_v")),
            (
                "\"odd.schema\".\"odd.table_pkey\"",
                RelationIdentity::new("odd.schema", "odd.table_pkey"),
            ),
        ] {
            for _ in 0..8 {
                assert_eq!(
                    engine
                        .catalog_index_in_execution(name)
                        .unwrap()
                        .unwrap()
                        .relation,
                    relation
                );
            }
        }
        assert!(engine
            .catalog_index_in_execution("missing")
            .unwrap()
            .is_none());
        assert!(engine
            .bound_catalog_index("public.missing")
            .unwrap()
            .is_none());
        assert!(engine.bound_catalog_index("items_pkey").is_err());
        assert_eq!(
            CATALOG_CAPTURES.get(),
            0,
            "index lookup must not capture every table, fillers={fillers}"
        );
    }
    engine
        .sql("CREATE TEMP TABLE items(id int PRIMARY KEY)", &[])
        .unwrap();
    engine.synchronize_catalog_registries().unwrap();
    CATALOG_CAPTURES.set(0);
    let temporary = engine
        .catalog_index_in_execution("items_pkey")
        .unwrap()
        .unwrap();
    assert_eq!(
        temporary.relation,
        RelationIdentity::new(engine.temporary_schema_name(), "items_pkey")
    );
    assert_eq!(
        engine
            .catalog_index_in_execution("pg_temp.items_pkey")
            .unwrap()
            .unwrap()
            .relation,
        temporary.relation
    );
    assert_eq!(CATALOG_CAPTURES.get(), 0);
}

#[test]
fn catalog_index_lookup_preserves_retained_constraint_fallback() {
    let mut engine = Engine::new();
    engine
        .sql("CREATE TABLE items(id int PRIMARY KEY)", &[])
        .unwrap();
    let original = engine
        .bound_catalog_index("public.items_pkey")
        .unwrap()
        .unwrap();
    let retained = Arc::new(crate::session::RetainedCatalogSnapshot {
        durable: engine.durable.snapshot(),
        read_view: engine.catalog_read_view(),
    });
    engine
        .sql("ALTER TABLE items DROP CONSTRAINT items_pkey", &[])
        .unwrap();
    assert!(engine
        .bound_catalog_index("public.items_pkey")
        .unwrap()
        .is_none());
    engine.query_catalog_snapshot = Some(retained);
    CATALOG_CAPTURES.set(0);
    let actual = engine
        .catalog_index_in_execution("items_pkey")
        .unwrap()
        .unwrap();
    assert_eq!(actual.relation, original.relation);
    assert_eq!(actual.index_type, original.index_type);
    assert_eq!(actual.table_name, original.table_name);
    assert_eq!(actual.columns_json, original.columns_json);
    assert_eq!(actual.parameters_json, original.parameters_json);
    assert_eq!(actual.definition_json, original.definition_json);
    assert!(engine
        .catalog_index_in_execution("missing")
        .unwrap()
        .is_none());
    assert_eq!(CATALOG_CAPTURES.get(), 0);
}
