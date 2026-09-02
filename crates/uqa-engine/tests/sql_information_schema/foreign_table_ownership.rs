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

fn setup_foreign_table_acl(engine: &Engine) {
    for sql in [
        "CREATE ROLE foreign_acl_next_owner",
        "CREATE ROLE foreign_acl_owner",
        "CREATE ROLE foreign_acl_delegate",
        "CREATE ROLE foreign_acl_reader",
        "CREATE ROLE foreign_acl_column_reader",
        "CREATE ROLE foreign_acl_outsider",
        "GRANT CREATE ON DATABASE uqa TO foreign_acl_owner",
        "GRANT foreign_acl_next_owner TO foreign_acl_owner WITH INHERIT FALSE, SET TRUE",
        "SET ROLE foreign_acl_owner",
        "CREATE SCHEMA foreign_acl",
        "GRANT USAGE ON SCHEMA foreign_acl TO foreign_acl_next_owner, foreign_acl_delegate, foreign_acl_reader, foreign_acl_column_reader, foreign_acl_outsider",
        "GRANT CREATE ON SCHEMA foreign_acl TO foreign_acl_next_owner",
        "RESET ROLE",
        "CREATE SERVER foreign_acl_memory FOREIGN DATA WRAPPER memory_fdw OPTIONS (kind 'memory')",
        "SET ROLE foreign_acl_owner",
        "CREATE FOREIGN TABLE foreign_acl.items(id integer, value text) SERVER foreign_acl_memory",
        "CREATE FOREIGN TABLE foreign_acl.all_items(id integer, value text) SERVER foreign_acl_memory",
        "CREATE FOREIGN TABLE foreign_acl.cascade_items(id integer, value text) SERVER foreign_acl_memory",
        "CREATE VIEW foreign_acl.definer_items AS SELECT id, value FROM foreign_acl.items",
        "CREATE VIEW foreign_acl.invoker_items WITH (security_invoker=true) AS SELECT id, value FROM foreign_acl.items",
        "RESET ROLE",
    ] {
        execute(engine, sql);
    }
    let rows = vec![
        std::collections::BTreeMap::from([
            ("id".into(), Value::Int(1)),
            ("value".into(), Value::Str("one".into())),
        ]),
        std::collections::BTreeMap::from([
            ("id".into(), Value::Int(2)),
            ("value".into(), Value::Str("two".into())),
        ]),
    ];
    for table in ["items", "all_items", "cascade_items"] {
        engine
            .load_memory_foreign_table(format!("foreign_acl.{table}"), rows.clone())
            .unwrap();
    }
}

fn grant_foreign_table_acl_fixture(engine: &Engine) {
    execute(engine, "SET ROLE foreign_acl_owner");
    execute(
        engine,
        "GRANT SELECT ON TABLE foreign_acl.items TO foreign_acl_delegate WITH GRANT OPTION",
    );
    execute(
        engine,
        "GRANT SELECT(id) ON TABLE foreign_acl.items TO foreign_acl_column_reader",
    );
    execute(
        engine,
        "GRANT ALL PRIVILEGES ON TABLE foreign_acl.all_items TO foreign_acl_outsider",
    );
    execute(
        engine,
        "GRANT INSERT(id), UPDATE(id), REFERENCES(id) ON TABLE foreign_acl.all_items TO foreign_acl_column_reader",
    );
    execute(
        engine,
        "GRANT SELECT ON TABLE foreign_acl.definer_items, foreign_acl.invoker_items TO foreign_acl_outsider",
    );
    execute(
        engine,
        "GRANT SELECT ON TABLE foreign_acl.cascade_items TO foreign_acl_delegate WITH GRANT OPTION",
    );
    execute(engine, "RESET ROLE");
    execute(engine, "SET ROLE foreign_acl_delegate");
    execute(
        engine,
        "GRANT SELECT ON TABLE foreign_acl.items, foreign_acl.cascade_items TO foreign_acl_reader",
    );
    execute(engine, "RESET ROLE");
}

