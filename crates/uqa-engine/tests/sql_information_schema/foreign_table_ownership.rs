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

fn foreign_table_owner(engine: &Engine, table: &str) -> Value {
    scalar(
        engine,
        &format!(
            "SELECT r.rolname AS v FROM pg_catalog.pg_class AS c JOIN pg_catalog.pg_roles AS r ON r.oid = c.relowner WHERE c.relnamespace = 'foreign_ownership'::regnamespace AND c.relname = '{table}'"
        ),
    )
}

fn setup_foreign_table_ownership(engine: &Engine) {
    for sql in [
        "CREATE ROLE foreign_schema_owner",
        "CREATE ROLE foreign_role_owner",
        "CREATE ROLE foreign_role_member INHERIT",
        "CREATE ROLE foreign_next_owner",
        "CREATE ROLE foreign_no_create",
        "CREATE ROLE foreign_no_set",
        "CREATE ROLE foreign_outsider",
        "GRANT CREATE ON DATABASE uqa TO foreign_schema_owner",
        "GRANT foreign_role_owner TO foreign_role_member",
        "GRANT foreign_next_owner, foreign_no_create TO foreign_role_owner WITH INHERIT FALSE, SET TRUE",
        "GRANT foreign_no_set TO foreign_role_owner WITH INHERIT FALSE, SET FALSE",
        "SET ROLE foreign_schema_owner",
        "CREATE SCHEMA foreign_ownership",
        "GRANT USAGE, CREATE ON SCHEMA foreign_ownership TO foreign_role_owner, foreign_next_owner, foreign_no_set",
        "GRANT USAGE ON SCHEMA foreign_ownership TO foreign_role_member, foreign_no_create, foreign_outsider",
        "RESET ROLE",
        "CREATE SERVER foreign_memory FOREIGN DATA WRAPPER memory_fdw OPTIONS (kind 'memory')",
        "SET ROLE foreign_role_owner",
        "CREATE FOREIGN TABLE foreign_ownership.items(id integer) SERVER foreign_memory",
        "CREATE FOREIGN TABLE foreign_ownership.schema_drop(id integer) SERVER foreign_memory",
        "CREATE FOREIGN TABLE foreign_ownership.spelling(id integer) SERVER foreign_memory",
        "CREATE FOREIGN TABLE foreign_ownership.dependency(id integer) SERVER foreign_memory",
        "CREATE FOREIGN TABLE foreign_ownership.drop_first(id integer) SERVER foreign_memory",
        "CREATE TABLE foreign_ownership.local_table(id integer PRIMARY KEY)",
        "CREATE VIEW foreign_ownership.dependent_view AS SELECT id FROM foreign_ownership.dependency",
        "CREATE VIEW foreign_ownership.dependent_view_child AS SELECT id FROM foreign_ownership.dependent_view",
        "RESET ROLE",
    ] {
        execute(engine, sql);
    }
}

#[test]
fn foreign_table_owner_controls_alter_drop_and_catalogs() {
    let engine = Engine::new();
    setup_foreign_table_ownership(&engine);
    assert_eq!(
        foreign_table_owner(&engine, "items"),
        Value::Str("foreign_role_owner".into())
    );

    execute(&engine, "SET ROLE foreign_outsider");
    assert_eq!(
        sqlstate(
            &engine,
            "ALTER FOREIGN TABLE foreign_ownership.items OWNER TO foreign_outsider"
        ),
        "42501"
    );
    assert_eq!(
        sqlstate(&engine, "DROP FOREIGN TABLE foreign_ownership.items"),
        "42501"
    );
    execute(&engine, "RESET ROLE");

    execute(&engine, "SET ROLE foreign_role_member");
    execute(
        &engine,
        "ALTER FOREIGN TABLE foreign_ownership.items OWNER TO foreign_role_owner",
    );
    execute(&engine, "RESET ROLE");

    execute(&engine, "SET ROLE foreign_schema_owner");
    execute(&engine, "DROP FOREIGN TABLE foreign_ownership.schema_drop");
    execute(&engine, "RESET ROLE");

    execute(&engine, "BEGIN READ ONLY");
    assert_eq!(
        sqlstate(
            &engine,
            "ALTER FOREIGN TABLE foreign_ownership.items OWNER TO foreign_role_owner"
        ),
        "25006"
    );
    execute(&engine, "ROLLBACK");
}

