//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Routine configuration preserves caller identities and independent SET lifetimes.

use crate::{
    tests::relation_lock_support::{error, sessions, sql},
    Engine,
};
use uqa_core::Value;

fn scalar(engine: &Engine, expression: &str) -> Value {
    sql(engine, &format!("SELECT {expression} AS value")).rows[0]["value"].clone()
}

fn assert_identity(engine: &Engine, session: &str, current: &str, selected: &str) {
    assert_eq!(scalar(engine, "SESSION_USER"), Value::Str(session.into()));
    assert_eq!(scalar(engine, "CURRENT_USER"), Value::Str(current.into()));
    assert_eq!(engine.show_variable("role").unwrap(), selected);
}

#[test]
fn configured_authorization_restores_the_callers_selected_identity() {
    for provider in 0..3 {
        let (_directory, engine, _peer) = sessions(provider);
        sql(&engine, "CREATE ROLE configured; CREATE ROLE caller; CREATE FUNCTION configured_role() RETURNS text LANGUAGE sql SET role='configured' AS 'SELECT current_user::text'; CREATE FUNCTION configured_auth() RETURNS text LANGUAGE sql SET session_authorization='configured' AS 'SELECT session_user::text || '':'' || current_user::text'; SET ROLE caller");
        assert_eq!(
            scalar(&engine, "configured_role()"),
            Value::Str("configured".into())
        );
        assert_identity(&engine, "uqa", "caller", "caller");
        assert_eq!(
            scalar(&engine, "configured_auth()"),
            Value::Str("configured:configured".into())
        );
        assert_identity(&engine, "uqa", "caller", "caller");
    }
}

#[test]
fn configuration_validation_keeps_outer_transaction_local_restoration() {
    for provider in 0..3 {
        let (_directory, engine, _peer) = sessions(provider);
        sql(&engine, "CREATE ROLE configured; CREATE ROLE changed; GRANT CREATE ON SCHEMA public TO configured");
        for parameter in ["role", "session_authorization"] {
            sql(
                &engine,
                &format!("BEGIN; SET LOCAL {parameter} = 'configured'; CREATE OR REPLACE FUNCTION registration_probe() RETURNS text LANGUAGE sql SET {parameter}='changed' AS 'SELECT current_user::text'"),
            );
            assert_eq!(
                scalar(&engine, "CURRENT_USER"),
                Value::Str("configured".into())
            );
            sql(&engine, "COMMIT");
            assert_identity(&engine, "uqa", "uqa", "none");
        }
    }
}

#[test]
fn ordinary_and_local_function_assignments_have_distinct_lifetimes() {
    for provider in 0..3 {
        let (_directory, engine, _peer) = sessions(provider);
        sql(&engine, "CREATE ROLE configured; CREATE ROLE changed; CREATE FUNCTION body_set_role() RETURNS text LANGUAGE plpgsql SET role='configured' AS $$ BEGIN SET ROLE changed; RETURN current_user::text; END $$; CREATE FUNCTION body_local_role() RETURNS text LANGUAGE plpgsql SET role='configured' AS $$ BEGIN SET LOCAL ROLE changed; RETURN current_user::text; END $$; CREATE FUNCTION body_set_parameter() RETURNS text LANGUAGE plpgsql SET application_name='configured' AS $$ BEGIN SET application_name='changed'; RETURN current_setting('application_name'); END $$; CREATE FUNCTION body_local_parameter() RETURNS text LANGUAGE plpgsql SET application_name='configured' AS $$ BEGIN SET LOCAL application_name='local'; RETURN current_setting('application_name'); END $$");
        assert_eq!(
            scalar(&engine, "body_set_role()"),
            Value::Str("changed".into())
        );
        assert_identity(&engine, "uqa", "changed", "changed");
        sql(&engine, "RESET ROLE");
        assert_eq!(
            scalar(&engine, "body_local_role()"),
            Value::Str("changed".into())
        );
        assert_identity(&engine, "uqa", "uqa", "none");
        assert_eq!(
            scalar(&engine, "body_set_parameter()"),
            Value::Str("changed".into())
        );
        assert_eq!(engine.show_variable("application_name").unwrap(), "changed");
        assert_eq!(
            scalar(&engine, "body_local_parameter()"),
            Value::Str("local".into())
        );
        assert_eq!(engine.show_variable("application_name").unwrap(), "changed");
    }
}

