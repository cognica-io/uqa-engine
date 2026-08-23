//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` 18 generated-column DDL, DML, catalog, dependency, and persistence parity.

use std::collections::BTreeMap;
use std::sync::Arc;

use tempfile::TempDir;
use uqa_core::{Predicate, Value};
use uqa_engine::operator_tree_bridge::EngineDriver;
use uqa_engine::Engine;
use uqa_operators::{OperatorTree, SumMonoid};
use uqa_planner::executor::{OperatorOutput, OperatorTreeDriver};
use uqa_sql::ColumnType;

fn int(row: &uqa_sql::ResultRow, column: &str) -> i64 {
    match row.get(column) {
        Some(Value::Int(value)) => *value,
        other => panic!("expected integer column `{column}`, got {other:?}"),
    }
}

#[test]
fn generated_columns_follow_pg18_insert_update_and_read_semantics() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE generated_rows (
                 id INTEGER PRIMARY KEY,
                 source INTEGER,
                 virtual_value INTEGER GENERATED ALWAYS AS (source + 1),
                 stored_value INTEGER GENERATED ALWAYS AS (source * 2) STORED
             )",
            &[],
        )
        .unwrap();

    let inserted = engine
        .sql(
            "INSERT INTO generated_rows VALUES (1, 4, DEFAULT, DEFAULT) RETURNING virtual_value, stored_value",
            &[],
        )
        .unwrap();
    assert_eq!(int(&inserted.rows[0], "virtual_value"), 5);
    assert_eq!(int(&inserted.rows[0], "stored_value"), 8);

    engine
        .sql("INSERT INTO generated_rows VALUES (2, 5)", &[])
        .unwrap();
    let error = engine
        .sql("INSERT INTO generated_rows VALUES (3, 6, 99, DEFAULT)", &[])
        .unwrap_err()
        .to_string();
    assert!(error.contains("only DEFAULT may be assigned"), "{error}");

    let updated = engine
        .sql(
            "UPDATE generated_rows SET source = 10, virtual_value = DEFAULT, stored_value = DEFAULT WHERE id = 1 RETURNING virtual_value, stored_value",
            &[],
        )
        .unwrap();
    assert_eq!(int(&updated.rows[0], "virtual_value"), 11);
    assert_eq!(int(&updated.rows[0], "stored_value"), 20);
    let error = engine
        .sql(
            "UPDATE generated_rows SET stored_value = 7 WHERE id = 1",
            &[],
        )
        .unwrap_err()
        .to_string();
    assert!(error.contains("only DEFAULT may be assigned"), "{error}");

    let selected = engine
        .sql(
            "SELECT id, virtual_value, stored_value FROM generated_rows WHERE virtual_value >= 6 ORDER BY virtual_value DESC",
            &[],
        )
        .unwrap();
    assert_eq!(selected.rows.len(), 2);
    assert_eq!(int(&selected.rows[0], "id"), 1);
    assert_eq!(int(&selected.rows[1], "id"), 2);

    let equality = engine
        .sql("SELECT id FROM generated_rows WHERE virtual_value = 6", &[])
        .unwrap();
    assert_eq!(equality.rows.len(), 1);
    assert_eq!(int(&equality.rows[0], "id"), 2);
}

#[test]
fn implicit_insert_slots_keep_generated_columns_in_declared_order() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE generated_slots (
                 source INTEGER,
                 derived INTEGER GENERATED ALWAYS AS (source * 10),
                 tail INTEGER
             )",
            &[],
        )
        .unwrap();
    engine
        .sql("INSERT INTO generated_slots VALUES (1, DEFAULT, 2)", &[])
        .unwrap();
    engine
        .sql("INSERT INTO generated_slots VALUES (3)", &[])
        .unwrap();
    let error = engine
        .sql("INSERT INTO generated_slots VALUES (4, 5)", &[])
        .unwrap_err()
        .to_string();
    assert!(error.contains("only DEFAULT may be assigned"), "{error}");
    let select_error = engine
        .sql("INSERT INTO generated_slots SELECT 6, 7", &[])
        .unwrap_err()
        .to_string();
    assert!(
        select_error.contains("only DEFAULT may be assigned"),
        "{select_error}"
    );
    engine
        .sql(
            "INSERT INTO generated_slots (source, tail) SELECT 8, 9",
            &[],
        )
        .unwrap();

    let result = engine
        .sql(
            "SELECT source, derived, tail FROM generated_slots ORDER BY source",
            &[],
        )
        .unwrap();
    assert_eq!(result.rows.len(), 3);
    assert_eq!(int(&result.rows[0], "derived"), 10);
    assert_eq!(result.rows[1].get("tail"), Some(&Value::Null));
    assert_eq!(int(&result.rows[2], "derived"), 80);
    assert_eq!(int(&result.rows[2], "tail"), 9);
}

