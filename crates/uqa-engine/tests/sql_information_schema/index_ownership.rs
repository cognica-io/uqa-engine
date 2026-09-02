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

fn failure(engine: &Engine, sql: &str) -> (String, String) {
    let error = engine.sql(sql, &[]).expect_err("statement should fail");
    (
        error.sqlstate().unwrap_or_default().to_string(),
        error.to_string(),
    )
}

fn scalar(engine: &Engine, sql: &str) -> Value {
    let result = engine
        .sql(sql, &[])
        .unwrap_or_else(|error| panic!("{sql}: {error}"));
    result.rows[0][&result.columns[0]].clone()
}

fn index_owner(engine: &Engine, index: &str) -> Value {
    scalar(
        engine,
        &format!(
            "SELECT r.rolname AS v FROM pg_catalog.pg_class AS c JOIN pg_catalog.pg_namespace AS n ON n.oid = c.relnamespace JOIN pg_catalog.pg_roles AS r ON r.oid = c.relowner WHERE n.nspname = 'index_ownership' AND c.relname = '{index}'"
        ),
    )
}

fn setup_index_ownership(engine: &Engine) {
    for sql in [
        "CREATE ROLE index_schema_owner",
        "CREATE ROLE index_table_owner",
        "CREATE ROLE index_owner_member INHERIT",
        "CREATE ROLE index_creator",
        "CREATE ROLE index_outsider",
        "CREATE ROLE index_next_owner",
        "GRANT index_table_owner TO index_owner_member",
        "GRANT CREATE ON DATABASE uqa TO index_schema_owner",
        "SET ROLE index_schema_owner",
        "CREATE SCHEMA index_ownership",
        "GRANT USAGE, CREATE ON SCHEMA index_ownership TO index_table_owner, index_creator, index_next_owner",
        "GRANT USAGE ON SCHEMA index_ownership TO index_owner_member, index_outsider",
        "RESET ROLE",
        "SET ROLE index_table_owner",
        "CREATE TABLE index_ownership.items(id integer, value integer)",
        "CREATE TABLE index_ownership.transfer_items(id integer)",
        "CREATE INDEX existing_idx ON index_ownership.items(id)",
        "CREATE INDEX owner_drop_idx ON index_ownership.items(id)",
        "CREATE INDEX member_drop_idx ON index_ownership.items(id)",
        "CREATE INDEX schema_drop_idx ON index_ownership.items(id)",
        "CREATE INDEX outsider_drop_idx ON index_ownership.items(id)",
        "CREATE INDEX multi_owner_idx ON index_ownership.items(id)",
        "CREATE INDEX transfer_idx ON index_ownership.transfer_items(id)",
        "RESET ROLE",
        "SET ROLE index_creator",
        "CREATE TABLE index_ownership.creator_items(id integer)",
        "CREATE INDEX multi_creator_idx ON index_ownership.creator_items(id)",
        "RESET ROLE",
    ] {
        execute(engine, sql);
    }
}

#[test]
fn index_creation_checks_table_owner_before_schema_create_and_definition() {
    let engine = Engine::new();
    setup_index_ownership(&engine);

    execute(&engine, "SET ROLE index_creator");
    for sql in [
        "CREATE INDEX denied_idx ON index_ownership.items(value)",
        "CREATE INDEX denied_method_idx ON index_ownership.items USING missing_method(value)",
        "CREATE INDEX IF NOT EXISTS existing_idx ON index_ownership.items(id)",
    ] {
        let (state, message) = failure(&engine, sql);
        assert_eq!(state, "42501", "{sql}: {message}");
        assert_eq!(message, "must be owner of table items", "{sql}");
    }
    execute(&engine, "RESET ROLE");

    execute(&engine, "SET ROLE index_schema_owner");
    assert_eq!(
        failure(
            &engine,
            "CREATE INDEX schema_owner_idx ON index_ownership.items(value)"
        ),
        ("42501".into(), "must be owner of table items".into())
    );
    execute(&engine, "RESET ROLE");

    execute(&engine, "SET ROLE index_owner_member");
    execute(
        &engine,
        "CREATE INDEX member_create_idx ON index_ownership.items(value)",
    );
    assert_eq!(
        index_owner(&engine, "member_create_idx"),
        Value::Str("index_table_owner".into())
    );
    execute(&engine, "RESET ROLE");

    execute(
        &engine,
        "REVOKE CREATE ON SCHEMA index_ownership FROM index_table_owner",
    );
    execute(&engine, "SET ROLE index_table_owner");
    assert_eq!(
        failure(
            &engine,
            "CREATE INDEX no_create_idx ON index_ownership.items(value)"
        ),
        (
            "42501".into(),
            "permission denied for schema index_ownership".into()
        )
    );
    execute(&engine, "RESET ROLE");
    execute(
        &engine,
        "GRANT CREATE ON SCHEMA index_ownership TO index_table_owner",
    );
    execute(
        &engine,
        "REVOKE USAGE ON SCHEMA index_ownership FROM index_table_owner",
    );
    execute(&engine, "SET ROLE index_table_owner");
    assert_eq!(
        failure(
            &engine,
            "CREATE INDEX no_usage_idx ON index_ownership.items(value)"
        ),
        (
            "42501".into(),
            "permission denied for schema index_ownership".into()
        )
    );
    execute(&engine, "RESET ROLE");
    execute(
        &engine,
        "GRANT USAGE ON SCHEMA index_ownership TO index_table_owner",
    );

    execute(
        &engine,
        "REVOKE CREATE ON SCHEMA index_ownership FROM index_creator",
    );
    execute(&engine, "SET ROLE index_creator");
    assert_eq!(
        failure(
            &engine,
            "CREATE INDEX still_not_owner_idx ON index_ownership.items USING missing_method(value)"
        ),
        ("42501".into(), "must be owner of table items".into())
    );
    assert_eq!(
        failure(
            &engine,
            "CREATE INDEX missing_table_idx ON index_ownership.missing(id)"
        )
        .0,
        "42P01"
    );
    execute(&engine, "RESET ROLE");

    assert_eq!(
        scalar(
            &engine,
            "SELECT count(*) AS v FROM pg_catalog.pg_indexes WHERE schemaname = 'index_ownership' AND indexname IN ('denied_idx', 'denied_method_idx', 'schema_owner_idx', 'no_create_idx', 'no_usage_idx', 'still_not_owner_idx', 'missing_table_idx')",
        ),
        Value::Int(0)
    );
}

