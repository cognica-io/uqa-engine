//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `information_schema.tables` / `information_schema.columns` and
//! `pg_catalog.pg_tables` virtual views.

use uqa_core::Value;
use uqa_engine::Engine;

#[test]
fn information_schema_tables_lists_user_tables() {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE accounts (id INTEGER PRIMARY KEY, balance INTEGER)",
        &[],
    )
    .unwrap();
    eng.sql(
        "CREATE TABLE owners (id INTEGER PRIMARY KEY, name TEXT)",
        &[],
    )
    .unwrap();
    let r = eng
        .sql(
            "SELECT table_name FROM information_schema.tables ORDER BY table_name",
            &[],
        )
        .unwrap();
    let names: Vec<String> = r
        .rows
        .iter()
        .filter_map(|row| match row.get("table_name") {
            Some(uqa_core::Value::Str(s)) => Some(s.clone()),
            _ => None,
        })
        .collect();
    assert!(names.contains(&"accounts".to_string()));
    assert!(names.contains(&"owners".to_string()));
}

#[test]
fn information_schema_columns_lists_each_column() {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE accounts (id INTEGER PRIMARY KEY, balance INTEGER, owner TEXT)",
        &[],
    )
    .unwrap();
    let r = eng
        .sql(
            "SELECT column_name, ordinal_position FROM information_schema.columns \
             WHERE table_name = 'accounts'",
            &[],
        )
        .unwrap();
    assert_eq!(r.rows.len(), 3);
}

#[test]
fn pg_tables_lists_user_tables() {
    let eng = Engine::new();
    eng.sql("CREATE TABLE accounts (id INTEGER PRIMARY KEY)", &[])
        .unwrap();
    let r = eng
        .sql("SELECT tablename FROM pg_catalog.pg_tables", &[])
        .unwrap();
    let names: Vec<String> = r
        .rows
        .iter()
        .filter_map(|row| match row.get("tablename") {
            Some(uqa_core::Value::Str(s)) => Some(s.clone()),
            _ => None,
        })
        .collect();
    assert!(names.contains(&"accounts".to_string()));
}

#[test]
fn information_schema_lists_schemas_views_sequences_and_routines() {
    let eng = Engine::new();
    eng.sql("CREATE SCHEMA app", &[]).unwrap();
    eng.sql(
        "CREATE TABLE app.accounts (id INTEGER PRIMARY KEY, name TEXT)",
        &[],
    )
    .unwrap();
    eng.sql(
        "CREATE VIEW app.account_names AS SELECT name FROM app.accounts",
        &[],
    )
    .unwrap();
    eng.sql("CREATE SEQUENCE app.account_ids", &[]).unwrap();

    let schemas = eng
        .sql(
            "SELECT schema_name FROM information_schema.schemata ORDER BY schema_name",
            &[],
        )
        .unwrap();
    let schema_names: Vec<String> = schemas
        .rows
        .iter()
        .filter_map(|row| match row.get("schema_name") {
            Some(Value::Str(s)) => Some(s.clone()),
            _ => None,
        })
        .collect();
    assert!(schema_names.contains(&"app".to_string()));
    assert!(schema_names.contains(&"pg_catalog".to_string()));
    assert!(schema_names.contains(&"information_schema".to_string()));

    let views = eng
        .sql(
            "SELECT table_schema, table_name FROM information_schema.views \
             WHERE table_schema = 'app'",
            &[],
        )
        .unwrap();
    assert_eq!(views.rows.len(), 1);
    assert_eq!(
        views.rows[0]["table_name"],
        Value::Str("account_names".into())
    );

    let sequences = eng
        .sql(
            "SELECT sequence_schema, sequence_name FROM information_schema.sequences \
             WHERE sequence_schema = 'app'",
            &[],
        )
        .unwrap();
    assert_eq!(sequences.rows.len(), 1);
    assert_eq!(
        sequences.rows[0]["sequence_name"],
        Value::Str("account_ids".into())
    );

    let routines = eng
        .sql(
            "SELECT routine_name FROM information_schema.routines \
             WHERE routine_name = 'text_match'",
            &[],
        )
        .unwrap();
    assert_eq!(routines.rows.len(), 1);
}

