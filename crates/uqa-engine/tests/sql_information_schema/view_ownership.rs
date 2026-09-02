//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn execute(engine: &Engine, sql: &str) {
    engine
        .sql(sql, &[])
        .unwrap_or_else(|error| panic!("{sql}: {error}"));
}

fn sqlstate(engine: &Engine, sql: &str) -> String {
    engine
        .sql(sql, &[])
        .expect_err("statement should fail")
        .sqlstate()
        .expect("failure should expose SQLSTATE")
        .to_string()
}

fn scalar(engine: &Engine, sql: &str) -> Value {
    let result = engine
        .sql(sql, &[])
        .unwrap_or_else(|error| panic!("{sql}: {error}"));
    result.rows[0][&result.columns[0]].clone()
}

fn view_owner(engine: &Engine, view: &str) -> Value {
    scalar(
        engine,
        &format!(
            "SELECT viewowner AS v FROM pg_catalog.pg_views WHERE schemaname = 'view_ownership' AND viewname = '{view}'"
        ),
    )
}

fn materialized_view_owner(engine: &Engine, view: &str) -> Value {
    scalar(
        engine,
        &format!(
            "SELECT matviewowner AS v FROM pg_catalog.pg_matviews WHERE schemaname = 'view_ownership' AND matviewname = '{view}'"
        ),
    )
}

fn setup_view_ownership(engine: &Engine) {
    for sql in [
        "CREATE ROLE view_schema_owner",
        "CREATE ROLE view_role_owner",
        "CREATE ROLE view_role_member INHERIT",
        "CREATE ROLE view_next_owner",
        "CREATE ROLE view_no_create",
        "CREATE ROLE view_no_set",
        "CREATE ROLE view_outsider",
        "GRANT CREATE ON DATABASE uqa TO view_schema_owner",
        "GRANT view_role_owner TO view_role_member",
        "GRANT view_next_owner, view_no_create TO view_role_owner WITH INHERIT FALSE, SET TRUE",
        "GRANT view_no_set TO view_role_owner WITH INHERIT FALSE, SET FALSE",
        "SET ROLE view_schema_owner",
        "CREATE SCHEMA view_ownership",
        "GRANT USAGE, CREATE ON SCHEMA view_ownership TO view_role_owner, view_next_owner, view_no_set",
        "GRANT USAGE ON SCHEMA view_ownership TO view_role_member, view_no_create, view_outsider",
        "RESET ROLE",
        "SET ROLE view_role_owner",
        "CREATE TABLE view_ownership.base(id integer PRIMARY KEY, value integer)",
        "INSERT INTO view_ownership.base VALUES (1, 10)",
        "GRANT SELECT ON TABLE view_ownership.base TO view_next_owner",
        "CREATE VIEW view_ownership.items AS SELECT id, value FROM view_ownership.base",
        "CREATE VIEW view_ownership.items_child AS SELECT id FROM view_ownership.items",
        "CREATE MATERIALIZED VIEW view_ownership.snapshot AS SELECT id, value FROM view_ownership.base",
        "CREATE VIEW view_ownership.schema_drop_view AS SELECT id FROM view_ownership.base",
        "CREATE MATERIALIZED VIEW view_ownership.schema_drop_snapshot AS SELECT id FROM view_ownership.base",
        "RESET ROLE",
    ] {
        execute(engine, sql);
    }
}

fn assert_initial_owner_catalogs(engine: &Engine) {
    assert_eq!(
        view_owner(engine, "items"),
        Value::Str("view_role_owner".into())
    );
    assert_eq!(
        materialized_view_owner(engine, "snapshot"),
        Value::Str("view_role_owner".into())
    );
    assert_eq!(
        scalar(
            engine,
            "SELECT count(*) AS v FROM pg_catalog.pg_class AS c JOIN pg_catalog.pg_roles AS r ON r.oid = c.relowner WHERE c.relnamespace = 'view_ownership'::regnamespace AND r.rolname = 'view_role_owner' AND c.relname IN ('items', 'snapshot')",
        ),
        Value::Int(2)
    );
}

