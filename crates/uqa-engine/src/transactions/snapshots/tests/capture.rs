//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::state::SnapshotTableCut;

fn capture(engine: &Engine, control: &StorageReadControl) -> SnapshotTableCut {
    let frames = engine
        .session
        .transactions
        .try_lock()
        .expect("the callback does not retain the transaction-state guard");
    frames[0]
        .data_snapshot
        .as_ref()
        .unwrap()
        .data
        .table_sources
        .as_ref()
        .unwrap()
        .capture(control)
        .unwrap()
}

#[test]
fn committed_table_sources_capture_during_a_private_callback_without_retaining_undo() {
    let engine = Arc::new(Engine::new());
    sql(&engine, "CREATE TABLE docs(id integer PRIMARY KEY, body text); INSERT INTO docs VALUES(1,'committed'); CREATE TEMP TABLE temp_data(payload text); INSERT INTO temp_data VALUES(repeat('t',1048576))");
    let table = engine.table("docs").unwrap().unwrap();
    let original_id = table.object_id();
    let weak = Arc::downgrade(&table);
    drop(table);
    let (entered, started) = mpsc::channel();
    let (release, proceed) = mpsc::channel();
    let worker_engine = engine.clone();
    let worker = std::thread::spawn(move || {
        worker_engine.transaction(|engine| {
            sql(engine, "UPDATE docs SET body=repeat('p',1048576); SAVEPOINT nested; INSERT INTO docs VALUES(2,'private'); ALTER TABLE docs ADD COLUMN extra boolean");
            entered.send(()).unwrap();
            proceed.recv().unwrap();
            Ok(())
        })
    });
    started
        .recv_timeout(std::time::Duration::from_secs(30))
        .unwrap();
    let control = StorageReadControl::with_limit(64 * 1024);
    let selected = capture(&engine, &control);
    assert_eq!(
        selected.names().unwrap(),
        [RelationIdentity::new("public", "docs")]
    );
    let saved = selected
        .table(&RelationIdentity::new("public", "docs"))
        .unwrap();
    saved.assert_original_allowance(control.memory());
    assert_eq!(saved.object_id, original_id);
    assert_eq!(saved.document_store.len().unwrap(), 1);
    assert_eq!(
        saved.document_store.get_field(1, "body").unwrap(),
        Some(Value::Str("committed".into()))
    );
    assert_eq!(saved.columns.get().unwrap().len(), 2);
    assert!(control.memory().used() < 64 * 1024);
    release.send(()).unwrap();
    worker.join().unwrap().unwrap();
    sql(
        &engine,
        "DROP TABLE docs; CREATE TABLE docs(replacement boolean)",
    );
    drop(engine);
    assert!(weak.upgrade().is_none());
    assert_eq!(
        saved.document_store.get_field(1, "body").unwrap(),
        Some(Value::Str("committed".into()))
    );
    drop(selected);
    assert!(control.memory().used() > 0);
    control.cancellation().cancel();
    assert!(saved.document_store.len().is_err());
    assert!(saved.columns.get().is_err());
    drop(saved);
    assert_eq!(control.memory().used(), 0);
}