#[test]
fn alter_generated_expression_rewrites_stored_rows_and_preserves_drop_value() {
    let engine = Engine::new();
    engine
        .sql("CREATE TABLE altered_generated (source INTEGER)", &[])
        .unwrap();
    engine
        .sql("INSERT INTO altered_generated VALUES (3), (5)", &[])
        .unwrap();
    engine
        .sql(
            "ALTER TABLE altered_generated ADD COLUMN stored_value INTEGER GENERATED ALWAYS AS (source * 2) STORED",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "ALTER TABLE altered_generated ADD COLUMN virtual_value INTEGER GENERATED ALWAYS AS (source + 1)",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "ALTER TABLE altered_generated ALTER COLUMN stored_value SET EXPRESSION AS (source * 3)",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "ALTER TABLE altered_generated ALTER COLUMN virtual_value SET EXPRESSION AS (source + 4)",
            &[],
        )
        .unwrap();
    let result = engine
        .sql(
            "SELECT source, stored_value, virtual_value FROM altered_generated ORDER BY source",
            &[],
        )
        .unwrap();
    assert_eq!(int(&result.rows[0], "stored_value"), 9);
    assert_eq!(int(&result.rows[0], "virtual_value"), 7);
    assert_eq!(int(&result.rows[1], "stored_value"), 15);
    assert_eq!(int(&result.rows[1], "virtual_value"), 9);

    let virtual_drop = engine
        .sql(
            "ALTER TABLE altered_generated ALTER COLUMN virtual_value DROP EXPRESSION",
            &[],
        )
        .unwrap_err()
        .to_string();
    assert!(virtual_drop.contains("virtual generated"), "{virtual_drop}");
    engine
        .sql(
            "ALTER TABLE altered_generated ALTER COLUMN stored_value DROP EXPRESSION",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "UPDATE altered_generated SET source = 10 WHERE source = 3",
            &[],
        )
        .unwrap();
    let retained = engine
        .sql(
            "SELECT stored_value FROM altered_generated WHERE source = 10",
            &[],
        )
        .unwrap();
    assert_eq!(int(&retained.rows[0], "stored_value"), 9);
}

