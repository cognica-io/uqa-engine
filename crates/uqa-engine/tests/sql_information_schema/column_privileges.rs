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

fn scalar(engine: &Engine, sql: &str) -> Value {
    let result = engine
        .sql(sql, &[])
        .unwrap_or_else(|error| panic!("{sql}: {error}"));
    result.rows[0][&result.columns[0]].clone()
}

fn sqlstate(engine: &Engine, sql: &str) -> String {
    engine
        .sql(sql, &[])
        .expect_err("statement should fail")
        .sqlstate()
        .expect("failure should expose SQLSTATE")
        .to_string()
}

fn setup(engine: &Engine) {
    for sql in [
        "CREATE ROLE column_acl_owner",
        "CREATE ROLE column_acl_reader",
        "CREATE ROLE column_acl_delegate",
        "GRANT CREATE ON DATABASE uqa TO column_acl_owner",
        "SET ROLE column_acl_owner",
        "CREATE SCHEMA column_acl",
        "GRANT USAGE ON SCHEMA column_acl TO column_acl_reader, column_acl_delegate",
        "CREATE TABLE column_acl.items(a integer, b integer, c integer)",
        "INSERT INTO column_acl.items VALUES (1, 2, 3)",
        "RESET ROLE",
    ] {
        execute(engine, sql);
    }
}

#[test]
fn column_acl_catalog_inquiry_visibility_and_select_enforcement() {
    let engine = Engine::new();
    setup(&engine);
    for sql in [
        "SET ROLE column_acl_owner",
        "GRANT SELECT(a), INSERT(b), UPDATE(c), REFERENCES(a) ON column_acl.items TO column_acl_reader",
        "RESET ROLE",
    ] {
        execute(&engine, sql);
    }
    assert_eq!(
        scalar(
            &engine,
            "SELECT relacl IS NULL FROM pg_catalog.pg_class WHERE oid = 'column_acl.items'::regclass",
        ),
        Value::Bool(true)
    );
    assert_eq!(
        scalar(
            &engine,
            "SELECT attacl::text FROM pg_catalog.pg_attribute WHERE attrelid = 'column_acl.items'::regclass AND attname = 'a'",
        ),
        Value::Str("{column_acl_reader=rx/column_acl_owner}".into())
    );
    assert_eq!(
        scalar(
            &engine,
            "SELECT has_column_privilege('column_acl_reader', 'column_acl.items', 'a', 'SELECT, UPDATE')",
        ),
        Value::Bool(true)
    );
    assert_eq!(
        scalar(
            &engine,
            "SELECT has_column_privilege('column_acl_reader'::regrole::oid, 'column_acl.items'::regclass, 1::smallint, 'SELECT')",
        ),
        Value::Bool(true)
    );
    execute(&engine, "SET ROLE column_acl_reader");
    assert_eq!(
        scalar(&engine, "SELECT a FROM column_acl.items"),
        Value::Int(1)
    );
    assert_eq!(
        scalar(&engine, "SELECT count(*) FROM column_acl.items"),
        Value::Int(1)
    );
    assert_eq!(
        scalar(&engine, "SELECT 1 FROM column_acl.items"),
        Value::Int(1)
    );
    assert_eq!(sqlstate(&engine, "SELECT b FROM column_acl.items"), "42501");
    assert_eq!(sqlstate(&engine, "SELECT * FROM column_acl.items"), "42501");
    assert_eq!(
        scalar(
            &engine,
            "SELECT string_agg(column_name, ',' ORDER BY ordinal_position) FROM information_schema.columns WHERE table_schema = 'column_acl' AND table_name = 'items'",
        ),
        Value::Str("a,b,c".into())
    );
    assert_eq!(
        scalar(
            &engine,
            "SELECT string_agg(column_name || ':' || privilege_type || ':' || is_grantable, ',' ORDER BY column_name, privilege_type) FROM information_schema.column_privileges WHERE table_schema = 'column_acl' AND table_name = 'items'",
        ),
        Value::Str("a:REFERENCES:NO,a:SELECT:NO,b:INSERT:NO,c:UPDATE:NO".into())
    );
    assert_eq!(
        scalar(
            &engine,
            "SELECT count(*) FROM information_schema.role_column_grants WHERE table_schema = 'column_acl' AND table_name = 'items'",
        ),
        Value::Int(4)
    );
    for sql in [
        "RESET ROLE",
        "SET ROLE column_acl_owner",
        "REVOKE ALL PRIVILEGES ON column_acl.items FROM column_acl_owner",
        "RESET ROLE",
    ] {
        execute(&engine, sql);
    }
    assert_eq!(
        scalar(
            &engine,
            "SELECT count(*) FROM information_schema.column_privileges WHERE table_schema = 'column_acl' AND table_name = 'items' AND grantee = 'column_acl_owner'",
        ),
        Value::Int(0)
    );
}