#[test]
fn nested_assignments_errors_and_unconfigured_local_values_keep_their_boundaries() {
    for provider in 0..3 {
        let (_directory, engine, _peer) = sessions(provider);
        let default_application = engine.show_variable("application_name").unwrap();
        sql(&engine, "CREATE ROLE configured; CREATE ROLE caller; CREATE ROLE changed; CREATE FUNCTION local_unconfigured() RETURNS text LANGUAGE plpgsql SET application_name='configured' AS $$ BEGIN SET LOCAL ROLE changed; RETURN current_user::text; END $$; CREATE FUNCTION body_set_role() RETURNS text LANGUAGE plpgsql SET role='configured' AS $$ BEGIN SET ROLE changed; RETURN current_user::text; END $$; CREATE FUNCTION nested_set_role() RETURNS text LANGUAGE plpgsql SET role='caller' AS $$ BEGIN PERFORM body_set_role(); RETURN current_user::text; END $$; CREATE FUNCTION failed_inner() RETURNS integer LANGUAGE plpgsql SET role='configured' AS $$ BEGIN SET ROLE changed; RETURN 1 / 0; END $$; CREATE FUNCTION caught_inner() RETURNS text LANGUAGE plpgsql SET role='caller' AS $$ BEGIN BEGIN PERFORM failed_inner(); EXCEPTION WHEN division_by_zero THEN NULL; END; RETURN current_user::text; END $$; CREATE FUNCTION body_reset_all() RETURNS text LANGUAGE plpgsql SET application_name='configured' AS $$ BEGIN RESET ALL; RETURN current_setting('application_name'); END $$; SET application_name='outer'");
        assert_eq!(
            scalar(&engine, "local_unconfigured()"),
            Value::Str("changed".into())
        );
        assert_identity(&engine, "uqa", "uqa", "none");
        sql(&engine, "BEGIN");
        assert_eq!(
            scalar(&engine, "local_unconfigured()"),
            Value::Str("changed".into())
        );
        assert_identity(&engine, "uqa", "changed", "changed");
        assert_eq!(engine.show_variable("application_name").unwrap(), "outer");
        sql(&engine, "COMMIT");
        assert_identity(&engine, "uqa", "uqa", "none");
        assert_eq!(
            scalar(&engine, "nested_set_role()"),
            Value::Str("changed".into())
        );
        assert_identity(&engine, "uqa", "changed", "changed");
        sql(&engine, "RESET ROLE");
        assert_eq!(
            scalar(&engine, "caught_inner()"),
            Value::Str("caller".into())
        );
        assert_identity(&engine, "uqa", "uqa", "none");
        sql(&engine, "BEGIN; SET LOCAL ROLE caller");
        assert_eq!(
            scalar(&engine, "body_reset_all()"),
            Value::Str(default_application.clone())
        );
        assert_identity(&engine, "uqa", "caller", "caller");
        assert_eq!(
            engine.show_variable("application_name").unwrap(),
            default_application
        );
        sql(&engine, "COMMIT");
        assert_identity(&engine, "uqa", "uqa", "none");
    }
}

#[test]
fn function_exit_restores_a_deleted_identity_without_adopting_its_replacement() {
    for provider in 0..3 {
        let (_directory, admin, peer) = sessions(provider);
        sql(&admin, "CREATE ROLE configured; CREATE FUNCTION configured_role() RETURNS text LANGUAGE sql SET role='configured' AS 'SELECT current_user::text'; CREATE FUNCTION configured_auth() RETURNS text LANGUAGE sql SET session_authorization='configured' AS 'SELECT current_user::text'");
        for parameter in ["role", "session_authorization"] {
            sql(&admin, "CREATE ROLE caller");
            let actor = peer.new_session().unwrap();
            sql(&actor, &format!("SET {parameter}='caller'"));
            sql(&admin, "DROP ROLE caller; CREATE ROLE caller SUPERUSER");
            let function = if parameter == "role" {
                "configured_role()"
            } else {
                "configured_auth()"
            };
            assert_eq!(scalar(&actor, function), Value::Str("configured".into()));
            error(&actor, "SELECT CURRENT_USER", "42704");
            assert_eq!(
                scalar(&actor, "pg_has_role('uqa', 'USAGE')"),
                Value::Bool(false)
            );
            if parameter == "session_authorization" {
                error(&actor, "SELECT SESSION_USER", "42704");
            }
            drop(actor);
            sql(&admin, "DROP ROLE caller");
        }
    }
}