#[test]
fn generated_column_validation_is_failure_atomic_and_rejects_virtual_indexes() {
    let engine = Engine::new();
    for (table, sql, expected) in [
        (
            "volatile_generated",
            "CREATE TABLE volatile_generated (source INTEGER, derived DOUBLE PRECISION GENERATED ALWAYS AS (random()))",
            "not immutable",
        ),
        (
            "volatile_range_generated",
            "CREATE TABLE volatile_range_generated (source INTEGER, derived INTEGER GENERATED ALWAYS AS (random(1, 10)))",
            "not immutable",
        ),
        (
            "chained_generated",
            "CREATE TABLE chained_generated (source INTEGER, first_value INTEGER GENERATED ALWAYS AS (source + 1), second_value INTEGER GENERATED ALWAYS AS (first_value + 1))",
            "cannot use generated column",
        ),
        (
            "unknown_function_generated",
            "CREATE TABLE unknown_function_generated (source INTEGER, derived INTEGER GENERATED ALWAYS AS (missing_function(source)))",
            "unknown function",
        ),
        (
            "keyed_virtual_generated",
            "CREATE TABLE keyed_virtual_generated (source INTEGER, derived INTEGER GENERATED ALWAYS AS (source + 1) PRIMARY KEY)",
            "primary keys on virtual generated columns",
        ),
    ] {
        let error = match engine.sql(sql, &[]) {
            Ok(_) => panic!("{table} unexpectedly succeeded"),
            Err(error) => error.to_string(),
        };
        assert!(error.to_ascii_lowercase().contains(expected), "{error}");
        assert!(!engine.has_table(table).unwrap());
    }

    engine
        .sql(
            "CREATE TABLE generated_indexes (
                 source INTEGER,
                 virtual_value INTEGER GENERATED ALWAYS AS (source + 1),
                 stored_value INTEGER GENERATED ALWAYS AS (source + 2) STORED
             )",
            &[],
        )
        .unwrap();
    let error = engine
        .sql(
            "CREATE INDEX generated_virtual_idx ON generated_indexes (virtual_value)",
            &[],
        )
        .unwrap_err()
        .to_string();
    assert!(error.contains("virtual generated"), "{error}");
    assert!(engine.list_catalog_indexes().unwrap().is_empty());
    engine
        .sql(
            "CREATE INDEX generated_stored_idx ON generated_indexes (stored_value)",
            &[],
        )
        .unwrap();
}

#[test]
fn generated_expression_types_are_resolved_before_catalog_mutation() {
    let engine = Engine::new();
    for (table, sql) in [
        (
            "generated_boolean_mismatch",
            "CREATE TABLE generated_boolean_mismatch (source INTEGER, derived BOOLEAN GENERATED ALWAYS AS (source + 1) STORED)",
        ),
        (
            "generated_invalid_literal",
            "CREATE TABLE generated_invalid_literal (derived INTEGER GENERATED ALWAYS AS ('not-an-integer') STORED)",
        ),
        (
            "generated_invalid_function_argument",
            "CREATE TABLE generated_invalid_function_argument (source INTEGER, derived TEXT GENERATED ALWAYS AS (lower(source)) STORED)",
        ),
        (
            "generated_text_to_integer",
            "CREATE TABLE generated_text_to_integer (source TEXT, derived INTEGER GENERATED ALWAYS AS (source) STORED)",
        ),
        (
            "generated_unknown_operator",
            "CREATE TABLE generated_unknown_operator (derived INTEGER GENERATED ALWAYS AS ('1' + '2') STORED)",
        ),
        (
            "generated_unknown_common_type",
            "CREATE TABLE generated_unknown_common_type (derived INTEGER GENERATED ALWAYS AS (coalesce('1', '2')) STORED)",
        ),
    ] {
        let error = engine.sql(sql, &[]).unwrap_err().to_string();
        assert!(!error.is_empty());
        assert!(!engine.has_table(table).unwrap(), "{table}: {error}");
    }

    engine
        .sql(
            "CREATE TABLE generated_typed_values (
                 id INTEGER PRIMARY KEY,
                 source INTEGER,
                 source_as_text TEXT GENERATED ALWAYS AS (source) STORED,
                 literal_integer INTEGER GENERATED ALWAYS AS ('1') STORED,
                 literal_boolean BOOLEAN GENERATED ALWAYS AS ('true') STORED,
                 lowered TEXT GENERATED ALWAYS AS (lower(source::text)) STORED
             )",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "INSERT INTO generated_typed_values (id, source) VALUES (1, 42)",
            &[],
        )
        .unwrap();
    let result = engine
        .sql(
            "SELECT source_as_text, literal_integer, literal_boolean, lowered FROM generated_typed_values",
            &[],
        )
        .unwrap();
    assert_eq!(result.rows[0]["source_as_text"], Value::Str("42".into()));
    assert_eq!(result.rows[0]["literal_integer"], Value::Int(1));
    assert_eq!(result.rows[0]["literal_boolean"], Value::Bool(true));
    assert_eq!(result.rows[0]["lowered"], Value::Str("42".into()));
}

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
        .unwrap_err()
        .to_string();
    assert!(error.contains("FOREIGN KEY"), "{error}");
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
        assert!(error.to_string().contains("FOREIGN KEY"), "{error}");
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

