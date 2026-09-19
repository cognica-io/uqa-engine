//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stored routines preserve owner and ACL role incarnations.

use crate::{
    tests::relation_lock_support::{error, sql},
    Engine,
};
use uqa_core::Value;

fn setup() -> Engine {
    let engine = Engine::new();
    sql(&engine, "CREATE ROLE routine_owner; CREATE ROLE delegate; CREATE ROLE recipient; GRANT CREATE ON SCHEMA public TO routine_owner; SET ROLE routine_owner; CREATE FUNCTION owned() RETURNS int LANGUAGE SQL AS 'SELECT 7'; CREATE FUNCTION identity_of_owner() RETURNS text LANGUAGE SQL SECURITY DEFINER AS 'SELECT CURRENT_USER::text'; REVOKE ALL ON FUNCTION owned() FROM PUBLIC; GRANT EXECUTE ON FUNCTION owned() TO delegate WITH GRANT OPTION; SET ROLE delegate; GRANT EXECUTE ON FUNCTION owned() TO recipient; RESET ROLE; REVOKE CREATE ON SCHEMA public FROM routine_owner");
    engine
}

fn rename_role(engine: &Engine, old: &str, new: &str) {
    // Install a renamed role snapshot directly to isolate retained routine authority.
    {
        let mut roles = engine.durable.roles.write();
        let mut role = roles.remove(old).unwrap();
        role.name = new.into();
        roles.insert(new.into(), role);
    }
    engine.note_catalog_registry_changed();
}

#[test]
fn routine_owner_projection_and_dependency_keep_the_original_incarnation() {
    let engine = setup();
    let owner = engine.durable.roles.read()["routine_owner"].identity();
    rename_role(&engine, "routine_owner", "renamed_owner");
    sql(&engine, "CREATE ROLE routine_owner");
    assert_eq!(
        sql(
            &engine,
            "SELECT proowner FROM pg_proc WHERE proname = 'owned'"
        )
        .rows[0]["proowner"],
        Value::Int(owner.oid)
    );
    error(&engine, "DROP ROLE renamed_owner", "2BP01");
    sql(&engine, "DROP ROLE routine_owner; SET ROLE renamed_owner; DROP FUNCTION owned(); DROP FUNCTION identity_of_owner(); RESET ROLE; DROP ROLE renamed_owner");
}

#[test]
fn routine_acl_roles_do_not_transfer_grants_to_reused_names() {
    let engine = setup();
    rename_role(&engine, "delegate", "renamed_delegate");
    rename_role(&engine, "recipient", "renamed_recipient");
    sql(&engine, "CREATE ROLE delegate; CREATE ROLE recipient");
    for (role, expected) in [
        ("delegate", false),
        ("recipient", false),
        ("renamed_delegate", true),
        ("renamed_recipient", true),
    ] {
        assert_eq!(
            sql(
                &engine,
                &format!(
                    "SELECT has_function_privilege('{role}', 'owned()', 'EXECUTE') AS allowed"
                )
            )
            .rows[0]["allowed"],
            Value::Bool(expected),
            "{role}"
        );
    }
    error(&engine, "DROP ROLE renamed_delegate", "2BP01");
    error(&engine, "DROP ROLE renamed_recipient", "2BP01");
    sql(&engine, "DROP ROLE delegate; DROP ROLE recipient; SET ROLE routine_owner; REVOKE GRANT OPTION FOR EXECUTE ON FUNCTION owned() FROM renamed_delegate CASCADE; RESET ROLE");
    assert_eq!(
        sql(
            &engine,
            "SELECT has_function_privilege('renamed_recipient', 'owned()', 'EXECUTE') AS allowed"
        )
        .rows[0]["allowed"],
        Value::Bool(false)
    );
}

#[test]
fn security_definer_uses_the_retained_owner_and_restores_the_caller() {
    let engine = setup();
    rename_role(&engine, "routine_owner", "renamed_owner");
    sql(&engine, "CREATE ROLE routine_owner; SET ROLE recipient");
    assert_eq!(
        sql(&engine, "SELECT identity_of_owner() AS owner").rows[0]["owner"],
        Value::Str("renamed_owner".into())
    );
    assert_eq!(
        sql(&engine, "SELECT CURRENT_USER AS caller").rows[0]["caller"],
        Value::Str("recipient".into())
    );
}