#[test]
fn table_range_aliases_preserve_physical_column_privileges() {
    let engine = Engine::new();
    setup(&engine);
    for sql in [
        "SET ROLE column_acl_owner",
        "GRANT SELECT(a) ON column_acl.items TO column_acl_reader",
        "RESET ROLE",
        "SET ROLE column_acl_reader",
    ] {
        execute(&engine, sql);
    }
    assert_eq!(
        scalar(
            &engine,
            "SELECT visible_a FROM column_acl.items AS source(visible_a, visible_b, visible_c)",
        ),
        Value::Int(1)
    );
    assert_eq!(
        sqlstate(
            &engine,
            "SELECT visible_b FROM column_acl.items AS source(visible_a, visible_b, visible_c)",
        ),
        "42501"
    );
}

#[test]
fn column_privilege_views_follow_postgresql_enabled_role_visibility() {
    let engine = Engine::new();
    setup(&engine);
    for sql in [
        "SET ROLE column_acl_owner",
        "GRANT SELECT(a) ON column_acl.items TO column_acl_reader",
        "GRANT UPDATE(a) ON column_acl.items TO PUBLIC",
        "GRANT SELECT ON column_acl.items TO column_acl_delegate WITH GRANT OPTION",
        "RESET ROLE",
        "SET ROLE column_acl_reader",
    ] {
        execute(&engine, sql);
    }
    assert_eq!(
        scalar(
            &engine,
            "SELECT string_agg(grantee || ':' || column_name || ':' || privilege_type, ',' ORDER BY grantee, column_name, privilege_type) FROM information_schema.column_privileges WHERE table_schema = 'column_acl' AND table_name = 'items'",
        ),
        Value::Str("PUBLIC:a:UPDATE,column_acl_reader:a:SELECT".into())
    );
    assert_eq!(
        scalar(
            &engine,
            "SELECT string_agg(grantee || ':' || column_name || ':' || privilege_type, ',' ORDER BY grantee, column_name, privilege_type) FROM information_schema.role_column_grants WHERE table_schema = 'column_acl' AND table_name = 'items'",
        ),
        Value::Str("column_acl_reader:a:SELECT".into())
    );
    execute(&engine, "RESET ROLE");
    execute(&engine, "SET ROLE column_acl_owner");
    assert_eq!(
        scalar(
            &engine,
            "SELECT count(*) FROM information_schema.role_column_grants WHERE table_schema = 'column_acl' AND table_name = 'items'",
        ),
        Value::Int(17)
    );
}

