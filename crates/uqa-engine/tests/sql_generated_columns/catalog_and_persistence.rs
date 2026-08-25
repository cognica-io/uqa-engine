//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Generated-column catalogs, dependencies, statistics, and persistence.

use super::*;

#[test]
fn generated_column_catalog_metadata_matches_pg18() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE generated_catalog (
                 source INTEGER,
                 virtual_value INTEGER GENERATED ALWAYS AS (source + 1),
                 stored_value INTEGER GENERATED ALWAYS AS (source * 2) STORED
             )",
            &[],
        )
        .unwrap();
    let info = engine
        .sql(
            "SELECT column_name, column_default, is_generated, generation_expression FROM information_schema.columns WHERE table_name = 'generated_catalog' ORDER BY ordinal_position",
            &[],
        )
        .unwrap();
    assert_eq!(info.rows[0]["is_generated"], Value::Str("NEVER".into()));
    assert_eq!(info.rows[0]["generation_expression"], Value::Null);
    assert_eq!(info.rows[1]["column_default"], Value::Null);
    assert_eq!(info.rows[1]["is_generated"], Value::Str("ALWAYS".into()));
    assert_eq!(
        info.rows[1]["generation_expression"],
        Value::Str("(source + 1)".into())
    );
    assert_eq!(
        info.rows[2]["generation_expression"],
        Value::Str("(source * 2)".into())
    );

    let attributes = engine
        .sql(
            "SELECT attname, atthasdef, attgenerated FROM pg_catalog.pg_attribute WHERE attname IN ('source', 'virtual_value', 'stored_value') ORDER BY attnum",
            &[],
        )
        .unwrap();
    assert_eq!(attributes.rows[0]["atthasdef"], Value::Bool(false));
    assert_eq!(
        attributes.rows[0]["attgenerated"],
        Value::Str(String::new())
    );
    assert_eq!(attributes.rows[1]["atthasdef"], Value::Bool(true));
    assert_eq!(attributes.rows[1]["attgenerated"], Value::Str("v".into()));
    assert_eq!(attributes.rows[2]["atthasdef"], Value::Bool(true));
    assert_eq!(attributes.rows[2]["attgenerated"], Value::Str("s".into()));

    let definitions = engine
        .sql(
            "SELECT adnum, adbin FROM pg_catalog.pg_attrdef ORDER BY adnum",
            &[],
        )
        .unwrap();
    assert_eq!(definitions.rows.len(), 2);
    assert_eq!(
        definitions.rows[0]["adbin"],
        Value::Str("(source + 1)".into())
    );
    assert_eq!(
        definitions.rows[1]["adbin"],
        Value::Str("(source * 2)".into())
    );
}

#[test]
fn analyze_excludes_virtual_generated_columns_and_keeps_stored_columns() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE generated_statistics (
                 source INTEGER,
                 virtual_value INTEGER GENERATED ALWAYS AS (source + 1),
                 stored_value INTEGER GENERATED ALWAYS AS (source * 2) STORED
             )",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "INSERT INTO generated_statistics (source) VALUES (1), (2), (3)",
            &[],
        )
        .unwrap();
    engine.run_analyze(Some("generated_statistics")).unwrap();

    let statistics = engine.column_stats("generated_statistics").unwrap();
    assert!(statistics.contains_key("source"));
    assert!(statistics.contains_key("stored_value"));
    assert!(!statistics.contains_key("virtual_value"));
}

#[test]
fn generated_schema_and_values_survive_reopen() {
    let directory = TempDir::new().unwrap();
    let database = directory.path().join("generated.sqlite");
    {
        let engine = Engine::open(&database).unwrap();
        engine
            .sql(
                "CREATE TABLE persistent_generated (
                     id INTEGER PRIMARY KEY,
                     source INTEGER,
                     virtual_value INTEGER GENERATED ALWAYS AS (source + 1),
                     stored_value INTEGER GENERATED ALWAYS AS (source * 2) STORED
                 )",
                &[],
            )
            .unwrap();
        engine
            .sql("INSERT INTO persistent_generated VALUES (1, 4)", &[])
            .unwrap();
    }
    let engine = Engine::open(&database).unwrap();
    let result = engine
        .sql(
            "SELECT virtual_value, stored_value FROM persistent_generated WHERE id = 1",
            &[],
        )
        .unwrap();
    assert_eq!(int(&result.rows[0], "virtual_value"), 5);
    assert_eq!(int(&result.rows[0], "stored_value"), 8);
    engine
        .sql(
            "UPDATE persistent_generated SET source = 7 WHERE id = 1",
            &[],
        )
        .unwrap();
    let result = engine
        .sql(
            "SELECT virtual_value, stored_value FROM persistent_generated WHERE id = 1",
            &[],
        )
        .unwrap();
    assert_eq!(int(&result.rows[0], "virtual_value"), 8);
    assert_eq!(int(&result.rows[0], "stored_value"), 14);
}

