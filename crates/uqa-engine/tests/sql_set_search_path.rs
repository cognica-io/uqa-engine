//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `SET search_path TO ...` parsing and execution, plus namespace
//! introspection through search-path, schema-list, and table-list accessors.

use uqa_engine::Engine;

#[test]
fn search_path_default_is_public_only() {
    let eng = Engine::new();
    assert_eq!(eng.search_path(), vec!["public".to_string()]);
}

#[test]
fn set_search_path_via_sql_updates_resolution_order() {
    let eng = Engine::new();
    eng.sql("CREATE SCHEMA app", &[]).unwrap();
    eng.sql("SET search_path TO app, public", &[]).unwrap();
    assert_eq!(
        eng.search_path(),
        vec!["app".to_string(), "public".to_string()]
    );
}

#[test]
fn quoted_search_path_preserves_commas_and_escaped_quotes() {
    let eng = Engine::new();
    eng.sql("CREATE SCHEMA \"a,b\"", &[]).unwrap();
    eng.sql("CREATE SCHEMA \"a\"\"b\"", &[]).unwrap();
    eng.sql("SET search_path TO \"a,b\", \"a\"\"b\", public", &[])
        .unwrap();
    assert_eq!(
        eng.search_path(),
        vec!["a,b".to_string(), "a\"b".to_string(), "public".to_string()]
    );

    eng.sql("CREATE TABLE items (id INTEGER PRIMARY KEY)", &[])
        .unwrap();
    eng.sql(
        "CREATE TABLE \"a\"\"b\".items (id INTEGER PRIMARY KEY)",
        &[],
    )
    .unwrap();
    eng.sql("INSERT INTO items VALUES (1)", &[]).unwrap();
    eng.sql("INSERT INTO \"a\"\"b\".items VALUES (2)", &[])
        .unwrap();
    assert_eq!(
        eng.sql("SELECT id FROM items", &[]).unwrap().rows[0]["id"],
        uqa_core::Value::Int(1)
    );
    assert_eq!(
        eng.sql("SELECT id FROM \"a\"\"b\".items", &[])
            .unwrap()
            .rows[0]["id"],
        uqa_core::Value::Int(2)
    );
}

#[test]
fn changing_search_path_rebinds_the_same_sql_to_the_new_relation() {
    let eng = Engine::new();
    eng.sql("CREATE SCHEMA s1", &[]).unwrap();
    eng.sql("CREATE SCHEMA s2", &[]).unwrap();
    eng.sql(
        "CREATE TABLE s1.items (id INTEGER PRIMARY KEY, label TEXT)",
        &[],
    )
    .unwrap();
    eng.sql(
        "CREATE TABLE s2.items (id INTEGER PRIMARY KEY, label TEXT)",
        &[],
    )
    .unwrap();
    eng.sql("CREATE INDEX s2_items_id ON s2.items USING btree (id)", &[])
        .unwrap();
    eng.sql("INSERT INTO s1.items VALUES (1, 'first')", &[])
        .unwrap();
    eng.sql("INSERT INTO s2.items VALUES (1, 'second')", &[])
        .unwrap();

    let sql = "SELECT label FROM items WHERE id = 1";
    eng.sql("SET search_path TO s1", &[]).unwrap();
    assert_eq!(
        eng.sql(sql, &[]).unwrap().rows[0]["label"],
        uqa_core::Value::Str("first".to_string())
    );
    eng.sql("SET search_path TO s2", &[]).unwrap();
    assert_eq!(
        eng.sql(sql, &[]).unwrap().rows[0]["label"],
        uqa_core::Value::Str("second".to_string())
    );
}

#[test]
fn current_schema_functions_follow_the_logical_session_catalog() {
    let eng = Engine::new();
    eng.sql("CREATE SCHEMA app", &[]).unwrap();
    // Nonexistent search-path entries are legal but are omitted from the
    // effective schema list and cannot become current_schema.
    eng.sql("SET search_path TO missing, app, public", &[])
        .unwrap();

    let current = eng.sql("SELECT current_schema() AS s", &[]).unwrap();
    assert_eq!(current.rows[0]["s"], uqa_core::Value::Str("app".into()));

    let explicit = eng
        .sql("SELECT current_schemas(FALSE) AS schemas", &[])
        .unwrap();
    assert_eq!(
        explicit.rows[0]["schemas"],
        uqa_core::Value::Array(
            uqa_core::ArrayValue::try_new(vec![
                uqa_core::Value::Str("app".into()),
                uqa_core::Value::Str("public".into()),
            ])
            .unwrap()
        )
    );

    let implicit = eng
        .sql("SELECT current_schemas(TRUE) AS schemas", &[])
        .unwrap();
    assert_eq!(
        implicit.rows[0]["schemas"],
        uqa_core::Value::Array(
            uqa_core::ArrayValue::try_new(vec![
                uqa_core::Value::Str("pg_catalog".into()),
                uqa_core::Value::Str("app".into()),
                uqa_core::Value::Str("public".into()),
            ])
            .unwrap()
        )
    );
}

