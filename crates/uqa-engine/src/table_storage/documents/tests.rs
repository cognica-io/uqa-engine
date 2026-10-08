//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn document(id: i64, value: i64) -> Document {
    BTreeMap::from([
        ("id".into(), Value::Int(id)),
        ("value".into(), Value::Int(value)),
    ])
}

#[test]
fn sql_rewrites_retain_unchanged_hnsw_vectors_and_replace_actual_canonical_differences() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("unchanged-vectors.db");
    let engine = Engine::open(&path).unwrap();
    engine.release_automatic_statistics_client();
    engine
        .session
        .statistics_worker
        .store(true, std::sync::atomic::Ordering::Release);
    engine.sql("CREATE TABLE items (id INTEGER PRIMARY KEY, body TEXT, embedding VECTOR(3)); CREATE INDEX vectors ON items USING hnsw (embedding); INSERT INTO items VALUES (1, 'first', ARRAY[1.0, 0.0, 0.0])", &[]).unwrap();
    let connection = uqa_storage_sqlite::ManagedConnection::open(&path).unwrap();
    let revision = || {
        connection.with_physical(|connection| {
        Ok(connection.query_row("SELECT revision FROM _hnsw_indexes WHERE table_name = 'public.items' AND field = 'embedding'", [], |row| row.get::<_, i64>(0))?)
    }).unwrap()
    };
    let original = revision();
    engine
        .sql("UPDATE items SET body = 'updated' WHERE id = 1", &[])
        .unwrap();
    assert_eq!(
        revision(),
        original,
        "a scalar update must not rewrite the HNSW graph"
    );
    engine.sql("INSERT INTO items VALUES (1, 'metadata upsert', ARRAY[0.0, 0.0, 1.0]) ON CONFLICT (id) DO UPDATE SET body = EXCLUDED.body", &[]).unwrap();
    assert_eq!(
        revision(),
        original,
        "an upsert that excludes the vector must retain its graph"
    );
    engine.sql("INSERT INTO items VALUES (1, 'upserted', ARRAY[1.0, 0.0, 0.0]) ON CONFLICT (id) DO UPDATE SET body = EXCLUDED.body, embedding = EXCLUDED.embedding", &[]).unwrap();
    assert_eq!(
        revision(),
        original,
        "an identical vector assignment must retain its graph"
    );
    assert_eq!(
        engine
            .sql("SELECT body FROM items WHERE id = 1", &[])
            .unwrap()
            .rows[0]["body"],
        Value::Str("upserted".into())
    );
    engine
        .sql(
            "UPDATE items SET embedding = ARRAY[0.0, 1.0, 0.0] WHERE id = 1",
            &[],
        )
        .unwrap();
    assert!(revision() > original);
    let changed = revision();
    engine
        .sql(
            "BEGIN; UPDATE items SET embedding = ARRAY[0.0, 0.0, 1.0] WHERE id = 1; ROLLBACK",
            &[],
        )
        .unwrap();
    assert_eq!(revision(), changed);
    engine
        .sql("UPDATE items SET body = 'after rollback' WHERE id = 1", &[])
        .unwrap();
    assert_eq!(revision(), changed);
    let table = engine.require_table("items").unwrap();
    let document = table.document_store.read().doc_ids().unwrap()[0];
    engine
        .add_vector_values("items", document, "embedding", vec![vec![0.0, 0.0, 1.0]])
        .unwrap();
    let direct = revision();
    engine.sql("UPDATE items SET body = 'restore canonical row', embedding = ARRAY[0.0, 1.0, 0.0] WHERE id = 1", &[]).unwrap();
    assert!(
        revision() > direct,
        "compare the index, not just the unchanged row field"
    );
    engine
        .sql("UPDATE items SET embedding = NULL WHERE id = 1", &[])
        .unwrap();
    let cleared = revision();
    engine
        .sql("UPDATE items SET body = 'empty vector' WHERE id = 1", &[])
        .unwrap();
    assert_eq!(revision(), cleared);
    assert_eq!(
        table
            .vector_indexes
            .read()
            .get("embedding")
            .unwrap()
            .count()
            .unwrap(),
        0
    );
}