#[test]
fn generated_columns_are_recomputed_for_upsert_and_merge() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE generated_mutations (
                 id INTEGER PRIMARY KEY,
                 source INTEGER,
                 virtual_value INTEGER GENERATED ALWAYS AS (source + 1),
                 stored_value INTEGER GENERATED ALWAYS AS (source * 2) STORED
             )",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "INSERT INTO generated_mutations (id, source) VALUES (1, 3)",
            &[],
        )
        .unwrap();
    let upsert = engine
        .sql(
            "INSERT INTO generated_mutations (id, source) VALUES (1, 7) ON CONFLICT (id) DO UPDATE SET source = EXCLUDED.source RETURNING virtual_value, stored_value",
            &[],
        )
        .unwrap();
    assert_eq!(int(&upsert.rows[0], "virtual_value"), 8);
    assert_eq!(int(&upsert.rows[0], "stored_value"), 14);

    engine
        .sql(
            "CREATE TABLE generated_source (id INTEGER PRIMARY KEY, source INTEGER)",
            &[],
        )
        .unwrap();
    engine
        .sql("INSERT INTO generated_source VALUES (1, 9), (2, 4)", &[])
        .unwrap();
    let merged = engine
        .sql(
            "MERGE INTO generated_mutations AS target USING generated_source AS incoming ON target.id = incoming.id WHEN MATCHED THEN UPDATE SET source = incoming.source WHEN NOT MATCHED THEN INSERT VALUES (incoming.id, incoming.source) RETURNING merge_action() AS action, new.virtual_value AS virtual_value, new.stored_value AS stored_value",
            &[],
        )
        .unwrap();
    assert_eq!(merged.rows.len(), 2);
    assert_eq!(
        merged.column_types,
        [
            Some(ColumnType::Text),
            Some(ColumnType::Integer),
            Some(ColumnType::Integer),
        ]
    );
    let update = merged
        .rows
        .iter()
        .find(|row| row.get("action") == Some(&Value::Str("UPDATE".into())))
        .unwrap();
    assert_eq!(int(update, "virtual_value"), 10);
    assert_eq!(int(update, "stored_value"), 18);
    let insert = merged
        .rows
        .iter()
        .find(|row| row.get("action") == Some(&Value::Str("INSERT".into())))
        .unwrap();
    assert_eq!(int(insert, "virtual_value"), 5);
    assert_eq!(int(insert, "stored_value"), 8);
}

#[test]
fn immutable_user_functions_are_stored_only_generation_expressions() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE FUNCTION generated_twice(value INTEGER) RETURNS INTEGER LANGUAGE SQL IMMUTABLE AS 'SELECT value * 2'",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "CREATE TABLE generated_with_function (
                 source INTEGER,
                 derived INTEGER GENERATED ALWAYS AS (generated_twice(source)) STORED
             )",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "INSERT INTO generated_with_function (source) VALUES (6)",
            &[],
        )
        .unwrap();
    let result = engine
        .sql("SELECT derived FROM generated_with_function", &[])
        .unwrap();
    assert_eq!(int(&result.rows[0], "derived"), 12);

    let error = engine
        .sql(
            "CREATE TABLE virtual_with_function (
                 source INTEGER,
                 derived INTEGER GENERATED ALWAYS AS (generated_twice(source))
             )",
            &[],
        )
        .unwrap_err()
        .to_string();
    assert!(error.contains("user-defined function"), "{error}");
    assert!(!engine.has_table("virtual_with_function").unwrap());
}

