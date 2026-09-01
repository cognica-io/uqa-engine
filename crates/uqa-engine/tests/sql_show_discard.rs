//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `SHOW <var>` and `DISCARD ...` compatibility coverage.

use uqa_core::Value;
use uqa_engine::Engine;
use uqa_sql::ast::DiscardTarget;

#[test]
fn show_search_path_returns_current_resolution_order() {
    let eng = Engine::new();
    eng.sql("SET search_path TO app, public", &[]).unwrap();
    let r = eng.sql("SHOW search_path", &[]).unwrap();
    assert_eq!(r.columns, vec!["search_path".to_string()]);
    let v = r.rows[0].get("search_path").unwrap();
    let Value::Str(s) = v else {
        panic!("expected string, got {v:?}");
    };
    assert!(s.contains("app"));
    assert!(s.contains("public"));
}

#[test]
fn show_server_version_reports_postgresql_18_compatibility() {
    let eng = Engine::new();
    let r = eng.sql("SHOW server_version", &[]).unwrap();
    assert_eq!(r.columns, vec!["server_version".to_string()]);
    assert_eq!(
        r.rows[0].get("server_version"),
        Some(&Value::Str("18.0-uqa".into()))
    );

    let settings = eng
        .sql(
            "SELECT setting FROM pg_catalog.pg_settings WHERE name = 'server_version'",
            &[],
        )
        .unwrap();
    assert_eq!(
        settings.rows[0].get("setting"),
        Some(&Value::Str("18.0-uqa".into()))
    );
}

#[test]
fn show_builtin_runtime_parameters_are_case_insensitive() {
    let eng = Engine::new();
    assert_eq!(
        eng.show_variable("TimeZone").unwrap(),
        "UTC",
        "TimeZone should expose PostgreSQL-compatible default"
    );
    eng.sql("SET TimeZone TO 'Asia/Seoul'", &[]).unwrap();
    assert_eq!(
        eng.show_variable("timezone").unwrap(),
        "Asia/Seoul",
        "session override lookup should be case-insensitive"
    );
    let settings = eng
        .sql(
            "SELECT setting FROM pg_catalog.pg_settings WHERE name = 'TimeZone'",
            &[],
        )
        .unwrap();
    assert_eq!(
        settings.rows[0].get("setting"),
        Some(&Value::Str("Asia/Seoul".into()))
    );
}

#[test]
fn registered_defaults_and_read_only_parameters_are_explicit() {
    let eng = Engine::new();
    let work_mem = eng.sql("SHOW work_mem", &[]).unwrap();
    assert_eq!(
        work_mem.rows[0].get("work_mem"),
        Some(&Value::Str("64MB".into()))
    );

    let err = eng.sql("SET server_version TO 'pretend'", &[]).unwrap_err();
    assert_eq!(err.sqlstate(), Some("55P02"));
    assert!(err.to_string().contains("cannot be changed"));

    let err = eng.sql("SET work_mem TO 'unbounded'", &[]).unwrap_err();
    assert!(err.to_string().contains("positive byte size"));
}

#[test]
fn session_replication_role_validates_values_privileges_and_transaction_scope() {
    let eng = Engine::new();
    assert_eq!(
        eng.show_variable("session_replication_role").unwrap(),
        "origin"
    );

    let settings = eng
        .sql(
            "SELECT setting, context, vartype, boot_val, reset_val FROM pg_catalog.pg_settings WHERE name = 'session_replication_role'",
            &[],
        )
        .unwrap();
    assert_eq!(settings.rows[0]["setting"], Value::Str("origin".into()));
    assert_eq!(settings.rows[0]["context"], Value::Str("superuser".into()));
    assert_eq!(settings.rows[0]["vartype"], Value::Str("enum".into()));
    assert_eq!(settings.rows[0]["boot_val"], Value::Str("origin".into()));
    assert_eq!(settings.rows[0]["reset_val"], Value::Str("origin".into()));

    let invalid = eng
        .sql("SET session_replication_role = rep", &[])
        .unwrap_err();
    assert_eq!(invalid.sqlstate(), Some("22023"));
    assert!(invalid
        .to_string()
        .contains("Available values: origin, replica, local"));

    eng.sql(
        "BEGIN; SET session_replication_role = replica; ROLLBACK",
        &[],
    )
    .unwrap();
    assert_eq!(
        eng.show_variable("session_replication_role").unwrap(),
        "origin"
    );

    eng.sql("CREATE ROLE replication_role_user", &[]).unwrap();
    eng.sql("SET ROLE replication_role_user", &[]).unwrap();
    let denied = eng
        .sql("SET session_replication_role = replica", &[])
        .unwrap_err();
    assert_eq!(denied.sqlstate(), Some("42501"));
    let denied = eng.sql("RESET session_replication_role", &[]).unwrap_err();
    assert_eq!(denied.sqlstate(), Some("42501"));
    eng.sql("RESET ROLE", &[]).unwrap();
}

