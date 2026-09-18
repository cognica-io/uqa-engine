//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Durable routine owner ACLs, shared session refresh and atomic legacy conversion.

use crate::{
    tests::relation_lock_support::{error, sessions, sql},
    Engine,
};
use std::sync::Arc;
use uqa_core::Value;

fn fixture(engine: &Engine) {
    sql(engine, "CREATE ROLE acl_owner; CREATE FUNCTION acl_routine() RETURNS integer LANGUAGE SQL AS 'SELECT 7'; ALTER FUNCTION acl_routine() OWNER TO acl_owner; REVOKE EXECUTE ON FUNCTION acl_routine() FROM PUBLIC");
}

fn access(engine: &Engine, allowed: bool) {
    sql(engine, "SET ROLE acl_owner");
    assert_eq!(
        sql(
            engine,
            "SELECT has_function_privilege('acl_routine()', 'EXECUTE') AS v"
        )
        .rows[0]["v"],
        Value::Bool(allowed)
    );
    assert_eq!(
        sql(
            engine,
            "SELECT has_function_privilege('acl_routine()', 'EXECUTE WITH GRANT OPTION') AS v"
        )
        .rows[0]["v"],
        Value::Bool(true)
    );
    if allowed {
        assert_eq!(
            sql(engine, "SELECT acl_routine() AS v").rows[0]["v"],
            Value::Int(7)
        );
    } else {
        error(engine, "SELECT acl_routine()", "42501");
    }
    sql(engine, "RESET ROLE");
}

#[test]
fn routine_owner_acl_refreshes_rolls_back_and_remains_revoked_after_reopen() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        fixture(&first);
        access(&second, true);
        sql(
            &first,
            "BEGIN; SAVEPOINT acl_change; REVOKE EXECUTE ON FUNCTION acl_routine() FROM acl_owner",
        );
        // Observe uncommitted privilege changes through the defining transaction's inquiry.
        assert_eq!(
            sql(
                &first,
                "SELECT has_function_privilege('acl_owner','acl_routine()','EXECUTE') AS v"
            )
            .rows[0]["v"],
            Value::Bool(false)
        );
        access(&second, true);
        sql(&first, "ROLLBACK TO acl_change; COMMIT");
        access(&second, true);
        sql(
            &first,
            "REVOKE EXECUTE ON FUNCTION acl_routine() FROM acl_owner",
        );
        access(&second, false);
        let factory = Arc::clone(first.storage.provider.as_ref().unwrap());
        drop(second);
        drop(first);
        let reopened = Engine::from_persistent_provider(factory).unwrap();
        access(&reopened, false);
        sql(
            &reopened,
            "SET ROLE acl_owner; GRANT EXECUTE ON FUNCTION acl_routine() TO acl_owner; RESET ROLE",
        );
        access(&reopened, true);
    }
}

#[test]
fn legacy_routine_owner_acl_migration_is_initial_open_only_and_rolls_back_atomically() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        fixture(&first);
        let factory = Arc::clone(first.storage.provider.as_ref().unwrap());
        let raw = factory.open_session().unwrap();
        let encoded = raw
            .catalog
            .get_metadata("sql_functions_json")
            .unwrap()
            .unwrap();
        let mut catalog: serde_json::Value = serde_json::from_str(&encoded).unwrap();
        assert_eq!(catalog["routine_catalog_format"], 1);
        let mut legacy = catalog["definitions"].take();
        for overloads in legacy.as_object_mut().unwrap().values_mut() {
            for definition in overloads.as_array_mut().unwrap() {
                let owner = definition["owner"].clone();
                definition["execute_acl"]
                    .as_array_mut()
                    .unwrap()
                    .retain(|entry| entry["role"]["name"] != owner);
            }
        }
        let legacy = serde_json::to_string(&legacy).unwrap();
        raw.catalog
            .set_metadata("sql_functions_json", &legacy)
            .unwrap();
        let Err(failure) = first.new_session() else {
            panic!("secondary restore migrated legacy ACLs");
        };
        assert!(failure.to_string().contains("initial-open"), "{failure}");
        let triggers = raw.catalog.get_metadata("sql_triggers_json").unwrap();
        raw.catalog.set_metadata("sql_triggers_json", "{").unwrap();
        drop(second);
        drop(first);
        let Err(failure) = Engine::from_persistent_provider(Arc::clone(&factory)) else {
            panic!("malformed trigger metadata must fail restoration");
        };
        assert!(failure.to_string().contains("EOF"), "{failure}");
        assert_eq!(
            raw.catalog.get_metadata("sql_functions_json").unwrap(),
            Some(legacy)
        );
        if let Some(triggers) = triggers {
            raw.catalog
                .set_metadata("sql_triggers_json", &triggers)
                .unwrap();
        } else {
            raw.catalog.delete_metadata("sql_triggers_json").unwrap();
        }
        let reopened = Engine::from_persistent_provider(Arc::clone(&factory)).unwrap();
        access(&reopened, true);
        let stored: serde_json::Value = serde_json::from_str(
            &raw.catalog
                .get_metadata("sql_functions_json")
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(stored["routine_catalog_format"], 1);
        let definition = stored["definitions"]
            .as_object()
            .unwrap()
            .values()
            .next()
            .unwrap()[0]
            .clone();
        assert_eq!(definition["execute_acl"].as_array().unwrap().len(), 1);
        assert_eq!(
            definition["execute_acl"][0]["role"],
            serde_json::json!({"kind": "role", "name": "acl_owner"})
        );
        sql(
            &reopened,
            "REVOKE EXECUTE ON FUNCTION acl_routine() FROM acl_owner",
        );
        drop(reopened);
        access(&Engine::from_persistent_provider(factory).unwrap(), false);
    }
}