#[test]
fn generated_function_bindings_select_and_depend_on_exact_overloads() {
    let directory = TempDir::new().unwrap();
    let database = directory.path().join("generated-function-bindings.sqlite");
    {
        let engine = Engine::open(&database).unwrap();
        for sql in [
            "CREATE FUNCTION generated_pick(value INTEGER) RETURNS TEXT LANGUAGE SQL IMMUTABLE AS 'SELECT ''integer'''",
            "CREATE FUNCTION generated_pick(value TEXT) RETURNS TEXT LANGUAGE SQL IMMUTABLE AS 'SELECT ''text'''",
            "CREATE FUNCTION generated_literal_pick(value INTEGER) RETURNS TEXT LANGUAGE SQL IMMUTABLE AS 'SELECT ''integer'''",
            "CREATE FUNCTION generated_literal_pick(value TEXT) RETURNS TEXT LANGUAGE SQL IMMUTABLE AS 'SELECT ''text'''",
            "CREATE TABLE generated_bound_call (id INTEGER PRIMARY KEY, source INTEGER, derived TEXT GENERATED ALWAYS AS (generated_pick(source)) STORED)",
            "CREATE TABLE generated_unknown_call (id INTEGER PRIMARY KEY, derived TEXT GENERATED ALWAYS AS (generated_literal_pick(NULL)) STORED)",
            "INSERT INTO generated_bound_call (id, source) VALUES (1, NULL), (2, 7)",
            "INSERT INTO generated_unknown_call (id) VALUES (1)",
        ] {
            engine.sql(sql, &[]).unwrap();
        }
        let bound = engine
            .sql(
                "SELECT id, derived FROM generated_bound_call ORDER BY id",
                &[],
            )
            .unwrap();
        assert_eq!(bound.rows[0]["derived"], Value::Str("integer".into()));
        assert_eq!(bound.rows[1]["derived"], Value::Str("integer".into()));
        let unknown = engine
            .sql("SELECT derived FROM generated_unknown_call", &[])
            .unwrap();
        assert_eq!(unknown.rows[0]["derived"], Value::Str("text".into()));

        engine
            .sql("DROP FUNCTION generated_pick(TEXT)", &[])
            .unwrap();
        engine
            .sql("DROP FUNCTION generated_literal_pick(INTEGER)", &[])
            .unwrap();
    }

    let engine = Engine::open(&database).unwrap();
    for sql in [
        "DROP FUNCTION generated_pick(INTEGER)",
        "DROP FUNCTION generated_literal_pick(TEXT)",
    ] {
        let error = engine.sql(sql, &[]).unwrap_err().to_string();
        assert!(error.contains("generated column"), "{error}");
    }
    engine
        .sql(
            "CREATE OR REPLACE FUNCTION generated_pick(value INTEGER) RETURNS TEXT LANGUAGE SQL VOLATILE AS 'SELECT ''replacement'''",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "INSERT INTO generated_bound_call (id, source) VALUES (3, 9)",
            &[],
        )
        .unwrap();
    let replacement = engine
        .sql("SELECT derived FROM generated_bound_call WHERE id = 3", &[])
        .unwrap();
    assert_eq!(
        replacement.rows[0]["derived"],
        Value::Str("replacement".into())
    );
}

#[test]
fn stored_generation_expression_is_evaluated_once_per_row_write() {
    let engine = Engine::new();
    for sql in [
        "CREATE SEQUENCE generated_evaluation_count START 1",
        "CREATE FUNCTION generated_counted(value INTEGER) RETURNS INTEGER LANGUAGE SQL IMMUTABLE AS 'SELECT nextval(''generated_evaluation_count'')'",
        "CREATE TABLE generated_evaluation_rows (id INTEGER PRIMARY KEY, source INTEGER, derived INTEGER GENERATED ALWAYS AS (generated_counted(source)) STORED)",
        "INSERT INTO generated_evaluation_rows (id, source) VALUES (1, 10)",
    ] {
        engine.sql(sql, &[]).unwrap();
    }
    let inserted = engine
        .sql(
            "SELECT derived, currval('generated_evaluation_count') AS calls FROM generated_evaluation_rows",
            &[],
        )
        .unwrap();
    assert_eq!(int(&inserted.rows[0], "derived"), 1);
    assert_eq!(int(&inserted.rows[0], "calls"), 1);

    engine
        .sql(
            "UPDATE generated_evaluation_rows SET source = 20 WHERE id = 1",
            &[],
        )
        .unwrap();
    let updated = engine
        .sql(
            "SELECT derived, currval('generated_evaluation_count') AS calls FROM generated_evaluation_rows",
            &[],
        )
        .unwrap();
    assert_eq!(int(&updated.rows[0], "derived"), 2);
    assert_eq!(int(&updated.rows[0], "calls"), 2);
}