#[test]
fn plpgsql_check_asserts_matches_boolean_setting_and_routine_scope() {
    let eng = Engine::new();
    eng.sql("LOAD 'plpgsql'", &[]).unwrap();
    eng.sql("LOAD '$libdir/plpgsql'", &[]).unwrap();
    assert_eq!(eng.show_variable("plpgsql.check_asserts").unwrap(), "on");

    let settings = eng
        .sql(
            "SELECT setting, category, short_desc, context, vartype, boot_val, reset_val
             FROM pg_catalog.pg_settings WHERE name = 'plpgsql.check_asserts'",
            &[],
        )
        .unwrap();
    let row = &settings.rows[0];
    assert_eq!(row["setting"], Value::Str("on".into()));
    assert_eq!(row["category"], Value::Str("Customized Options".into()));
    assert_eq!(
        row["short_desc"],
        Value::Str("Perform checks given in ASSERT statements.".into())
    );
    assert_eq!(row["context"], Value::Str("user".into()));
    assert_eq!(row["vartype"], Value::Str("bool".into()));
    assert_eq!(row["boot_val"], Value::Str("on".into()));
    assert_eq!(row["reset_val"], Value::Str("on".into()));

    eng.sql("SET plpgsql.check_asserts = of", &[]).unwrap();
    assert_eq!(eng.show_variable("plpgsql.check_asserts").unwrap(), "off");
    eng.sql("SET plpgsql.check_asserts = t", &[]).unwrap();
    assert_eq!(eng.show_variable("plpgsql.check_asserts").unwrap(), "on");
    let invalid = eng.sql("SET plpgsql.check_asserts = o", &[]).unwrap_err();
    assert_eq!(invalid.sqlstate(), Some("22023"));
    assert_eq!(
        invalid.to_string(),
        "parameter \"plpgsql.check_asserts\" requires a Boolean value"
    );

    eng.sql("BEGIN; SET plpgsql.check_asserts = off; ROLLBACK", &[])
        .unwrap();
    assert_eq!(eng.show_variable("plpgsql.check_asserts").unwrap(), "on");
    eng.sql(
        "CREATE FUNCTION disabled_assert() RETURNS integer LANGUAGE plpgsql
         SET plpgsql.check_asserts = off AS $$ BEGIN ASSERT false; RETURN 7; END $$",
        &[],
    )
    .unwrap();
    let result = eng.sql("SELECT disabled_assert() AS value", &[]).unwrap();
    assert_eq!(result.rows[0]["value"], Value::Int(7));
    assert_eq!(eng.show_variable("plpgsql.check_asserts").unwrap(), "on");
}

#[test]
fn show_unknown_variable_is_an_error() {
    let eng = Engine::new();
    let err = eng.sql("SHOW some_unknown_var", &[]).unwrap_err();
    assert_eq!(err.sqlstate(), Some("42704"));
    assert!(err
        .to_string()
        .contains("unrecognized configuration parameter"));

    let err = eng
        .sql("SET some_unknown_var TO 'ignored'", &[])
        .unwrap_err();
    assert_eq!(err.sqlstate(), Some("42704"));
}

#[test]
fn set_then_show_round_trips_value() {
    let eng = Engine::new();
    eng.sql("SET work_mem TO '64MB'", &[]).unwrap();
    let r = eng.sql("SHOW work_mem", &[]).unwrap();
    let v = r.rows[0].get("work_mem").unwrap();
    let Value::Str(s) = v else {
        panic!("expected string");
    };
    assert!(s.contains("64MB"));
}

#[test]
fn discard_all_clears_session_state() {
    let eng = Engine::new();
    eng.sql("SET search_path TO app, public", &[]).unwrap();
    eng.sql("SET work_mem TO '64MB'", &[]).unwrap();
    eng.sql("DISCARD ALL", &[]).unwrap();
    let r = eng.sql("SHOW work_mem", &[]).unwrap();
    let v = r.rows[0].get("work_mem").unwrap();
    assert_eq!(v, &Value::Str("64MB".into()));
    let search_path = eng.sql("SHOW search_path", &[]).unwrap();
    assert_eq!(
        search_path.rows[0].get("search_path"),
        Some(&Value::Str("public".into()))
    );
}

#[test]
fn discard_plans_drops_prepared_only() {
    let eng = Engine::new();
    eng.sql("CREATE TABLE t (id INTEGER PRIMARY KEY)", &[])
        .unwrap();
    eng.sql("PREPARE p1 AS SELECT id FROM t", &[]).unwrap();
    assert!(eng.lookup_prepared("p1").is_some());
    eng.discard(DiscardTarget::Plans).unwrap();
    assert!(eng.lookup_prepared("p1").is_none());
}

#[test]
fn discard_temp_drops_temporary_relations_and_rejects_transaction_blocks() {
    let eng = Engine::new();
    eng.sql("CREATE TEMP TABLE scratch (id INTEGER)", &[])
        .unwrap();
    eng.sql("DISCARD TEMP", &[]).unwrap();
    assert_eq!(
        eng.sql("SELECT * FROM scratch", &[])
            .unwrap_err()
            .sqlstate(),
        Some("42P01")
    );
    eng.sql("BEGIN", &[]).unwrap();
    let error = eng.sql("DISCARD TEMP", &[]).unwrap_err();
    assert_eq!(error.sqlstate(), Some("25001"));
    eng.sql("ROLLBACK", &[]).unwrap();
}

#[test]
fn discard_distinguishes_a_simple_query_batch_from_explicit_begin() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE SEQUENCE discard_batch_sequence; \
             SELECT nextval('discard_batch_sequence'); \
             DISCARD SEQUENCES",
            &[],
        )
        .unwrap();
    assert!(engine.currval("discard_batch_sequence").is_err());

    let error = engine.sql("BEGIN; DISCARD SEQUENCES", &[]).unwrap_err();
    assert_eq!(error.sqlstate(), Some("25001"));
    engine.sql("ROLLBACK", &[]).unwrap();
}
