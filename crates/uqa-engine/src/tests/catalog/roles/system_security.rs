//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! System ACL identities survive private catalog refresh and atomic initial conversion.

use super::{identity::reopen, snapshots::refresh_catalog};
use crate::{
    tests::relation_lock_support::{error, sessions, sql},
    Engine,
};
use std::sync::Arc;
use uqa_core::Value;
use uqa_sql::catalog::{
    security::system_relations::{metadata_key, METADATA_PREFIX},
    SystemRelation,
};
use uqa_storage::CatalogFacade;

fn has_privilege(engine: &Engine, role: &str, column: bool) -> bool {
    let expression = if column {
        format!("has_column_privilege('{role}', 'pg_catalog.pg_authid', 'rolname', 'UPDATE')")
    } else {
        format!("has_table_privilege('{role}', 'pg_catalog.pg_authid', 'SELECT')")
    };
    sql(engine, &format!("SELECT {expression} AS permitted")).rows[0]["permitted"]
        == Value::Bool(true)
}

fn stored(catalog: &dyn CatalogFacade) -> Vec<(String, String)> {
    catalog.metadata_with_prefix(METADATA_PREFIX).unwrap()
}

#[test]
fn private_role_catalog_projects_attributes_and_checks_relation_and_column_acls() {
    let engine = Engine::new();
    sql(
        &engine,
        "CREATE ROLE reader LOGIN NOINHERIT CONNECTION LIMIT 7; CREATE ROLE unrelated",
    );
    let rows = sql(&engine, "SELECT oid, rolname, rolsuper, rolinherit, rolcreaterole, rolcreatedb, rolcanlogin, rolreplication, rolbypassrls, rolconnlimit, rolpassword, rolvaliduntil FROM pg_catalog.pg_authid WHERE rolname = 'reader'").rows;
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0]["oid"],
        Value::Int(engine.durable.roles.read()["reader"].oid)
    );
    assert_eq!(rows[0]["rolname"], Value::Str("reader".into()));
    assert_eq!(rows[0]["rolcanlogin"], Value::Bool(true));
    assert_eq!(rows[0]["rolconnlimit"], Value::Int(7));
    assert_eq!(rows[0]["rolpassword"], Value::Null);
    assert_eq!(rows[0]["rolvaliduntil"], Value::Null);
    for attribute in [
        "rolsuper",
        "rolinherit",
        "rolcreaterole",
        "rolcreatedb",
        "rolreplication",
        "rolbypassrls",
    ] {
        assert_eq!(rows[0][attribute], Value::Bool(false), "{attribute}");
    }
    sql(&engine, "SET ROLE unrelated");
    error(&engine, "SELECT rolname FROM pg_authid", "42501");
    sql(
        &engine,
        "RESET ROLE; GRANT SELECT(rolname) ON pg_authid TO reader; SET ROLE reader",
    );
    assert_eq!(
        sql(&engine, "SELECT rolname FROM pg_authid ORDER BY rolname")
            .rows
            .len(),
        3
    );
    error(&engine, "SELECT rolpassword FROM pg_authid", "42501");
    sql(
        &engine,
        "RESET ROLE; GRANT SELECT ON pg_authid TO reader; SET ROLE reader",
    );
    assert_eq!(sql(&engine, "SELECT * FROM pg_authid").rows.len(), 3);
    sql(
        &engine,
        "RESET ROLE; REVOKE SELECT ON pg_authid FROM reader; SET ROLE reader",
    );
    error(&engine, "SELECT rolname FROM pg_authid", "42501");
}

