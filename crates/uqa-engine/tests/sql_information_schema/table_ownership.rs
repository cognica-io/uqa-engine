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

fn table_owner(engine: &Engine, table: &str) -> Value {
    scalar(
        engine,
        &format!(
            "SELECT tableowner AS v FROM pg_catalog.pg_tables WHERE schemaname = 'table_ownership' AND tablename = '{table}'"
        ),
    )
}

fn sequence_owner(engine: &Engine, sequence: &str) -> Value {
    scalar(
        engine,
        &format!(
            "SELECT sequenceowner AS v FROM pg_catalog.pg_sequences WHERE schemaname = 'table_ownership' AND sequencename = '{sequence}'"
        ),
    )
}

fn setup_table_ownership(engine: &Engine) {
    for sql in [
        "CREATE ROLE table_schema_owner",
        "CREATE ROLE table_role_owner",
        "CREATE ROLE table_role_member INHERIT",
        "CREATE ROLE table_next_owner",
        "CREATE ROLE table_no_create",
        "CREATE ROLE table_no_set",
        "CREATE ROLE table_outsider",
        "GRANT CREATE ON DATABASE uqa TO table_schema_owner",
        "GRANT table_role_owner TO table_role_member",
        "GRANT table_next_owner, table_no_create TO table_role_owner WITH INHERIT FALSE, SET TRUE",
        "GRANT table_no_set TO table_role_owner WITH INHERIT FALSE, SET FALSE",
        "SET ROLE table_schema_owner",
        "CREATE SCHEMA table_ownership",
        "GRANT USAGE, CREATE ON SCHEMA table_ownership TO table_role_owner, table_next_owner, table_no_set",
        "GRANT USAGE ON SCHEMA table_ownership TO table_role_member, table_no_create, table_outsider",
        "RESET ROLE",
        "SET ROLE table_role_owner",
        "CREATE TABLE table_ownership.items(id integer PRIMARY KEY, serial_id serial)",
        "CREATE INDEX items_id_idx ON table_ownership.items(id)",
        "CREATE SEQUENCE table_ownership.independent_ids",
        "CREATE TABLE table_ownership.schema_drop_items(id integer)",
        "RESET ROLE",
    ] {
        execute(engine, sql);
    }
}

fn assert_initial_table_owner_catalogs(engine: &Engine) {
    assert_eq!(
        table_owner(engine, "items"),
        Value::Str("table_role_owner".into())
    );
    assert_eq!(
        sequence_owner(engine, "items_serial_id_seq"),
        Value::Str("table_role_owner".into())
    );
    assert_eq!(
        scalar(
            engine,
            "SELECT count(*) AS v FROM pg_catalog.pg_class AS c JOIN pg_catalog.pg_roles AS r ON r.oid = c.relowner WHERE r.rolname = 'table_role_owner' AND c.relname IN ('items', 'items_id_idx')",
        ),
        Value::Int(2)
    );
}

fn assert_schema_owner_and_superuser_authority(engine: &Engine) {
    execute(engine, "SET ROLE table_schema_owner");
    execute(engine, "DROP TABLE table_ownership.schema_drop_items");
    execute(engine, "RESET ROLE");

    execute(
        engine,
        "ALTER TABLE table_ownership.items OWNER TO table_no_create",
    );
    assert_eq!(
        table_owner(engine, "items"),
        Value::Str("table_no_create".into())
    );
    assert_eq!(
        sequence_owner(engine, "items_serial_id_seq"),
        Value::Str("table_no_create".into())
    );
    execute(
        engine,
        "ALTER SEQUENCE table_ownership.independent_ids OWNER TO table_no_create",
    );
    assert_eq!(
        sequence_owner(engine, "independent_ids"),
        Value::Str("table_no_create".into())
    );
}

#[test]
fn table_role_owner_controls_alter_drop_catalogs_and_owned_sequences() {
    let engine = Engine::new();
    setup_table_ownership(&engine);
    assert_initial_table_owner_catalogs(&engine);

    execute(&engine, "SET ROLE table_outsider");
    assert_eq!(
        sqlstate(
            &engine,
            "ALTER TABLE table_ownership.items ADD COLUMN rejected integer"
        ),
        "42501"
    );
    assert_eq!(
        sqlstate(&engine, "DROP TABLE table_ownership.items"),
        "42501"
    );
    execute(&engine, "RESET ROLE");

    execute(&engine, "SET ROLE table_role_member");
    execute(
        &engine,
        "ALTER TABLE table_ownership.items ADD COLUMN member_value integer",
    );
    execute(&engine, "RESET ROLE");

    execute(&engine, "SET ROLE table_role_owner");
    assert_eq!(
        sqlstate(
            &engine,
            "ALTER TABLE table_ownership.items OWNER TO missing_table_owner"
        ),
        "42704"
    );
    assert_eq!(
        sqlstate(
            &engine,
            "ALTER TABLE table_ownership.items OWNER TO table_no_create"
        ),
        "42501"
    );
    assert_eq!(
        sqlstate(
            &engine,
            "ALTER TABLE table_ownership.items OWNER TO table_no_set"
        ),
        "42501"
    );
    execute(
        &engine,
        "ALTER TABLE table_ownership.items OWNER TO table_next_owner",
    );
    execute(&engine, "RESET ROLE");

    assert_eq!(
        table_owner(&engine, "items"),
        Value::Str("table_next_owner".into())
    );
    assert_eq!(
        sequence_owner(&engine, "items_serial_id_seq"),
        Value::Str("table_next_owner".into())
    );
    assert_eq!(
        scalar(
            &engine,
            "SELECT count(*) AS v FROM pg_catalog.pg_class AS c JOIN pg_catalog.pg_roles AS r ON r.oid = c.relowner WHERE r.rolname = 'table_next_owner' AND c.relname IN ('items', 'items_id_idx')",
        ),
        Value::Int(2)
    );
    assert_eq!(sqlstate(&engine, "DROP ROLE table_next_owner"), "2BP01");

    execute(&engine, "REVOKE table_next_owner FROM table_role_owner");
    execute(&engine, "SET ROLE table_role_owner");
    assert_eq!(
        sqlstate(
            &engine,
            "ALTER TABLE table_ownership.items ADD COLUMN former_value integer"
        ),
        "42501"
    );
    execute(&engine, "RESET ROLE");
    execute(&engine, "SET ROLE table_next_owner");
    execute(
        &engine,
        "ALTER TABLE table_ownership.items ADD COLUMN target_value integer",
    );
    execute(&engine, "RESET ROLE");

    assert_schema_owner_and_superuser_authority(&engine);
}

