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