#[test]
fn memory_system_acl_keeps_grantees_and_grantors_through_renamed_catalog_snapshots() {
    let engine = Engine::new();
    sql(&engine, "CREATE ROLE grantor; CREATE ROLE reader; GRANT SELECT ON pg_catalog.pg_authid TO grantor WITH GRANT OPTION; SET ROLE grantor; GRANT SELECT(rolname) ON pg_catalog.pg_authid TO reader; RESET ROLE");
    let original = engine.durable.system_relation_security.snapshot();
    sql(&engine, "BEGIN; SAVEPOINT before_rename");
    {
        let mut roles = engine.durable.roles.write();
        for name in ["grantor", "reader"] {
            let mut role = roles.remove(name).unwrap();
            role.name = format!("renamed_{name}");
            roles.insert(role.name.clone(), role);
        }
    }
    engine.note_catalog_registry_changed();
    sql(&engine, "CREATE ROLE reader; CREATE ROLE grantor; SET ROLE renamed_reader; SELECT rolname FROM pg_catalog.pg_authid; RESET ROLE");
    sql(&engine, "SAVEPOINT before_dependency_error");
    error(&engine, "DROP ROLE renamed_reader", "2BP01");
    sql(
        &engine,
        "ROLLBACK TO before_dependency_error; SET ROLE reader; SAVEPOINT before_permission_error",
    );
    error(&engine, "SELECT rolname FROM pg_catalog.pg_authid", "42501");
    sql(&engine, "ROLLBACK TO before_permission_error; RESET ROLE");
    assert_eq!(*engine.durable.system_relation_security.read(), *original);
    sql(
        &engine,
        "REVOKE SELECT ON pg_catalog.pg_authid FROM renamed_grantor CASCADE",
    );
    sql(&engine, "SET ROLE renamed_reader");
    error(&engine, "SELECT rolname FROM pg_catalog.pg_authid", "42501");
    sql(&engine, "ROLLBACK TO before_rename; ROLLBACK; SET ROLE reader; SELECT rolname FROM pg_catalog.pg_authid; RESET ROLE");
    assert_eq!(*engine.durable.system_relation_security.read(), *original);
}

#[test]
fn system_acl_identities_preserve_private_table_and_column_tuples_refresh_undo_and_reopen() {
    for provider in 0..3 {
        for isolation in ["READ COMMITTED", "REPEATABLE READ", "SERIALIZABLE"] {
            for finish in ["COMMIT", "ROLLBACK", "ROLLBACK TO private; COMMIT"] {
                let (directory, first, second) = sessions(provider);
                sql(&first, "CREATE ROLE reader; CREATE ROLE other");
                sql(&first, &format!("BEGIN ISOLATION LEVEL {isolation}; SELECT * FROM t; SAVEPOINT private; GRANT SELECT, UPDATE(rolname) ON pg_catalog.pg_authid TO reader"));
                let catalog = first.storage.catalog.as_ref().unwrap();
                let encoded = stored(catalog.as_ref());
                assert_eq!(encoded.len(), 2);
                let reader = first.durable.roles.read()["reader"].identity();
                for (_, json) in &encoded {
                    let value: serde_json::Value = serde_json::from_str(json).unwrap();
                    assert_eq!(value["system_acl_format"], 1);
                    assert!(value["entry"]["acl"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|entry| entry["role"] == serde_json::to_value(reader).unwrap()));
                }
                sql(&second, "ALTER ROLE other LOGIN; CREATE SCHEMA unrelated");
                refresh_catalog(&first, isolation);
                for column in [false, true] {
                    assert!(has_privilege(&first, "reader", column));
                    assert!(!has_privilege(&second, "reader", column));
                }
                assert_eq!(stored(catalog.as_ref()), encoded);
                sql(&first, finish);
                for column in [false, true] {
                    assert_eq!(has_privilege(&second, "reader", column), finish == "COMMIT");
                }
                if finish == "COMMIT" {
                    error(&second, "DROP ROLE reader", "2BP01");
                }
                drop(second);
                drop(first);
                let reopened = reopen(provider, &directory.path().join("table-locks.db"));
                for column in [false, true] {
                    assert_eq!(
                        has_privilege(&reopened, "reader", column),
                        finish == "COMMIT"
                    );
                }
                if finish == "COMMIT" {
                    sql(&reopened, "REVOKE SELECT, UPDATE(rolname) ON pg_catalog.pg_authid FROM reader; DROP ROLE reader; CREATE ROLE reader");
                    assert_ne!(reopened.durable.roles.read()["reader"].identity(), reader);
                    assert!(!has_privilege(&reopened, "reader", false));
                    assert!(!has_privilege(&reopened, "reader", true));
                }
            }
        }
    }
}