#[test]
fn private_routine_authority_survives_refresh_savepoint_and_reopen() {
    use super::{identity::reopen, snapshots::refresh_catalog};
    use crate::tests::relation_lock_support::sessions;
    for provider in 0..3 {
        for isolation in ["READ COMMITTED", "REPEATABLE READ", "SERIALIZABLE"] {
            for finish in ["COMMIT", "ROLLBACK", "ROLLBACK TO private; COMMIT"] {
                let (directory, first, second) = sessions(provider);
                sql(&first, "CREATE ROLE other; CREATE ROLE delegate; CREATE FUNCTION removed_routine() RETURNS int RETURN 1; CREATE FUNCTION changed_acl() RETURNS int RETURN 2; REVOKE ALL ON FUNCTION changed_acl() FROM PUBLIC; GRANT EXECUTE ON FUNCTION changed_acl() TO delegate");
                sql(&first, &format!("BEGIN ISOLATION LEVEL {isolation}; SELECT * FROM t; SAVEPOINT private; CREATE ROLE private_owner; GRANT CREATE ON SCHEMA public TO private_owner; SET ROLE private_owner; CREATE FUNCTION private_routine() RETURNS text LANGUAGE SQL SECURITY DEFINER AS 'SELECT CURRENT_USER::text'; CREATE PROCEDURE private_procedure() LANGUAGE SQL AS 'SELECT 1'; REVOKE ALL ON FUNCTION private_routine() FROM PUBLIC; GRANT EXECUTE ON FUNCTION private_routine() TO delegate; RESET ROLE; REVOKE CREATE ON SCHEMA public FROM private_owner; DROP FUNCTION removed_routine(); REVOKE EXECUTE ON FUNCTION changed_acl() FROM delegate"));
                let owner = first.durable.roles.read()["private_owner"].identity();
                let original = first.durable.sql_user_functions.read()["public.private_routine"][0]
                    .def
                    .object_id;
                sql(
                    &second,
                    "ALTER ROLE other LOGIN; CREATE TABLE unrelated(id int)",
                );
                refresh_catalog(&first, isolation);
                {
                    let registry = first.durable.sql_user_functions.read();
                    assert_eq!(
                        registry["public.private_routine"][0].def.owner,
                        Some(owner),
                        "provider {provider}, {isolation}, {finish}"
                    );
                    assert_eq!(
                        registry["public.private_procedure"][0].def.owner,
                        Some(owner)
                    );
                    assert!(!registry.contains_key("public.removed_routine"));
                }
                assert_eq!(sql(&first, "SELECT has_function_privilege('delegate', 'changed_acl()', 'EXECUTE') AS allowed").rows[0]["allowed"], Value::Bool(false));
                assert_eq!(sql(&second, "SELECT has_function_privilege('delegate', 'changed_acl()', 'EXECUTE') AS allowed").rows[0]["allowed"], Value::Bool(true));
                sql(&first, "SET ROLE delegate");
                assert_eq!(
                    sql(&first, "SELECT private_routine() AS owner").rows[0]["owner"],
                    Value::Str("private_owner".into())
                );
                sql(&first, "RESET ROLE");
                assert!(sql(&second, "SELECT proowner FROM pg_proc WHERE proname IN ('private_routine', 'private_procedure')").rows.is_empty());
                sql(&first, finish);
                assert_eq!(sql(&second, "SELECT proowner FROM pg_proc WHERE proname IN ('private_routine', 'private_procedure')").rows.len(), if finish == "COMMIT" { 2 } else { 0 });
                assert_eq!(sql(&second, "SELECT has_function_privilege('delegate', 'changed_acl()', 'EXECUTE') AS allowed").rows[0]["allowed"], Value::Bool(finish != "COMMIT"));
                drop(second);
                drop(first);
                let restored = reopen(provider, &directory.path().join("table-locks.db"));
                let registry = restored.durable.sql_user_functions.read();
                assert_eq!(
                    registry.contains_key("public.removed_routine"),
                    finish != "COMMIT"
                );
                let routine = registry.get("public.private_routine");
                assert_eq!(routine.is_some(), finish == "COMMIT");
                if let Some(routine) = routine {
                    assert_eq!(routine[0].def.owner, Some(owner));
                    assert_eq!(routine[0].def.object_id, original);
                }
            }
        }
    }
}

