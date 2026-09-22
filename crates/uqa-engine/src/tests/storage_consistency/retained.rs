//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retained query resources keep their storage visibility and physical index identities.

use super::*;

fn engines() -> (tempfile::TempDir, Vec<Engine>) {
    let directory = tempfile::tempdir().unwrap();
    let engines = vec![
        Engine::new(),
        Engine::open(&directory.path().join("native.db")).unwrap(),
        Engine::from_persistent_provider(Arc::new(
            uqa_storage_sqlite::SQLiteKeyValueStorage::open(&directory.path().join("kv.db"))
                .unwrap(),
        ))
        .unwrap(),
        Engine::from_persistent_provider(Arc::new(
            uqa_storage_redb::RedbStorage::open(directory.path().join("redb.db")).unwrap(),
        ))
        .unwrap(),
    ];
    (directory, engines)
}

fn prepare(engine: &Engine) {
    engine.sql("CREATE TABLE retained (id INTEGER PRIMARY KEY, body TEXT, embedding VECTOR(2)); CREATE INDEX retained_text ON retained USING gin (body); CREATE INDEX retained_vectors ON retained USING hnsw (embedding); INSERT INTO retained VALUES (1, 'original', ARRAY[1.0, 0.0])", &[]).unwrap();
}

#[test]
fn retained_table_keeps_rows_index_kind_and_occurrences_after_source_close() {
    let (_directory, engines) = engines();
    for engine in engines {
        prepare(&engine);
        let live = engine.require_table("retained").unwrap();
        let id = engine.table_doc_ids("retained").unwrap()[0];
        let original = live.document_store.read().get_stored(id).unwrap();
        let kind = live.vector_indexes.read()["embedding"].index_kind();
        let key = live
            .inverted_index
            .read()
            .vocabulary_keys("body")
            .unwrap()
            .remove(0);
        let occurrences = live
            .inverted_index
            .read()
            .get_occurrences(id, "body", &key)
            .unwrap();
        assert!(!occurrences.is_empty());
        let metadata = live
            .inverted_index
            .read()
            .indexed_field_metadata(id, "body")
            .unwrap();
        let retained = engine.detach_query_table(&live, &live, None).unwrap();
        assert_eq!(
            retained.vector_indexes.read()["embedding"].index_kind(),
            kind
        );
        engine.sql("UPDATE retained SET body = 'replacement', embedding = ARRAY[0.0, 1.0]; INSERT INTO retained VALUES (2, 'newcomer', ARRAY[0.0, 1.0])", &[]).unwrap();
        engine.sql("DROP TABLE retained", &[]).unwrap();
        engine.close().unwrap();
        drop(live);
        drop(engine);

        assert_eq!(
            retained.document_store.read().get_stored(id).unwrap(),
            original
        );
        assert_eq!(retained.document_store.read().len().unwrap(), 1);
        let index = retained.inverted_index.read();
        assert_eq!(index.doc_count().unwrap(), 1);
        assert_eq!(
            index.get_occurrences(id, "body", &key).unwrap(),
            occurrences
        );
        assert_eq!(index.indexed_field_metadata(id, "body").unwrap(), metadata);
        let postings = retained.vector_indexes.read()["embedding"]
            .search_knn(&[1.0, 0.0], 1)
            .unwrap();
        assert_eq!(postings.doc_ids().collect::<Vec<_>>(), vec![id]);
        assert!(retained.document_store.write().clear().is_err());
    }
}

#[test]
fn statement_snapshot_keeps_private_rows_through_later_changes_and_rollback() {
    let (_directory, engines) = engines();
    for engine in engines {
        prepare(&engine);
        engine.sql("BEGIN; UPDATE retained SET body = 'private'; INSERT INTO retained VALUES (2, 'inserted', ARRAY[0.0, 1.0])", &[]).unwrap();
        let snapshot = engine.capture_statement_read_snapshot().unwrap();
        engine
            .sql(
                "DELETE FROM retained WHERE id = 2; UPDATE retained SET body = 'later'; ROLLBACK",
                &[],
            )
            .unwrap();
        let reader = engine.statement_read_snapshot_engine(&snapshot);
        let result = reader
            .sql("SELECT body FROM retained ORDER BY id", &[])
            .unwrap();
        assert_eq!(
            result
                .rows
                .iter()
                .map(|row| row["body"].clone())
                .collect::<Vec<_>>(),
            vec![s("private"), s("inserted")]
        );
        assert_eq!(
            engine.sql("SELECT body FROM retained", &[]).unwrap().rows[0]["body"],
            s("original")
        );
    }
}

