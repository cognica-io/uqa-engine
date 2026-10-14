//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Relation binding reads names without copying unrelated table definitions.

use super::{Engine, RelationResolution, CATALOG_CAPTURES};
use std::sync::{atomic::Ordering, Arc};

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_kv(2)]
#[case::redb(3)]
fn relation_kind_lookups_avoid_unrelated_catalog_captures(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("relation-names.db");
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
    engine.sql("CREATE TABLE items(id int PRIMARY KEY); CREATE VIEW shown AS SELECT id FROM items; CREATE MATERIALIZED VIEW saved AS SELECT id FROM items; CREATE SEQUENCE seq; CREATE TYPE pair AS (a int, b text); CREATE FOREIGN DATA WRAPPER names; CREATE SERVER names FOREIGN DATA WRAPPER names; CREATE FOREIGN TABLE remote_items(id int) SERVER names", &[]).unwrap();
    for fillers in [0, 60] {
        for index in 0..fillers {
            engine.sql(&format!("CREATE TABLE filler_{index}(id int PRIMARY KEY, body text CHECK(length(body)>0))"), &[]).unwrap();
        }
        CATALOG_CAPTURES.set(0);
        for (name, kind) in [
            ("items", "table"),
            ("items_pkey", "index"),
            ("shown", "view"),
            ("saved", "materialized view"),
            ("seq", "sequence"),
            ("pair", "composite type"),
            ("remote_items", "foreign table"),
        ] {
            let canonical = format!("public.{name}");
            let expected = RelationResolution::Found(canonical.clone(), kind);
            assert_eq!(
                engine.resolve_visible_relation_kind(name).unwrap(),
                expected
            );
            assert_eq!(
                engine.resolve_loaded_visible_relation_kind(name).unwrap(),
                expected
            );
            assert_eq!(
                engine.resolve_bound_relation_kind(&canonical).unwrap(),
                expected
            );
        }
        assert_eq!(
            engine.resolve_visible_relation_kind("pg_class").unwrap(),
            RelationResolution::Found("pg_catalog.pg_class".into(), "table")
        );
        assert_eq!(
            engine
                .resolve_visible_relation_kind("absent.items")
                .unwrap(),
            RelationResolution::MissingSchema("absent".into())
        );
        assert_eq!(
            engine.resolve_bound_relation_kind("absent.items").unwrap(),
            RelationResolution::MissingRelation
        );
        assert_eq!(
            engine.resolve_visible_relation_kind("absent").unwrap(),
            RelationResolution::MissingRelation
        );
        assert_eq!(
            CATALOG_CAPTURES.get(),
            0,
            "relation names captured unrelated metadata, fillers={fillers}"
        );
    }
}

#[test]
fn relation_kind_lookup_retains_statement_names_and_schema_privileges() {
    let mut engine = Engine::new();
    engine.sql("CREATE ROLE reader; CREATE SCHEMA app; GRANT USAGE ON SCHEMA app TO reader; CREATE TABLE app.items(id int); CREATE VIEW app.shown AS SELECT id FROM app.items; SET search_path = app, public", &[]).unwrap();
    let retained = Arc::new(crate::session::RetainedCatalogSnapshot {
        durable: engine.durable.snapshot(),
        read_view: engine.catalog_read_view(),
    });
    engine.sql("ALTER TABLE app.items RENAME TO renamed; REVOKE USAGE ON SCHEMA app FROM reader; SET ROLE reader", &[]).unwrap();
    assert_eq!(
        engine.resolve_visible_relation_kind("items").unwrap(),
        RelationResolution::MissingRelation
    );
    let error = engine
        .resolve_visible_relation_kind("app.items")
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("42501"));
    engine.query_catalog_snapshot = Some(retained);
    CATALOG_CAPTURES.set(0);
    for name in ["items", "app.items"] {
        assert_eq!(
            engine.resolve_loaded_visible_relation_kind(name).unwrap(),
            RelationResolution::Found("app.items".into(), "table")
        );
    }
    assert_eq!(
        engine.resolve_bound_relation_kind("app.shown").unwrap(),
        RelationResolution::Found("app.shown".into(), "view")
    );
    assert_eq!(
        engine.resolve_bound_relation_kind("app.renamed").unwrap(),
        RelationResolution::MissingRelation
    );
    assert_eq!(CATALOG_CAPTURES.get(), 0);
    engine.query_catalog_snapshot = None;
    engine.sql("RESET ROLE", &[]).unwrap();
    engine.sql("BEGIN", &[]).unwrap();
    engine
        .sql("ALTER TABLE app.renamed RENAME TO changed", &[])
        .unwrap();
    assert_eq!(
        engine.resolve_visible_relation_kind("changed").unwrap(),
        RelationResolution::Found("app.changed".into(), "table")
    );
    engine.sql("ROLLBACK", &[]).unwrap();
    assert_eq!(
        engine.resolve_visible_relation_kind("app.renamed").unwrap(),
        RelationResolution::Found("app.renamed".into(), "table")
    );
    assert_eq!(
        engine.resolve_visible_relation_kind("changed").unwrap(),
        RelationResolution::MissingRelation
    );
}
