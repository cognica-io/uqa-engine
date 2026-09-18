//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Selected authorization survives role removal without adopting a replacement's privileges.

use crate::{
    tests::relation_lock_support::{error, sessions, sql},
    Engine,
};
use uqa_core::Value;

fn scalar(engine: &Engine, expression: &str) -> Value {
    sql(engine, &format!("SELECT {expression} AS value")).rows[0]["value"].clone()
}

fn fixture(admin: &Engine) {
    sql(admin, "GRANT SELECT ON t TO PUBLIC; CREATE TABLE private_items(v integer); INSERT INTO private_items VALUES (7); CREATE VIEW public_items WITH (security_invoker = true) AS SELECT v FROM t; GRANT SELECT ON public_items TO PUBLIC; CREATE FUNCTION private_answer() RETURNS integer LANGUAGE sql AS 'SELECT 7'; REVOKE ALL ON FUNCTION private_answer() FROM PUBLIC; CREATE SEQUENCE private_counter");
}

fn assert_removed_authority(actor: &Engine) {
    assert_eq!(scalar(actor, "1"), Value::Int(1));
    assert_eq!(sql(actor, "SELECT v FROM t").rows[0]["v"], Value::Int(1));
    assert_eq!(
        sql(actor, "EXECUTE public_query").rows[0]["v"],
        Value::Int(1)
    );
    error(actor, "SELECT CURRENT_USER", "42704");
    for statement in [
        "SELECT * FROM private_items",
        "WITH source AS (SELECT * FROM private_items) SELECT * FROM source",
        "INSERT INTO t VALUES (2)",
        "SELECT private_answer()",
        "SELECT nextval('private_counter')",
        "CREATE ROLE unauthorized",
    ] {
        error(actor, statement, "42501");
    }
    sql(actor, "BEGIN");
    error(actor, "LOCK private_items IN ACCESS SHARE MODE", "42501");
    sql(actor, "ROLLBACK");
    for inquiry in [
        "has_table_privilege('private_items', 'SELECT')",
        "has_column_privilege('private_items', 'v', 'SELECT')",
        "has_sequence_privilege('private_counter', 'USAGE')",
        "has_function_privilege('private_answer()', 'EXECUTE')",
        "has_database_privilege('uqa', 'CREATE')",
        "has_schema_privilege('public', 'CREATE')",
        "pg_has_role('uqa', 'USAGE')",
    ] {
        assert_eq!(scalar(actor, inquiry), Value::Bool(false), "{inquiry}");
    }
    assert_eq!(
        scalar(
            actor,
            "has_table_privilege('active_user', 'private_items', 'SELECT')"
        ),
        Value::Bool(true)
    );
    assert_eq!(
        scalar(actor, "has_table_privilege('t', 'SELECT')"),
        Value::Bool(true)
    );
}

#[test]
fn deleted_session_roles_do_not_adopt_recreated_authority_for_any_provider() {
    for provider in 0..3 {
        let (_directory, admin, peer) = sessions(provider);
        fixture(&admin);
        for initially_superuser in [false, true] {
            for mode in 0..3 {
                sql(
                    &admin,
                    &format!(
                        "CREATE ROLE active_user LOGIN {}",
                        if initially_superuser {
                            "SUPERUSER"
                        } else {
                            "NOSUPERUSER"
                        }
                    ),
                );
                let actor = if mode == 2 {
                    admin.new_session_for_user("active_user").unwrap()
                } else {
                    let actor = peer.new_session().unwrap();
                    sql(
                        &actor,
                        if mode == 0 {
                            "SET ROLE active_user"
                        } else {
                            "SET SESSION AUTHORIZATION active_user"
                        },
                    );
                    actor
                };
                assert_eq!(
                    scalar(&actor, "CURRENT_USER"),
                    Value::Str("active_user".into())
                );
                sql(&actor, "PREPARE public_query AS SELECT v FROM public_items");
                if initially_superuser {
                    sql(
                        &actor,
                        "PREPARE private_query AS SELECT v FROM private_items",
                    );
                }
                sql(
                    &admin,
                    "DROP ROLE active_user; CREATE ROLE active_user LOGIN SUPERUSER",
                );
                assert_removed_authority(&actor);
                if initially_superuser {
                    error(&actor, "EXECUTE private_query", "42501");
                }
                if mode != 0 {
                    error(&actor, "SET ROLE active_user", "42501");
                    error(&actor, "SELECT SESSION_USER", "42704");
                }
                sql(&actor, "RESET ROLE");
                if mode == 0 {
                    assert_eq!(scalar(&actor, "CURRENT_USER"), Value::Str("uqa".into()));
                    sql(&actor, "SET ROLE active_user");
                    assert_eq!(
                        scalar(&actor, "has_table_privilege('private_items', 'SELECT')"),
                        Value::Bool(true)
                    );
                } else {
                    error(&actor, "SELECT CURRENT_USER", "42704");
                }
                sql(&actor, "SET SESSION AUTHORIZATION DEFAULT");
                if mode == 2 {
                    error(&actor, "SELECT SESSION_USER", "42704");
                    error(&actor, "SET SESSION AUTHORIZATION active_user", "42501");
                } else {
                    assert_eq!(scalar(&actor, "SESSION_USER"), Value::Str("uqa".into()));
                }
                drop(actor);
                sql(&admin, "DROP ROLE active_user");
            }
        }
    }
}