#[test]
fn column_insert_update_and_references_use_exact_columns() {
    let engine = Engine::new();
    setup(&engine);
    for sql in [
        "SET ROLE column_acl_owner",
        "GRANT INSERT(b), UPDATE(c) ON column_acl.items TO column_acl_reader",
        "RESET ROLE",
        "SET ROLE column_acl_reader",
        "INSERT INTO column_acl.items(b) VALUES (4)",
        "UPDATE column_acl.items SET c = 8",
    ] {
        execute(&engine, sql);
    }
    assert_eq!(
        sqlstate(&engine, "INSERT INTO column_acl.items(a) VALUES (4)"),
        "42501"
    );
    assert_eq!(
        sqlstate(&engine, "UPDATE column_acl.items SET c = c + 1"),
        "42501"
    );
    assert_eq!(
        sqlstate(&engine, "UPDATE column_acl.items SET b = 9"),
        "42501"
    );
    for sql in [
        "RESET ROLE",
        "SET ROLE column_acl_owner",
        "GRANT SELECT(b) ON column_acl.items TO column_acl_reader",
        "RESET ROLE",
        "SET ROLE column_acl_reader",
    ] {
        execute(&engine, sql);
    }
    assert_eq!(
        sqlstate(
            &engine,
            "UPDATE column_acl.items AS target SET c = 9 WHERE EXISTS (SELECT 1 WHERE target.a = 1)",
        ),
        "42501"
    );
    execute(
        &engine,
        "UPDATE column_acl.items AS target SET c = 9 WHERE EXISTS (SELECT 1 WHERE target.b = 2)",
    );
    for sql in [
        "RESET ROLE",
        "SET ROLE column_acl_owner",
        "CREATE TABLE column_acl.shadow_source(a integer)",
        "INSERT INTO column_acl.shadow_source VALUES (1)",
        "GRANT SELECT(a) ON column_acl.shadow_source TO column_acl_reader",
        "RESET ROLE",
        "SET ROLE column_acl_reader",
        "UPDATE column_acl.items AS target SET c = 9 WHERE EXISTS (SELECT 1 FROM column_acl.shadow_source AS target WHERE target.a = 1)",
        "RESET ROLE",
        "SET ROLE column_acl_owner",
        "CREATE TABLE column_acl.parents(id integer PRIMARY KEY, other integer UNIQUE)",
        "GRANT REFERENCES(id) ON column_acl.parents TO column_acl_reader",
        "GRANT CREATE ON SCHEMA column_acl TO column_acl_reader",
        "RESET ROLE",
        "SET ROLE column_acl_reader",
        "CREATE TABLE column_acl.children(parent_id integer REFERENCES column_acl.parents(id))",
    ] {
        execute(&engine, sql);
    }
    assert_eq!(
        sqlstate(
            &engine,
            "CREATE TABLE column_acl.denied(parent_id integer REFERENCES column_acl.parents(other))",
        ),
        "42501"
    );
}

#[test]
fn implicit_insert_and_merge_columns_follow_the_supplied_value_width() {
    let engine = Engine::new();
    setup(&engine);
    for sql in [
        "SET ROLE column_acl_owner",
        "CREATE TABLE column_acl.implicit_items(a integer, b integer DEFAULT 2, c integer DEFAULT 3)",
        "CREATE TABLE column_acl.implicit_source(a integer)",
        "INSERT INTO column_acl.implicit_source VALUES (4)",
        "GRANT INSERT(a) ON column_acl.implicit_items TO column_acl_reader",
        "GRANT SELECT(a) ON column_acl.implicit_source TO column_acl_reader",
        "RESET ROLE",
        "SET ROLE column_acl_reader",
        "INSERT INTO column_acl.implicit_items VALUES (1)",
        "INSERT INTO column_acl.implicit_items SELECT a FROM column_acl.implicit_source",
        "MERGE INTO column_acl.implicit_items AS target USING (VALUES (5)) AS source(a) ON false WHEN NOT MATCHED THEN INSERT VALUES (source.a)",
    ] {
        execute(&engine, sql);
    }
    assert_eq!(
        sqlstate(
            &engine,
            "INSERT INTO column_acl.implicit_items VALUES (6, 7)",
        ),
        "42501"
    );
    assert_eq!(
        sqlstate(
            &engine,
            "MERGE INTO column_acl.implicit_items AS target USING (VALUES (8, 9)) AS source(a, b) ON false WHEN NOT MATCHED THEN INSERT VALUES (source.a, source.b)",
        ),
        "42501"
    );
}