#[test]
fn tables_in_schema_buckets_qualified_names() {
    let eng = Engine::new();
    eng.sql("CREATE SCHEMA app", &[]).unwrap();
    eng.sql("CREATE TABLE app.users (id INTEGER PRIMARY KEY)", &[])
        .unwrap();
    eng.sql("CREATE TABLE plain (id INTEGER PRIMARY KEY)", &[])
        .unwrap();
    let app_tables = eng.tables_in_schema("app").unwrap();
    assert_eq!(app_tables, vec!["users".to_string()]);
    let pub_tables = eng.tables_in_schema("public").unwrap();
    assert!(pub_tables.contains(&"plain".to_string()));
}

#[test]
fn search_path_resolves_qualified_tables_by_unqualified_name() {
    let eng = Engine::new();
    eng.sql("CREATE SCHEMA app", &[]).unwrap();
    eng.sql(
        "CREATE TABLE app.users (id INTEGER PRIMARY KEY, name TEXT)",
        &[],
    )
    .unwrap();
    eng.sql("SET search_path TO app, public", &[]).unwrap();

    eng.sql("INSERT INTO users (id, name) VALUES (1, 'Alice')", &[])
        .unwrap();
    eng.sql(
        "ALTER TABLE users ADD COLUMN active BOOLEAN DEFAULT TRUE",
        &[],
    )
    .unwrap();
    let rows = eng
        .sql("SELECT name, active FROM users WHERE id = 1", &[])
        .unwrap()
        .rows;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["name"], uqa_core::Value::Str("Alice".into()));
    assert_eq!(rows[0]["active"], uqa_core::Value::Bool(true));
}

#[test]
fn public_qualification_uses_one_catalog_identity() {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)",
        &[],
    )
    .unwrap();
    eng.sql("INSERT INTO public.users VALUES (1, 'Alice')", &[])
        .unwrap();

    let rows = eng
        .sql("SELECT name FROM public.users WHERE id = 1", &[])
        .unwrap()
        .rows;
    assert_eq!(rows[0]["name"], uqa_core::Value::Str("Alice".into()));
    assert!(eng
        .sql("CREATE TABLE public.users (id INTEGER)", &[])
        .is_err());
}

#[test]
fn search_path_does_not_fall_back_to_public() {
    let eng = Engine::new();
    eng.sql("CREATE TABLE public_only (id INTEGER PRIMARY KEY)", &[])
        .unwrap();
    eng.sql("CREATE SCHEMA app", &[]).unwrap();
    eng.sql("SET search_path TO app", &[]).unwrap();

    assert!(eng.sql("SELECT * FROM public_only", &[]).is_err());
    assert!(eng.sql("SELECT * FROM public.public_only", &[]).is_ok());

    eng.sql("SET search_path TO missing", &[]).unwrap();
    assert!(eng.sql("CREATE TABLE no_schema (id INTEGER)", &[]).is_err());
}

#[test]
fn search_path_resolves_views_sequences_and_foreign_tables() {
    let eng = Engine::new();
    eng.sql("CREATE SCHEMA app", &[]).unwrap();
    eng.sql("SET search_path TO app, public", &[]).unwrap();
    eng.sql(
        "CREATE TABLE app.users (id INTEGER PRIMARY KEY, name TEXT)",
        &[],
    )
    .unwrap();
    eng.sql("INSERT INTO users (id, name) VALUES (1, 'Alice')", &[])
        .unwrap();
    eng.sql("CREATE VIEW app.user_names AS SELECT name FROM users", &[])
        .unwrap();
    let view_rows = eng.sql("SELECT name FROM user_names", &[]).unwrap().rows;
    assert_eq!(view_rows[0]["name"], uqa_core::Value::Str("Alice".into()));

    eng.sql("CREATE SEQUENCE app.user_seq START 10", &[])
        .unwrap();
    let seq = eng.sql("SELECT nextval('user_seq') AS v", &[]).unwrap();
    assert_eq!(seq.rows[0]["v"], uqa_core::Value::Int(10));

    eng.sql(
        "CREATE SERVER mem FOREIGN DATA WRAPPER memory_fdw OPTIONS (kind 'memory')",
        &[],
    )
    .unwrap();
    eng.sql(
        "CREATE FOREIGN TABLE app.remote_users (id INTEGER, name TEXT) \
         SERVER mem OPTIONS (source 'memory')",
        &[],
    )
    .unwrap();
    assert!(eng.foreign_table("remote_users").unwrap().is_some());
}