#[rstest::rstest]
#[case("exact")]
#[case("ivf")]
#[case("hnsw")]
#[case("diskann")]
fn committed_table_sources_preserve_unread_indexes_across_savepoint_commit_and_closure(
    #[case] method: &str,
) {
    let engine = Engine::new();
    sql(&engine, "CREATE TABLE docs(id integer PRIMARY KEY, body text, embedding vector(2)); CREATE INDEX text_idx ON docs USING gin(body); INSERT INTO docs VALUES(1,'original term',ARRAY[1.0,0.0]),(2,'second',ARRAY[0.0,1.0]); CREATE INDEX saved_expression ON docs(lower(body))");
    if method != "exact" {
        sql(
            &engine,
            &format!("CREATE INDEX vector_idx ON docs USING {method}(embedding)"),
        );
    }
    let table = engine.table("docs").unwrap().unwrap();
    let output = StorageReadControl::with_limit(16 << 20);
    let mut expected_text = Vec::new();
    table
        .inverted_index
        .read()
        .write_image_document(1, &mut expected_text, &output)
        .unwrap()
        .unwrap();
    let mut expected_vector = Vec::new();
    table.vector_indexes.read()["embedding"]
        .write_image(&mut expected_vector, &output)
        .unwrap();
    let field = engine
        .value_indexable_fields("docs")
        .unwrap()
        .into_iter()
        .find(|key| matches!(key, uqa_storage::ValueIndexKey::Index(_)))
        .unwrap();
    drop(table);
    sql(&engine, "BEGIN; UPDATE docs SET body='private',embedding=ARRAY[-1.0,0.0] WHERE id=1; SAVEPOINT nested; TRUNCATE docs; ROLLBACK TO nested");
    let control = StorageReadControl::with_limit(32 << 20);
    let selected = capture(&engine, &control);
    sql(
        &engine,
        "COMMIT; DROP TABLE docs; CREATE TABLE docs(replacement boolean)",
    );
    drop(engine);
    let saved = selected
        .table(&RelationIdentity::new("public", "docs"))
        .unwrap();
    let mut text = Vec::new();
    saved
        .inverted_index
        .write_image_document(1, &mut text, &output)
        .unwrap()
        .unwrap();
    assert_eq!(text, expected_text);
    let mut vector = Vec::new();
    let image = saved
        .vector_indexes
        .image("embedding", &selected.cut)
        .unwrap();
    image.write_image(&mut vector, &output).unwrap();
    assert_eq!(vector, expected_vector);
    assert_eq!(
        saved
            .value_indexes
            .get(&field)
            .unwrap()
            .unwrap()
            .value(1)
            .unwrap(),
        Some(&Value::Row(vec![Value::Str("original term".into())].into()))
    );
    assert_eq!(saved.columns.get().unwrap().len(), 3);
    assert_eq!(saved.document_store.len().unwrap(), 2);
    drop((saved, selected));
    assert!(control.memory().used() > 0);
    vector.clear();
    image.write_image(&mut vector, &output).unwrap();
    assert_eq!(vector, expected_vector);
    control.cancellation().cancel();
    assert!(image.write_image(&mut Vec::new(), &output).is_err());
    drop(image);
    assert_eq!(control.memory().used(), 0);
}

#[rstest::rstest]
#[case("COMMIT")]
#[case("ROLLBACK")]
fn committed_source_limits_and_cancellation_do_not_consume_transaction_completion(
    #[case] completion: &str,
) {
    let engine = Engine::new();
    sql(&engine, "CREATE TABLE docs(id integer PRIMARY KEY, body text); INSERT INTO docs VALUES(1,repeat('o',32768))");
    sql(&engine, "BEGIN; UPDATE docs SET body='private'");
    let limited = StorageReadControl::with_limit(16 * 1024);
    let stopped = StorageReadControl::with_limit(1 << 20);
    let funded = StorageReadControl::with_limit(1 << 20);
    let small = capture(&engine, &limited);
    let cancelled = capture(&engine, &stopped);
    let saved = capture(&engine, &funded);
    stopped.cancellation().cancel();
    assert!(small
        .table(&RelationIdentity::new("public", "docs"))
        .is_err());
    sql(&engine, completion);
    assert_eq!(
        engine
            .sql("SELECT body FROM docs", &[])
            .unwrap()
            .value_at(0, 0),
        Some(&Value::Str(if completion == "COMMIT" {
            "private".into()
        } else {
            "o".repeat(32768)
        }))
    );
    drop(engine);
    assert!(small.cut.check().is_err());
    assert!(cancelled.cut.check().is_err());
    assert_eq!(
        saved
            .table(&RelationIdentity::new("public", "docs"))
            .unwrap()
            .document_store
            .get_field(1, "body")
            .unwrap(),
        Some(Value::Str("o".repeat(32768)))
    );
    drop((small, cancelled, saved));
    assert_eq!(limited.memory().used(), 0);
    assert_eq!(stopped.memory().used(), 0);
    assert_eq!(funded.memory().used(), 0);
}