#[test]
fn virtual_generation_is_deferred_until_required_by_read_or_constraint() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE generated_virtual_late (id INTEGER PRIMARY KEY, source INTEGER, derived INTEGER GENERATED ALWAYS AS (1 / source) VIRTUAL)",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "INSERT INTO generated_virtual_late (id, source) VALUES (1, 0)",
            &[],
        )
        .unwrap();
    let source_only = engine
        .sql("SELECT source FROM generated_virtual_late", &[])
        .unwrap();
    assert_eq!(int(&source_only.rows[0], "source"), 0);
    let error = engine
        .sql("SELECT derived FROM generated_virtual_late", &[])
        .unwrap_err()
        .to_string();
    assert!(
        error.to_ascii_lowercase().contains("division by zero"),
        "{error}"
    );

    engine
        .sql(
            "CREATE TABLE generated_virtual_checked (source INTEGER, derived INTEGER GENERATED ALWAYS AS (source + 1) VIRTUAL, CHECK (derived > 0))",
            &[],
        )
        .unwrap();
    let error = engine
        .sql(
            "INSERT INTO generated_virtual_checked (source) VALUES (-2)",
            &[],
        )
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("23514"));
    assert_eq!(
        error.to_string(),
        "new row for relation \"generated_virtual_checked\" violates check constraint \"generated_virtual_checked_derived_check\""
    );
}

fn generated_operator_fixture() -> Engine {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE generated_operator_rows (
                 id INTEGER PRIMARY KEY,
                 source INTEGER,
                 virtual_group INTEGER GENERATED ALWAYS AS (CASE WHEN source = 2 THEN 0 ELSE 1 END),
                 virtual_value INTEGER GENERATED ALWAYS AS (source * 10),
                 embedding VECTOR(2)
             )",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "INSERT INTO generated_operator_rows (id, source, embedding) VALUES
             (1, 1, ARRAY[1.0, 0.0]),
             (2, 2, ARRAY[0.0, 1.0]),
             (3, 3, ARRAY[0.9, 0.1])",
            &[],
        )
        .unwrap();
    engine
}

fn all_generated_operator_vectors() -> OperatorTree {
    OperatorTree::VectorSimilarity {
        query_vector: vec![1.0, 0.0],
        threshold: -1.0,
        field: "embedding".into(),
    }
}

#[test]
fn operator_filter_and_facet_project_virtual_generated_columns() {
    let engine = generated_operator_fixture();
    let driver = EngineDriver::new(&engine, "generated_operator_rows", &[]);

    let OperatorOutput::Posting(filtered) = driver
        .execute_node(&OperatorTree::Filter {
            field: "virtual_value".into(),
            predicate: Predicate::Equals(Value::Int(20)),
            source: None,
        })
        .unwrap()
    else {
        panic!("generated filter must return a posting list");
    };
    assert_eq!(filtered.doc_ids().collect::<Vec<_>>(), vec![2]);

    let OperatorOutput::Posting(faceted) = driver
        .execute_node(&OperatorTree::Facet {
            field: "virtual_group".into(),
            source: None,
        })
        .unwrap()
    else {
        panic!("generated facet must return a posting list");
    };
    let facet_counts = faceted
        .entries()
        .iter()
        .map(|entry| {
            let Value::Str(value) = &entry.payload.fields["_facet_value"] else {
                panic!("facet value must be text");
            };
            let Value::Int(count) = entry.payload.fields["_facet_count"] else {
                panic!("facet count must be an integer");
            };
            (value.clone(), count)
        })
        .collect::<BTreeMap<_, _>>();
    assert_eq!(
        facet_counts,
        BTreeMap::from([("0".into(), 1), ("1".into(), 2)])
    );
}

