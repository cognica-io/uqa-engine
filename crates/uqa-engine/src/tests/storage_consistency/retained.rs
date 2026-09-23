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

#[test]
fn current_memory_text_capture_uses_session_retention_in_both_adapters() {
    let engine = Engine::new();
    engine.sql("CREATE TABLE memory_text (id INTEGER PRIMARY KEY, body TEXT); CREATE INDEX memory_text_index ON memory_text USING gin (body); INSERT INTO memory_text VALUES (1, 'original original')", &[]).unwrap();
    let live = engine.require_table("memory_text").unwrap();
    let id = engine.table_doc_ids("memory_text").unwrap()[0];
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
    let metadata = live
        .inverted_index
        .read()
        .indexed_field_metadata(id, "body")
        .unwrap();
    let control = engine.query_retention_control().unwrap();
    let view = engine.detach_query_table(&live, &live, None).unwrap();
    let context = engine.snapshot_context("memory_text").unwrap().unwrap();
    assert!(control.memory().used() > 0);
    let full = control
        .memory()
        .reserve(control.memory().limit() - control.memory().used())
        .unwrap();
    for error in [
        engine.detach_query_table(&live, &live, None).err().unwrap(),
        engine.snapshot_context("memory_text").err().unwrap(),
    ] {
        assert_eq!(error.sqlstate(), Some("53200"), "{error}");
    }
    drop(full);
    engine
        .sql("UPDATE memory_text SET body = 'replacement'", &[])
        .unwrap();
    engine.close().unwrap();
    drop(live);
    drop(engine);
    let nested = view.inverted_index.read().snapshot().unwrap();
    drop(view);
    for reader in [&nested, context.inverted_index.as_ref().unwrap()] {
        assert_eq!(
            reader.get_occurrences(id, "body", &key).unwrap(),
            occurrences
        );
        assert_eq!(reader.indexed_field_metadata(id, "body").unwrap(), metadata);
    }
    control.cancellation().cancel();
    assert!(matches!(
        nested.doc_count(),
        Err(uqa_storage::StorageBackendError::Cancelled(_))
    ));
    drop(context);
    drop(nested);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn current_memory_vector_capture_uses_the_session_allowance_and_preserves_old_readers() {
    for kind in ["memory-bruteforce", "hnsw", "ivf"] {
        let engine = Engine::new();
        engine.sql("CREATE TABLE memory_vectors (id INTEGER PRIMARY KEY, v VECTOR(2)); INSERT INTO memory_vectors VALUES (1, ARRAY[1.0, 0.0])", &[]).unwrap();
        if kind != "memory-bruteforce" {
            engine
                .sql(
                    &format!(
                        "CREATE INDEX memory_vectors_index ON memory_vectors USING {kind} (v)"
                    ),
                    &[],
                )
                .unwrap();
        }
        let live = engine.require_table("memory_vectors").unwrap();
        let id = engine.table_doc_ids("memory_vectors").unwrap()[0];
        let control = engine.query_retention_control().unwrap();
        let view = engine.detach_query_table(&live, &live, None).unwrap();
        assert_eq!(view.vector_indexes.read()["v"].index_kind(), kind);
        assert!(control.memory().used() > 0);
        let full = control
            .memory()
            .reserve(control.memory().limit() - control.memory().used())
            .unwrap();
        for error in [
            engine.detach_query_table(&live, &live, None).err().unwrap(),
            engine.snapshot_context("memory_vectors").err().unwrap(),
        ] {
            assert_eq!(error.sqlstate(), Some("53200"), "{error}");
        }
        assert!(matches!(
            view.vector_indexes.read()["v"].search_knn(&[1.0, 0.0], 1),
            Err(uqa_storage::StorageBackendError::Memory(_))
        ));
        assert!(matches!(
            view.vector_indexes.read()["v"].search_threshold(&[1.0, 0.0], 0.9),
            Err(uqa_storage::StorageBackendError::Memory(_))
        ));
        drop(full);
        engine
            .sql("UPDATE memory_vectors SET v = ARRAY[0.0, 1.0]", &[])
            .unwrap();
        engine.close().unwrap();
        drop(live);
        drop(engine);
        let nested = view.vector_indexes.read()["v"].snapshot().unwrap();
        drop(view);
        control.cancellation().cancel();
        assert!(matches!(
            nested.search_threshold(&[1.0, 0.0], 0.9),
            Err(uqa_storage::StorageBackendError::Cancelled(_))
        ));
        control.cancellation().reset();
        assert_eq!(
            nested
                .search_threshold(&[1.0, 0.0], 0.9)
                .unwrap()
                .doc_ids()
                .collect::<Vec<_>>(),
            [id]
        );
        assert!(control.memory().used() > 0);
        drop(nested);
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn reconstructed_vector_capture_keeps_session_quota_and_the_prior_view_after_rejection() {
    use uqa_execution::query::document_changes::DocumentChanges;
    let (_directory, engines) = engines();
    for engine in engines {
        engine.sql("CREATE TABLE vector_quota (id INTEGER PRIMARY KEY, v VECTOR(2)); CREATE INDEX vector_quota_index ON vector_quota USING hnsw (v); INSERT INTO vector_quota VALUES (1, ARRAY[1.0, 0.0])", &[]).unwrap();
        let live = engine.require_table("vector_quota").unwrap();
        let id = engine.table_doc_ids("vector_quota").unwrap()[0];
        let control = engine.query_retention_control().unwrap();
        let changes =
            DocumentChanges::from_shared([(id.checked_add(1).unwrap(), None)], &control).unwrap();
        let view = engine
            .detach_query_table(&live, &live, Some(changes.clone()))
            .unwrap();
        let full = control
            .memory()
            .reserve(control.memory().limit() - control.memory().used())
            .unwrap();
        let error = engine
            .detach_query_table(&live, &live, Some(changes.clone()))
            .err()
            .unwrap();
        assert_eq!(error.sqlstate(), Some("53200"), "{error}");
        drop(full);
        engine
            .sql("UPDATE vector_quota SET v = ARRAY[0.0, 1.0]", &[])
            .unwrap();
        engine.close().unwrap();
        drop(changes);
        drop(live);
        drop(engine);
        assert_eq!(
            view.vector_indexes.read()["v"]
                .search_threshold(&[1.0, 0.0], 0.9)
                .unwrap()
                .doc_ids()
                .collect::<Vec<_>>(),
            [id]
        );
        assert!(view.vector_indexes.write().live_mut().is_err());
        assert!(control.memory().used() > 0);
        drop(view);
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn reconstructed_text_capture_keeps_session_quota_and_the_prior_view_after_rejection() {
    use uqa_execution::query::document_changes::DocumentChanges;
    let (_directory, engines) = engines();
    for engine in engines {
        engine.sql("CREATE TABLE text_quota (id INTEGER PRIMARY KEY, body TEXT); CREATE INDEX text_quota_index ON text_quota USING gin (body); INSERT INTO text_quota VALUES (1, 'original original')", &[]).unwrap();
        let live = engine.require_table("text_quota").unwrap();
        let id = engine.table_doc_ids("text_quota").unwrap()[0];
        let vocabulary = live.inverted_index.read().vocabulary_keys("body").unwrap();
        assert_eq!(vocabulary.len(), 1);
        let key = vocabulary[0].clone();
        let occurrences = live
            .inverted_index
            .read()
            .get_occurrences(id, "body", &key)
            .unwrap();
        let metadata = live
            .inverted_index
            .read()
            .indexed_field_metadata(id, "body")
            .unwrap();
        assert_eq!(occurrences.len(), 2);
        let control = engine.query_retention_control().unwrap();
        let changes =
            DocumentChanges::from_shared([(id.checked_add(1).unwrap(), None)], &control).unwrap();
        let view = engine
            .detach_query_table(&live, &live, Some(changes.clone()))
            .unwrap();
        assert_eq!(
            view.inverted_index
                .read()
                .get_occurrences(id, "body", &key)
                .unwrap(),
            occurrences
        );
        assert_eq!(
            view.inverted_index
                .read()
                .indexed_field_metadata(id, "body")
                .unwrap(),
            metadata
        );
        let full = control
            .memory()
            .reserve(control.memory().limit() - control.memory().used())
            .unwrap();
        let error = engine
            .detach_query_table(&live, &live, Some(changes.clone()))
            .err()
            .unwrap();
        assert_eq!(error.sqlstate(), Some("53200"), "{error}");
        drop(full);
        engine.sql("BEGIN; UPDATE text_quota SET body = 'rolled back'; ROLLBACK; UPDATE text_quota SET body = 'replacement'", &[]).unwrap();
        let replacement = live.inverted_index.read().vocabulary_keys("body").unwrap();
        assert_eq!(replacement.len(), 1);
        assert_ne!(replacement[0], key);
        engine.close().unwrap();
        drop(changes);
        drop(live);
        drop(engine);
        let nested = view
            .inverted_index
            .read()
            .snapshot()
            .unwrap()
            .snapshot()
            .unwrap();
        assert!(view.inverted_index.write().clear().is_err());
        drop(view);
        assert_eq!(
            nested.get_occurrences(id, "body", &key).unwrap(),
            occurrences
        );
        assert_eq!(nested.indexed_field_metadata(id, "body").unwrap(), metadata);
        assert_eq!(nested.doc_freq_key("body", &replacement[0]).unwrap(), 0);
        assert!(control.memory().used() > 0);
        drop(nested);
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn private_payload_quota_failure_rolls_back_the_statement_and_savepoint_remains_usable() {
    let (_directory, engines) = engines();
    for engine in engines {
        engine.sql("CREATE TABLE payload_quota (id INTEGER PRIMARY KEY, body TEXT); INSERT INTO payload_quota VALUES (1, 'original'); BEGIN; SAVEPOINT before_payload", &[]).unwrap();
        let control = engine.query_retention_control().unwrap();
        let full = control
            .memory()
            .reserve(control.memory().limit() - control.memory().used() - 128 * 1024)
            .unwrap();
        let error = engine
            .sql(
                "INSERT INTO payload_quota VALUES (2, 'small'), (3, $1)",
                &[uqa_sql::SQLParam::Scalar(Value::Str(
                    "x".repeat(512 * 1024),
                ))],
            )
            .unwrap_err();
        assert_eq!(error.sqlstate(), Some("53200"), "{error}");
        drop(full);
        engine.sql("ROLLBACK TO before_payload", &[]).unwrap();
        let rows = engine
            .sql("SELECT id, body FROM payload_quota ORDER BY id", &[])
            .unwrap();
        assert_eq!(rows.rows.len(), 1);
        assert_eq!(rows.value_at(0, 0), Some(&Value::Int(1)));
        assert_eq!(rows.value_at(0, 1), Some(&s("original")));
        engine
            .sql(
                "INSERT INTO payload_quota VALUES (4, 'after recovery'); COMMIT",
                &[],
            )
            .unwrap();
        assert_eq!(
            engine
                .sql("SELECT id FROM payload_quota ORDER BY id", &[])
                .unwrap()
                .rows
                .len(),
            2
        );
    }
}

#[test]
fn retained_query_selection_shares_provider_memory_and_survives_budget_rejection() {
    use uqa_execution::query::document_changes::DocumentSelection;
    let (_directory, engines) = engines();
    for engine in engines {
        engine.sql("CREATE TABLE selected_memory (id INTEGER PRIMARY KEY, body TEXT); INSERT INTO selected_memory VALUES (1, 'original')", &[]).unwrap();
        engine.sql("BEGIN ISOLATION LEVEL REPEATABLE READ; SELECT * FROM selected_memory; UPDATE selected_memory SET body = 'private'", &[]).unwrap();
        let control = engine.query_retention_control().unwrap();
        if let Some(backend) = &engine.storage.backend {
            let provider_control = backend.retention_control().unwrap();
            assert!(control.memory().shares_allowance(provider_control.memory()));
            let reader = backend
                .open_retained_read_session(&engine.runtime.cancellation)
                .unwrap();
            let nested = reader
                .backend
                .open_retained_read_session(&engine.runtime.cancellation)
                .unwrap();
            assert!(nested
                .backend
                .retention_control()
                .unwrap()
                .memory()
                .shares_allowance(control.memory()));
        }
        let table = engine.require_table("selected_memory").unwrap();
        let id = table.document_store.read().doc_ids().unwrap()[0];
        let desired = || {
            let mut rows = DocumentSelection::new(&control);
            rows.insert(id, true, &control).unwrap();
            rows
        };
        let selected = engine
            .capture_query_document_changes(&table, desired())
            .unwrap();
        let retained = selected.snapshot().unwrap();
        let next = desired();
        let full = control
            .memory()
            .reserve(control.memory().limit() - control.memory().used())
            .unwrap();
        let error = engine
            .capture_query_document_changes(&table, next)
            .err()
            .unwrap();
        assert_eq!(error.sqlstate(), Some("53200"));
        drop(full);
        assert_eq!(retained.get_field(id, "body").unwrap(), Some(s("private")));
        assert_eq!(selected.get_field(id, "body").unwrap(), Some(s("private")));
        engine.rollback().unwrap();
        drop(selected);
        drop(table);
        engine.close().unwrap();
        drop(engine);
        assert!(control.memory().used() > 0);
        assert_eq!(retained.get_field(id, "body").unwrap(), Some(s("private")));
        drop(retained);
        assert_eq!(control.memory().used(), 0);
    }
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
fn changed_schema_snapshot_retains_base_rows_until_the_query_reads_them() {
    let engine = Engine::new();
    engine.sql("CREATE TABLE adapted_rows (id INTEGER PRIMARY KEY, payload TEXT); INSERT INTO adapted_rows VALUES (1, 'original'), (2, 'other')", &[]).unwrap();
    let live = engine.require_table("adapted_rows").unwrap();
    let ids = engine.table_doc_ids("adapted_rows").unwrap();
    let base = engine.detach_query_table(&live, &live, None).unwrap();
    let probe = PortalSnapshotProbeStore::from_table(&engine, "adapted_rows");
    let row_reads = Arc::clone(&probe.row_reads);
    let id_reads = Arc::clone(&probe.doc_id_calls);
    let captures = Arc::clone(&probe.snapshot_calls);
    engine
        .sql(
            "ALTER TABLE adapted_rows ADD COLUMN added INTEGER DEFAULT 17",
            &[],
        )
        .unwrap();
    *base.document_store.write() = Box::new(probe);
    let selected = engine.detach_query_table(&base, &live, None).unwrap();
    assert_eq!(row_reads.load(std::sync::atomic::Ordering::Relaxed), 0);
    assert_eq!(id_reads.load(std::sync::atomic::Ordering::Relaxed), 0);
    assert_eq!(captures.load(std::sync::atomic::Ordering::Relaxed), 1);
    assert_eq!(selected.document_store.read().len().unwrap(), 2);
    engine.sql("DROP TABLE adapted_rows", &[]).unwrap();
    engine.close().unwrap();
    drop(engine);
    drop(live);
    drop(base);
    let rows = selected
        .document_store
        .read()
        .get_fields_multi(&ids, &["id", "added"])
        .unwrap();
    assert_eq!(rows[&ids[0]], vec![Value::Int(1), Value::Int(17)]);
    assert_eq!(rows[&ids[1]], vec![Value::Int(2), Value::Int(17)]);
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

#[test]
fn fixed_private_captures_share_rows_without_decoding_or_enumerating_the_source() {
    use std::sync::atomic::Ordering;
    let directory = tempfile::tempdir().unwrap();
    let engine = Engine::open(&directory.path().join("private-capture.db")).unwrap();
    engine.sql("CREATE TABLE private_capture (id INTEGER PRIMARY KEY, body TEXT); INSERT INTO private_capture VALUES (1, 'original'), (2, 'deleted')", &[]).unwrap();
    engine.sql("BEGIN ISOLATION LEVEL REPEATABLE READ; SELECT * FROM private_capture; UPDATE private_capture SET body = 'private' WHERE id = 1; DELETE FROM private_capture WHERE id = 2; INSERT INTO private_capture VALUES (3, 'inserted')", &[]).unwrap();
    let probe = PortalSnapshotProbeStore::from_table(&engine, "private_capture");
    let reads = Arc::clone(&probe.row_reads);
    let ids = Arc::clone(&probe.doc_id_calls);
    let live = engine.require_table("private_capture").unwrap();
    let original = std::mem::replace(&mut *live.document_store.write(), Box::new(probe));
    let first = engine.capture_statement_read_snapshot().unwrap();
    let second = engine.capture_statement_read_snapshot().unwrap();
    let selected = engine.try_query_table("private_capture").unwrap().unwrap();
    let changes = engine
        .command_overlay_changes("private_capture")
        .unwrap()
        .unwrap();
    assert_eq!(reads.load(Ordering::Relaxed), 0);
    assert_eq!(ids.load(Ordering::Relaxed), 0);
    let private_ids = changes.doc_ids().unwrap();
    let projected = engine
        .get_query_document_fields_multi("private_capture", &private_ids, &["body", "xmin"])
        .unwrap();
    assert_eq!(projected.len(), 2);
    assert_eq!(reads.load(Ordering::Relaxed), 0);
    assert_eq!(ids.load(Ordering::Relaxed), 0);
    *live.document_store.write() = original;
    drop(live);
    engine
        .sql("UPDATE private_capture SET body = 'later'; ROLLBACK", &[])
        .unwrap();
    for snapshot in [first, second] {
        let reader = engine.statement_read_snapshot_engine(&snapshot);
        let rows = reader
            .sql("SELECT body FROM private_capture ORDER BY id", &[])
            .unwrap()
            .rows;
        assert_eq!(
            rows.iter()
                .map(|row| row["body"].clone())
                .collect::<Vec<_>>(),
            vec![s("private"), s("inserted")]
        );
    }
    engine.close().unwrap();
    let rows = selected
        .document_store
        .read()
        .get_fields_multi(&private_ids, &["body"])
        .unwrap();
    assert_eq!(
        rows.values().map(|row| row[0].clone()).collect::<Vec<_>>(),
        vec![s("private"), s("inserted")]
    );
}

#[test]
fn successive_private_captures_keep_distinct_boundaries_across_provider_rollback_and_close() {
    let (_directory, engines) = engines();
    for engine in engines {
        engine.sql("CREATE TABLE private_capture (id INTEGER PRIMARY KEY, body TEXT); INSERT INTO private_capture VALUES (1, 'original'), (2, 'deleted')", &[]).unwrap();
        engine.sql("BEGIN ISOLATION LEVEL REPEATABLE READ; SELECT * FROM private_capture; UPDATE private_capture SET body = 'first' WHERE id = 1; DELETE FROM private_capture WHERE id = 2; INSERT INTO private_capture VALUES (3, 'inserted')", &[]).unwrap();
        // Query selection can return the live in-memory handle; explicitly capture it before testing lifetime across later mutations.
        let capture = || {
            let selected = engine.require_query_table("private_capture").unwrap();
            engine
                .detach_query_table(&selected, &selected, None)
                .unwrap()
        };
        let first = capture();
        let snapshot = engine.capture_statement_read_snapshot().unwrap();
        engine.sql("SAVEPOINT private_rows; UPDATE private_capture SET body = 'second' WHERE id = 1; DELETE FROM private_capture WHERE id = 3; INSERT INTO private_capture VALUES (4, 'later')", &[]).unwrap();
        let second = capture();
        engine.sql("ROLLBACK TO private_rows", &[]).unwrap();
        let restored = capture();
        engine.rollback().unwrap();
        let reader = engine.statement_read_snapshot_engine(&snapshot);
        assert_eq!(
            reader
                .sql("SELECT body FROM private_capture ORDER BY id", &[])
                .unwrap()
                .rows
                .iter()
                .map(|row| row["body"].clone())
                .collect::<Vec<_>>(),
            vec![s("first"), s("inserted")]
        );
        drop(reader);
        drop(snapshot);
        engine.close().unwrap();
        drop(engine);
        for (table, expected) in [
            (first, vec![s("first"), s("inserted")]),
            (second, vec![s("second"), s("later")]),
            (restored, vec![s("first"), s("inserted")]),
        ] {
            let store = table.document_store.read();
            let ids = store.next_doc_ids(None, 8).unwrap();
            let rows = store.get_fields_multi(&ids, &["body"]).unwrap();
            assert_eq!(
                rows.values().map(|row| row[0].clone()).collect::<Vec<_>>(),
                expected
            );
        }
    }
}

#[test]
fn private_capture_failures_keep_transaction_diagnostics_and_the_original_view() {
    use uqa_execution::query::document_changes::DocumentChanges;
    let directory = tempfile::tempdir().unwrap();
    let engine = Engine::open(&directory.path().join("capture-errors.db")).unwrap();
    engine.sql("CREATE TABLE capture_errors (id INT PRIMARY KEY, body TEXT); INSERT INTO capture_errors VALUES (1, 'original')", &[]).unwrap();
    engine.sql("BEGIN ISOLATION LEVEL REPEATABLE READ; SELECT * FROM capture_errors; UPDATE capture_errors SET body = 'private'", &[]).unwrap();
    let changes =
        DocumentChanges::from_shared([(2, None)], &engine.query_retention_control().unwrap())
            .unwrap();
    let failures: [(SnapshotErrorFactory, &str); 3] = [
        (|| uqa_core::QueryCancelled.into(), "57014"),
        (
            || uqa_core::memory::MemoryError::SizeOverflow.into(),
            "53200",
        ),
        (
            || {
                uqa_storage::mvcc::VersionError::ReadConflict {
                    dependency: 0,
                    expected: None,
                    actual: None,
                }
                .into_storage_error()
            },
            "40001",
        ),
    ];
    for (failure, state) in failures {
        let mut probe = PortalSnapshotProbeStore::from_table(&engine, "capture_errors");
        probe.snapshot_error = Some(failure);
        let live = engine.require_table("capture_errors").unwrap();
        let original = std::mem::replace(&mut *live.document_store.write(), Box::new(probe));
        let direct_error = engine
            .detach_query_table(&live, &live, None)
            .err()
            .expect("source capture must fail");
        let rebuilt_error = engine
            .detach_query_table(&live, &live, Some(changes.clone()))
            .err()
            .expect("source capture before reconstruction must fail");
        let snapshot_error = engine
            .capture_statement_read_snapshot()
            .err()
            .expect("capture must fail");
        let query_error = engine
            .get_query_document_fields_multi("capture_errors", &[1], &["body"])
            .unwrap_err();
        *live.document_store.write() = original;
        assert_eq!(direct_error.sqlstate(), Some(state), "{direct_error}");
        assert_eq!(rebuilt_error.sqlstate(), Some(state), "{rebuilt_error}");
        assert_eq!(snapshot_error.sqlstate(), Some(state), "{snapshot_error}");
        assert_eq!(query_error.sqlstate(), Some(state), "{query_error}");
        if state == "57014" {
            assert!(matches!(query_error, SQLError::Cancelled(_)));
        }
        assert_eq!(
            engine
                .get_query_document_fields_multi("capture_errors", &[1], &["body"])
                .unwrap()[&1],
            vec![s("private")]
        );
    }
    engine.rollback().unwrap();
    assert_eq!(
        engine
            .sql("SELECT body FROM capture_errors", &[])
            .unwrap()
            .rows[0]["body"],
        s("original")
    );
}
