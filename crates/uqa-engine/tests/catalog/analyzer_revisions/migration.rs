//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Original-source migration and catalog publication failure atomicity.

use super::{execute, fixture, hits, synonym_file, Engine, Path, TempDir, KEYWORD};
use uqa_storage_sqlite::{Catalog, ManagedConnection};

fn legacy_catalog(database: &Path, directory: &Path) {
    let engine = Engine::open(database).unwrap();
    fixture(&engine);
    execute(&engine, "INSERT INTO docs VALUES (1, 'seed')");
    drop(engine);
    let index = synonym_file(&directory.join("index.txt"), "seed, stable\n");
    let search = synonym_file(&directory.join("search.txt"), "query, stable\n");
    let invalid_unused_default = serde_json::json!({
        "tokenizer": {"type": "whitespace"},
        "token_filters": [{"type": "synonym", "synonyms_path": directory.join("missing.txt")}],
    })
    .to_string();
    ManagedConnection::open(database).unwrap().with(|db| {
        db.execute_batch("ALTER TABLE _analyzers DROP COLUMN descriptor_json;
            ALTER TABLE _table_field_analyzers DROP COLUMN binding_json;
            UPDATE _metadata SET value = '46' WHERE key = 'schema_version';
            DELETE FROM _table_field_analyzers;
            INSERT INTO _table_field_analyzers VALUES ('public.docs', 'body', 'INDEX', 'indexed'), ('public.docs', 'body', 'query', 'searched');")?;
        db.execute("INSERT INTO _analyzers (name, config_json) VALUES ('indexed', ?1), ('searched', ?2)", [&index, &search])?;
        db.execute("UPDATE _tables SET analyzer = ?1 WHERE schema_name = 'public' AND relation_name = 'docs'", [&invalid_unused_default])?;
        Ok(())
    }).unwrap();
}

#[test]
fn legacy_analyzers_rebuild_from_source_once_and_freeze_both_explicit_sides() {
    let directory = TempDir::new().unwrap();
    let database = directory.path().join("legacy.db");
    legacy_catalog(&database, directory.path());
    let engine = Engine::open(&database).unwrap();
    assert_eq!(hits(&engine, "docs", "body", "query"), [1]);
    let catalog = Catalog::open(ManagedConnection::open(&database).unwrap()).unwrap();
    assert_eq!(catalog.load_analyzer_descriptors().unwrap().len(), 2);
    assert_eq!(
        catalog.load_table_field_analyzer_bindings().unwrap().len(),
        1
    );
    drop(engine);
    std::fs::remove_file(directory.path().join("index.txt")).unwrap();
    std::fs::remove_file(directory.path().join("search.txt")).unwrap();
    let reopened = Engine::open(&database).unwrap();
    execute(&reopened, "INSERT INTO docs VALUES (2, 'seed')");
    assert_eq!(hits(&reopened, "docs", "body", "query"), [1, 2]);
}

#[test]
fn failed_legacy_binding_publication_rolls_back_descriptors_and_rebuilt_postings() {
    let directory = TempDir::new().unwrap();
    let database = directory.path().join("failed-migration.db");
    legacy_catalog(&database, directory.path());
    let connection = ManagedConnection::open(&database).unwrap();
    let catalog = Catalog::open(connection.clone()).unwrap();
    let before_labels = catalog.load_table_field_analyzers().unwrap();
    connection.with(|db| {
        db.execute_batch("CREATE TRIGGER reject_binding BEFORE INSERT ON _table_field_analyzers BEGIN SELECT RAISE(ABORT, 'forced binding failure'); END;")?;
        Ok(())
    }).unwrap();
    let Err(error) = Engine::open(&database) else {
        panic!("migration accepted a failing binding write")
    };
    assert!(
        error.to_string().contains("forced binding failure"),
        "{error}"
    );
    assert!(catalog.load_analyzer_descriptors().unwrap().is_empty());
    assert!(catalog
        .load_table_field_analyzer_bindings()
        .unwrap()
        .is_empty());
    assert_eq!(catalog.load_table_field_analyzers().unwrap(), before_labels);
    connection
        .with(|db| {
            let count: i64 = db.query_row(
                "SELECT count(*) FROM _posting_clusters WHERE term = 'stable'",
                [],
                |row| row.get(0),
            )?;
            assert_eq!(count, 0, "failed migration published replacement postings");
            db.execute_batch("DROP TRIGGER reject_binding")?;
            Ok(())
        })
        .unwrap();
    assert_eq!(
        hits(&Engine::open(&database).unwrap(), "docs", "body", "query"),
        [1]
    );
}

#[test]
fn failed_analyzer_assignment_restores_the_complete_previous_binding_and_postings() {
    let directory = TempDir::new().unwrap();
    let database = directory.path().join("failed-assignment.db");
    let engine = Engine::open(&database).unwrap();
    fixture(&engine);
    engine.register_named_analyzer("whole", KEYWORD).unwrap();
    execute(&engine, "INSERT INTO docs VALUES (1, 'Alpha Beta')");
    let connection = ManagedConnection::open(&database).unwrap();
    let catalog = Catalog::open(connection.clone()).unwrap();
    let before = catalog.load_table_field_analyzer_bindings().unwrap();
    connection.with(|db| {
        db.execute_batch("CREATE TRIGGER reject_binding BEFORE INSERT ON _table_field_analyzers BEGIN SELECT RAISE(ABORT, 'forced assignment failure'); END;")?;
        Ok(())
    }).unwrap();
    let error = engine
        .set_table_field_analyzer("docs", "body", "whole", "both")
        .unwrap_err();
    assert!(error.contains("forced assignment failure"), "{error}");
    assert_eq!(
        catalog.load_table_field_analyzer_bindings().unwrap(),
        before
    );
    assert_eq!(engine.table_field_analyzer("docs", "body").unwrap(), None);
    assert_eq!(hits(&engine, "docs", "body", "alpha"), [1]);
    drop(engine);
    assert_eq!(
        hits(&Engine::open(&database).unwrap(), "docs", "body", "alpha"),
        [1]
    );
}

#[test]
fn corrupt_binding_is_rejected_before_any_legacy_analyzer_is_migrated() {
    let directory = TempDir::new().unwrap();
    let database = directory.path().join("corrupt.db");
    let engine = Engine::open(&database).unwrap();
    fixture(&engine);
    drop(engine);
    let connection = ManagedConnection::open(&database).unwrap();
    let catalog = Catalog::open(connection.clone()).unwrap();
    catalog.save_analyzer("legacy", KEYWORD).unwrap();
    connection.with(|db| {
        db.execute("UPDATE _table_field_analyzers SET binding_json = json_set(binding_json, '$.search.descriptor.fingerprint', ?1)", ["0".repeat(64)])?;
        Ok(())
    }).unwrap();
    let Err(error) = Engine::open(&database) else {
        panic!("corrupt descriptor reopened")
    };
    assert!(error.to_string().contains("fingerprint"), "{error}");
    assert!(catalog.load_analyzer_descriptors().unwrap().is_empty());
}