#[test]
fn view_role_owner_controls_alter_drop_refresh_replace_and_catalogs() {
    let engine = Engine::new();
    setup_view_ownership(&engine);
    assert_initial_owner_catalogs(&engine);

    execute(&engine, "SET ROLE view_outsider");
    assert_eq!(
        sqlstate(
            &engine,
            "ALTER VIEW view_ownership.items SET (security_barrier=true)"
        ),
        "42501"
    );
    assert_eq!(sqlstate(&engine, "DROP VIEW view_ownership.items"), "42501");
    assert_eq!(
        sqlstate(&engine, "REFRESH MATERIALIZED VIEW view_ownership.snapshot"),
        "42501"
    );
    assert_eq!(
        sqlstate(
            &engine,
            "CREATE OR REPLACE VIEW view_ownership.items AS SELECT 2 AS id, 20 AS value"
        ),
        "42501"
    );
    execute(&engine, "RESET ROLE");

    execute(&engine, "SET ROLE view_role_member");
    execute(
        &engine,
        "ALTER VIEW view_ownership.items SET (security_barrier=true)",
    );
    execute(&engine, "REFRESH MATERIALIZED VIEW view_ownership.snapshot");
    execute(
        &engine,
        "CREATE OR REPLACE VIEW view_ownership.items AS SELECT id, value FROM view_ownership.base WHERE id > 0",
    );
    execute(&engine, "RESET ROLE");
    assert_eq!(
        view_owner(&engine, "items"),
        Value::Str("view_role_owner".into())
    );

    execute(&engine, "SET ROLE view_schema_owner");
    execute(&engine, "DROP VIEW view_ownership.schema_drop_view");
    execute(
        &engine,
        "DROP MATERIALIZED VIEW view_ownership.schema_drop_snapshot",
    );
    execute(&engine, "RESET ROLE");
}

#[test]
fn view_owner_transfer_validates_role_set_and_schema_create_before_publication() {
    let engine = Engine::new();
    setup_view_ownership(&engine);

    execute(&engine, "SET ROLE view_role_owner");
    assert_eq!(
        sqlstate(
            &engine,
            "ALTER VIEW view_ownership.items OWNER TO missing_view_owner"
        ),
        "42704"
    );
    assert_eq!(
        sqlstate(
            &engine,
            "ALTER VIEW view_ownership.items OWNER TO view_no_create"
        ),
        "42501"
    );
    assert_eq!(
        sqlstate(
            &engine,
            "ALTER VIEW view_ownership.items OWNER TO view_no_set"
        ),
        "42501"
    );
    execute(
        &engine,
        "ALTER VIEW view_ownership.items OWNER TO view_next_owner",
    );
    execute(
        &engine,
        "ALTER MATERIALIZED VIEW view_ownership.snapshot OWNER TO view_next_owner",
    );
    execute(&engine, "RESET ROLE");

    assert_eq!(
        view_owner(&engine, "items"),
        Value::Str("view_next_owner".into())
    );
    assert_eq!(
        materialized_view_owner(&engine, "snapshot"),
        Value::Str("view_next_owner".into())
    );
    assert_eq!(sqlstate(&engine, "DROP ROLE view_next_owner"), "2BP01");

    execute(&engine, "REVOKE view_next_owner FROM view_role_owner");
    execute(&engine, "SET ROLE view_role_owner");
    assert_eq!(
        sqlstate(
            &engine,
            "ALTER VIEW view_ownership.items RESET (security_barrier)"
        ),
        "42501"
    );
    execute(&engine, "RESET ROLE");
    execute(&engine, "SET ROLE view_next_owner");
    execute(
        &engine,
        "ALTER VIEW view_ownership.items RESET (security_barrier)",
    );
    execute(&engine, "RESET ROLE");

    execute(
        &engine,
        "ALTER VIEW view_ownership.items OWNER TO view_no_create",
    );
    execute(
        &engine,
        "ALTER MATERIALIZED VIEW view_ownership.snapshot OWNER TO view_no_create",
    );
    assert_eq!(
        view_owner(&engine, "items"),
        Value::Str("view_no_create".into())
    );
    assert_eq!(
        materialized_view_owner(&engine, "snapshot"),
        Value::Str("view_no_create".into())
    );
}

fn exercise_transactional_view_ownership(engine: &Engine) -> (Value, Value) {
    setup_view_ownership(engine);
    let view_oid = scalar(
        engine,
        "SELECT oid AS v FROM pg_catalog.pg_class WHERE oid = 'view_ownership.items'::regclass",
    );
    let materialized_oid = scalar(
        engine,
        "SELECT oid AS v FROM pg_catalog.pg_class WHERE oid = 'view_ownership.snapshot'::regclass",
    );

    execute(engine, "BEGIN");
    execute(
        engine,
        "ALTER VIEW view_ownership.items OWNER TO view_next_owner",
    );
    execute(
        engine,
        "ALTER MATERIALIZED VIEW view_ownership.snapshot OWNER TO view_next_owner",
    );
    assert_eq!(
        view_owner(engine, "items"),
        Value::Str("view_next_owner".into())
    );
    execute(engine, "ROLLBACK");
    assert_initial_owner_catalogs(engine);

    execute(engine, "BEGIN");
    execute(engine, "SAVEPOINT view_owner_change");
    execute(
        engine,
        "ALTER VIEW view_ownership.items OWNER TO view_next_owner",
    );
    execute(engine, "ROLLBACK TO SAVEPOINT view_owner_change");
    assert_eq!(
        view_owner(engine, "items"),
        Value::Str("view_role_owner".into())
    );
    execute(
        engine,
        "ALTER VIEW view_ownership.items OWNER TO view_next_owner",
    );
    execute(
        engine,
        "ALTER MATERIALIZED VIEW view_ownership.snapshot OWNER TO view_next_owner",
    );
    execute(engine, "COMMIT");

    execute(engine, "SET ROLE view_role_owner");
    execute(
        engine,
        "CREATE TEMP VIEW temporary_owner_view AS SELECT id FROM view_ownership.base",
    );
    execute(engine, "BEGIN");
    execute(
        engine,
        "ALTER VIEW temporary_owner_view OWNER TO view_next_owner",
    );
    execute(engine, "ROLLBACK");
    assert_eq!(
        scalar(
            engine,
            "SELECT viewowner AS v FROM pg_catalog.pg_views WHERE viewname = 'temporary_owner_view'",
        ),
        Value::Str("view_role_owner".into())
    );
    execute(engine, "RESET ROLE");
    (view_oid, materialized_oid)
}

