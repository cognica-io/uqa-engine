//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Constraint names inspect current declarations without unrelated catalog captures.

use super::{Engine, CATALOG_CAPTURES};
use std::{collections::BTreeSet, sync::Arc};
use uqa_core::RelationIdentity;

fn names(values: &[&str]) -> BTreeSet<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_kv(2)]
#[case::redb(3)]
fn constraint_names_avoid_unrelated_catalog_captures(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("constraint-names.db");
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
        .store(true, std::sync::atomic::Ordering::Release);
    engine.sql("CREATE SCHEMA app; CREATE SCHEMA other; CREATE TABLE app.parents(id int PRIMARY KEY); CREATE TABLE app.items(id int CONSTRAINT ck CHECK(id>0), code int CONSTRAINT nn NOT NULL, parent_id int CONSTRAINT fk REFERENCES app.parents(id), CONSTRAINT uq UNIQUE(code)); CREATE DOMAIN app.credit AS integer CONSTRAINT dc CHECK(VALUE>=0) CONSTRAINT dn NOT NULL; CREATE FOREIGN DATA WRAPPER names; CREATE SERVER names FOREIGN DATA WRAPPER names; CREATE FOREIGN TABLE app.foreign_items(code int CONSTRAINT fn NOT NULL, CONSTRAINT fc CHECK(code>0)) SERVER names; CREATE FUNCTION app.keep_row() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NEW; END $$; CREATE CONSTRAINT TRIGGER ct AFTER INSERT ON app.items DEFERRABLE FOR EACH ROW EXECUTE FUNCTION app.keep_row(); CREATE TABLE other.ignored(id int CONSTRAINT outside CHECK(id>0));", &[]).unwrap();
    let expected = names(&["ck", "ct", "fk", "nn", "uq"]);
    let mut expected_schema = names(&[
        "ck",
        "ct",
        "dc",
        "dn",
        "fc",
        "fk",
        "fn",
        "nn",
        "parents_id_not_null",
        "parents_pkey",
        "uq",
    ]);
    let relation = RelationIdentity::new("app", "items");
    for fillers in [0, 60] {
        for index in 0..fillers {
            engine.sql(&format!("CREATE TABLE app.filler_{index}(id int CONSTRAINT filler_nn_{index} NOT NULL, body text CONSTRAINT filler_ck_{index} CHECK(length(body)>0), CONSTRAINT filler_pk_{index} PRIMARY KEY(id))"), &[]).unwrap();
            expected_schema.extend([
                format!("filler_nn_{index}"),
                format!("filler_ck_{index}"),
                format!("filler_pk_{index}"),
            ]);
        }
        let context = engine.schema_publication_context().constraint_names();
        CATALOG_CAPTURES.set(0);
        for _ in 0..4 {
            assert_eq!(context.existing_names("app.items").unwrap(), expected);
            assert_eq!(
                context.automatic_names("app.items").unwrap(),
                expected_schema
            );
            let scope = context.name_scope(&relation);
            assert_eq!(scope.events, names(&["ct"]));
            assert_eq!(scope.schema, expected_schema);
            let owner = context.bind("app.items").unwrap();
            for name in &expected {
                assert!(context.in_use(owner, name).unwrap());
            }
            assert!(!context.in_use(owner, "absent").unwrap());
            assert!(context
                .ensure_available("app.items", Some("available"))
                .is_ok());
            assert!(context.ensure_available("app.items", Some("ck")).is_err());
        }
        assert_eq!(
            CATALOG_CAPTURES.get(),
            0,
            "constraint names captured unrelated metadata, fillers={fillers}"
        );
    }
}

#[test]
fn constraint_names_keep_current_identity_across_rename_rollback_and_replacement() {
    let mut engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE items(id int CONSTRAINT original CHECK(id>0))",
            &[],
        )
        .unwrap();
    engine.sql("BEGIN", &[]).unwrap();
    let retained = Arc::new(crate::session::RetainedCatalogSnapshot {
        durable: engine.durable.snapshot(),
        read_view: engine.catalog_read_view(),
    });
    let original_owner = engine
        .schema_publication_context()
        .constraint_names()
        .bind("public.items")
        .unwrap();
    engine
        .sql(
            "ALTER TABLE items RENAME CONSTRAINT original TO renamed",
            &[],
        )
        .unwrap();
    engine.query_catalog_snapshot = Some(retained);
    CATALOG_CAPTURES.set(0);
    let context = engine.schema_publication_context().constraint_names();
    assert!(context.in_use(original_owner, "renamed").unwrap());
    assert!(!context.in_use(original_owner, "original").unwrap());
    assert_eq!(
        context.existing_names("public.items").unwrap(),
        names(&["renamed"])
    );
    assert_eq!(CATALOG_CAPTURES.get(), 0);
    engine.query_catalog_snapshot = None;
    engine.sql("ROLLBACK", &[]).unwrap();
    assert!(engine
        .schema_publication_context()
        .constraint_names()
        .in_use(original_owner, "original")
        .unwrap());
    engine
        .sql("ALTER TABLE items RENAME TO moved", &[])
        .unwrap();
    assert!(engine
        .schema_publication_context()
        .constraint_names()
        .in_use(original_owner, "original")
        .unwrap());
    engine
        .sql(
            "DROP TABLE moved; CREATE TABLE moved(id int CONSTRAINT original CHECK(id>0))",
            &[],
        )
        .unwrap();
    let context = engine.schema_publication_context().constraint_names();
    assert_ne!(context.bind("public.moved").unwrap(), original_owner);
    assert!(context.in_use(original_owner, "original").is_err());
}
