//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Schema role identities survive catalog refresh, undo and atomic initial conversion.

use super::{identity::reopen, snapshots::refresh_catalog};
use crate::{
    tests::relation_lock_support::{error, sessions, sql},
    Engine,
};
use std::sync::Arc;
use uqa_core::Value;
use uqa_storage::{CatalogFacade, SchemaRow};

fn can_create(engine: &Engine, role: &str) -> bool {
    sql(
        engine,
        &format!("SELECT has_schema_privilege('{role}', 'secured', 'CREATE') AS permitted"),
    )
    .rows[0]["permitted"]
        == Value::Bool(true)
}

fn stored(catalog: &dyn CatalogFacade) -> SchemaRow {
    catalog
        .load_schema_rows()
        .unwrap()
        .into_iter()
        .find(|row| row.name() == "secured")
        .unwrap()
}

#[test]
fn memory_schema_acl_keeps_identities_in_catalog_snapshots() {
    let engine = Engine::new();
    sql(&engine, "CREATE ROLE reader; CREATE ROLE other; CREATE SCHEMA secured; GRANT CREATE ON SCHEMA secured TO reader");
    let original = engine.durable.schemas.snapshot();
    sql(&engine, "BEGIN; SAVEPOINT before_catalog_change");
    // Inject a catalog change to verify the state adapter independently of SQL RENAME.
    {
        let mut roles = engine.durable.roles.write();
        let mut reader = roles.remove("reader").unwrap();
        reader.name = "renamed".into();
        roles.insert(reader.name.clone(), reader);
    }
    engine.note_catalog_registry_changed();
    sql(&engine, "CREATE ROLE reader");
    assert!(can_create(&engine, "renamed"));
    assert!(!can_create(&engine, "reader"));
    assert_eq!(*engine.durable.schemas.read(), *original);
    error(&engine, "DROP ROLE renamed", "2BP01");
    sql(&engine, "ROLLBACK TO before_catalog_change");
    assert!(can_create(&engine, "reader"));
    sql(&engine, "GRANT CREATE ON SCHEMA secured TO other");
    assert!(can_create(&engine, "other"));
    sql(&engine, "ROLLBACK");
    assert!(!can_create(&engine, "other"));
    assert_eq!(*engine.durable.schemas.read(), *original);
}

#[test]
fn schema_acl_identities_preserve_private_changes_refresh_undo_and_reopen() {
    for provider in 0..3 {
        for isolation in ["READ COMMITTED", "REPEATABLE READ", "SERIALIZABLE"] {
            for finish in ["COMMIT", "ROLLBACK", "ROLLBACK TO private; COMMIT"] {
                let (directory, first, second) = sessions(provider);
                sql(
                    &first,
                    "CREATE ROLE reader; CREATE ROLE other; CREATE SCHEMA secured",
                );
                sql(&first, &format!("BEGIN ISOLATION LEVEL {isolation}; SELECT * FROM t; SAVEPOINT private; GRANT CREATE ON SCHEMA secured TO reader"));
                let catalog = first.storage.catalog.as_ref().unwrap();
                let encoded = stored(catalog.as_ref());
                let SchemaRow::Bound(bound) = &encoded else {
                    panic!("new schema references must retain role identities");
                };
                let reader = first.durable.roles.read()["reader"].identity();
                assert!(bound
                    .acl
                    .as_ref()
                    .unwrap()
                    .iter()
                    .any(|entry| entry.role == Some(reader)));
                sql(&second, "ALTER ROLE other LOGIN; CREATE SCHEMA unrelated");
                refresh_catalog(&first, isolation);
                assert!(can_create(&first, "reader"));
                assert!(!can_create(&second, "reader"));
                assert_eq!(stored(catalog.as_ref()), encoded);
                sql(&first, finish);
                assert_eq!(can_create(&second, "reader"), finish == "COMMIT");
                if finish == "COMMIT" {
                    error(&second, "DROP ROLE reader", "2BP01");
                }
                drop(second);
                drop(first);
                let reopened = reopen(provider, &directory.path().join("table-locks.db"));
                assert_eq!(can_create(&reopened, "reader"), finish == "COMMIT");
                if finish == "COMMIT" {
                    sql(&reopened, "REVOKE CREATE ON SCHEMA secured FROM reader; DROP ROLE reader; CREATE ROLE reader");
                    assert!(!can_create(&reopened, "reader"));
                    assert_ne!(reopened.durable.roles.read()["reader"].identity(), reader);
                }
            }
        }
    }
}

