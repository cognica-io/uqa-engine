//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A session's first temporary object creates `pg_temp_N` and then `pg_toast_temp_N` with the counter's next two OIDs, as `InitTempTableNamespace` does, before the object takes its own. The offsets below are `PostgreSQL` 18.4's from a table created just before.

use uqa_core::Value;
use uqa_engine::Engine;

fn int(engine: &Engine, sql: &str) -> i64 {
    let result = engine.sql(sql, &[]).unwrap();
    assert_eq!(result.rows.len(), 1, "{sql}");
    match result.value_at(0, 0) {
        Some(Value::Int(value)) => *value,
        other => panic!("{sql} returned {other:?}"),
    }
}

fn text(engine: &Engine, sql: &str) -> Option<String> {
    let result = engine.sql(sql, &[]).unwrap();
    assert_eq!(result.rows.len(), 1, "{sql}");
    match result.value_at(0, 0) {
        Some(Value::Str(value)) => Some(value.clone()),
        Some(Value::Bool(value)) => Some(if *value { "t" } else { "f" }.to_string()),
        Some(Value::Null) => None,
        other => panic!("{sql} returned {other:?}"),
    }
}

fn run(engine: &Engine, sql: &str) {
    engine.sql(sql, &[]).unwrap();
}

/// The OID of a table created now, which takes the counter's next OID and then its array and row types.
fn create_marker(engine: &Engine, name: &str) -> i64 {
    run(engine, &format!("CREATE TABLE {name} (a integer)"));
    int(engine, &format!("SELECT '{name}'::regclass::oid::bigint"))
}

fn temporary_namespaces(engine: &Engine) -> Vec<(String, i64, i64, Option<String>)> {
    let result = engine
        .sql(
            "SELECT nspname::text, oid::bigint, nspowner::bigint, nspacl::text FROM pg_namespace \
             WHERE nspname LIKE 'pg_temp_%' OR nspname LIKE 'pg_toast_temp_%' ORDER BY oid",
            &[],
        )
        .unwrap();
    (0..result.rows.len())
        .map(|row| {
            let Some(Value::Str(name)) = result.value_at(row, 0) else {
                panic!("pg_namespace.nspname");
            };
            let (Some(Value::Int(oid)), Some(Value::Int(owner))) =
                (result.value_at(row, 1), result.value_at(row, 2))
            else {
                panic!("pg_namespace.oid and nspowner");
            };
            let acl = match result.value_at(row, 3) {
                Some(Value::Str(acl)) => Some(acl.clone()),
                _ => None,
            };
            (name.clone(), *oid, *owner, acl)
        })
        .collect()
}

#[test]
fn the_first_temporary_object_creates_the_temporary_namespaces_with_the_next_oids() {
    let engine = Engine::new();
    assert_eq!(int(&engine, "SELECT pg_my_temp_schema()::bigint"), 0);
    let marker = create_marker(&engine, "marker");
    run(&engine, "CREATE TEMP TABLE t1 (a integer)");
    let namespaces = temporary_namespaces(&engine);
    assert_eq!(namespaces.len(), 2, "{namespaces:?}");
    let (schema, namespace, owner, acl) = namespaces[0].clone();
    let number = schema.strip_prefix("pg_temp_").unwrap().to_string();
    assert_eq!(namespace, marker + 3);
    assert_eq!((owner, acl), (10, None));
    assert_eq!(
        namespaces[1],
        (format!("pg_toast_temp_{number}"), marker + 4, 10, None)
    );
    assert_eq!(
        int(&engine, "SELECT pg_my_temp_schema()::bigint"),
        namespace
    );
    assert_eq!(
        text(&engine, "SELECT pg_my_temp_schema()::regnamespace::text").as_deref(),
        Some(schema.as_str())
    );
    assert_eq!(
        int(&engine, "SELECT 't1'::regclass::oid::bigint"),
        marker + 5
    );
    assert_eq!(
        int(
            &engine,
            "SELECT relnamespace::bigint FROM pg_class WHERE oid = 't1'::regclass"
        ),
        namespace
    );
    // The array type takes the OID between the relation and its row type.
    assert_eq!(
        int(
            &engine,
            "SELECT reltype::bigint FROM pg_class WHERE oid = 't1'::regclass"
        ),
        marker + 7
    );
    assert_eq!(
        int(
            &engine,
            &format!("SELECT '{schema}'::regnamespace::oid::bigint")
        ),
        namespace
    );
    assert_eq!(
        int(
            &engine,
            &format!("SELECT 'pg_toast_temp_{number}'::regnamespace::oid::bigint")
        ),
        marker + 4
    );
    assert_eq!(
        text(&engine, "SELECT (current_schemas(true))[1]::text").as_deref(),
        Some(schema.as_str())
    );
    // Neither of the session's own namespaces is another session's, and an ordinary schema is no temporary one.
    for oid in [namespace, marker + 4, 11] {
        assert_eq!(
            text(
                &engine,
                &format!("SELECT pg_is_other_temp_schema({oid}::oid)")
            )
            .as_deref(),
            Some("f"),
            "{oid}"
        );
    }
    assert_eq!(text(&engine, "SELECT pg_is_other_temp_schema(NULL)"), None);
    // Later temporary objects find the namespace.
    let second = marker_after(&engine, "second_marker", marker + 8);
    run(&engine, "CREATE TEMP TABLE t2 (a integer)");
    assert_eq!(
        int(&engine, "SELECT 't2'::regclass::oid::bigint"),
        second + 3
    );
    assert_eq!(temporary_namespaces(&engine), namespaces);
}

fn marker_after(engine: &Engine, name: &str, expected: i64) -> i64 {
    let oid = create_marker(engine, name);
    assert_eq!(oid, expected, "{name}");
    oid
}

