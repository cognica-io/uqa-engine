//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn empty_schema_and_public_are_durable_catalog_objects() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("schemas.db");
    {
        let engine = Engine::open(Path::new(&path)).unwrap();
        engine.sql("CREATE SCHEMA empty_app", &[]).unwrap();
        assert!(engine.has_schema("public").unwrap());
        assert!(engine.has_schema("empty_app").unwrap());
    }
    {
        let reopened = Engine::open(Path::new(&path)).unwrap();
        assert_eq!(
            reopened.list_schemas().unwrap(),
            vec!["empty_app".to_string(), "public".to_string()]
        );
        assert!(reopened.tables_in_schema("empty_app").unwrap().is_empty());
    }
}

#[test]
fn relation_names_validate_schema_lifecycle() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("namespace.db");
    {
        let engine = Engine::open(Path::new(&path)).unwrap();
        assert!(engine
            .sql("CREATE TABLE missing_schema.t (id INTEGER)", &[])
            .is_err());
        assert!(!engine.has_table("missing_schema.t").unwrap());

        engine.sql("CREATE SCHEMA app", &[]).unwrap();
        engine
            .sql("CREATE TABLE app.items (id INTEGER)", &[])
            .unwrap();
        assert!(engine.drop_schema("app").is_err());
        assert!(engine.drop_table("app.items").unwrap());
        assert!(engine.drop_schema("app").unwrap());
    }
    let reopened = Engine::open(Path::new(&path)).unwrap();
    assert!(!reopened.has_schema("app").unwrap());
    assert!(reopened.has_schema("public").unwrap());
}

#[test]
fn sqlite_reopen_preserves_structural_ownership_for_every_relation_kind() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("relation-kinds.db");
    {
        let engine = Engine::open(Path::new(&path)).unwrap();
        engine.sql("CREATE SCHEMA app", &[]).unwrap();
        engine
            .sql("CREATE TABLE app.items (id INTEGER PRIMARY KEY)", &[])
            .unwrap();
        engine.sql("INSERT INTO app.items VALUES (1)", &[]).unwrap();
        engine
            .sql("CREATE VIEW app.answer AS SELECT 42 AS value", &[])
            .unwrap();
        engine
            .sql("CREATE SEQUENCE app.item_seq START 10", &[])
            .unwrap();
        engine
            .sql(
                "CREATE SERVER app_mem FOREIGN DATA WRAPPER memory_fdw OPTIONS (kind 'memory')",
                &[],
            )
            .unwrap();
        engine
            .sql(
                "CREATE FOREIGN TABLE app.remote_items (id INTEGER) SERVER app_mem",
                &[],
            )
            .unwrap();
    }

    let reopened = Engine::open(Path::new(&path)).unwrap();
    assert_eq!(
        reopened.sql("SELECT id FROM app.items", &[]).unwrap().rows[0]["id"],
        Value::Int(1)
    );
    assert_eq!(
        reopened
            .sql("SELECT value FROM app.answer", &[])
            .unwrap()
            .rows[0]["value"],
        Value::Int(42)
    );
    assert_eq!(
        reopened
            .sql("SELECT nextval('app.item_seq') AS value", &[])
            .unwrap()
            .rows[0]["value"],
        Value::Int(10)
    );
    assert!(reopened
        .foreign_table("app.remote_items")
        .unwrap()
        .is_some());
    assert!(reopened.drop_schema("app").is_err());

    let connection = ManagedConnection::open(&path).unwrap();
    connection
        .with(|conn| {
            let rows: i64 = conn.query_row(
                "SELECT COUNT(*) FROM _relations WHERE schema_name = 'app'",
                [],
                |row| row.get(0),
            )?;
            assert_eq!(rows, 4);
            let kinds: String = conn.query_row(
                "SELECT group_concat(kind, ',') FROM (
                     SELECT kind FROM _relations WHERE schema_name = 'app' ORDER BY kind
                 )",
                [],
                |row| row.get(0),
            )?;
            assert_eq!(kinds, "foreign_table,sequence,table,view");
            Ok(())
        })
        .unwrap();
}

fn populate_quoted_dot_relations(engine: &Engine) {
    for statement in [
        "CREATE SCHEMA \"a.b\"",
        "CREATE SCHEMA a",
        "CREATE TABLE \"a.b\".c (id INTEGER PRIMARY KEY)",
        "CREATE TABLE a.\"b.c\" (id INTEGER PRIMARY KEY)",
        "CREATE TABLE \"a.b\" (id INTEGER PRIMARY KEY)",
        "CREATE TABLE a.b (id INTEGER PRIMARY KEY)",
        "INSERT INTO \"a.b\".c VALUES (11)",
        "INSERT INTO a.\"b.c\" VALUES (22)",
        "INSERT INTO \"a.b\" VALUES (33)",
        "INSERT INTO a.b VALUES (44)",
        "ALTER TABLE \"a.b\".c RENAME TO \"d.e\"",
        "CREATE VIEW \"a.b\".\"v.one\" AS SELECT id FROM \"a.b\".\"d.e\"",
        "CREATE SEQUENCE a.\"s.one\" START 7",
        "CREATE SERVER quoted_mem FOREIGN DATA WRAPPER memory_fdw OPTIONS (kind 'memory')",
        "CREATE FOREIGN TABLE a.\"f.one\" (id INTEGER) SERVER quoted_mem",
    ] {
        engine
            .sql(statement, &[])
            .unwrap_or_else(|error| panic!("quoted-dot setup failed for `{statement}`: {error}"));
    }
    assert!(engine.sql("CREATE SEQUENCE \"a.b\".\"d.e\"", &[]).is_err());
}

fn assert_quoted_dot_relation_values(engine: &Engine) {
    for (relation, expected) in [
        ("\"a.b\".\"d.e\"", 11),
        ("a.\"b.c\"", 22),
        ("\"a.b\"", 33),
        ("a.b", 44),
    ] {
        let result = engine
            .sql(&format!("SELECT id FROM {relation}"), &[])
            .unwrap();
        assert_eq!(result.rows[0]["id"], Value::Int(expected));
    }
    assert_eq!(
        engine
            .sql("SELECT id FROM \"a.b\".\"v.one\"", &[])
            .unwrap()
            .rows[0]["id"],
        Value::Int(11)
    );
    assert_eq!(
        engine
            .sql("SELECT nextval('a.\"s.one\"') AS value", &[])
            .unwrap()
            .rows[0]["value"],
        Value::Int(7)
    );
    assert!(engine.foreign_table("a.\"f.one\"").unwrap().is_some());
}

fn assert_structural_table_identities(path: &Path) {
    let connection = ManagedConnection::open(path).unwrap();
    let identities = connection
        .with(|conn| {
            let mut stmt = conn.prepare(
                "SELECT schema_name, relation_name FROM _relations WHERE kind = 'table' \
                 ORDER BY schema_name, relation_name",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            Ok(rows.collect::<Result<Vec<_>, _>>()?)
        })
        .unwrap();
    assert_eq!(
        identities,
        vec![
            ("a".to_string(), "b".to_string()),
            ("a".to_string(), "b.c".to_string()),
            ("a.b".to_string(), "d.e".to_string()),
            ("public".to_string(), "a.b".to_string()),
        ]
    );
}

#[test]
fn quoted_dot_relations_are_distinct_through_rename_and_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("quoted-dot-relations.db");
    populate_quoted_dot_relations(&Engine::open(Path::new(&path)).unwrap());
    assert_quoted_dot_relation_values(&Engine::open(Path::new(&path)).unwrap());
    assert_structural_table_identities(&path);
}