#[test]
fn current_private_snapshot_does_not_read_rows_to_reapply_its_own_changes() {
    let engine = Engine::new();
    engine.sql("CREATE TABLE private_rows (id INTEGER); BEGIN; INSERT INTO private_rows VALUES (1), (2)", &[]).unwrap();
    let probe = PortalSnapshotProbeStore::from_table(&engine, "private_rows");
    let reads = Arc::clone(&probe.row_reads);
    let ids = Arc::clone(&probe.doc_id_calls);
    let captures = Arc::clone(&probe.snapshot_calls);
    *engine
        .require_table("private_rows")
        .unwrap()
        .document_store
        .write() = Box::new(probe);
    let snapshot = engine.capture_statement_read_snapshot().unwrap();
    assert_eq!(reads.load(std::sync::atomic::Ordering::Relaxed), 0);
    assert_eq!(ids.load(std::sync::atomic::Ordering::Relaxed), 0);
    assert_eq!(captures.load(std::sync::atomic::Ordering::Relaxed), 1);
    let reader = engine.statement_read_snapshot_engine(&snapshot);
    assert_eq!(
        reader
            .sql("SELECT id FROM private_rows ORDER BY id", &[])
            .unwrap()
            .rows
            .iter()
            .map(|row| row["id"].clone())
            .collect::<Vec<_>>(),
        vec![Value::Int(1), Value::Int(2)]
    );
    engine.rollback().unwrap();
}

#[test]
fn cursor_snapshot_preserves_added_defaults_and_virtual_generated_values() {
    let (_directory, engines) = engines();
    for engine in engines {
        engine.sql("CREATE TABLE columns_snapshot (id INTEGER); INSERT INTO columns_snapshot VALUES (1); ALTER TABLE columns_snapshot ADD COLUMN added INTEGER DEFAULT 17; ALTER TABLE columns_snapshot ADD COLUMN computed INTEGER GENERATED ALWAYS AS (id + added) VIRTUAL; BEGIN; DECLARE columns_cursor CURSOR FOR SELECT id, added, computed FROM columns_snapshot", &[]).unwrap();
        engine
            .sql("UPDATE columns_snapshot SET id = 2, added = 19", &[])
            .unwrap();
        let rows = engine
            .sql("FETCH ALL FROM columns_cursor", &[])
            .unwrap()
            .rows;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["id"], Value::Int(1));
        assert_eq!(rows[0]["added"], Value::Int(17));
        assert_eq!(rows[0]["computed"], Value::Int(18));
        engine.rollback().unwrap();
    }
}

#[test]
fn fixed_snapshot_keeps_private_values_after_column_rename_and_name_reuse() {
    let (_directory, engines) = engines();
    for engine in engines {
        engine.sql("CREATE TABLE renamed (id INTEGER PRIMARY KEY, a INTEGER, b INTEGER); INSERT INTO renamed VALUES (1, 10, 20), (2, 11, 21)", &[]).unwrap();
        engine.sql("BEGIN ISOLATION LEVEL REPEATABLE READ; SELECT * FROM renamed; ALTER TABLE renamed RENAME COLUMN a TO old_a; ALTER TABLE renamed ADD COLUMN a INTEGER DEFAULT 17; UPDATE renamed SET a = 99, old_a = 33 WHERE id = 1", &[]).unwrap();
        let rows = engine
            .sql("SELECT id, old_a, a, b FROM renamed ORDER BY id", &[])
            .unwrap()
            .rows;
        assert_eq!(rows[0]["old_a"], Value::Int(33));
        assert_eq!(rows[0]["a"], Value::Int(99));
        assert_eq!(rows[1]["old_a"], Value::Int(11));
        assert_eq!(rows[1]["a"], Value::Int(17));
        engine.sql("SAVEPOINT private_row; UPDATE renamed SET a = 100 WHERE id = 1; ROLLBACK TO private_row; DECLARE renamed_cursor CURSOR FOR SELECT old_a, a FROM renamed WHERE id = 1; UPDATE renamed SET a = 101 WHERE id = 1", &[]).unwrap();
        let row = &engine
            .sql("FETCH ALL FROM renamed_cursor", &[])
            .unwrap()
            .rows[0];
        assert_eq!(row["old_a"], Value::Int(33));
        assert_eq!(row["a"], Value::Int(99));
        engine.rollback().unwrap();
    }
}