fn assert_foreign_table_acl_defaults(engine: &Engine) {
    assert_eq!(
        scalar(
            engine,
            "SELECT relacl IS NULL FROM pg_catalog.pg_class WHERE oid = 'foreign_acl.items'::regclass",
        ),
        Value::Bool(true)
    );
    assert_eq!(
        scalar(
            engine,
            "SELECT bool_and(attacl IS NULL) FROM pg_catalog.pg_attribute WHERE attrelid = 'foreign_acl.items'::regclass AND attnum > 0",
        ),
        Value::Bool(true)
    );
}

fn prepare_foreign_table_acl_catalog_fixture() -> Engine {
    let engine = Engine::new();
    setup_foreign_table_acl(&engine);
    assert_foreign_table_acl_defaults(&engine);
    grant_foreign_table_acl_fixture(&engine);
    assert_eq!(
        scalar(
            &engine,
            "SELECT relacl::text FROM pg_catalog.pg_class WHERE oid = 'foreign_acl.items'::regclass",
        ),
        Value::Str("{foreign_acl_owner=arwdDxtm/foreign_acl_owner,foreign_acl_delegate=r*/foreign_acl_owner,foreign_acl_reader=r/foreign_acl_delegate}".into())
    );
    assert_eq!(
        scalar(
            &engine,
            "SELECT attacl::text FROM pg_catalog.pg_attribute WHERE attrelid = 'foreign_acl.items'::regclass AND attname = 'id'",
        ),
        Value::Str("{foreign_acl_column_reader=r/foreign_acl_owner}".into())
    );
    assert_eq!(
        scalar(
            &engine,
            "SELECT relacl::text FROM pg_catalog.pg_class WHERE oid = 'foreign_acl.all_items'::regclass",
        ),
        Value::Str("{foreign_acl_owner=arwdDxtm/foreign_acl_owner,foreign_acl_outsider=arwdDxtm/foreign_acl_owner}".into())
    );
    for privilege in [
        "SELECT",
        "INSERT",
        "UPDATE",
        "DELETE",
        "TRUNCATE",
        "REFERENCES",
        "TRIGGER",
        "MAINTAIN",
    ] {
        assert_eq!(
            scalar(
                &engine,
                &format!(
                    "SELECT has_table_privilege('foreign_acl_outsider', 'foreign_acl.all_items', '{privilege}')"
                ),
            ),
            Value::Bool(true),
            "missing {privilege} foreign-table privilege"
        );
    }
    assert_eq!(
        scalar(
            &engine,
            "SELECT attacl::text FROM pg_catalog.pg_attribute WHERE attrelid = 'foreign_acl.all_items'::regclass AND attname = 'id'",
        ),
        Value::Str("{foreign_acl_column_reader=awx/foreign_acl_owner}".into())
    );
    for privilege in ["INSERT", "UPDATE", "REFERENCES"] {
        assert_eq!(
            scalar(
                &engine,
                &format!(
                    "SELECT has_column_privilege('foreign_acl_column_reader', 'foreign_acl.all_items', 'id', '{privilege}')"
                ),
            ),
            Value::Bool(true),
            "missing {privilege} foreign-table column privilege"
        );
    }
    assert_eq!(
        scalar(
            &engine,
            "SELECT has_table_privilege('foreign_acl_reader', 'foreign_acl.items', 'SELECT') AND has_table_privilege((SELECT oid FROM pg_catalog.pg_roles WHERE rolname = 'foreign_acl_reader'), (SELECT oid FROM pg_catalog.pg_class WHERE oid = 'foreign_acl.items'::regclass), 'SELECT')",
        ),
        Value::Bool(true)
    );
    assert_eq!(
        scalar(
            &engine,
            "SELECT has_column_privilege('foreign_acl_column_reader', 'foreign_acl.items', 'id', 'SELECT') AND has_column_privilege((SELECT oid FROM pg_catalog.pg_roles WHERE rolname = 'foreign_acl_column_reader'), (SELECT oid FROM pg_catalog.pg_class WHERE oid = 'foreign_acl.items'::regclass), 1::smallint, 'SELECT')",
        ),
        Value::Bool(true)
    );
    assert_eq!(
        scalar(
            &engine,
            "SELECT has_column_privilege('foreign_acl_column_reader', 'foreign_acl.items', 'ctid', 'SELECT')",
        ),
        Value::Bool(false)
    );
    engine
}