#[test]
fn committed_table_membership_preserves_renames_replacements_and_rollback_identities() {
    let engine = Engine::new();
    sql(&engine, "CREATE TABLE kept(id integer); CREATE UNLOGGED TABLE moving(id integer); INSERT INTO kept VALUES(1); INSERT INTO moving VALUES(2)");
    let original = engine.table("kept").unwrap().unwrap();
    let object_id = original.object_id();
    let oids = original.recorded_catalog_oids();
    drop(original);
    sql(&engine, "BEGIN; ALTER TABLE moving RENAME TO renamed; DROP TABLE kept; CREATE TABLE kept(replacement boolean); CREATE TABLE private_data(id integer); SAVEPOINT nested; INSERT INTO renamed VALUES(3)");
    let control = StorageReadControl::with_limit(1 << 20);
    let selected = capture(&engine, &control);
    assert_eq!(
        selected.names().unwrap(),
        [
            RelationIdentity::new("public", "kept"),
            RelationIdentity::new("public", "moving")
        ]
    );
    selected.assert_unread();
    sql(&engine, "ROLLBACK TO nested; ROLLBACK");
    drop(engine);
    let kept = selected
        .table(&RelationIdentity::new("public", "kept"))
        .unwrap();
    assert_eq!(kept.object_id, object_id);
    assert_eq!(kept.catalog_oids, oids);
    assert_eq!(kept.columns.get().unwrap()[0].name, "id");
    let moving = selected
        .table(&RelationIdentity::new("public", "moving"))
        .unwrap();
    assert_eq!(
        moving.persistence,
        uqa_sql::ast::RelationPersistence::Unlogged
    );
    assert_eq!(moving.document_store.len().unwrap(), 1);
    assert_eq!(
        moving.document_store.get_field(1, "id").unwrap(),
        Some(Value::Int(2))
    );
    drop((kept, moving, selected));
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn committed_table_capture_admission_does_not_walk_or_freeze_the_table_corpus() {
    let mut allocations = Vec::new();
    let mut admissions = Vec::new();
    for count in [1, 128] {
        let engine = Engine::new();
        for id in 0..count {
            engine
                .create_default_table(format!("table_{id}"), vec![])
                .unwrap();
        }
        sql(&engine, "BEGIN");
        let control = StorageReadControl::with_limit(16 << 20);
        let mut saved = None;
        let measured = allocation_counter::measure(|| saved = Some(capture(&engine, &control)));
        let selected = saved.unwrap();
        assert_eq!(selected.names().unwrap().len(), count);
        selected.assert_unread();
        allocations.push((measured.bytes_total, measured.count_total));
        admissions.push(control.memory().used());
        drop(selected);
        sql(&engine, "ROLLBACK");
        assert_eq!(control.memory().used(), 0);
    }
    assert_eq!(allocations[0], allocations[1]);
    assert!(allocations[1].0 < 4096, "{allocations:?}");
    assert!(admissions[1] > admissions[0]);
}

#[test]
fn committed_table_selection_excludes_definitions_created_in_the_outer_message() {
    let engine = Engine::new();
    sql(&engine, "CREATE TABLE private_data(id integer); INSERT INTO private_data VALUES(1); BEGIN; INSERT INTO private_data VALUES(2)");
    let control = StorageReadControl::with_limit(4096);
    let selected = capture(&engine, &control);
    assert!(selected.names().unwrap().is_empty());
    sql(&engine, "COMMIT");
    assert_eq!(
        engine
            .sql("SELECT count(*) FROM private_data", &[])
            .unwrap()
            .value_at(0, 0),
        Some(&Value::Int(2))
    );
    drop((selected, engine));
    assert_eq!(control.memory().used(), 0);
}