#[test]
fn authenticated_role_authority_tracks_live_attributes_and_keeps_its_incarnation() {
    for provider in 0..3 {
        let (_directory, admin, _peer) = sessions(provider);
        sql(
            &admin,
            "CREATE ROLE original LOGIN; CREATE ROLE destination",
        );
        let actor = admin.new_session_for_user("original").unwrap();
        assert_eq!(
            scalar(&actor, "SESSION_USER"),
            Value::Str("original".into())
        );
        error(&actor, "SET SESSION AUTHORIZATION destination", "42501");
        sql(&admin, "ALTER ROLE original SUPERUSER");
        sql(&actor, "SET SESSION AUTHORIZATION destination");
        assert_eq!(
            scalar(&actor, "SESSION_USER"),
            Value::Str("destination".into())
        );
        sql(&admin, "ALTER ROLE original NOSUPERUSER");
        sql(&actor, "RESET SESSION AUTHORIZATION");
        error(&actor, "SET SESSION AUTHORIZATION destination", "42501");
        sql(
            &admin,
            "DROP ROLE original; CREATE ROLE original LOGIN SUPERUSER",
        );
        error(&actor, "SET SESSION AUTHORIZATION destination", "42501");
        error(&actor, "SET SESSION AUTHORIZATION original", "42501");
        sql(&actor, "RESET SESSION AUTHORIZATION");
        error(&actor, "SELECT CURRENT_USER", "42704");
    }
}

#[test]
fn local_authorization_role_and_savepoint_restoration_are_independent() {
    for provider in 0..3 {
        let (_directory, engine, _peer) = sessions(provider);
        sql(
            &engine,
            "CREATE ROLE original; CREATE ROLE selected; GRANT selected TO original",
        );
        sql(
            &engine,
            "BEGIN; SET LOCAL SESSION AUTHORIZATION original; SET ROLE selected; COMMIT",
        );
        assert_eq!(scalar(&engine, "SESSION_USER"), Value::Str("uqa".into()));
        assert_eq!(
            scalar(&engine, "CURRENT_USER"),
            Value::Str("selected".into())
        );
        sql(&engine, "BEGIN; SET LOCAL SESSION AUTHORIZATION original; SAVEPOINT auth; SET SESSION AUTHORIZATION uqa; ROLLBACK TO auth; COMMIT");
        assert_eq!(scalar(&engine, "SESSION_USER"), Value::Str("uqa".into()));
        assert_eq!(
            scalar(&engine, "CURRENT_USER"),
            Value::Str("selected".into())
        );
        sql(
            &engine,
            "BEGIN; SET SESSION AUTHORIZATION original; SET LOCAL ROLE selected; COMMIT",
        );
        assert_eq!(
            scalar(&engine, "SESSION_USER"),
            Value::Str("original".into())
        );
        assert_eq!(
            scalar(&engine, "CURRENT_USER"),
            Value::Str("original".into())
        );
        assert_eq!(engine.show_variable("role").unwrap(), "none");
        assert_eq!(
            engine.show_variable("session_authorization").unwrap(),
            "original"
        );
        sql(&engine, "DISCARD ALL");
        assert_eq!(scalar(&engine, "SESSION_USER"), Value::Str("uqa".into()));
    }
}

#[test]
fn active_transaction_and_role_restoration_never_rebind_a_removed_identity() {
    for provider in 0..3 {
        let (_directory, admin, actor) = sessions(provider);
        for isolation in ["READ COMMITTED", "REPEATABLE READ", "SERIALIZABLE"] {
            sql(&admin, "CREATE ROLE original SUPERUSER");
            sql(&actor, "SET ROLE original");
            sql(
                &actor,
                &format!("BEGIN ISOLATION LEVEL {isolation}; SELECT * FROM t"),
            );
            sql(&admin, "DROP ROLE original; CREATE ROLE original SUPERUSER");
            error(&actor, "SELECT * FROM t", "42501");
            sql(&actor, "ROLLBACK");
            error(&actor, "SELECT CURRENT_USER", "42704");
            error(&actor, "SELECT * FROM t", "42501");
            sql(&actor, "RESET ROLE; SET ROLE original");
            sql(
                &actor,
                &format!("BEGIN ISOLATION LEVEL {isolation}; SET LOCAL ROLE uqa; SELECT * FROM t"),
            );
            sql(&admin, "DROP ROLE original; CREATE ROLE original SUPERUSER");
            sql(&actor, "COMMIT");
            error(&actor, "SELECT CURRENT_USER", "42704");
            error(&actor, "SELECT * FROM t", "42501");
            sql(&actor, "RESET ROLE");
            sql(&admin, "DROP ROLE original");
        }
    }
}