fn persistent_fixture(engine: &Engine) {
    sql(engine, "CREATE ROLE routine_owner; GRANT CREATE ON SCHEMA public TO routine_owner; SET ROLE routine_owner; CREATE FUNCTION stored_routine() RETURNS int RETURN 7; REVOKE ALL ON FUNCTION stored_routine() FROM PUBLIC, routine_owner; RESET ROLE; REVOKE CREATE ON SCHEMA public FROM routine_owner");
}

#[test]
fn routine_format_one_conversion_preserves_revoked_owner_execute() {
    use crate::tests::relation_lock_support::sessions;
    use std::sync::Arc;
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        persistent_fixture(&first);
        let owner = first.durable.roles.read()["routine_owner"].identity();
        let factory = Arc::clone(first.storage.provider.as_ref().unwrap());
        let raw = factory.open_session().unwrap();
        let original = raw
            .catalog
            .get_metadata("sql_functions_json")
            .unwrap()
            .unwrap();
        let mut legacy: serde_json::Value = serde_json::from_str(&original).unwrap();
        legacy["routine_catalog_format"] = 1.into();
        legacy["definitions"]["public.stored_routine"][0]["owner"] = "routine_owner".into();
        raw.catalog
            .set_metadata("sql_functions_json", &legacy.to_string())
            .unwrap();
        assert!(first.new_session().is_err());
        drop(second);
        drop(first);
        let restored = Engine::from_persistent_provider(factory).unwrap();
        assert_eq!(
            restored.durable.sql_user_functions.read()["public.stored_routine"][0]
                .def
                .owner,
            Some(owner)
        );
        assert_eq!(sql(&restored, "SELECT has_function_privilege('routine_owner', 'stored_routine()', 'EXECUTE') AS allowed").rows[0]["allowed"], Value::Bool(false));
        assert_eq!(sql(&restored, "SELECT has_function_privilege('routine_owner', 'stored_routine()', 'EXECUTE WITH GRANT OPTION') AS allowed").rows[0]["allowed"], Value::Bool(true));
        let stored: serde_json::Value = serde_json::from_str(
            &raw.catalog
                .get_metadata("sql_functions_json")
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(stored["routine_catalog_format"], 2);
        assert!(
            stored["definitions"]["public.stored_routine"][0]["execute_acl"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }
}

#[test]
fn corrupt_current_routine_authority_never_rebinds_or_installs_placeholders() {
    use crate::{open::CatalogRestoreMode, tests::relation_lock_support::sessions};
    use std::sync::Arc;
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        persistent_fixture(&first);
        let factory = Arc::clone(first.storage.provider.as_ref().unwrap());
        let raw = factory.open_session().unwrap();
        let original = raw
            .catalog
            .get_metadata("sql_functions_json")
            .unwrap()
            .unwrap();
        let mut corrupt: serde_json::Value = serde_json::from_str(&original).unwrap();
        corrupt["definitions"]["public.stored_routine"][0]["owner"]["object_id"] =
            serde_json::to_value([99_u8; 16]).unwrap();
        let corrupt = corrupt.to_string();
        raw.catalog
            .set_metadata("sql_functions_json", &corrupt)
            .unwrap();
        let before = first.durable.sql_user_functions.snapshot();
        assert!(first
            .install_sql_function_restore_placeholders(
                raw.catalog.as_ref(),
                CatalogRestoreMode::LoadOnly
            )
            .is_err());
        assert!(Arc::ptr_eq(
            &before,
            &first.durable.sql_user_functions.snapshot()
        ));
        assert!(first.new_session().is_err());
        drop(second);
        drop(first);
        assert!(Engine::from_persistent_provider(Arc::clone(&factory)).is_err());
        assert_eq!(
            raw.catalog
                .get_metadata("sql_functions_json")
                .unwrap()
                .unwrap(),
            corrupt
        );
        raw.catalog
            .set_metadata("sql_functions_json", &original)
            .unwrap();
        let restored = Engine::from_persistent_provider(factory).unwrap();
        assert_eq!(
            sql(&restored, "SELECT stored_routine() AS value").rows[0]["value"],
            Value::Int(7)
        );
        error(&restored, "DROP ROLE routine_owner", "2BP01");
    }
}
