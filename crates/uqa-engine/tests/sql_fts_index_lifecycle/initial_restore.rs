//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Initial catalog preparation and source repairs share one rollback boundary.

use super::{
    create_notes_gin_fixture, rewrite_fts_tables_to_legacy_shape,
    rewrite_fts_tables_to_valid_v21_postings, Engine, Path, TempDir,
};

fn snapshot(database: &Path) -> Vec<(String, Vec<String>)> {
    let db = rusqlite::Connection::open(database).unwrap();
    let mut tables: Vec<String> = db
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    tables.push("sqlite_master".into());
    tables
        .into_iter()
        .map(|table| {
            let quoted = table.replace('"', "\"\"");
            let mut statement = db.prepare(&format!("SELECT * FROM \"{quoted}\"")).unwrap();
            let columns = statement.column_count();
            let mut rows = statement
                .query_map([], |row| {
                    (0..columns)
                        .map(|column| row.get::<_, rusqlite::types::Value>(column))
                        .collect::<Result<Vec<_>, _>>()
                        .map(|values| format!("{values:?}"))
                })
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            rows.sort();
            (table, rows)
        })
        .collect()
}

#[test]
fn failed_analyzer_restore_preserves_legacy_schema_and_every_durable_row() {
    for shape_repair in [false, true] {
        let directory = TempDir::new().unwrap();
        let database = directory.path().join("initial-restore.db");
        create_notes_gin_fixture(&database);
        if shape_repair {
            rewrite_fts_tables_to_legacy_shape(&database);
        } else {
            rewrite_fts_tables_to_valid_v21_postings(&database, "notes");
        }
        let db = rusqlite::Connection::open(&database).unwrap();
        db.execute_batch(
            "ALTER TABLE _analyzers DROP COLUMN descriptor_json;
            ALTER TABLE _table_field_analyzers DROP COLUMN binding_json;
            INSERT INTO _analyzers(name, config_json) VALUES ('broken', '{');",
        )
        .unwrap();
        if shape_repair {
            db.execute(
                "UPDATE _metadata SET value = '46' WHERE key = 'schema_version'",
                [],
            )
            .unwrap();
        }
        drop(db);
        let before = snapshot(&database);
        let Err(error) = Engine::open(&database) else {
            panic!("initial open accepted the invalid analyzer");
        };
        assert!(error.to_string().contains("broken"), "{error}");
        assert_eq!(snapshot(&database), before, "shape repair: {shape_repair}");

        let db = rusqlite::Connection::open(&database).unwrap();
        db.execute("DELETE FROM _analyzers WHERE name = 'broken'", [])
            .unwrap();
        drop(db);
        let engine = Engine::open(&database).unwrap();
        let count = engine.sql("SELECT count(*) AS n FROM notes", &[]).unwrap();
        assert_eq!(count.rows[0]["n"], uqa_core::Value::Int(2));
    }
}

#[test]
fn failed_source_rebuild_restores_replaced_fts_schema_and_all_catalog_changes() {
    let directory = TempDir::new().unwrap();
    let database = directory.path().join("failed-source-rebuild.db");
    create_notes_gin_fixture(&database);
    rewrite_fts_tables_to_legacy_shape(&database);
    let db = rusqlite::Connection::open(&database).unwrap();
    db.execute_batch(
        "CREATE TRIGGER reject_source_rebuild BEFORE UPDATE ON _cache_revisions
        WHEN NEW.kind = 'data' AND NEW.name = 'public.notes'
        BEGIN SELECT RAISE(ABORT, 'forced source rebuild failure'); END;",
    )
    .unwrap();
    drop(db);
    let before = snapshot(&database);
    let Err(error) = Engine::open(&database) else {
        panic!("source rebuild ignored the failing write");
    };
    assert!(
        error.to_string().contains("forced source rebuild failure"),
        "{error}"
    );
    assert_eq!(snapshot(&database), before);
    let db = rusqlite::Connection::open(&database).unwrap();
    db.execute("DROP TRIGGER reject_source_rebuild", [])
        .unwrap();
    drop(db);
    super::assert_legacy_gin_reopens_with_restored_index(&database);
}

#[test]
fn migrated_fts_columns_rename_and_drop_with_retired_legacy_table_shapes() {
    let directory = TempDir::new().unwrap();
    let database = directory.path().join("legacy-column-lifecycle.db");
    create_notes_gin_fixture(&database);
    rewrite_fts_tables_to_legacy_shape(&database);
    let engine = Engine::open(&database).unwrap();
    let before = engine
        .sql(
            "SELECT id FROM notes WHERE text_match(content, 'Learning') ORDER BY id",
            &[],
        )
        .unwrap()
        .rows;
    assert!(!before.is_empty());
    engine
        .sql("ALTER TABLE notes RENAME COLUMN content TO caption", &[])
        .unwrap();
    let after = engine
        .sql(
            "SELECT id FROM notes WHERE text_match(caption, 'Learning') ORDER BY id",
            &[],
        )
        .unwrap();
    assert_eq!(after.rows, before);
    engine
        .sql("ALTER TABLE notes DROP COLUMN caption", &[])
        .unwrap();
    drop(engine);
    let reopened = Engine::open(&database).unwrap();
    assert_eq!(
        reopened
            .sql("SELECT count(*) AS n FROM notes", &[])
            .unwrap()
            .rows[0]["n"],
        uqa_core::Value::Int(2)
    );
}