fn assert_foreign_table_select_enforcement_and_visibility(engine: &Engine) {
    execute(engine, "SET ROLE foreign_acl_outsider");
    assert_eq!(
        sqlstate(engine, "SELECT value FROM foreign_acl.items"),
        "42501"
    );
    assert_eq!(
        scalar(
            engine,
            "SELECT count(*) FROM foreign_acl.definer_items WHERE value IS NOT NULL",
        ),
        Value::Int(2)
    );
    assert_eq!(
        sqlstate(
            engine,
            "SELECT count(*) FROM foreign_acl.invoker_items WHERE value IS NOT NULL",
        ),
        "42501"
    );
    execute(engine, "RESET ROLE");
    execute(engine, "SET ROLE foreign_acl_reader");
    assert_eq!(
        scalar(engine, "SELECT count(*) FROM foreign_acl.items"),
        Value::Int(2)
    );
    assert_eq!(
        scalar(
            engine,
            "SELECT count(*) FROM foreign_acl.items AS f JOIN (VALUES (1), (2)) AS wanted(id) ON f.id = wanted.id",
        ),
        Value::Int(2)
    );
    execute(engine, "RESET ROLE");
    execute(engine, "SET ROLE foreign_acl_column_reader");
    assert_eq!(
        scalar(
            engine,
            "SELECT id FROM foreign_acl.items ORDER BY id LIMIT 1"
        ),
        Value::Int(1)
    );
    assert_eq!(
        sqlstate(engine, "SELECT value FROM foreign_acl.items"),
        "42501"
    );
    assert_eq!(
        scalar(engine, "SELECT count(*) FROM foreign_acl.items"),
        Value::Int(2)
    );
    assert_eq!(
        scalar(
            engine,
            "SELECT (SELECT count(*) FROM information_schema.tables WHERE table_schema = 'foreign_acl')::text || '|' || (SELECT string_agg(column_name, ',' ORDER BY ordinal_position) FROM information_schema.columns WHERE table_schema = 'foreign_acl' AND table_name = 'items') || '|' || (SELECT string_agg(column_name || ':' || privilege_type, ',' ORDER BY column_name, privilege_type) FROM information_schema.column_privileges WHERE table_schema = 'foreign_acl' AND table_name = 'items') || '|' || (SELECT table_type || ':' || is_insertable_into FROM information_schema.tables WHERE table_schema = 'foreign_acl' AND table_name = 'items') || '|' || (SELECT is_updatable FROM information_schema.columns WHERE table_schema = 'foreign_acl' AND table_name = 'items' AND column_name = 'id')",
        ),
        Value::Str("2|id|id:SELECT|FOREIGN:NO|NO".into())
    );
    execute(engine, "RESET ROLE");
}

#[test]
fn foreign_table_acl_catalog_inquiry_visibility_and_select_enforcement() {
    let engine = prepare_foreign_table_acl_catalog_fixture();
    assert_foreign_table_select_enforcement_and_visibility(&engine);
}