#[test]
fn a_rollback_past_the_creation_forgets_the_namespaces_and_their_oids_stay_used() {
    let engine = Engine::new();
    let marker = create_marker(&engine, "marker");
    run(&engine, "BEGIN");
    run(&engine, "CREATE TEMP TABLE r1 (a integer)");
    assert_eq!(
        int(&engine, "SELECT pg_my_temp_schema()::bigint"),
        marker + 3
    );
    assert_eq!(
        int(&engine, "SELECT 'r1'::regclass::oid::bigint"),
        marker + 5
    );
    run(&engine, "ROLLBACK");
    assert_eq!(int(&engine, "SELECT pg_my_temp_schema()::bigint"), 0);
    assert_eq!(temporary_namespaces(&engine).len(), 0);
    run(&engine, "CREATE TEMP TABLE r2 (a integer)");
    assert_eq!(
        int(&engine, "SELECT pg_my_temp_schema()::bigint"),
        marker + 8
    );
    assert_eq!(
        int(&engine, "SELECT 'r2'::regclass::oid::bigint"),
        marker + 10
    );

    let engine = Engine::new();
    let marker = create_marker(&engine, "marker");
    run(&engine, "BEGIN");
    run(&engine, "SAVEPOINT before_temporary");
    run(&engine, "CREATE TEMP TABLE s1 (a integer)");
    assert_eq!(
        int(&engine, "SELECT pg_my_temp_schema()::bigint"),
        marker + 3
    );
    run(&engine, "ROLLBACK TO SAVEPOINT before_temporary");
    assert_eq!(int(&engine, "SELECT pg_my_temp_schema()::bigint"), 0);
    run(&engine, "CREATE TEMP TABLE s2 (a integer)");
    run(&engine, "COMMIT");
    assert_eq!(
        int(&engine, "SELECT pg_my_temp_schema()::bigint"),
        marker + 8
    );
    assert_eq!(
        int(&engine, "SELECT 's2'::regclass::oid::bigint"),
        marker + 10
    );
}

#[test]
fn a_failed_temporary_creation_still_uses_the_namespace_oids() {
    let engine = Engine::new();
    let marker = create_marker(&engine, "marker");
    assert!(engine
        .sql("CREATE TEMP TABLE broken (a no_such_type)", &[])
        .is_err());
    assert_eq!(int(&engine, "SELECT pg_my_temp_schema()::bigint"), 0);
    marker_after(&engine, "after_failure", marker + 5);
}

#[test]
fn every_temporary_relation_kind_creates_the_namespaces_first() {
    for statement in [
        "CREATE TEMP TABLE created (a integer)",
        "CREATE TEMP VIEW created AS SELECT 1 AS a",
        "CREATE TEMP SEQUENCE created",
        "CREATE TEMP TABLE created AS SELECT 1 AS a",
        "SELECT 1 AS a INTO TEMP created",
    ] {
        let engine = Engine::new();
        let marker = create_marker(&engine, "marker");
        run(&engine, statement);
        assert_eq!(
            int(&engine, "SELECT pg_my_temp_schema()::bigint"),
            marker + 3,
            "{statement}"
        );
        assert_eq!(
            int(&engine, "SELECT 'created'::regclass::oid::bigint"),
            marker + 5,
            "{statement}"
        );
    }
}

#[test]
fn discarding_temporary_objects_keeps_the_namespaces() {
    let engine = Engine::new();
    run(&engine, "CREATE TEMP TABLE kept (a integer)");
    let namespace = int(&engine, "SELECT pg_my_temp_schema()::bigint");
    let namespaces = temporary_namespaces(&engine);
    run(&engine, "DISCARD TEMP");
    run(&engine, "DISCARD ALL");
    assert_eq!(
        int(&engine, "SELECT pg_my_temp_schema()::bigint"),
        namespace
    );
    assert_eq!(temporary_namespaces(&engine), namespaces);
    let marker = create_marker(&engine, "marker");
    run(&engine, "CREATE TEMP TABLE again (a integer)");
    assert_eq!(
        int(&engine, "SELECT 'again'::regclass::oid::bigint"),
        marker + 3
    );
}

#[test]
fn the_toast_namespace_is_the_bootstrap_superusers_alone() {
    let engine = Engine::new();
    run(&engine, "CREATE TEMP TABLE t (a integer)");
    let schema = text(&engine, "SELECT pg_my_temp_schema()::regnamespace::text").unwrap();
    let toast = schema.replacen("pg_temp_", "pg_toast_temp_", 1);
    run(&engine, "CREATE ROLE temporary_reader");
    run(&engine, "SET ROLE temporary_reader");
    assert_eq!(
        text(
            &engine,
            &format!(
                "SELECT has_schema_privilege('{schema}', 'CREATE') AND has_schema_privilege('{schema}', 'USAGE')"
            )
        )
        .as_deref(),
        Some("t")
    );
    assert_eq!(
        text(
            &engine,
            &format!("SELECT has_schema_privilege('{toast}', 'USAGE')")
        )
        .as_deref(),
        Some("f")
    );
    let visible = engine
        .sql(
            "SELECT schema_name::text FROM information_schema.schemata \
             WHERE schema_name LIKE 'pg_temp_%' OR schema_name LIKE 'pg_toast_temp_%'",
            &[],
        )
        .unwrap();
    assert_eq!(visible.rows.len(), 1);
    assert_eq!(visible.value_at(0, 0), Some(&Value::Str(schema)));
    run(&engine, "RESET ROLE");
    assert_eq!(
        text(
            &engine,
            &format!("SELECT has_schema_privilege('{toast}', 'USAGE')")
        )
        .as_deref(),
        Some("t")
    );
}