#[test]
fn foreign_table_owner_transfer_validates_role_set_and_schema_create() {
    let engine = Engine::new();
    setup_foreign_table_ownership(&engine);

    execute(&engine, "SET ROLE foreign_role_owner");
    assert_eq!(
        sqlstate(
            &engine,
            "ALTER FOREIGN TABLE foreign_ownership.items OWNER TO foreign_missing_owner"
        ),
        "42704"
    );
    assert_eq!(
        sqlstate(
            &engine,
            "ALTER FOREIGN TABLE foreign_ownership.items OWNER TO foreign_no_create"
        ),
        "42501"
    );
    assert_eq!(
        sqlstate(
            &engine,
            "ALTER FOREIGN TABLE foreign_ownership.items OWNER TO foreign_no_set"
        ),
        "42501"
    );
    execute(
        &engine,
        "ALTER FOREIGN TABLE foreign_ownership.items OWNER TO foreign_next_owner",
    );
    execute(&engine, "RESET ROLE");
    assert_eq!(
        foreign_table_owner(&engine, "items"),
        Value::Str("foreign_next_owner".into())
    );
    assert_eq!(sqlstate(&engine, "DROP ROLE foreign_next_owner"), "2BP01");

    execute(&engine, "SET ROLE foreign_role_owner");
    assert_eq!(
        sqlstate(
            &engine,
            "ALTER FOREIGN TABLE foreign_ownership.items OWNER TO foreign_role_owner"
        ),
        "42501"
    );
    execute(&engine, "RESET ROLE");
    execute(&engine, "SET ROLE foreign_next_owner");
    execute(
        &engine,
        "ALTER FOREIGN TABLE foreign_ownership.items OWNER TO foreign_next_owner",
    );
    execute(&engine, "RESET ROLE");

    execute(
        &engine,
        "ALTER FOREIGN TABLE foreign_ownership.items OWNER TO foreign_no_create",
    );
    assert_eq!(
        foreign_table_owner(&engine, "items"),
        Value::Str("foreign_no_create".into())
    );
}

#[test]
fn foreign_table_owner_follows_transactions_reopen_and_catalog_refresh() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("foreign-table-owner.db");
    let first = Engine::open(&database).unwrap();
    setup_foreign_table_ownership(&first);
    let second = Engine::open(&database).unwrap();

    execute(&first, "SET ROLE foreign_role_owner");
    execute(&first, "BEGIN");
    execute(
        &first,
        "ALTER FOREIGN TABLE foreign_ownership.items OWNER TO foreign_next_owner",
    );
    assert_eq!(
        foreign_table_owner(&first, "items"),
        Value::Str("foreign_next_owner".into())
    );
    execute(&first, "ROLLBACK");
    assert_eq!(
        foreign_table_owner(&first, "items"),
        Value::Str("foreign_role_owner".into())
    );
    execute(&first, "BEGIN");
    execute(&first, "SAVEPOINT foreign_owner_change");
    execute(
        &first,
        "ALTER FOREIGN TABLE foreign_ownership.items OWNER TO foreign_next_owner",
    );
    execute(&first, "ROLLBACK TO SAVEPOINT foreign_owner_change");
    assert_eq!(
        foreign_table_owner(&first, "items"),
        Value::Str("foreign_role_owner".into())
    );
    execute(
        &first,
        "ALTER FOREIGN TABLE foreign_ownership.items OWNER TO foreign_next_owner",
    );
    execute(&first, "COMMIT");
    execute(&first, "RESET ROLE");
    assert_eq!(
        foreign_table_owner(&second, "items"),
        Value::Str("foreign_next_owner".into())
    );
    drop(second);
    drop(first);

    let reopened = Engine::open(&database).unwrap();
    assert_eq!(
        foreign_table_owner(&reopened, "items"),
        Value::Str("foreign_next_owner".into())
    );
    execute(&reopened, "SET ROLE foreign_next_owner");
    execute(
        &reopened,
        "ALTER TABLE foreign_ownership.items OWNER TO foreign_next_owner",
    );
}

#[test]
fn foreign_table_drop_preflights_all_targets_and_cascades_views() {
    let engine = Engine::new();
    setup_foreign_table_ownership(&engine);

    execute(&engine, "SET ROLE foreign_role_owner");
    assert_eq!(
        sqlstate(&engine, "DROP FOREIGN TABLE foreign_ownership.dependency"),
        "2BP01"
    );
    execute(
        &engine,
        "DROP FOREIGN TABLE foreign_ownership.dependency CASCADE",
    );
    assert_eq!(
        scalar(
            &engine,
            "SELECT count(*) AS v FROM pg_catalog.pg_class WHERE relnamespace = 'foreign_ownership'::regnamespace AND relname IN ('dependency', 'dependent_view', 'dependent_view_child')",
        ),
        Value::Int(0)
    );
    assert_eq!(
        sqlstate(
            &engine,
            "DROP FOREIGN TABLE foreign_ownership.drop_first, foreign_ownership.local_table"
        ),
        "42809"
    );
    assert_eq!(
        scalar(
            &engine,
            "SELECT count(*) AS v FROM pg_catalog.pg_class WHERE oid = 'foreign_ownership.drop_first'::regclass",
        ),
        Value::Int(1)
    );
    execute(
        &engine,
        "DROP FOREIGN TABLE IF EXISTS foreign_ownership.missing",
    );
    execute(&engine, "RESET ROLE");
}

#[test]
fn engine_open_rejects_foreign_table_with_missing_owner_role() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("invalid-foreign-table-owner.db");
    {
        let engine = Engine::open(&database).unwrap();
        setup_foreign_table_ownership(&engine);
    }
    let connection = rusqlite::Connection::open(&database).unwrap();
    connection
        .execute(
            "UPDATE _foreign_tables SET role_owner = 'missing_foreign_owner' WHERE relation_name = 'items'",
            [],
        )
        .unwrap();
    drop(connection);

    let Err(error) = Engine::open(&database) else {
        panic!("invalid owner must reject catalog open");
    };
    assert!(error.to_string().contains("missing owner role"), "{error}");
}