#[test]
fn current_setting_reads_live_parameters_and_keeps_builtin_bindings() {
    let engine = Engine::new();
    sql(&engine, "SET application_name='caller'; PREPARE read_setting AS SELECT pg_catalog.current_setting('application_name') AS value");
    assert_eq!(
        sql(&engine, "EXECUTE read_setting").rows[0]["value"],
        Value::Str("caller".into())
    );
    sql(&engine, "SET application_name='changed'");
    assert_eq!(
        sql(&engine, "EXECUTE read_setting").rows[0]["value"],
        Value::Str("changed".into())
    );
    assert_eq!(
        scalar(&engine, "current_setting('APPLICATION_NAME')"),
        Value::Str("changed".into())
    );
    for expression in [
        "current_setting(NULL)",
        "current_setting('missing', true)",
        "current_setting('missing', NULL)",
    ] {
        assert_eq!(scalar(&engine, expression), Value::Null);
    }
    error(&engine, "SELECT current_setting('missing')", "42704");
    error(&engine, "SELECT current_setting(42)", "42883");
    error(
        &engine,
        "SELECT current_setting('application_name', 1)",
        "42883",
    );
    assert_eq!(
        scalar(&engine, "'current_setting(text)'::regprocedure::oid"),
        Value::Int(2077)
    );
    assert_eq!(
        scalar(
            &engine,
            "'current_setting(text,boolean)'::regprocedure::oid"
        ),
        Value::Int(3294)
    );
    let metadata = sql(&engine, "SELECT oid, proisstrict, provolatile, proparallel FROM pg_proc WHERE proname='current_setting' ORDER BY oid");
    assert_eq!(metadata.rows.len(), 2);
    for row in &metadata.rows {
        assert_eq!(row["proisstrict"], Value::Bool(true));
        assert_eq!(row["provolatile"], Value::Str("s".into()));
        assert_eq!(row["proparallel"], Value::Str("s".into()));
    }
    sql(&engine, "CREATE SCHEMA app; CREATE FUNCTION app.current_setting(text) RETURNS text LANGUAGE sql AS $$ SELECT 'shadow'::text $$; SET search_path=app,pg_catalog");
    assert_eq!(
        scalar(&engine, "current_setting('application_name')"),
        Value::Str("shadow".into())
    );
    assert_eq!(
        scalar(&engine, "pg_catalog.current_setting('application_name')"),
        Value::Str("changed".into())
    );
    assert_eq!(
        sql(&engine, "EXECUTE read_setting").rows[0]["value"],
        Value::Str("changed".into())
    );
}

#[test]
fn direct_search_path_assignment_survives_function_and_transaction_local_restoration() {
    let engine = Engine::new();
    sql(&engine, "BEGIN; SET LOCAL search_path=pg_catalog");
    let mut guard = engine.routine_invocation_state_guard(true, false);
    engine
        .set_configured_parameter("search_path", "public")
        .unwrap();
    engine.set_search_path(vec!["pg_catalog".into(), "public".into()]);
    guard.finish();
    assert_eq!(
        engine.show_variable("search_path").unwrap(),
        "pg_catalog,public"
    );
    sql(&engine, "COMMIT");
    assert_eq!(
        engine.show_variable("search_path").unwrap(),
        "pg_catalog,public"
    );
}

#[test]
fn definer_identity_restores_independently_of_successful_session_assignments() {
    for provider in 0..3 {
        let (_directory, engine, _peer) = sessions(provider);
        sql(&engine, "CREATE ROLE caller; CREATE FUNCTION definer_setting() RETURNS text LANGUAGE plpgsql SECURITY DEFINER SET application_name='configured' AS $$ BEGIN SET application_name='changed'; RETURN current_user::text || ':' || current_setting('application_name'); END $$; CREATE FUNCTION failed_setting() RETURNS integer LANGUAGE plpgsql SECURITY DEFINER SET application_name='configured' AS $$ BEGIN SET application_name='failed'; RETURN 1 / 0; END $$; SET ROLE caller; SET application_name='caller'");
        assert_eq!(
            scalar(&engine, "definer_setting()"),
            Value::Str("uqa:changed".into())
        );
        assert_identity(&engine, "uqa", "caller", "caller");
        assert_eq!(engine.show_variable("application_name").unwrap(), "changed");
        error(&engine, "SELECT failed_setting()", "22012");
        assert_identity(&engine, "uqa", "caller", "caller");
        assert_eq!(engine.show_variable("application_name").unwrap(), "changed");
    }
}

#[test]
fn session_characteristics_supersede_local_and_configured_defaults() {
    let engine = Engine::new();
    sql(&engine, "CREATE FUNCTION assign_defaults() RETURNS text LANGUAGE plpgsql SET default_transaction_isolation='read committed' SET default_transaction_read_only=off SET default_transaction_deferrable=off AS $$ BEGIN SET SESSION CHARACTERISTICS AS TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY, DEFERRABLE; RETURN current_setting('default_transaction_isolation'); END $$; BEGIN; SET LOCAL default_transaction_isolation='serializable'; SET LOCAL default_transaction_read_only=off; SET LOCAL default_transaction_deferrable=off");
    assert_eq!(
        scalar(&engine, "assign_defaults()"),
        Value::Str("repeatable read".into())
    );
    for ending in [None, Some("COMMIT")] {
        if let Some(ending) = ending {
            sql(&engine, ending);
        }
        assert_eq!(
            engine
                .show_variable("default_transaction_isolation")
                .unwrap(),
            "repeatable read"
        );
        assert_eq!(
            engine
                .show_variable("default_transaction_read_only")
                .unwrap(),
            "on"
        );
        assert_eq!(
            engine
                .show_variable("default_transaction_deferrable")
                .unwrap(),
            "on"
        );
    }
}