#[test]
fn generated_expression_dependencies_follow_rename_and_block_drop() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE generated_dependencies (
                 source INTEGER,
                 derived INTEGER GENERATED ALWAYS AS (generated_dependencies.source + 1) STORED
             )",
            &[],
        )
        .unwrap();
    engine
        .sql("INSERT INTO generated_dependencies VALUES (2)", &[])
        .unwrap();
    engine
        .sql(
            "ALTER TABLE generated_dependencies RENAME COLUMN source TO renamed_source",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "ALTER TABLE generated_dependencies RENAME TO renamed_generated_dependencies",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "UPDATE renamed_generated_dependencies SET renamed_source = 4",
            &[],
        )
        .unwrap();
    let result = engine
        .sql("SELECT derived FROM renamed_generated_dependencies", &[])
        .unwrap();
    assert_eq!(int(&result.rows[0], "derived"), 5);
    let error = engine
        .sql(
            "ALTER TABLE renamed_generated_dependencies DROP COLUMN renamed_source",
            &[],
        )
        .unwrap_err()
        .to_string();
    assert!(error.contains("generation expression"), "{error}");
    assert!(engine
        .table_has_column("renamed_generated_dependencies", "renamed_source")
        .unwrap());
}

#[test]
fn generated_expression_binding_preserves_quoted_dotted_identifiers() {
    let engine = Engine::new();
    engine.sql("CREATE SCHEMA generated_ns", &[]).unwrap();
    engine
        .sql(
            "CREATE TABLE generated_ns.\"base.table\" (
                 \"source.value\" INTEGER,
                 derived INTEGER GENERATED ALWAYS AS (\"base.table\".\"source.value\" + 1) STORED
             )",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "INSERT INTO generated_ns.\"base.table\" (\"source.value\") VALUES (4)",
            &[],
        )
        .unwrap();
    let selected = engine
        .sql(
            "SELECT \"source.value\", derived FROM generated_ns.\"base.table\"",
            &[],
        )
        .unwrap();
    assert_eq!(selected.rows[0]["source.value"], Value::Int(4));
    assert_eq!(selected.rows[0]["derived"], Value::Int(5));

    let error = engine
        .sql(
            "CREATE TABLE generated_ns.invalid_qualifier (
                 source INTEGER,
                 derived INTEGER GENERATED ALWAYS AS (\"other.table\".source + 1) STORED
             )",
            &[],
        )
        .unwrap_err();
    assert!(matches!(error, uqa_sql::SQLError::UnknownTable(name) if name == "other.table"));
}

#[test]
fn generated_dependencies_block_base_type_changes_but_allow_generated_type_changes() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE generated_type_changes (
                 source INTEGER,
                 derived INTEGER GENERATED ALWAYS AS (source + 1) STORED
             )",
            &[],
        )
        .unwrap();
    engine
        .sql("INSERT INTO generated_type_changes VALUES (1)", &[])
        .unwrap();

    let error = engine
        .sql(
            "ALTER TABLE generated_type_changes ALTER COLUMN source TYPE TEXT",
            &[],
        )
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("generated column(s) `derived` depend"),
        "{error}"
    );
    let unchanged = engine
        .sql("SELECT source, derived FROM generated_type_changes", &[])
        .unwrap();
    assert_eq!(int(&unchanged.rows[0], "source"), 1);
    assert_eq!(int(&unchanged.rows[0], "derived"), 2);

    engine
        .sql(
            "ALTER TABLE generated_type_changes ALTER COLUMN derived TYPE TEXT",
            &[],
        )
        .unwrap();
    let changed = engine
        .sql("SELECT source, derived FROM generated_type_changes", &[])
        .unwrap();
    assert_eq!(changed.rows[0]["derived"], Value::Str("2".into()));
}