#[test]
fn index_drop_uses_table_or_schema_owner_and_preflights_every_target() {
    let engine = Engine::new();
    setup_index_ownership(&engine);

    execute(&engine, "SET ROLE index_outsider");
    assert_eq!(
        failure(&engine, "DROP INDEX index_ownership.outsider_drop_idx"),
        (
            "42501".into(),
            "must be owner of index outsider_drop_idx".into()
        )
    );
    execute(&engine, "RESET ROLE");

    execute(&engine, "SET ROLE index_creator");
    assert_eq!(
        failure(&engine, "DROP INDEX index_ownership.owner_drop_idx"),
        (
            "42501".into(),
            "must be owner of index owner_drop_idx".into()
        )
    );
    execute(&engine, "RESET ROLE");

    execute(&engine, "SET ROLE index_owner_member");
    execute(&engine, "DROP INDEX index_ownership.member_drop_idx");
    execute(&engine, "RESET ROLE");

    execute(&engine, "SET ROLE index_schema_owner");
    execute(&engine, "DROP INDEX index_ownership.schema_drop_idx");
    execute(&engine, "RESET ROLE");

    execute(&engine, "SET ROLE index_table_owner");
    assert_eq!(
        failure(
            &engine,
            "DROP INDEX index_ownership.multi_owner_idx, index_ownership.multi_creator_idx"
        ),
        (
            "42501".into(),
            "must be owner of index multi_creator_idx".into()
        )
    );
    assert_eq!(
        failure(
            &engine,
            "DROP INDEX IF EXISTS index_ownership.multi_creator_idx"
        ),
        (
            "42501".into(),
            "must be owner of index multi_creator_idx".into()
        )
    );
    execute(&engine, "RESET ROLE");
    assert_eq!(
        scalar(
            &engine,
            "SELECT count(*) AS v FROM pg_catalog.pg_indexes WHERE schemaname = 'index_ownership' AND indexname IN ('multi_owner_idx', 'multi_creator_idx')",
        ),
        Value::Int(2)
    );

    execute(&engine, "SET ROLE index_schema_owner");
    execute(
        &engine,
        "DROP INDEX index_ownership.multi_owner_idx, index_ownership.multi_creator_idx",
    );
    execute(&engine, "RESET ROLE");
    assert_eq!(
        scalar(
            &engine,
            "SELECT count(*) AS v FROM pg_catalog.pg_indexes WHERE schemaname = 'index_ownership' AND indexname IN ('multi_owner_idx', 'multi_creator_idx')",
        ),
        Value::Int(0)
    );
}

#[test]
fn index_owner_follows_table_transfer_transactions_refresh_and_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("index-ownership.db");
    let index_oid;
    {
        let first = Engine::open(&database).unwrap();
        setup_index_ownership(&first);
        let second = Engine::open(&database).unwrap();
        index_oid = scalar(
            &first,
            "SELECT oid AS v FROM pg_catalog.pg_class WHERE relname = 'transfer_idx'",
        );
        execute(
            &first,
            "GRANT index_next_owner TO index_table_owner WITH INHERIT FALSE, SET TRUE",
        );
        execute(&first, "SET ROLE index_table_owner");
        execute(&first, "BEGIN");
        execute(
            &first,
            "ALTER TABLE index_ownership.transfer_items OWNER TO index_next_owner",
        );
        assert_eq!(
            index_owner(&first, "transfer_idx"),
            Value::Str("index_next_owner".into())
        );
        execute(&first, "ROLLBACK");
        assert_eq!(
            index_owner(&first, "transfer_idx"),
            Value::Str("index_table_owner".into())
        );
        execute(
            &first,
            "ALTER TABLE index_ownership.transfer_items OWNER TO index_next_owner",
        );
        execute(&first, "RESET ROLE");
        assert_eq!(
            index_owner(&second, "transfer_idx"),
            Value::Str("index_next_owner".into())
        );
        execute(&first, "REVOKE index_next_owner FROM index_table_owner");
        execute(&first, "SET ROLE index_table_owner");
        assert_eq!(
            failure(&first, "DROP INDEX index_ownership.transfer_idx"),
            ("42501".into(), "must be owner of index transfer_idx".into())
        );
        execute(&first, "RESET ROLE");
    }

    let reopened = Engine::open(&database).unwrap();
    assert_eq!(
        index_owner(&reopened, "transfer_idx"),
        Value::Str("index_next_owner".into())
    );
    assert_eq!(
        scalar(
            &reopened,
            "SELECT oid AS v FROM pg_catalog.pg_class WHERE relname = 'transfer_idx'",
        ),
        index_oid
    );
    execute(&reopened, "SET ROLE index_next_owner");
    execute(&reopened, "DROP INDEX index_ownership.transfer_idx");
}