#[test]
fn select_column_privileges_follow_join_and_correlated_subquery_references() {
    let engine = Engine::new();
    setup(&engine);
    for sql in [
        "SET ROLE column_acl_owner",
        "CREATE TABLE column_acl.left_items(a integer, b integer)",
        "CREATE TABLE column_acl.right_items(a integer, b integer)",
        "INSERT INTO column_acl.left_items VALUES (1, 10)",
        "INSERT INTO column_acl.right_items VALUES (1, 20)",
        "GRANT SELECT(b) ON column_acl.left_items TO column_acl_reader",
        "GRANT SELECT(a) ON column_acl.right_items TO column_acl_reader",
        "RESET ROLE",
        "SET ROLE column_acl_reader",
    ] {
        execute(&engine, sql);
    }
    assert_eq!(
        sqlstate(
            &engine,
            "SELECT 1 FROM (column_acl.left_items AS l JOIN column_acl.right_items AS r ON l.a = r.a) AS joined",
        ),
        "42501"
    );
    assert_eq!(
        sqlstate(
            &engine,
            "SELECT (SELECT l.a) FROM column_acl.left_items AS l",
        ),
        "42501"
    );
    assert_eq!(
        sqlstate(&engine, "SELECT count(l.*) FROM column_acl.left_items AS l",),
        "42501"
    );
    assert_eq!(
        scalar(&engine, "SELECT 1 FROM column_acl.left_items"),
        Value::Int(1)
    );
    for sql in [
        "RESET ROLE",
        "SET ROLE column_acl_owner",
        "GRANT SELECT(a) ON column_acl.left_items TO column_acl_reader",
        "RESET ROLE",
        "SET ROLE column_acl_reader",
    ] {
        execute(&engine, sql);
    }
    assert_eq!(
        scalar(
            &engine,
            "SELECT 1 FROM (column_acl.left_items AS l JOIN column_acl.right_items AS r ON l.a = r.a) AS joined",
        ),
        Value::Int(1)
    );
    assert_eq!(
        scalar(
            &engine,
            "SELECT (SELECT l.a) FROM column_acl.left_items AS l",
        ),
        Value::Int(1)
    );
    assert_eq!(
        scalar(
            &engine,
            "SELECT (WITH unused AS (SELECT b FROM column_acl.right_items) SELECT 1) FROM column_acl.left_items AS l",
        ),
        Value::Int(1)
    );
    assert_eq!(
        scalar(
            &engine,
            "SELECT (WITH right_items(a) AS (VALUES (1)) SELECT 1 FROM right_items) FROM column_acl.left_items AS l",
        ),
        Value::Int(1)
    );
}

#[test]
fn update_delete_and_merge_sources_check_only_referenced_columns() {
    let engine = Engine::new();
    setup(&engine);
    for sql in [
        "SET ROLE column_acl_owner",
        "CREATE TABLE column_acl.source_items(a integer, b integer)",
        "CREATE TABLE column_acl.merge_items(id integer PRIMARY KEY, value integer)",
        "INSERT INTO column_acl.source_items VALUES (1, 2)",
        "INSERT INTO column_acl.merge_items VALUES (1, 0)",
        "GRANT SELECT(a) ON column_acl.source_items TO column_acl_reader",
        "GRANT UPDATE(c), DELETE ON column_acl.items TO column_acl_reader",
        "GRANT SELECT(id), UPDATE(value) ON column_acl.merge_items TO column_acl_reader",
        "RESET ROLE",
        "SET ROLE column_acl_reader",
        "UPDATE column_acl.items SET c = 9 FROM column_acl.source_items AS source WHERE source.a = 1",
    ] {
        execute(&engine, sql);
    }
    assert_eq!(
        sqlstate(
            &engine,
            "UPDATE column_acl.items SET c = 9 FROM column_acl.source_items AS source WHERE source.b = 2",
        ),
        "42501"
    );
    execute(
        &engine,
        "DELETE FROM column_acl.items USING column_acl.source_items AS source WHERE source.a = 1",
    );
    execute(
        &engine,
        "MERGE INTO column_acl.merge_items AS target USING column_acl.source_items AS source ON target.id = source.a WHEN MATCHED THEN UPDATE SET value = 7",
    );
    assert_eq!(
        sqlstate(
            &engine,
            "MERGE INTO column_acl.merge_items AS target USING column_acl.source_items AS source ON target.id = source.b WHEN MATCHED THEN UPDATE SET value = 8",
        ),
        "42501"
    );
}