#[test]
fn foreign_table_acl_chains_all_schema_transfer_and_atomic_validation() {
    let engine = Engine::new();
    setup_foreign_table_acl(&engine);
    grant_foreign_table_acl_fixture(&engine);

    execute(&engine, "SET ROLE foreign_acl_owner");
    assert_eq!(
        sqlstate(
            &engine,
            "REVOKE GRANT OPTION FOR SELECT ON TABLE foreign_acl.cascade_items FROM foreign_acl_delegate RESTRICT",
        ),
        "2BP01"
    );
    execute(
        &engine,
        "REVOKE GRANT OPTION FOR SELECT ON TABLE foreign_acl.cascade_items FROM foreign_acl_delegate CASCADE",
    );
    assert_eq!(
        scalar(
            &engine,
            "SELECT has_table_privilege('foreign_acl_delegate', 'foreign_acl.cascade_items', 'SELECT') AND NOT has_table_privilege('foreign_acl_delegate', 'foreign_acl.cascade_items', 'SELECT WITH GRANT OPTION') AND NOT has_table_privilege('foreign_acl_reader', 'foreign_acl.cascade_items', 'SELECT')",
        ),
        Value::Bool(true)
    );
    execute(
        &engine,
        "GRANT SELECT ON ALL TABLES IN SCHEMA foreign_acl TO foreign_acl_outsider",
    );
    execute(
        &engine,
        "ALTER FOREIGN TABLE foreign_acl.items OWNER TO foreign_acl_next_owner",
    );
    execute(&engine, "RESET ROLE");
    assert_eq!(
        scalar(
            &engine,
            "SELECT relowner::regrole::text || '|' || relacl::text FROM pg_catalog.pg_class WHERE oid = 'foreign_acl.items'::regclass",
        ),
        Value::Str("foreign_acl_next_owner|{foreign_acl_next_owner=arwdDxtm/foreign_acl_next_owner,foreign_acl_delegate=r*/foreign_acl_next_owner,foreign_acl_reader=r/foreign_acl_delegate,foreign_acl_outsider=r/foreign_acl_next_owner}".into())
    );
    assert_eq!(
        scalar(
            &engine,
            "SELECT attacl::text FROM pg_catalog.pg_attribute WHERE attrelid = 'foreign_acl.items'::regclass AND attname = 'id'",
        ),
        Value::Str("{foreign_acl_column_reader=r/foreign_acl_next_owner}".into())
    );
    assert_eq!(sqlstate(&engine, "DROP ROLE foreign_acl_delegate"), "2BP01");

    execute(&engine, "BEGIN READ ONLY");
    assert_eq!(
        sqlstate(
            &engine,
            "GRANT DELETE ON TABLE foreign_acl.items TO foreign_acl_reader"
        ),
        "25006"
    );
    execute(&engine, "ROLLBACK");

    execute(&engine, "SET ROLE foreign_acl_next_owner");
    assert_eq!(
        sqlstate(
            &engine,
            "GRANT SELECT(id), UPDATE(missing) ON TABLE foreign_acl.items, foreign_acl.all_items TO foreign_acl_reader",
        ),
        "42703"
    );
    execute(&engine, "RESET ROLE");
    assert_eq!(
        scalar(
            &engine,
            "SELECT has_column_privilege('foreign_acl_reader', 'foreign_acl.items', 'id', 'UPDATE')",
        ),
        Value::Bool(false)
    );
}

