//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Database role references retain private changes and share initial restoration's rollback boundary.

use super::{identity::reopen, snapshots::refresh_catalog};
use crate::{
    tests::relation_lock_support::{error, sessions, sql},
    Engine,
};
use std::sync::Arc;
use uqa_core::Value;
use uqa_execution::catalog::security::database_lifecycle::DATABASE_SECURITY_METADATA_KEY;
use uqa_sql::catalog::security::database::binding::BoundDatabaseSecurity;

fn can_create(engine: &Engine, role: &str) -> bool {
    sql(
        engine,
        &format!("SELECT has_database_privilege('{role}', 'uqa', 'CREATE') AS permitted"),
    )
    .rows[0]["permitted"]
        == Value::Bool(true)
}

#[test]
fn memory_database_acl_keeps_identities_in_catalog_snapshots() {
    let engine = Engine::new();
    sql(
        &engine,
        "CREATE ROLE reader; CREATE ROLE other; GRANT CREATE ON DATABASE uqa TO reader",
    );
    let original = engine.durable.database_security.snapshot();
    sql(&engine, "BEGIN; SAVEPOINT before_catalog_change");
    // Inject a changed role catalog to exercise the adapter without pretending to execute SQL RENAME.
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
    assert_eq!(*engine.durable.database_security.read(), *original);
    error(&engine, "DROP ROLE renamed", "2BP01");
    sql(&engine, "ROLLBACK TO before_catalog_change");
    assert!(can_create(&engine, "reader"));
    sql(&engine, "GRANT CREATE ON DATABASE uqa TO other");
    assert!(can_create(&engine, "other"));
    sql(&engine, "ROLLBACK");
    assert!(!can_create(&engine, "other"));
    assert_eq!(*engine.durable.database_security.read(), *original);
}

#[test]
fn database_acl_identities_preserve_private_changes_refresh_undo_and_reopen() {
    for provider in 0..3 {
        for isolation in ["READ COMMITTED", "REPEATABLE READ", "SERIALIZABLE"] {
            for finish in ["COMMIT", "ROLLBACK", "ROLLBACK TO private; COMMIT"] {
                let (directory, first, second) = sessions(provider);
                sql(&first, "CREATE ROLE reader; CREATE ROLE other; REVOKE CREATE ON DATABASE uqa FROM PUBLIC");
                sql(&first, &format!("BEGIN ISOLATION LEVEL {isolation}; SELECT * FROM t; SAVEPOINT private; GRANT CREATE ON DATABASE uqa TO reader"));
                let catalog = first.storage.catalog.as_ref().unwrap();
                assert!(catalog
                    .metadata_has_private_changes(DATABASE_SECURITY_METADATA_KEY)
                    .unwrap());
                let encoded = catalog
                    .get_metadata(DATABASE_SECURITY_METADATA_KEY)
                    .unwrap()
                    .unwrap();
                let value: serde_json::Value = serde_json::from_str(&encoded).unwrap();
                assert_eq!(value["database_security_format"], 1);
                let bound: BoundDatabaseSecurity =
                    serde_json::from_value(value["security"].clone()).unwrap();
                let reader = first.durable.roles.read()["reader"].identity();
                assert!(bound
                    .acl
                    .as_ref()
                    .unwrap()
                    .iter()
                    .any(|entry| entry.role == Some(reader)));
                sql(&second, "ALTER ROLE other LOGIN");
                refresh_catalog(&first, isolation);
                assert!(can_create(&first, "reader"));
                assert!(!can_create(&second, "reader"));
                assert_eq!(
                    catalog
                        .get_metadata(DATABASE_SECURITY_METADATA_KEY)
                        .unwrap()
                        .unwrap(),
                    encoded
                );
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
                    sql(&reopened, "REVOKE CREATE ON DATABASE uqa FROM reader; DROP ROLE reader; CREATE ROLE reader");
                    assert!(!can_create(&reopened, "reader"));
                    assert_ne!(reopened.durable.roles.read()["reader"].identity(), reader);
                }
            }
        }
    }
}