#[test]
fn view_role_owner_follows_transactions_temporary_views_refresh_and_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("view-role-owner.db");
    let (view_oid, materialized_oid) = {
        let engine = Engine::open(&database).unwrap();
        exercise_transactional_view_ownership(&engine)
    };

    let reopened = Engine::open(&database).unwrap();
    assert_eq!(
        view_owner(&reopened, "items"),
        Value::Str("view_next_owner".into())
    );
    assert_eq!(
        materialized_view_owner(&reopened, "snapshot"),
        Value::Str("view_next_owner".into())
    );
    assert_eq!(
        scalar(
            &reopened,
            "SELECT oid AS v FROM pg_catalog.pg_class WHERE oid = 'view_ownership.items'::regclass",
        ),
        view_oid
    );
    assert_eq!(
        scalar(
            &reopened,
            "SELECT oid AS v FROM pg_catalog.pg_class WHERE oid = 'view_ownership.snapshot'::regclass",
        ),
        materialized_oid
    );
    assert_eq!(
        scalar(
            &reopened,
            "SELECT count(*) AS v FROM pg_catalog.pg_views WHERE viewname = 'temporary_owner_view'",
        ),
        Value::Int(0)
    );
    execute(&reopened, "SET ROLE view_next_owner");
    execute(
        &reopened,
        "ALTER VIEW view_ownership.items SET (security_barrier=false)",
    );
    execute(
        &reopened,
        "REFRESH MATERIALIZED VIEW view_ownership.snapshot",
    );
}

#[test]
fn view_role_owner_refreshes_across_persistent_engines() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("view-role-owner-refresh.db");
    let first = Engine::open(&database).unwrap();
    setup_view_ownership(&first);
    let second = Engine::open(&database).unwrap();

    execute(
        &first,
        "ALTER VIEW view_ownership.items OWNER TO view_next_owner",
    );
    execute(
        &first,
        "ALTER MATERIALIZED VIEW view_ownership.snapshot OWNER TO view_next_owner",
    );
    assert_eq!(
        view_owner(&second, "items"),
        Value::Str("view_next_owner".into())
    );
    assert_eq!(
        materialized_view_owner(&second, "snapshot"),
        Value::Str("view_next_owner".into())
    );
    execute(&second, "SET ROLE view_next_owner");
    execute(
        &second,
        "ALTER VIEW view_ownership.items SET (security_barrier=true)",
    );
    assert_eq!(
        scalar(
            &first,
            "SELECT reloptions::text AS v FROM pg_catalog.pg_class WHERE oid = 'view_ownership.items'::regclass",
        ),
        Value::Str("{security_barrier=true}".into())
    );
}

#[test]
fn cascading_table_drop_does_not_require_ownership_of_its_dependent_view() {
    let engine = Engine::new();
    for sql in [
        "CREATE ROLE cascade_schema_owner",
        "CREATE ROLE cascade_table_owner",
        "CREATE ROLE cascade_view_owner",
        "GRANT CREATE ON DATABASE uqa TO cascade_schema_owner",
        "SET ROLE cascade_schema_owner",
        "CREATE SCHEMA cascade_view_ownership",
        "GRANT USAGE, CREATE ON SCHEMA cascade_view_ownership TO cascade_table_owner, cascade_view_owner",
        "RESET ROLE",
        "SET ROLE cascade_table_owner",
        "CREATE TABLE cascade_view_ownership.base(id integer PRIMARY KEY)",
        "GRANT SELECT ON TABLE cascade_view_ownership.base TO cascade_view_owner",
        "RESET ROLE",
        "SET ROLE cascade_view_owner",
        "CREATE VIEW cascade_view_ownership.items AS SELECT id FROM cascade_view_ownership.base",
        "RESET ROLE",
        "SET ROLE cascade_table_owner",
        "DROP TABLE cascade_view_ownership.base CASCADE",
    ] {
        execute(&engine, sql);
    }
    assert_eq!(
        scalar(
            &engine,
            "SELECT count(*) AS v FROM pg_catalog.pg_views WHERE schemaname = 'cascade_view_ownership' AND viewname = 'items'",
        ),
        Value::Int(0)
    );
}