fn create_default_document_fields(database: &Path) {
    let engine = crate::native_storage::legacy_engine(database);
    engine
        .sql(
            "CREATE TABLE notes (id INTEGER PRIMARY KEY, content TEXT, turn_index INTEGER)",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "INSERT INTO notes VALUES (1, 'retained token', 7), (2, 'another token', 8)",
            &[],
        )
        .unwrap();
    engine.add_fts_field("notes", "content".into()).unwrap();
    engine.add_fts_field("notes", "turn_index".into()).unwrap();
}

#[test]
fn default_document_fields_preserve_non_text_columns_through_migration_and_reopen() {
    for legacy_descriptors in [false, true] {
        let directory = TempDir::new().unwrap();
        let database = directory.path().join("default-document-fields.db");
        create_default_document_fields(&database);
        if legacy_descriptors {
            rewrite_fts_tables_to_valid_v21_postings(&database, "notes");
            let db = rusqlite::Connection::open(&database).unwrap();
            db.execute_batch(
                "DELETE FROM _table_field_analyzers;
                 ALTER TABLE _table_field_analyzers DROP COLUMN binding_json;
                 ALTER TABLE _analyzers DROP COLUMN descriptor_json;
                 UPDATE _metadata SET value = '46' WHERE key = 'schema_version';",
            )
            .unwrap();
        }

        for reopen in 0..2 {
            let engine = Engine::open(&database).unwrap();
            let result = engine
                .sql(
                    "SELECT id, turn_index FROM notes WHERE text_match(content, 'retained')",
                    &[],
                )
                .unwrap();
            assert_eq!(result.rows.len(), 1);
            assert_eq!(result.rows[0]["id"], uqa_core::Value::Int(1));
            assert_eq!(result.rows[0]["turn_index"], uqa_core::Value::Int(7));
            assert!(engine
                .sql(
                    "SELECT id FROM notes WHERE text_match(turn_index, '7')",
                    &[]
                )
                .unwrap()
                .rows
                .is_empty());
            let error = engine
                .set_table_field_analyzer("notes", "turn_index", "standard", "both")
                .unwrap_err();
            assert!(error.contains("must be TEXT"), "{error}");
            if reopen == 0 {
                engine
                    .sql("INSERT INTO notes VALUES (3, 'new token', 9)", &[])
                    .unwrap();
            }
            assert_eq!(
                engine
                    .sql("SELECT count(*) AS n FROM notes", &[])
                    .unwrap()
                    .rows[0]["n"],
                uqa_core::Value::Int(3)
            );
        }
    }
}

#[test]
fn legacy_graph_names_restore_before_document_field_migration_and_reopen() {
    let directory = TempDir::new().unwrap();
    let database = directory.path().join("legacy-graph-and-text.db");
    create_default_document_fields(&database);
    let db = rusqlite::Connection::open(&database).unwrap();
    db.execute_batch(
        "CREATE TABLE _graph_catalog (graph_name TEXT PRIMARY KEY);
         INSERT INTO _graph_catalog VALUES ('legacy_empty');",
    )
    .unwrap();
    drop(db);
    for _ in 0..2 {
        let engine = Engine::open(&database).unwrap();
        assert!(engine.has_graph("legacy_empty").unwrap());
        assert_eq!(
            engine
                .sql("SELECT count(*) AS n FROM notes", &[])
                .unwrap()
                .rows[0]["n"],
            uqa_core::Value::Int(2)
        );
    }
}

#[test]
fn legacy_named_assignment_to_non_text_column_still_rolls_back_initial_restore() {
    let directory = TempDir::new().unwrap();
    let database = directory.path().join("named-non-text-field.db");
    create_default_document_fields(&database);
    let db = rusqlite::Connection::open(&database).unwrap();
    db.execute_batch(
        "DELETE FROM _table_field_analyzers;
         ALTER TABLE _table_field_analyzers DROP COLUMN binding_json;
         ALTER TABLE _analyzers DROP COLUMN descriptor_json;
         INSERT INTO _table_field_analyzers VALUES ('public.notes', 'turn_index', 'both', 'standard');
         UPDATE _metadata SET value = '46' WHERE key = 'schema_version';",
    )
    .unwrap();
    drop(db);
    let before = snapshot(&database);
    let Err(error) = Engine::open(&database) else {
        panic!("accepted an explicit analyzer assignment to an integer column");
    };
    assert!(error.to_string().contains("must be TEXT"), "{error}");
    assert_eq!(snapshot(&database), before);
}

#[test]
fn gin_definition_with_non_text_column_still_rolls_back_initial_restore() {
    for analyzer_option in [false, true] {
        let directory = TempDir::new().unwrap();
        let database = directory.path().join("invalid-gin-field.db");
        create_notes_gin_fixture(&database);
        let db = rusqlite::Connection::open(&database).unwrap();
        let columns: String = db
            .query_row(
                "SELECT columns FROM _tables WHERE relation_name = 'notes'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let mut columns: serde_json::Value = serde_json::from_str(&columns).unwrap();
        for column in columns.as_array_mut().unwrap() {
            if column["name"] == "content" {
                column["ty"] = serde_json::Value::String("Integer".into());
            }
        }
        db.execute(
            "UPDATE _tables SET columns = ?1 WHERE relation_name = 'notes'",
            [columns.to_string()],
        )
        .unwrap();
        if !analyzer_option {
            db.execute(
                "UPDATE _catalog_indexes SET parameters = '{}' WHERE index_type = 'gin'",
                [],
            )
            .unwrap();
        }
        drop(db);
        let before = snapshot(&database);
        let Err(error) = Engine::open(&database) else {
            panic!("accepted a GIN definition over an integer column");
        };
        assert!(error.to_string().contains("must be TEXT"), "{error}");
        assert_eq!(snapshot(&database), before);
    }
}