#[test]
fn database_acl_migration_rolls_back_on_later_catalog_restore_failure() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        sql(
            &first,
            "CREATE ROLE reader; GRANT CREATE ON DATABASE uqa TO reader",
        );
        let expected = first.durable.database_security.snapshot();
        let legacy =
            serde_json::to_string(&expected.resolve(&first.durable.roles.read()).unwrap()).unwrap();
        let factory = Arc::clone(first.storage.provider.as_ref().unwrap());
        let raw = factory.open_session().unwrap();
        raw.catalog
            .set_metadata(DATABASE_SECURITY_METADATA_KEY, &legacy)
            .unwrap();
        let Err(error) = first.new_session() else {
            panic!("secondary restoration must not bind legacy database ACL names");
        };
        assert!(
            error.to_string().contains("initial catalog migration"),
            "{error}"
        );
        raw.catalog.set_metadata("sql_functions_json", "{").unwrap();
        drop(second);
        drop(first);
        let Err(error) = Engine::from_persistent_provider(Arc::clone(&factory)) else {
            panic!("initial restoration accepted invalid routine metadata");
        };
        assert!(error.to_string().contains("EOF"), "{error}");
        assert_eq!(
            raw.catalog
                .get_metadata(DATABASE_SECURITY_METADATA_KEY)
                .unwrap(),
            Some(legacy)
        );
        raw.catalog.delete_metadata("sql_functions_json").unwrap();
        let reopened = Engine::from_persistent_provider(Arc::clone(&factory)).unwrap();
        assert_eq!(*reopened.durable.database_security.read(), *expected);
        assert!(can_create(&reopened, "reader"));
        let encoded = raw
            .catalog
            .get_metadata(DATABASE_SECURITY_METADATA_KEY)
            .unwrap()
            .unwrap();
        assert!(
            serde_json::from_str::<uqa_sql::catalog::security::database::DatabaseSecurity>(
                &encoded
            )
            .is_err()
        );
        drop(reopened);
        let reopened = Engine::from_persistent_provider(factory).unwrap();
        assert_eq!(*reopened.durable.database_security.read(), *expected);
        assert_eq!(
            raw.catalog
                .get_metadata(DATABASE_SECURITY_METADATA_KEY)
                .unwrap()
                .unwrap(),
            encoded
        );
    }
}

#[test]
fn database_acl_corruption_does_not_acquire_a_same_oid_replacement() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        sql(
            &first,
            "CREATE ROLE reader; GRANT CREATE ON DATABASE uqa TO reader",
        );
        let reader = first.durable.roles.read()["reader"].identity();
        let factory = Arc::clone(first.storage.provider.as_ref().unwrap());
        let raw = factory.open_session().unwrap();
        let encoded = raw
            .catalog
            .get_metadata(DATABASE_SECURITY_METADATA_KEY)
            .unwrap()
            .unwrap();
        let mut value: serde_json::Value = serde_json::from_str(&encoded).unwrap();
        for entry in value["security"]["acl"].as_array_mut().unwrap() {
            if entry["role"]["oid"] == reader.oid {
                entry["role"]["object_id"] = serde_json::json!(vec![42; 16]);
            }
        }
        let corrupt = value.to_string();
        assert_ne!(corrupt, encoded);
        raw.catalog
            .set_metadata(DATABASE_SECURITY_METADATA_KEY, &corrupt)
            .unwrap();
        let Err(error) = first.new_session() else {
            panic!("invalid database ACL incarnation was accepted");
        };
        assert!(
            error.to_string().contains("missing role incarnation"),
            "{error}"
        );
        drop(second);
        drop(first);
        let Err(error) = Engine::from_persistent_provider(Arc::clone(&factory)) else {
            panic!("initial restoration rebound an invalid database ACL");
        };
        assert!(
            error.to_string().contains("missing role incarnation"),
            "{error}"
        );
        assert_eq!(
            raw.catalog
                .get_metadata(DATABASE_SECURITY_METADATA_KEY)
                .unwrap()
                .unwrap(),
            corrupt
        );
        raw.catalog
            .set_metadata(DATABASE_SECURITY_METADATA_KEY, &encoded)
            .unwrap();
        let restored = Engine::from_persistent_provider(factory).unwrap();
        assert!(can_create(&restored, "reader"));
    }
}