#[test]
fn memory_catalog_uses_one_public_identity_and_one_cross_kind_namespace() {
    let eng = Engine::new();
    eng.sql("CREATE TABLE shared (id INTEGER)", &[]).unwrap();
    assert_eq!(
        eng.table_names().unwrap(),
        vec!["public.shared".to_string()]
    );
    assert!(eng
        .sql("CREATE VIEW public.shared AS SELECT 1", &[])
        .is_err());
    assert!(eng.sql("CREATE SEQUENCE public.shared", &[]).is_err());
    eng.sql(
        "CREATE SERVER mem2 FOREIGN DATA WRAPPER memory_fdw OPTIONS (kind 'memory')",
        &[],
    )
    .unwrap();
    assert!(eng
        .sql(
            "CREATE FOREIGN TABLE public.shared (id INTEGER) SERVER mem2",
            &[],
        )
        .is_err());

    eng.sql("DROP TABLE shared", &[]).unwrap();
    eng.sql("CREATE SEQUENCE shared", &[]).unwrap();
    assert!(eng
        .sql("CREATE TABLE public.shared (id INTEGER)", &[])
        .is_err());
    assert!(eng.sql("CREATE VIEW shared AS SELECT 1", &[]).is_err());
    assert!(eng
        .sql("CREATE FOREIGN TABLE shared (id INTEGER) SERVER mem2", &[],)
        .is_err());
}

#[test]
fn view_sources_bind_to_creation_namespace_across_nested_query_shapes() {
    let eng = Engine::new();
    eng.sql("CREATE SCHEMA s1", &[]).unwrap();
    eng.sql("CREATE SCHEMA s2", &[]).unwrap();
    for schema in ["s1", "s2"] {
        eng.sql(
            &format!("CREATE TABLE {schema}.items (id INTEGER PRIMARY KEY, label TEXT)"),
            &[],
        )
        .unwrap();
    }
    eng.sql(
        "INSERT INTO s1.items VALUES (1, 'first'), (2, 'second')",
        &[],
    )
    .unwrap();
    eng.sql("INSERT INTO s2.items VALUES (1, 'wrong')", &[])
        .unwrap();

    eng.sql("SET search_path TO s1", &[]).unwrap();
    eng.sql(
        "CREATE VIEW public.bound_items AS
         WITH chosen AS (
             SELECT label FROM items
             UNION ALL
             SELECT label FROM (SELECT label FROM items) nested_items
         )
         SELECT label FROM chosen
         WHERE EXISTS (SELECT 1 FROM items probe WHERE probe.id = 1)",
        &[],
    )
    .unwrap();

    eng.sql("SET search_path TO s2", &[]).unwrap();
    let result = eng
        .sql("SELECT label FROM public.bound_items ORDER BY label", &[])
        .unwrap();
    let labels = result
        .rows
        .iter()
        .map(|row| row["label"].clone())
        .collect::<Vec<_>>();
    assert_eq!(
        labels,
        vec![
            uqa_core::Value::Str("first".into()),
            uqa_core::Value::Str("first".into()),
            uqa_core::Value::Str("second".into()),
            uqa_core::Value::Str("second".into()),
        ]
    );

    // Dependency checks use the same canonical identity. A same-local-name
    // table in another schema is unrelated, while the bound source remains
    // protected from DROP and RENAME.
    eng.sql("DROP TABLE s2.items", &[]).unwrap();
    for sql in [
        "DROP TABLE s1.items",
        "ALTER TABLE s1.items RENAME TO renamed_items",
    ] {
        let error = eng.sql(sql, &[]).unwrap_err();
        assert!(error.to_string().contains("public.bound_items"), "{error}");
    }
}

#[test]
fn persisted_view_binding_survives_reopen_and_search_path_change() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("view-binding.db");
    {
        let eng = Engine::open(&path).unwrap();
        eng.sql("CREATE SCHEMA s1", &[]).unwrap();
        eng.sql("CREATE SCHEMA s2", &[]).unwrap();
        eng.sql(
            "CREATE TABLE s1.items (id INTEGER PRIMARY KEY, label TEXT)",
            &[],
        )
        .unwrap();
        eng.sql(
            "CREATE TABLE s2.items (id INTEGER PRIMARY KEY, label TEXT)",
            &[],
        )
        .unwrap();
        eng.sql("INSERT INTO s1.items VALUES (1, 'first')", &[])
            .unwrap();
        eng.sql("INSERT INTO s2.items VALUES (1, 'wrong')", &[])
            .unwrap();
        eng.sql("SET search_path TO s1", &[]).unwrap();
        eng.sql(
            "CREATE VIEW public.bound_items AS SELECT label FROM items",
            &[],
        )
        .unwrap();
    }

    let eng = Engine::open(&path).unwrap();
    eng.sql("SET search_path TO s2", &[]).unwrap();
    let result = eng
        .sql("SELECT label FROM public.bound_items", &[])
        .unwrap();
    assert_eq!(
        result.rows[0]["label"],
        uqa_core::Value::Str("first".into())
    );
}

#[test]
fn view_binding_preserves_implicit_pg_catalog_precedence() {
    let eng = Engine::new();
    eng.sql("CREATE TABLE public.pg_class (marker TEXT)", &[])
        .unwrap();
    eng.sql(
        "CREATE VIEW public.catalog_classes AS SELECT oid FROM pg_class",
        &[],
    )
    .unwrap();

    let plan = eng.view("public.catalog_classes").unwrap().unwrap();
    let uqa_planner::RelationalPlan::QueryBlock(block) = plan.root else {
        panic!("expected query block");
    };
    let Some(uqa_planner::SourcePlan::Table { name, .. }) = block.from else {
        panic!("expected table source");
    };
    assert_eq!(name, "pg_catalog.pg_class");
}
