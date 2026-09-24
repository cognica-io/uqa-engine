//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stored index addresses and opaque physical keys survive refresh, schema changes and undo.

use crate::tests::relation_lock_support::{error, sessions, sql};
use crate::Engine;
use std::sync::Arc;
use uqa_core::Value;
use uqa_sql::catalog::index::IndexCatalogIdentity;

fn identity(engine: &Engine, name: &str) -> IndexCatalogIdentity {
    crate::catalog_indexes::index_definition(&engine.catalog_index(name).unwrap().unwrap())
        .unwrap()
        .catalog
        .unwrap()
}

fn assert_catalog(engine: &Engine, name: &str, expected: i64) {
    let row = engine.sql("SELECT c.oid, i.indexrelid FROM pg_class c JOIN pg_index i ON c.oid = i.indexrelid WHERE c.relname = $1", &[uqa_sql::SQLParam::Scalar(Value::Str(name.into()))]).unwrap();
    assert_eq!(row.rows.len(), 1);
    assert_eq!(row.rows[0]["oid"], Value::Int(expected));
    assert_eq!(row.rows[0]["indexrelid"], Value::Int(expected));
}

fn exercise_undo(engine: &Engine) -> IndexCatalogIdentity {
    sql(
        engine,
        "CREATE UNIQUE INDEX expr_index ON t((v+10)) WHERE v>0",
    );
    let original = identity(engine, "expr_index");
    assert_catalog(engine, "expr_index", original.identity.oid);
    assert!(original.physical_key.starts_with("uqa:index:"));
    error(engine, "INSERT INTO t VALUES(1)", "23505");
    sql(engine, "INSERT INTO t VALUES(0), (0); BEGIN; SAVEPOINT retained; DROP INDEX expr_index; CREATE UNIQUE INDEX expr_index ON t((v+20)) WHERE v>0");
    let replacement = identity(engine, "expr_index");
    assert_ne!(replacement.identity, original.identity);
    assert_ne!(replacement.physical_key, original.physical_key);
    assert_catalog(engine, "expr_index", replacement.identity.oid);
    sql(engine, "ROLLBACK TO retained; COMMIT");
    assert_eq!(identity(engine, "expr_index"), original);
    assert_catalog(engine, "expr_index", original.identity.oid);
    error(engine, "INSERT INTO t VALUES(1)", "23505");
    sql(
        engine,
        "ALTER TABLE t RENAME COLUMN v TO value; ALTER TABLE t RENAME TO renamed",
    );
    assert_eq!(identity(engine, "expr_index"), original);
    error(engine, "INSERT INTO renamed VALUES(1)", "23505");
    original
}

#[test]
fn index_incarnations_and_physical_namespaces_survive_savepoints_schema_changes_and_reopen() {
    let memory = Engine::new();
    sql(
        &memory,
        "CREATE TABLE t(v integer); INSERT INTO t VALUES(1)",
    );
    exercise_undo(&memory);
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        let original = exercise_undo(&first);
        assert_eq!(identity(&second, "expr_index"), original);
        error(&second, "INSERT INTO renamed VALUES(1)", "23505");
        let factory = Arc::clone(first.storage.provider.as_ref().unwrap());
        drop(second);
        drop(first);
        let reopened = Engine::from_persistent_provider(factory).unwrap();
        assert_eq!(identity(&reopened, "expr_index"), original);
        assert_catalog(&reopened, "expr_index", original.identity.oid);
        error(&reopened, "INSERT INTO renamed VALUES(1)", "23505");
        sql(
            &reopened,
            "INSERT INTO renamed VALUES(2); ALTER TABLE renamed DROP COLUMN value",
        );
        assert!(reopened.catalog_index("expr_index").unwrap().is_none());
    }
}

#[test]
fn current_index_identity_corruption_is_rejected_without_repair() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        sql(&first, "CREATE INDEX stored_index ON t(v)");
        let factory = Arc::clone(first.storage.provider.as_ref().unwrap());
        let raw = factory.open_session().unwrap();
        let valid = raw.catalog.load_catalog_indexes().unwrap().remove(0);
        drop(second);
        drop(first);
        for damage in 0..4 {
            let mut malformed = valid.clone();
            let mut definition = crate::catalog_indexes::index_definition(&malformed).unwrap();
            match damage {
                0 => definition.catalog = None,
                1 => definition.catalog.as_mut().unwrap().table_object_id = [99; 16],
                2 => definition.catalog.as_mut().unwrap().identity.oid = 0,
                3 => definition.catalog.as_mut().unwrap().physical_key.clear(),
                _ => unreachable!(),
            }
            malformed.definition_json = Some(serde_json::to_string(&definition).unwrap());
            raw.catalog.save_catalog_index_row(&malformed).unwrap();
            let Err(failure) = Engine::from_persistent_provider(Arc::clone(&factory)) else {
                panic!("malformed current index metadata accepted: {damage}");
            };
            assert!(failure.to_string().contains("index"), "{failure}");
            assert_eq!(
                raw.catalog.load_catalog_indexes().unwrap()[0].definition_json,
                malformed.definition_json
            );
        }
        raw.catalog.save_catalog_index_row(&valid).unwrap();
        let restored = Engine::from_persistent_provider(factory).unwrap();
        assert!(restored.catalog_index("stored_index").unwrap().is_some());
    }
}

