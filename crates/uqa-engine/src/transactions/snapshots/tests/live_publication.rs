//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{capture::capture, *};
use uqa_storage::vector_index::{MemoryVectorIndex, VectorIndex};

fn image(index: &dyn VectorIndex) -> Vec<u8> {
    let mut bytes = Vec::new();
    index
        .write_image(&mut bytes, &StorageReadControl::with_limit(16 << 20))
        .unwrap();
    bytes
}

#[test]
fn idle_capture_selects_published_roots_without_walking_tables_or_vector_fields() {
    let mut allocations = Vec::new();
    for count in [1, 128] {
        let engine = Engine::new();
        for id in 0..count {
            let name = format!("table_{id:04}");
            engine.create_default_table(&name, vec![]).unwrap();
            let table = engine.table(&name).unwrap().unwrap();
            table
                .vector_indexes
                .write()
                .live_mut()
                .unwrap()
                .insert("embedding".into(), Box::new(MemoryVectorIndex::new(2)))
                .unwrap();
        }
        sql(&engine, "CREATE TEMP TABLE private_temp(body text); INSERT INTO private_temp VALUES(repeat('x',1048576))");
        let control = StorageReadControl::with_limit(32 << 20);
        let mut selected = None;
        let measured = allocation_counter::measure(|| selected = Some(capture(&engine, &control)));
        allocations.push((measured.bytes_total, measured.count_total));
        let selected = selected.unwrap();
        assert_eq!(engine.transaction_depth(), 0);
        assert_eq!(selected.names().unwrap().len(), count);
        selected.assert_unread();
        let table = selected
            .table(&RelationIdentity::new("public", "table_0000"))
            .unwrap();
        assert!(table
            .vector_indexes
            .image("embedding", &selected.cut)
            .is_ok());
        drop((table, selected, engine));
        assert_eq!(control.memory().used(), 0);
    }
    assert_eq!(allocations[0], allocations[1], "{allocations:?}");
    assert!(allocations[1].0 < 64 << 10, "{allocations:?}");
}

#[test]
fn detached_table_children_cannot_reenter_idle_selection_after_same_name_replacement() {
    let engine = Engine::new();
    sql(&engine, "CREATE TABLE docs(id integer, embedding vector(2)); INSERT INTO docs VALUES(1,ARRAY[1.0,0.0])");
    let detached = engine.table("docs").unwrap().unwrap();
    let expected = image(&detached.vector_indexes.read()["embedding"]);
    let original_control = StorageReadControl::with_limit(4 << 20);
    let original = capture(&engine, &original_control);
    sql(
        &engine,
        "DROP TABLE docs; CREATE TABLE docs(id integer); INSERT INTO docs VALUES(9)",
    );
    let current_control = StorageReadControl::with_limit(4 << 20);
    let current = capture(&engine, &current_control);
    let admitted = current_control.memory().used();
    detached
        .vector_indexes
        .write()
        .live_mut()
        .unwrap()
        .insert("detached".into(), Box::new(MemoryVectorIndex::new(3)))
        .unwrap();
    detached
        .vector_indexes
        .write()
        .live_mut()
        .unwrap()
        .remove("embedding")
        .unwrap();
    assert_eq!(current_control.memory().used(), admitted);
    current.cut.check().unwrap();
    let saved = original
        .table(&RelationIdentity::new("public", "docs"))
        .unwrap();
    let image = saved
        .vector_indexes
        .image("embedding", &original.cut)
        .unwrap();
    let mut bytes = Vec::new();
    image
        .write_image(&mut bytes, &StorageReadControl::with_limit(4 << 20))
        .unwrap();
    assert_eq!(bytes, expected);
    let replacement = current
        .table(&RelationIdentity::new("public", "docs"))
        .unwrap();
    assert_ne!(replacement.object_id, saved.object_id);
    assert_eq!(replacement.vector_indexes.fields().unwrap().count(), 0);
    assert_eq!(
        replacement.document_store.get_field(1, "id").unwrap(),
        Some(Value::Int(9))
    );
    drop((
        detached,
        engine,
        current,
        original,
        saved,
        replacement,
        image,
    ));
    assert_eq!(current_control.memory().used(), 0);
    assert_eq!(original_control.memory().used(), 0);
}

#[rstest::rstest]
#[case("exact")]
#[case("ivf")]
#[case("hnsw")]
#[case("diskann")]
fn idle_publication_tracks_restored_physical_vector_incarnations_and_later_fields(
    #[case] method: &str,
) {
    let engine = Engine::new();
    sql(&engine, "CREATE TABLE docs(id integer, embedding vector(2)); INSERT INTO docs VALUES(1,ARRAY[1.0,0.0]),(2,ARRAY[0.0,1.0])");
    if method != "exact" {
        sql(
            &engine,
            &format!("CREATE INDEX vector_idx ON docs USING {method}(embedding)"),
        );
    }
    let table = engine.table("docs").unwrap().unwrap();
    let expected = image(&table.vector_indexes.read()["embedding"]);
    drop(table);
    sql(&engine, "BEGIN; UPDATE docs SET embedding=ARRAY[-1.0,0.0] WHERE id=1; SAVEPOINT nested; DROP TABLE docs; CREATE TABLE docs(replacement boolean); ROLLBACK TO nested; ROLLBACK; ALTER TABLE docs RENAME TO moved");
    let control = StorageReadControl::with_limit(16 << 20);
    let original = capture(&engine, &control);
    sql(
        &engine,
        "ALTER TABLE moved ADD COLUMN extra vector(3); UPDATE moved SET extra=ARRAY[1.0,0.0,0.0]",
    );
    let changed = capture(&engine, &control);
    drop(engine);
    let table = original
        .table(&RelationIdentity::new("public", "moved"))
        .unwrap();
    let mut bytes = Vec::new();
    table
        .vector_indexes
        .image("embedding", &original.cut)
        .unwrap()
        .write_image(&mut bytes, &StorageReadControl::with_limit(16 << 20))
        .unwrap();
    assert_eq!(bytes, expected, "{method}");
    assert!(table.vector_indexes.image("extra", &original.cut).is_err());
    let changed_table = changed
        .table(&RelationIdentity::new("public", "moved"))
        .unwrap();
    assert_eq!(
        changed_table
            .vector_indexes
            .fields()
            .unwrap()
            .collect::<Vec<_>>(),
        ["embedding", "extra"]
    );
    let extra = changed_table
        .vector_indexes
        .image("extra", &changed.cut)
        .unwrap();
    let mut extra_bytes = Vec::new();
    extra
        .write_image(&mut extra_bytes, &StorageReadControl::with_limit(16 << 20))
        .unwrap();
    assert!(!extra_bytes.is_empty());
    drop((table, changed_table, extra, original, changed));
    assert_eq!(control.memory().used(), 0);
}