#[test]
fn foreign_table_acl_follows_transactions_cross_engine_refresh_and_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("foreign-table-acl.db");
    let first = Engine::open(&database).unwrap();
    setup_foreign_table_acl(&first);
    let second = Engine::open(&database).unwrap();

    execute(&first, "SET ROLE foreign_acl_owner");
    execute(&first, "BEGIN");
    execute(
        &first,
        "GRANT SELECT ON TABLE foreign_acl.items TO foreign_acl_reader",
    );
    assert_eq!(
        scalar(
            &first,
            "SELECT has_table_privilege('foreign_acl_reader', 'foreign_acl.items', 'SELECT')",
        ),
        Value::Bool(true)
    );
    execute(&first, "ROLLBACK");
    assert_eq!(
        scalar(
            &first,
            "SELECT has_table_privilege('foreign_acl_reader', 'foreign_acl.items', 'SELECT')",
        ),
        Value::Bool(false)
    );
    execute(&first, "BEGIN");
    execute(&first, "SAVEPOINT foreign_acl_change");
    execute(
        &first,
        "GRANT SELECT(id) ON TABLE foreign_acl.items TO foreign_acl_column_reader",
    );
    execute(&first, "ROLLBACK TO SAVEPOINT foreign_acl_change");
    assert_eq!(
        scalar(
            &first,
            "SELECT has_column_privilege('foreign_acl_column_reader', 'foreign_acl.items', 'id', 'SELECT')",
        ),
        Value::Bool(false)
    );
    execute(
        &first,
        "GRANT SELECT ON TABLE foreign_acl.items TO foreign_acl_reader",
    );
    execute(
        &first,
        "GRANT SELECT(id) ON TABLE foreign_acl.items TO foreign_acl_column_reader",
    );
    execute(&first, "COMMIT");
    execute(&first, "RESET ROLE");
    assert_eq!(
        scalar(
            &second,
            "SELECT has_table_privilege('foreign_acl_reader', 'foreign_acl.items', 'SELECT') AND has_column_privilege('foreign_acl_column_reader', 'foreign_acl.items', 'id', 'SELECT')",
        ),
        Value::Bool(true)
    );
    execute(&first, "SET ROLE foreign_acl_owner");
    execute(
        &first,
        "ALTER FOREIGN TABLE foreign_acl.items OWNER TO foreign_acl_next_owner",
    );
    execute(&first, "RESET ROLE");
    assert_eq!(
        scalar(
            &second,
            "SELECT relowner::regrole::text FROM pg_catalog.pg_class WHERE oid = 'foreign_acl.items'::regclass",
        ),
        Value::Str("foreign_acl_next_owner".into())
    );
    drop(second);
    drop(first);

    let reopened = Engine::open(&database).unwrap();
    assert_eq!(
        scalar(
            &reopened,
            "SELECT has_table_privilege('foreign_acl_reader', 'foreign_acl.items', 'SELECT') AND has_column_privilege('foreign_acl_column_reader', 'foreign_acl.items', 'id', 'SELECT') AND (SELECT relowner::regrole::text = 'foreign_acl_next_owner' FROM pg_catalog.pg_class WHERE oid = 'foreign_acl.items'::regclass)",
        ),
        Value::Bool(true)
    );
    assert_eq!(
        scalar(
            &reopened,
            "SELECT c.relacl::text || '|' || a.attacl::text FROM pg_catalog.pg_class AS c JOIN pg_catalog.pg_attribute AS a ON a.attrelid = c.oid AND a.attname = 'id' WHERE c.oid = 'foreign_acl.items'::regclass",
        ),
        Value::Str("{foreign_acl_next_owner=arwdDxtm/foreign_acl_next_owner,foreign_acl_reader=r/foreign_acl_next_owner}|{foreign_acl_column_reader=r/foreign_acl_next_owner}".into())
    );
}

#[test]
fn engine_open_rejects_foreign_table_acl_with_missing_role() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("invalid-foreign-table-acl.db");
    {
        let engine = Engine::open(&database).unwrap();
        setup_foreign_table_acl(&engine);
    }
    let connection = rusqlite::Connection::open(&database).unwrap();
    connection
        .execute(
            "UPDATE _foreign_tables SET acl_json = '[{\"role\":\"missing_foreign_acl_role\",\"grantor\":\"foreign_acl_owner\",\"privileges\":{\"select\":true}}]' WHERE relation_name = 'items'",
            [],
        )
        .unwrap();
    drop(connection);

    let Err(error) = Engine::open(&database) else {
        panic!("invalid foreign-table ACL must reject catalog open");
    };
    assert!(
        error.to_string().contains("missing grantee role"),
        "{error}"
    );
}