#[test]
fn legacy_physical_keys_and_catalog_addresses_are_preserved_during_initial_conversion() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        sql(&first, "CREATE UNIQUE INDEX expr_index ON t((v+10))");
        let factory = Arc::clone(first.storage.provider.as_ref().unwrap());
        let raw = factory.open_session().unwrap();
        let mut stored = raw.catalog.load_catalog_indexes().unwrap().remove(0);
        let mut definition = crate::catalog_indexes::index_definition(&stored).unwrap();
        let original = definition.catalog.take().unwrap();
        let old_key = uqa_storage::ValueIndexKey::Index(original.physical_key);
        let legacy_key = uqa_storage::ValueIndexKey::Index(stored.relation.qualified_name());
        let values = raw
            .backend
            .load_btree_index("public.t", &old_key)
            .unwrap()
            .unwrap();
        raw.backend
            .replace_btree_indexes("public.t", &[(&legacy_key, &values)])
            .unwrap();
        raw.backend.drop_btree_index("public.t", &old_key).unwrap();
        stored.definition_json = Some(serde_json::to_string(&definition).unwrap());
        raw.catalog.save_catalog_index_row(&stored).unwrap();
        raw.catalog
            .delete_metadata("sql_index_catalog_identity_version")
            .unwrap();
        raw.catalog
            .delete_metadata("sql_index_registry_version")
            .unwrap();
        drop(second);
        drop(first);
        let restored = Engine::from_persistent_provider(factory).unwrap();
        let converted = identity(&restored, "expr_index");
        assert_eq!(converted.physical_key, "public.expr_index");
        assert_eq!(
            converted.identity.oid,
            uqa_sql::catalog::oids::relation_oid("i", "public", "expr_index")
        );
        assert_catalog(&restored, "expr_index", converted.identity.oid);
        error(&restored, "INSERT INTO t VALUES(1)", "23505");
        assert_eq!(
            raw.backend
                .load_btree_index("public.t", &legacy_key)
                .unwrap()
                .unwrap(),
            values
        );
    }
}

#[test]
fn later_index_restore_failure_rolls_back_identity_conversion_and_its_marker() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        sql(&first, "CREATE INDEX legacy_index ON t(v)");
        let factory = Arc::clone(first.storage.provider.as_ref().unwrap());
        let raw = factory.open_session().unwrap();
        let mut legacy = raw.catalog.load_catalog_indexes().unwrap().remove(0);
        let mut definition = crate::catalog_indexes::index_definition(&legacy).unwrap();
        definition.catalog = None;
        legacy.definition_json = Some(serde_json::to_string(&definition).unwrap());
        // Catalog conversion can inspect this definition; hydrating its physical vector index must fail on the integer column inside the same initial transaction.
        legacy.index_type = "ivf".into();
        raw.catalog.save_catalog_index_row(&legacy).unwrap();
        raw.catalog
            .delete_metadata("sql_index_catalog_identity_version")
            .unwrap();
        raw.catalog
            .delete_metadata("sql_index_registry_version")
            .unwrap();
        drop(second);
        drop(first);
        let Err(failure) = Engine::from_persistent_provider(Arc::clone(&factory)) else {
            panic!("vector index on an integer column accepted");
        };
        assert!(failure.to_string().contains("non-vector"), "{failure}");
        assert_eq!(
            raw.catalog.load_catalog_indexes().unwrap()[0].definition_json,
            legacy.definition_json
        );
        assert!(raw
            .catalog
            .get_metadata("sql_index_catalog_identity_version")
            .unwrap()
            .is_none());
        legacy.index_type = "btree".into();
        raw.catalog.save_catalog_index_row(&legacy).unwrap();
        let restored = Engine::from_persistent_provider(factory).unwrap();
        assert_eq!(
            identity(&restored, "legacy_index").physical_key,
            "public.legacy_index"
        );
    }
}