#[test]
fn command_payload_rejection_preserves_the_previous_row_and_cached_exact_key() {
    use uqa_execution::query::exact_lookup::FieldPresence;
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE charged_command (id INTEGER PRIMARY KEY, value INTEGER, body TEXT)",
            &[],
        )
        .unwrap();
    engine
        .mutation_coordinator()
        .begin_command_mutation_overlay();
    engine
        .stage_command_document("charged_command", 1, Some(document(1, 10)))
        .unwrap();
    let lookup = |value| {
        engine
            .command_overlay_exact_match(
                "charged_command",
                &["value".into()],
                &[Value::Int(value)],
                FieldPresence::MissingIsNull,
            )
            .unwrap()
    };
    assert_eq!(lookup(10), Some(1));
    let control = engine.query_retention_control().unwrap();
    let before = control.memory().used();
    let full = control
        .memory()
        .reserve(control.memory().limit() - before - 4096)
        .unwrap();
    let mut larger = document(1, 20);
    larger.insert("body".into(), Value::Str("x".repeat(8192)));
    let error = engine
        .stage_command_document("charged_command", 1, Some(larger))
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("53200"));
    drop(full);
    assert_eq!(control.memory().used(), before);
    assert_eq!(lookup(10), Some(1));
    assert_eq!(lookup(20), None);
    let retained = engine
        .command_overlay_changes("charged_command")
        .unwrap()
        .unwrap();
    let with_cache = control.memory().used();
    engine.mutation_coordinator().end_command_mutation_overlay();
    assert_eq!(
        retained.get_field(1, "value").unwrap(),
        Some(Value::Int(10))
    );
    assert!(control.memory().used() > 0);
    assert!(control.memory().used() < with_cache);
    drop(retained);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn command_overlay_scan_merges_persisted_and_staged_rows_in_document_order() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE command_scan (id INTEGER PRIMARY KEY, value INTEGER); INSERT INTO command_scan VALUES (1, 10), (2, 20), (4, 40)",
            &[],
        )
        .unwrap();
    engine
        .mutation_coordinator()
        .begin_command_mutation_overlay();
    engine
        .stage_command_document("command_scan", 2, Some(document(2, 200)))
        .unwrap();
    engine
        .stage_command_document("command_scan", 3, Some(document(3, 300)))
        .unwrap();
    engine
        .stage_command_document("command_scan", 4, None)
        .unwrap();
    engine
        .stage_command_document("command_scan", 5, Some(document(5, 500)))
        .unwrap();

    let result = engine
        .sql("SELECT id, value FROM command_scan ORDER BY id", &[])
        .unwrap();

    assert_eq!(engine.table_doc_count("command_scan").unwrap(), 4);
    assert_eq!(engine.table_doc_ids("command_scan").unwrap(), [1, 2, 3, 5]);
    assert_eq!(result.rows.len(), 4);
    assert_eq!(result.value_at(0, 1), Some(&Value::Int(10)));
    assert_eq!(result.value_at(1, 1), Some(&Value::Int(200)));
    assert_eq!(result.value_at(2, 1), Some(&Value::Int(300)));
    assert_eq!(result.value_at(3, 1), Some(&Value::Int(500)));
    engine.mutation_coordinator().end_command_mutation_overlay();
}

#[test]
fn command_overlay_scan_pages_without_losing_filtered_or_changed_rows() {
    use std::sync::atomic::Ordering;

    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE paged_command_scan (id INTEGER PRIMARY KEY, value INTEGER)",
            &[],
        )
        .unwrap();
    let table = engine.require_table("paged_command_scan").unwrap();
    {
        let mut store = table.document_store.write();
        for doc_id in 1..=2050 {
            let value = i64::try_from(doc_id).unwrap();
            store.put(doc_id, document(value, value)).unwrap();
        }
    }
    table.doc_count_dirty.store(true, Ordering::Release);
    engine
        .mutation_coordinator()
        .begin_command_mutation_overlay();
    engine
        .stage_command_document("paged_command_scan", 2, Some(document(2, 9002)))
        .unwrap();
    engine
        .stage_command_document("paged_command_scan", 1025, Some(document(1025, 9025)))
        .unwrap();
    engine
        .stage_command_document("paged_command_scan", 2048, None)
        .unwrap();
    engine
        .stage_command_document("paged_command_scan", 4096, Some(document(4096, 9999)))
        .unwrap();

    let ids = engine
        .sql(
            "SELECT id FROM paged_command_scan WHERE value >= 2047 ORDER BY id",
            &[],
        )
        .unwrap()
        .rows
        .into_iter()
        .map(|row| row["id"].clone())
        .collect::<Vec<_>>();

    assert_eq!(
        ids,
        [2, 1025, 2047, 2049, 2050, 4096].map(Value::Int).to_vec()
    );
    engine.mutation_coordinator().end_command_mutation_overlay();
}