#[test]
fn pg_catalog_exposes_namespace_class_and_attribute_rows() {
    let eng = Engine::new();
    eng.sql("CREATE SCHEMA app", &[]).unwrap();
    eng.sql(
        "CREATE TABLE app.accounts (id INTEGER PRIMARY KEY, balance INTEGER, owner TEXT NOT NULL)",
        &[],
    )
    .unwrap();

    let rels = eng
        .sql(
            "SELECT c.relname, c.relkind \
             FROM pg_catalog.pg_class c \
             JOIN pg_catalog.pg_namespace n ON c.relnamespace = n.oid \
             WHERE n.nspname = 'app' AND c.relname = 'accounts'",
            &[],
        )
        .unwrap();
    assert_eq!(rels.rows.len(), 1);
    assert_eq!(rels.rows[0]["relkind"], Value::Str("r".into()));

    let attrs = eng
        .sql(
            "SELECT a.attname, a.attnum, a.attnotnull \
             FROM pg_catalog.pg_attribute a \
             JOIN pg_catalog.pg_class c ON a.attrelid = c.oid \
             JOIN pg_catalog.pg_namespace n ON c.relnamespace = n.oid \
             WHERE n.nspname = 'app' AND c.relname = 'accounts' \
             ORDER BY a.attnum",
            &[],
        )
        .unwrap();
    let attr_names: Vec<String> = attrs
        .rows
        .iter()
        .filter_map(|row| match row.get("attname") {
            Some(Value::Str(s)) => Some(s.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(attr_names, vec!["id", "balance", "owner"]);
    assert_eq!(attrs.rows[0]["attnotnull"], Value::Bool(true));
    assert_eq!(attrs.rows[2]["attnotnull"], Value::Bool(true));
}

#[test]
fn pg_catalog_exposes_indexes_types_functions_and_roles() {
    let eng = Engine::new();
    eng.sql("CREATE TABLE docs (id INTEGER PRIMARY KEY, body TEXT)", &[])
        .unwrap();
    eng.sql("CREATE INDEX docs_body_idx ON docs USING gin (body)", &[])
        .unwrap();
    eng.sql("SET search_path TO public", &[]).unwrap();

    let indexes = eng
        .sql(
            "SELECT schemaname, tablename, indexname, indexdef \
             FROM pg_catalog.pg_indexes \
             WHERE tablename = 'docs'",
            &[],
        )
        .unwrap();
    assert_eq!(indexes.rows.len(), 1);
    assert_eq!(
        indexes.rows[0]["indexname"],
        Value::Str("docs_body_idx".into())
    );
    match &indexes.rows[0]["indexdef"] {
        Value::Str(def) => assert!(def.contains("USING gin")),
        other => panic!("expected indexdef string, got {other:?}"),
    }

    let pg_index = eng
        .sql(
            "SELECT i.indisvalid \
             FROM pg_catalog.pg_index i \
             JOIN pg_catalog.pg_class c ON i.indexrelid = c.oid \
             WHERE c.relname = 'docs_body_idx'",
            &[],
        )
        .unwrap();
    assert_eq!(pg_index.rows.len(), 1);
    assert_eq!(pg_index.rows[0]["indisvalid"], Value::Bool(true));

    let types = eng
        .sql("SELECT typname FROM pg_catalog.pg_type WHERE oid = 23", &[])
        .unwrap();
    assert_eq!(types.rows[0]["typname"], Value::Str("int4".into()));

    let procs = eng
        .sql(
            "SELECT proname FROM pg_catalog.pg_proc WHERE proname = 'deep_predict'",
            &[],
        )
        .unwrap();
    assert_eq!(procs.rows.len(), 1);

    let roles = eng
        .sql("SELECT rolname, rolcanlogin FROM pg_catalog.pg_roles", &[])
        .unwrap();
    assert_eq!(roles.rows[0]["rolname"], Value::Str("uqa".into()));
    assert_eq!(roles.rows[0]["rolcanlogin"], Value::Bool(true));

    let settings = eng
        .sql(
            "SELECT setting FROM pg_catalog.pg_settings WHERE name = 'search_path'",
            &[],
        )
        .unwrap();
    assert_eq!(settings.rows[0]["setting"], Value::Str("public".into()));
}