#[test]
fn column_grant_paths_revoke_rename_and_drop_with_the_column() {
    let engine = Engine::new();
    setup(&engine);
    for sql in [
        "SET ROLE column_acl_owner",
        "GRANT SELECT(a) ON column_acl.items TO column_acl_delegate WITH GRANT OPTION",
        "RESET ROLE",
        "SET ROLE column_acl_delegate",
        "GRANT SELECT(a) ON column_acl.items TO column_acl_reader",
        "RESET ROLE",
    ] {
        execute(&engine, sql);
    }
    assert_eq!(
        sqlstate(
            &engine,
            "SET ROLE column_acl_owner; REVOKE GRANT OPTION FOR SELECT(a) ON column_acl.items FROM column_acl_delegate RESTRICT",
        ),
        "2BP01"
    );
    for sql in [
        "RESET ROLE",
        "SET ROLE column_acl_owner",
        "REVOKE GRANT OPTION FOR SELECT(a) ON column_acl.items FROM column_acl_delegate CASCADE",
        "ALTER TABLE column_acl.items RENAME COLUMN a TO renamed",
        "RESET ROLE",
    ] {
        execute(&engine, sql);
    }
    assert_eq!(
        scalar(
            &engine,
            "SELECT has_column_privilege('column_acl_delegate', 'column_acl.items', 'renamed', 'SELECT')",
        ),
        Value::Bool(true)
    );
    assert_eq!(
        sqlstate(
            &engine,
            "SELECT has_column_privilege('column_acl_delegate', 'column_acl.items', 'a', 'SELECT')",
        ),
        "42703"
    );
    execute(&engine, "SET ROLE column_acl_owner");
    execute(&engine, "ALTER TABLE column_acl.items DROP COLUMN renamed");
    assert_eq!(
        scalar(
            &engine,
            "SELECT count(*) FROM pg_catalog.pg_attribute WHERE attrelid = 'column_acl.items'::regclass AND attacl IS NOT NULL",
        ),
        Value::Int(0)
    );
}

#[test]
fn column_grants_validate_every_mixed_relation_target_before_mutation() {
    let engine = Engine::new();
    setup(&engine);
    execute(&engine, "SET ROLE column_acl_owner");
    execute(&engine, "CREATE SEQUENCE column_acl.ids");
    assert_eq!(
        sqlstate(
            &engine,
            "GRANT SELECT(a) ON TABLE column_acl.items, column_acl.ids TO column_acl_reader",
        ),
        "42703"
    );
    assert_eq!(
        scalar(
            &engine,
            "SELECT attacl IS NULL FROM pg_catalog.pg_attribute WHERE attrelid = 'column_acl.items'::regclass AND attname = 'a'",
        ),
        Value::Bool(true)
    );
    execute(
        &engine,
        "GRANT SELECT ON SEQUENCE column_acl.ids TO column_acl_reader",
    );
    assert_eq!(
        scalar(
            &engine,
            "SELECT has_column_privilege('column_acl_reader', 'column_acl.ids', 'last_value', 'SELECT')",
        ),
        Value::Bool(true)
    );
    assert_eq!(
        scalar(
            &engine,
            "SELECT has_column_privilege('column_acl_reader', 'column_acl.ids', 3::smallint, 'SELECT')",
        ),
        Value::Bool(true)
    );
    assert_eq!(
        scalar(
            &engine,
            "SELECT has_column_privilege('column_acl_reader', 'column_acl.ids', 4::smallint, 'SELECT') IS NULL",
        ),
        Value::Bool(true)
    );
    assert_eq!(
        sqlstate(
            &engine,
            "SELECT has_column_privilege('column_acl_reader', 'column_acl.ids', 'missing', 'SELECT')",
        ),
        "42703"
    );
}

#[test]
fn column_acl_role_dependencies_and_owner_transfer_rewrite_grantors() {
    let engine = Engine::new();
    setup(&engine);
    for sql in [
        "CREATE ROLE column_acl_new_owner",
        "GRANT CREATE ON SCHEMA column_acl TO column_acl_new_owner",
        "SET ROLE column_acl_owner",
        "GRANT SELECT(a) ON column_acl.items TO column_acl_delegate WITH GRANT OPTION",
        "RESET ROLE",
        "SET ROLE column_acl_delegate",
        "GRANT SELECT(a) ON column_acl.items TO column_acl_reader",
        "RESET ROLE",
    ] {
        execute(&engine, sql);
    }
    assert_eq!(sqlstate(&engine, "DROP ROLE column_acl_reader"), "2BP01");
    for sql in [
        "GRANT column_acl_new_owner TO column_acl_owner WITH INHERIT FALSE, SET TRUE",
        "SET ROLE column_acl_owner",
        "ALTER TABLE column_acl.items OWNER TO column_acl_new_owner",
        "RESET ROLE",
    ] {
        execute(&engine, sql);
    }
    assert_eq!(
        scalar(
            &engine,
            "SELECT attacl::text FROM pg_catalog.pg_attribute WHERE attrelid = 'column_acl.items'::regclass AND attname = 'a'",
        ),
        Value::Str(
            "{column_acl_delegate=r*/column_acl_new_owner,column_acl_reader=r/column_acl_delegate}"
                .into()
        )
    );
}