fn exercise_transactional_table_ownership(engine: &Engine) -> Value {
    setup_table_ownership(engine);
    let table_oid = scalar(
        engine,
        "SELECT oid AS v FROM pg_catalog.pg_class WHERE relname = 'items'",
    );

    execute(engine, "BEGIN");
    execute(
        engine,
        "ALTER TABLE table_ownership.items OWNER TO table_next_owner",
    );
    assert_eq!(
        table_owner(engine, "items"),
        Value::Str("table_next_owner".into())
    );
    assert_eq!(
        sequence_owner(engine, "items_serial_id_seq"),
        Value::Str("table_next_owner".into())
    );
    execute(engine, "ROLLBACK");
    assert_eq!(
        table_owner(engine, "items"),
        Value::Str("table_role_owner".into())
    );
    assert_eq!(
        sequence_owner(engine, "items_serial_id_seq"),
        Value::Str("table_role_owner".into())
    );

    execute(engine, "BEGIN");
    execute(engine, "SAVEPOINT table_owner_change");
    execute(
        engine,
        "ALTER TABLE table_ownership.items OWNER TO table_next_owner",
    );
    execute(engine, "ROLLBACK TO SAVEPOINT table_owner_change");
    assert_eq!(
        table_owner(engine, "items"),
        Value::Str("table_role_owner".into())
    );
    execute(
        engine,
        "ALTER TABLE table_ownership.items OWNER TO table_next_owner",
    );
    execute(engine, "COMMIT");

    execute(engine, "SET ROLE table_role_owner");
    execute(engine, "CREATE TEMP TABLE temporary_owner_items(id serial)");
    execute(engine, "BEGIN");
    execute(
        engine,
        "ALTER TABLE temporary_owner_items OWNER TO table_next_owner",
    );
    execute(engine, "ROLLBACK");
    assert_eq!(
        scalar(
            engine,
            "SELECT tableowner AS v FROM pg_catalog.pg_tables WHERE tablename = 'temporary_owner_items'",
        ),
        Value::Str("table_role_owner".into())
    );
    assert_eq!(
        scalar(
            engine,
            "SELECT sequenceowner AS v FROM pg_catalog.pg_sequences WHERE sequencename = 'temporary_owner_items_id_seq'",
        ),
        Value::Str("table_role_owner".into())
    );
    execute(engine, "RESET ROLE");
    table_oid
}

#[test]
fn table_role_owner_follows_transactions_savepoints_temporary_tables_and_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("table-role-owner.db");
    let table_oid = {
        let engine = Engine::open(&database).unwrap();
        exercise_transactional_table_ownership(&engine)
    };

    let reopened = Engine::open(&database).unwrap();
    assert_eq!(
        table_owner(&reopened, "items"),
        Value::Str("table_next_owner".into())
    );
    assert_eq!(
        sequence_owner(&reopened, "items_serial_id_seq"),
        Value::Str("table_next_owner".into())
    );
    assert_eq!(
        scalar(
            &reopened,
            "SELECT oid AS v FROM pg_catalog.pg_class WHERE relname = 'items'",
        ),
        table_oid
    );
    assert_eq!(
        scalar(
            &reopened,
            "SELECT count(*) AS v FROM pg_catalog.pg_tables WHERE tablename = 'temporary_owner_items'",
        ),
        Value::Int(0)
    );
    execute(&reopened, "SET ROLE table_next_owner");
    execute(
        &reopened,
        "ALTER TABLE table_ownership.items ADD COLUMN reopened_value integer",
    );
}

#[test]
fn table_role_owner_refreshes_across_persistent_engines() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("table-role-owner-refresh.db");
    let first = Engine::open(&database).unwrap();
    setup_table_ownership(&first);
    let second = Engine::open(&database).unwrap();

    execute(
        &first,
        "ALTER TABLE table_ownership.items OWNER TO table_next_owner",
    );
    assert_eq!(
        table_owner(&second, "items"),
        Value::Str("table_next_owner".into())
    );
    assert_eq!(
        sequence_owner(&second, "items_serial_id_seq"),
        Value::Str("table_next_owner".into())
    );
    execute(&second, "SET ROLE table_next_owner");
    execute(
        &second,
        "ALTER TABLE table_ownership.items ADD COLUMN sibling_value integer",
    );
    assert_eq!(
        scalar(
            &first,
            "SELECT count(*) AS v FROM information_schema.columns WHERE table_schema = 'table_ownership' AND table_name = 'items' AND column_name = 'sibling_value'",
        ),
        Value::Int(1)
    );
}