#[test]
fn stored_generated_primary_keys_are_remapped_and_failed_rewrites_roll_back() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE generated_primary (
                 source INTEGER,
                 derived INTEGER GENERATED ALWAYS AS (source + 10) STORED PRIMARY KEY
             )",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "INSERT INTO generated_primary (source) VALUES (1), (2)",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "ALTER TABLE generated_primary ALTER COLUMN derived SET EXPRESSION AS (source + 20)",
            &[],
        )
        .unwrap();
    assert!(engine
        .get_document("generated_primary", 11)
        .unwrap()
        .is_none());
    assert_eq!(
        engine
            .get_document("generated_primary", 21)
            .unwrap()
            .unwrap()["derived"],
        Value::Int(21)
    );

    engine
        .sql(
            "CREATE TABLE generated_parent (
                 source INTEGER,
                 derived INTEGER GENERATED ALWAYS AS (source + 10) STORED UNIQUE
             )",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "CREATE TABLE generated_child (parent_value INTEGER REFERENCES generated_parent(derived))",
            &[],
        )
        .unwrap();
    engine
        .sql("INSERT INTO generated_parent (source) VALUES (1)", &[])
        .unwrap();
    engine
        .sql("INSERT INTO generated_child VALUES (11)", &[])
        .unwrap();
    let error = engine
        .sql(
            "ALTER TABLE generated_parent ALTER COLUMN derived SET EXPRESSION AS (source + 20)",
            &[],
        )
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("23503"), "{error}");
    let parent = engine
        .sql("SELECT derived FROM generated_parent", &[])
        .unwrap();
    assert_eq!(int(&parent.rows[0], "derived"), 11);
    let catalog = engine
        .sql(
            "SELECT generation_expression FROM information_schema.columns WHERE table_name = 'generated_parent' AND column_name = 'derived'",
            &[],
        )
        .unwrap();
    assert_eq!(
        catalog.rows[0]["generation_expression"],
        Value::Str("(source + 10)".into())
    );
}

#[test]
fn persistent_generated_expression_rewrites_commit_or_roll_back_as_one_change() {
    let directory = TempDir::new().unwrap();
    let database = directory.path().join("generated-rewrite-atomicity.sqlite");
    {
        let engine = Engine::open(&database).unwrap();
        engine
            .sql(
                "CREATE TABLE persistent_generated_parent (
                     source INTEGER,
                     derived INTEGER GENERATED ALWAYS AS (source + 10) STORED UNIQUE
                 )",
                &[],
            )
            .unwrap();
        engine
            .sql(
                "CREATE TABLE persistent_generated_child (
                     parent_value INTEGER REFERENCES persistent_generated_parent(derived)
                 )",
                &[],
            )
            .unwrap();
        engine
            .sql(
                "INSERT INTO persistent_generated_parent (source) VALUES (1)",
                &[],
            )
            .unwrap();
        engine
            .sql("INSERT INTO persistent_generated_child VALUES (11)", &[])
            .unwrap();

        let error = engine
            .sql(
                "ALTER TABLE persistent_generated_parent ALTER COLUMN derived SET EXPRESSION AS (source + 20)",
                &[],
            )
            .unwrap_err();
        assert_eq!(error.sqlstate(), Some("23503"), "{error}");
    }
    {
        let engine = Engine::open(&database).unwrap();
        let unchanged = engine
            .sql("SELECT derived FROM persistent_generated_parent", &[])
            .unwrap();
        assert_eq!(unchanged.rows[0]["derived"], Value::Int(11));
        let catalog = engine
            .sql(
                "SELECT generation_expression FROM information_schema.columns WHERE table_name = 'persistent_generated_parent' AND column_name = 'derived'",
                &[],
            )
            .unwrap();
        assert_eq!(
            catalog.rows[0]["generation_expression"],
            Value::Str("(source + 10)".into())
        );

        engine
            .sql("DELETE FROM persistent_generated_child", &[])
            .unwrap();
        engine
            .sql(
                "ALTER TABLE persistent_generated_parent ALTER COLUMN derived SET EXPRESSION AS (source + 20)",
                &[],
            )
            .unwrap();
    }
    let engine = Engine::open(&database).unwrap();
    let changed = engine
        .sql("SELECT derived FROM persistent_generated_parent", &[])
        .unwrap();
    assert_eq!(changed.rows[0]["derived"], Value::Int(21));
    let catalog = engine
        .sql(
            "SELECT generation_expression FROM information_schema.columns WHERE table_name = 'persistent_generated_parent' AND column_name = 'derived'",
            &[],
        )
        .unwrap();
    assert_eq!(
        catalog.rows[0]["generation_expression"],
        Value::Str("(source + 20)".into())
    );
}