fn assert_has_column_privilege_system_columns(engine: &Engine) {
    execute(engine, "SET ROLE column_acl_reader");
    assert_eq!(
        sqlstate(engine, "SELECT tableoid FROM column_acl.items"),
        "42501"
    );
    execute(engine, "RESET ROLE");
    execute(engine, "SET ROLE column_acl_delegate");
    execute(engine, "SELECT tableoid FROM column_acl.items");
    execute(engine, "RESET ROLE");

    assert_eq!(
        scalar(
            engine,
            "SELECT has_column_privilege('column_acl_reader', 'column_acl.items', 1::smallint, 'SELECT')",
        ),
        Value::Bool(true)
    );
    assert_eq!(
        scalar(
            engine,
            "SELECT has_column_privilege('column_acl_reader', 'column_acl.items', 2::smallint, 'SELECT')",
        ),
        Value::Bool(false)
    );
    for column in ["ctid", "xmin", "cmin", "xmax", "cmax", "tableoid"] {
        assert_eq!(
            scalar(
                engine,
                &format!(
                    "SELECT has_column_privilege('column_acl_reader', 'column_acl.items', '{column}', 'SELECT')",
                ),
            ),
            Value::Bool(false)
        );
        assert_eq!(
            scalar(
                engine,
                &format!(
                    "SELECT has_column_privilege('column_acl_delegate', 'column_acl.items', '{column}', 'SELECT')",
                ),
            ),
            Value::Bool(true)
        );
    }
    for attnum in -6..=-1 {
        assert_eq!(
            scalar(
                engine,
                &format!(
                    "SELECT has_column_privilege('column_acl_reader', 'column_acl.items', ({attnum})::smallint, 'SELECT')",
                ),
            ),
            Value::Bool(false)
        );
        assert_eq!(
            scalar(
                engine,
                &format!(
                    "SELECT has_column_privilege('column_acl_delegate', 'column_acl.items', ({attnum})::smallint, 'SELECT')",
                ),
            ),
            Value::Bool(true)
        );
    }
}

fn assert_has_column_privilege_invalid_inputs(engine: &Engine) {
    for attnum in [-7, 0, 4] {
        assert_eq!(
            scalar(
                engine,
                &format!(
                    "SELECT has_column_privilege('column_acl_reader', 'column_acl.items', ({attnum})::smallint, 'SELECT') IS NULL",
                ),
            ),
            Value::Bool(true)
        );
    }
    assert_eq!(
        scalar(
            engine,
            "SELECT has_column_privilege('column_acl_reader', 4294967290::oid, 'a', 'SELECT') IS NULL",
        ),
        Value::Bool(true)
    );
    assert_eq!(
        scalar(
            engine,
            "SELECT has_column_privilege(4294967290::oid, 'column_acl.items', 'a', 'SELECT')",
        ),
        Value::Bool(false)
    );
    assert_eq!(
        scalar(
            engine,
            "SELECT has_column_privilege('column_acl.items', NULL::text, 'SELECT') IS NULL",
        ),
        Value::Bool(true)
    );
    assert_eq!(
        sqlstate(
            engine,
            "SELECT has_column_privilege('column_acl_reader', 'column_acl.items', 'missing', 'SELECT')",
        ),
        "42703"
    );
    assert_eq!(
        sqlstate(
            engine,
            "SELECT has_column_privilege('missing_role', 'column_acl.items', 'a', 'SELECT')",
        ),
        "42704"
    );
    assert_eq!(
        sqlstate(
            engine,
            "SELECT has_column_privilege('column_acl_reader', 'column_acl.items', 'a', 'DELETE')",
        ),
        "22023"
    );
}