#[test]
fn operator_aggregate_and_group_by_project_virtual_generated_columns() {
    let engine = generated_operator_fixture();
    let driver = EngineDriver::new(&engine, "generated_operator_rows", &[]);
    let OperatorOutput::Posting(aggregate) = driver
        .execute_node(&OperatorTree::Aggregate {
            source: None,
            field: "virtual_value".into(),
            monoid: Arc::new(SumMonoid),
        })
        .unwrap()
    else {
        panic!("generated aggregate must return a posting list");
    };
    assert_eq!(
        aggregate.entries()[0].payload.fields.get("_aggregate"),
        Some(&Value::Float(60.0))
    );

    let all_rows = || OperatorTree::Filter {
        field: "id".into(),
        predicate: Predicate::IsNotNull,
        source: None,
    };
    let OperatorOutput::Posting(grouped) = driver
        .execute_node(&OperatorTree::GroupBy {
            source: Box::new(all_rows()),
            group_field: "virtual_group".into(),
            agg_field: "virtual_value".into(),
            monoid: Arc::new(SumMonoid),
        })
        .unwrap()
    else {
        panic!("generated group-by must return a posting list");
    };
    let grouped_values = grouped
        .entries()
        .iter()
        .map(|entry| {
            let Value::Str(key) = &entry.payload.fields["_group_key"] else {
                panic!("group key must be text");
            };
            let Value::Float(value) = entry.payload.fields["_aggregate_result"] else {
                panic!("group aggregate must be numeric");
            };
            (key.clone(), value)
        })
        .collect::<BTreeMap<_, _>>();
    assert_eq!(
        grouped_values,
        BTreeMap::from([("0".into(), 20.0), ("1".into(), 40.0)])
    );
}

#[test]
fn operator_vector_facet_and_join_project_virtual_generated_columns() {
    let engine = generated_operator_fixture();
    let driver = EngineDriver::new(&engine, "generated_operator_rows", &[]);
    let OperatorOutput::Posting(vector_facets) = driver
        .execute_node(&OperatorTree::FacetVector {
            vector_op: Box::new(all_generated_operator_vectors()),
            facet_field: "virtual_group".into(),
        })
        .unwrap()
    else {
        panic!("generated vector facet must return a posting list");
    };
    assert_eq!(vector_facets.len(), 2);

    let hybrid_operand = || {
        OperatorTree::Intersect(vec![
            OperatorTree::Filter {
                field: "virtual_group".into(),
                predicate: Predicate::Equals(Value::Int(1)),
                source: None,
            },
            all_generated_operator_vectors(),
        ])
    };
    let OperatorOutput::Generalized(joined) = driver
        .execute_node(&OperatorTree::HybridJoin {
            left: Box::new(hybrid_operand()),
            right: Box::new(hybrid_operand()),
        })
        .unwrap()
    else {
        panic!("generated hybrid join must return generalized rows");
    };
    assert_eq!(joined.len(), 4);
}

#[test]
fn deep_learning_table_reads_virtual_generated_labels() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE generated_training_rows (
                 id INTEGER PRIMARY KEY,
                 features REAL[],
                 source INTEGER,
                 label INTEGER GENERATED ALWAYS AS (source - 1)
             )",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "INSERT INTO generated_training_rows (id, features, source) VALUES
             (1, ARRAY[2.0, 0.0], 1),
             (2, ARRAY[3.0, 0.0], 1),
             (3, ARRAY[0.0, 2.0], 2),
             (4, ARRAY[0.0, 3.0], 2)",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "SELECT deep_learn('generated-label-model', 'generated_training_rows')",
            &[],
        )
        .unwrap();
    assert!(engine
        .load_model("generated-label-model")
        .unwrap()
        .is_some());
}