#[test]
fn overlay_projection_materializes_only_requested_virtual_columns() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE overlay_virtual (id INTEGER PRIMARY KEY, source INTEGER, derived INTEGER GENERATED ALWAYS AS (1 / source) VIRTUAL); INSERT INTO overlay_virtual (id, source) VALUES (1, 1), (2, 2)",
            &[],
        )
        .unwrap();
    engine
        .mutation_coordinator()
        .begin_command_mutation_overlay();
    engine
        .stage_command_document(
            "overlay_virtual",
            1,
            Some(BTreeMap::from([
                ("id".into(), Value::Int(1)),
                ("source".into(), Value::Int(0)),
            ])),
        )
        .unwrap();
    engine
        .stage_command_document("overlay_virtual", 2, None)
        .unwrap();
    engine
        .stage_command_document(
            "overlay_virtual",
            3,
            Some(BTreeMap::from([
                ("id".into(), Value::Int(3)),
                ("source".into(), Value::Int(3)),
            ])),
        )
        .unwrap();

    let documents = engine
        .get_documents_with_materialized_projection(
            "overlay_virtual",
            &[1, 2, 3],
            &["source".into()],
        )
        .unwrap();
    assert_eq!(documents[&1]["source"], Value::Int(0));
    assert!(!documents.contains_key(&2));
    assert_eq!(documents[&3]["source"], Value::Int(3));
    let fields = engine
        .get_query_document_fields_multi("overlay_virtual", &[1, 2, 3], &["source"])
        .unwrap();
    assert_eq!(fields[&1], [Value::Int(0)]);
    assert!(!fields.contains_key(&2));
    assert_eq!(fields[&3], [Value::Int(3)]);
    assert!(engine
        .get_documents_with_materialized_projection("overlay_virtual", &[1], &["derived".into()],)
        .is_err());
    engine.mutation_coordinator().end_command_mutation_overlay();
}

/// Whether a read of `table` would merge changes from the command overlay, and whether the predicate the single-table path asks agrees.
fn overlay_merges(engine: &Engine, table: &str) -> bool {
    let merged = engine
        .command_overlay_changes(table)
        .unwrap()
        .is_some_and(|changes| changes.has_changes());
    assert_eq!(
        engine.command_overlay_holds(table).unwrap(),
        merged,
        "{table}"
    );
    merged
}

#[test]
fn the_command_overlay_holds_exactly_the_tables_a_read_merges() {
    let directory = tempfile::tempdir().unwrap();
    for engine in [
        Engine::new(),
        Engine::open(&directory.path().join("overlay-tables.db")).unwrap(),
    ] {
        engine
            .sql(
                "CREATE TABLE items (id INTEGER PRIMARY KEY, value INTEGER); CREATE TABLE log (id INTEGER PRIMARY KEY, value INTEGER); INSERT INTO items VALUES (1, 1)",
                &[],
            )
            .unwrap();
        // A table the transaction has not written is read as stored at every isolation level.
        for isolation in ["READ COMMITTED", "REPEATABLE READ", "SERIALIZABLE"] {
            engine
                .sql(
                    &format!("BEGIN ISOLATION LEVEL {isolation}; SELECT count(*) FROM items; INSERT INTO log VALUES (1, 1)"),
                    &[],
                )
                .unwrap();
            assert!(!overlay_merges(&engine, "items"), "{isolation}");
            let own_writes = overlay_merges(&engine, "log");
            if isolation == "READ COMMITTED" {
                // Its reads see its own changes in storage.
                assert!(!own_writes);
            }
            engine.sql("ROLLBACK", &[]).unwrap();
        }
        // Documents a running command staged are merged for their table alone.
        engine
            .mutation_coordinator()
            .begin_command_mutation_overlay();
        engine
            .stage_command_document("log", 7, Some(document(7, 70)))
            .unwrap();
        assert!(overlay_merges(&engine, "log"));
        assert!(!overlay_merges(&engine, "items"));
        engine.mutation_coordinator().end_command_mutation_overlay();
        assert!(!overlay_merges(&engine, "log"));
    }
}