fn assert_has_column_privilege_catalog_entries(engine: &Engine) {
    for (oid, source) in [
        (3012, "has_column_privilege_name_name_name"),
        (3013, "has_column_privilege_name_name_attnum"),
        (3014, "has_column_privilege_name_id_name"),
        (3015, "has_column_privilege_name_id_attnum"),
        (3016, "has_column_privilege_id_name_name"),
        (3017, "has_column_privilege_id_name_attnum"),
        (3018, "has_column_privilege_id_id_name"),
        (3019, "has_column_privilege_id_id_attnum"),
        (3020, "has_column_privilege_name_name"),
        (3021, "has_column_privilege_name_attnum"),
        (3022, "has_column_privilege_id_name"),
        (3023, "has_column_privilege_id_attnum"),
    ] {
        assert_eq!(
            scalar(
                engine,
                &format!("SELECT prosrc FROM pg_catalog.pg_proc WHERE oid = {oid}"),
            ),
            Value::Str(source.into())
        );
    }
}

#[test]
fn has_column_privilege_matches_postgresql_signatures_and_system_column_boundaries() {
    let engine = Engine::new();
    setup(&engine);
    execute(&engine, "SET ROLE column_acl_owner");
    execute(
        &engine,
        "REVOKE SELECT(c) ON column_acl.items FROM column_acl_reader",
    );
    assert_eq!(
        scalar(
            &engine,
            "SELECT attacl IS NULL FROM pg_catalog.pg_attribute WHERE attrelid = 'column_acl.items'::regclass AND attname = 'c'",
        ),
        Value::Bool(true)
    );
    execute(&engine, "RESET ROLE");
    for sql in [
        "SET ROLE column_acl_owner",
        "GRANT SELECT(a) ON column_acl.items TO column_acl_reader",
        "GRANT SELECT ON column_acl.items TO column_acl_delegate",
        "RESET ROLE",
    ] {
        execute(&engine, sql);
    }

    assert_has_column_privilege_system_columns(&engine);
    assert_has_column_privilege_invalid_inputs(&engine);
    assert_has_column_privilege_catalog_entries(&engine);
}

#[test]
fn column_acl_is_transactional_refreshes_other_engines_and_reopens() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("column-acl.db");
    {
        let writer = Engine::open(&database).unwrap();
        for sql in [
            "CREATE ROLE durable_column_owner",
            "CREATE ROLE durable_column_reader",
            "GRANT CREATE ON SCHEMA public TO durable_column_owner",
            "SET ROLE durable_column_owner",
            "CREATE TABLE durable_column_items(a integer, b integer)",
            "INSERT INTO durable_column_items VALUES (1, 2)",
            "RESET ROLE",
        ] {
            execute(&writer, sql);
        }
        let observer = Engine::open(&database).unwrap();
        assert_eq!(
            scalar(
                &observer,
                "SELECT has_column_privilege('durable_column_reader', 'durable_column_items', 'a', 'SELECT')",
            ),
            Value::Bool(false)
        );

        for sql in [
            "SET ROLE durable_column_owner",
            "BEGIN",
            "GRANT SELECT(a) ON durable_column_items TO durable_column_reader",
        ] {
            execute(&writer, sql);
        }
        assert_eq!(
            scalar(
                &writer,
                "SELECT has_column_privilege('durable_column_reader', 'durable_column_items', 'a', 'SELECT')",
            ),
            Value::Bool(true)
        );
        assert_eq!(
            scalar(
                &observer,
                "SELECT has_column_privilege('durable_column_reader', 'durable_column_items', 'a', 'SELECT')",
            ),
            Value::Bool(false)
        );
        execute(&writer, "ROLLBACK");

        for sql in [
            "BEGIN",
            "GRANT SELECT(a) ON durable_column_items TO durable_column_reader",
            "SAVEPOINT before_column_revoke",
            "REVOKE SELECT(a) ON durable_column_items FROM durable_column_reader",
            "ROLLBACK TO SAVEPOINT before_column_revoke",
            "COMMIT",
        ] {
            execute(&writer, sql);
        }
        assert_eq!(
            scalar(
                &observer,
                "SELECT has_column_privilege('durable_column_reader', 'durable_column_items', 'a', 'SELECT')",
            ),
            Value::Bool(true)
        );
    }

    let reopened = Engine::open(&database).unwrap();
    execute(&reopened, "SET ROLE durable_column_reader");
    assert_eq!(
        scalar(&reopened, "SELECT a FROM durable_column_items"),
        Value::Int(1)
    );
    assert_eq!(
        sqlstate(&reopened, "SELECT b FROM durable_column_items"),
        "42501"
    );
}