#[test]
fn schema_acl_migration_rolls_back_on_later_catalog_restore_failure() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        sql(
            &first,
            "CREATE ROLE reader; CREATE SCHEMA secured; GRANT CREATE ON SCHEMA secured TO reader",
        );
        let expected = first.durable.schemas.read()["secured"].clone();
        let legacy = SchemaRow::Legacy(
            expected
                .resolve(&first.durable.roles.read())
                .unwrap()
                .row("secured"),
        );
        let factory = Arc::clone(first.storage.provider.as_ref().unwrap());
        let raw = factory.open_session().unwrap();
        raw.catalog.save_schema_row(&legacy).unwrap();
        let Err(error) = first.new_session() else {
            panic!("secondary restoration must not bind legacy schema names");
        };
        assert!(
            error.to_string().contains("initial catalog migration"),
            "{error}"
        );
        raw.catalog.set_metadata("sql_functions_json", "{").unwrap();
        drop(second);
        drop(first);
        let Err(error) = Engine::from_persistent_provider(Arc::clone(&factory)) else {
            panic!("initial restoration accepted malformed routine metadata");
        };
        assert!(error.to_string().contains("EOF"), "{error}");
        assert_eq!(stored(raw.catalog.as_ref()), legacy);
        raw.catalog.delete_metadata("sql_functions_json").unwrap();
        let reopened = Engine::from_persistent_provider(Arc::clone(&factory)).unwrap();
        assert_eq!(reopened.durable.schemas.read()["secured"], expected);
        assert!(can_create(&reopened, "reader"));
        let encoded = stored(raw.catalog.as_ref());
        assert!(matches!(encoded, SchemaRow::Bound(_)));
        drop(reopened);
        let reopened = Engine::from_persistent_provider(factory).unwrap();
        assert_eq!(reopened.durable.schemas.read()["secured"], expected);
        assert_eq!(stored(raw.catalog.as_ref()), encoded);
    }
}

#[test]
fn schema_acl_corruption_never_rebinds_owner_grantee_or_grantor() {
    for provider in 0..3 {
        for reference in ["owner", "grantee", "grantor"] {
            let (_directory, first, second) = sessions(provider);
            sql(&first, "CREATE ROLE reader; CREATE SCHEMA secured; GRANT CREATE ON SCHEMA secured TO reader");
            let reader = first.durable.roles.read()["reader"].identity();
            let factory = Arc::clone(first.storage.provider.as_ref().unwrap());
            let raw = factory.open_session().unwrap();
            let original = stored(raw.catalog.as_ref());
            let SchemaRow::Bound(mut changed) = original.clone() else {
                unreachable!()
            };
            let target = match reference {
                "owner" => &mut changed.role_owner,
                "grantee" => changed
                    .acl
                    .as_mut()
                    .unwrap()
                    .iter_mut()
                    .find(|entry| entry.role == Some(reader))
                    .unwrap()
                    .role
                    .as_mut()
                    .unwrap(),
                _ => &mut changed.acl.as_mut().unwrap()[0].grantor,
            };
            target.object_id = [42; 16];
            let corrupt = SchemaRow::Bound(changed);
            raw.catalog.save_schema_row(&corrupt).unwrap();
            let Err(error) = first.new_session() else {
                panic!("invalid schema reference accepted")
            };
            assert!(
                error.to_string().contains("missing role incarnation"),
                "{error}"
            );
            drop(second);
            drop(first);
            let Err(error) = Engine::from_persistent_provider(Arc::clone(&factory)) else {
                panic!("invalid schema reference rebound")
            };
            assert!(
                error.to_string().contains("missing role incarnation"),
                "{error}"
            );
            assert_eq!(stored(raw.catalog.as_ref()), corrupt);
            raw.catalog.save_schema_row(&original).unwrap();
            let restored = Engine::from_persistent_provider(factory).unwrap();
            assert!(can_create(&restored, "reader"));
        }
    }
}