#[test]
fn system_acl_migration_rolls_back_with_later_catalog_restore_failure() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        sql(
            &first,
            "CREATE ROLE reader; GRANT SELECT ON pg_catalog.pg_authid TO reader",
        );
        let expected = first.durable.system_relation_security.snapshot();
        let relation = SystemRelation::Projected(uqa_sql::catalog::VirtualRelation::PgAuthid);
        let entry = expected.values().next().unwrap().table.as_ref().unwrap();
        let named = expected
            .values()
            .next()
            .unwrap()
            .security(relation)
            .resolve(&first.durable.roles.read())
            .unwrap();
        let legacy =
            serde_json::json!({"revision": entry.revision, "acl": named.acl.unwrap()}).to_string();
        let key = metadata_key(relation, None);
        let factory = Arc::clone(first.storage.provider.as_ref().unwrap());
        let raw = factory.open_session().unwrap();
        raw.catalog.set_metadata(&key, &legacy).unwrap();
        let Err(failure) = first.new_session() else {
            panic!("secondary session bound legacy ACL names");
        };
        assert!(
            failure.to_string().contains("initial catalog migration"),
            "{failure}"
        );
        raw.catalog.set_metadata("sql_functions_json", "{").unwrap();
        drop(second);
        drop(first);
        let Err(failure) = Engine::from_persistent_provider(Arc::clone(&factory)) else {
            panic!("restoration accepted invalid routines");
        };
        assert!(failure.to_string().contains("EOF"), "{failure}");
        assert_eq!(
            raw.catalog.get_metadata(&key).unwrap().as_deref(),
            Some(legacy.as_str())
        );
        raw.catalog.delete_metadata("sql_functions_json").unwrap();
        let reopened = Engine::from_persistent_provider(Arc::clone(&factory)).unwrap();
        assert_eq!(*reopened.durable.system_relation_security.read(), *expected);
        assert!(has_privilege(&reopened, "reader", false));
        let encoded = stored(raw.catalog.as_ref());
        drop(reopened);
        let reopened = Engine::from_persistent_provider(factory).unwrap();
        assert_eq!(*reopened.durable.system_relation_security.read(), *expected);
        assert_eq!(stored(raw.catalog.as_ref()), encoded);
    }
}

#[test]
fn corrupt_system_acl_endpoints_never_rebind_existing_names() {
    for provider in 0..3 {
        for column in [false, true] {
            for endpoint in ["role", "grantor"] {
                let (_directory, first, second) = sessions(provider);
                sql(&first, "CREATE ROLE reader");
                let privilege = if column { "UPDATE(rolname)" } else { "SELECT" };
                sql(
                    &first,
                    &format!("GRANT {privilege} ON pg_catalog.pg_authid TO reader"),
                );
                let factory = Arc::clone(first.storage.provider.as_ref().unwrap());
                let raw = factory.open_session().unwrap();
                let (key, original) = stored(raw.catalog.as_ref()).remove(0);
                let mut value: serde_json::Value = serde_json::from_str(&original).unwrap();
                value["entry"]["acl"]
                    .as_array_mut()
                    .unwrap()
                    .last_mut()
                    .unwrap()[endpoint]["object_id"] = serde_json::json!(vec![42; 16]);
                let invalid = value.to_string();
                raw.catalog.set_metadata(&key, &invalid).unwrap();
                let Err(failure) = first.new_session() else {
                    panic!("invalid system ACL identity accepted");
                };
                assert!(
                    failure.to_string().contains("missing role incarnation"),
                    "{failure}"
                );
                drop(second);
                drop(first);
                let Err(failure) = Engine::from_persistent_provider(Arc::clone(&factory)) else {
                    panic!("invalid system ACL identity rebound");
                };
                assert!(
                    failure.to_string().contains("missing role incarnation"),
                    "{failure}"
                );
                assert_eq!(
                    raw.catalog.get_metadata(&key).unwrap().as_deref(),
                    Some(invalid.as_str())
                );
                raw.catalog.set_metadata(&key, &original).unwrap();
                let reopened = Engine::from_persistent_provider(factory).unwrap();
                assert!(has_privilege(&reopened, "reader", column));
            }
        }
    }
}