#[test]
fn generated_columns_apply_postgresql_assignment_casts() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE generated_temporal_casts (
                 source_date DATE,
                 generated_timestamp TIMESTAMP GENERATED ALWAYS AS (source_date) STORED,
                 source_timestamp TIMESTAMP,
                 generated_date DATE GENERATED ALWAYS AS (source_timestamp) STORED
             )",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "INSERT INTO generated_temporal_casts (source_date, source_timestamp)
             VALUES (DATE '2020-01-02', TIMESTAMP '2020-01-03 04:05:06')",
            &[],
        )
        .unwrap();
    let result = engine
        .sql(
            "SELECT generated_timestamp::text AS generated_timestamp, generated_date::text AS generated_date
             FROM generated_temporal_casts",
            &[],
        )
        .unwrap();
    assert_eq!(
        result.rows[0]["generated_timestamp"],
        Value::Str("2020-01-02 00:00:00".into())
    );
    assert_eq!(
        result.rows[0]["generated_date"],
        Value::Str("2020-01-03".into())
    );

    let error = engine
        .sql(
            "CREATE TABLE generated_uuid_rejects_text (
                 source TEXT,
                 generated UUID GENERATED ALWAYS AS (source) STORED
             )",
            &[],
        )
        .unwrap_err();
    assert!(error.to_string().contains("uuid"));
}

#[test]
fn generated_columns_accept_immutable_uuid_extraction_functions() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE generated_uuid_extraction (
                 source UUID,
                 version SMALLINT GENERATED ALWAYS AS (uuid_extract_version(source)) STORED,
                 extracted_at TIMESTAMPTZ GENERATED ALWAYS AS (uuid_extract_timestamp(source)) STORED
             )",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "INSERT INTO generated_uuid_extraction (source) VALUES ('00000000-0001-7000-8000-000000000000')",
            &[],
        )
        .unwrap();
    let row = engine
        .sql(
            "SELECT version, extracted_at FROM generated_uuid_extraction",
            &[],
        )
        .unwrap()
        .rows
        .pop()
        .unwrap();
    assert_eq!(row["version"], Value::Int(7));
    assert_eq!(
        row["extracted_at"],
        Value::Temporal(uqa_core::TemporalValue::TimestampTz { micros: 1_000 })
    );

    let error = engine
        .sql(
            "CREATE TABLE generated_bad_uuid_extraction (
                 source TEXT,
                 version SMALLINT GENERATED ALWAYS AS (uuid_extract_version(source)) STORED
             )",
            &[],
        )
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("42883"));
    assert_eq!(
        error.to_string(),
        "function uuid_extract_version(text) does not exist"
    );
}

#[test]
fn generated_columns_reject_nonexistent_builtin_signatures() {
    let engine = Engine::new();
    for expression in [
        "cardinality(source, 1)",
        "array_reverse(source, true)",
        "array_remove(source, 1, 2)",
    ] {
        let sql = format!(
            "CREATE TABLE generated_bad_array_signature (source INTEGER[], generated INTEGER GENERATED ALWAYS AS ({expression}) STORED)"
        );
        assert!(engine.sql(&sql, &[]).is_err(), "{expression}");
    }
    assert!(engine
        .sql(
            "CREATE TABLE generated_bad_justify_hours (source DATE, generated INTERVAL GENERATED ALWAYS AS (justify_hours(source)) STORED)",
            &[],
        )
        .is_err());
    assert!(engine
        .sql(
            "CREATE TABLE generated_bad_make_timestamp (source INTEGER, generated TIMESTAMP GENERATED ALWAYS AS (make_timestamp(2020, 1, 1, 0, 0, 0, source)) STORED)",
            &[],
        )
        .is_err());
}

#[test]
fn generated_expressions_preserve_declared_integer_widths() {
    let engine = Engine::new();
    for kind in ["VIRTUAL", "STORED"] {
        let table = format!("generated_width_{}", kind.to_ascii_lowercase());
        engine
            .sql(
                &format!(
                    "CREATE TABLE {table} (source SMALLINT, bytes BYTEA GENERATED ALWAYS AS (source::bytea) {kind})"
                ),
                &[],
            )
            .unwrap();
        engine
            .sql(&format!("INSERT INTO {table} (source) VALUES (-1)"), &[])
            .unwrap();
        let result = engine
            .sql(&format!("SELECT bytes FROM {table}"), &[])
            .unwrap();
        assert_eq!(
            result.rows[0].get("bytes"),
            Some(&Value::Bytes(vec![0xff, 0xff]))
        );
    }
}
