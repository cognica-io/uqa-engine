//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `SHOW <var>` / `DISCARD ...` compatibility `_compile_show` /
//! `_compile_discard`.

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
fn show_server_version_reports_postgresql_17_compatibility() {
    let eng = Engine::new();
    let r = eng.sql("SHOW server_version", &[]).unwrap();
    assert_eq!(r.columns, vec!["server_version".to_string()]);
    assert_eq!(
        r.rows[0].get("server_version"),
        Some(&Value::Str("17.0-uqa".into()))
    );

    let settings = eng
        .sql(
            "SELECT setting FROM pg_catalog.pg_settings WHERE name = 'server_version'",
            &[],
        )
        .unwrap();
    assert_eq!(
        settings.rows[0].get("setting"),
        Some(&Value::Str("17.0-uqa".into()))
    );
}

#[test]
fn show_builtin_runtime_parameters_are_case_insensitive() {
    let eng = Engine::new();
    assert_eq!(
        eng.show_variable("TimeZone"),
        "UTC",
        "TimeZone should expose PostgreSQL-compatible default"
    );
    eng.sql("SET TimeZone TO 'Asia/Seoul'", &[]).unwrap();
    assert_eq!(
        eng.show_variable("timezone"),
        "Asia/Seoul",
        "session override lookup should be case-insensitive"
    );
}

#[test]
fn show_unknown_variable_returns_empty_string() {
    let eng = Engine::new();
    let r = eng.sql("SHOW some_unknown_var", &[]).unwrap();
    let v = r.rows[0].get("some_unknown_var").unwrap();
    assert_eq!(v, &Value::Str(String::new()));
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
    assert_eq!(v, &Value::Str(String::new()));
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
    eng.discard(DiscardTarget::Plans);
    assert!(eng.lookup_prepared("p1").is_none());
}
