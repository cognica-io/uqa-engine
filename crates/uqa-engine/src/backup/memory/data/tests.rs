//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{state::SnapshotCut, Engine};

fn engine() -> Engine {
    let engine = Engine::new();
    engine.sql("CREATE TABLE saved(id integer PRIMARY KEY, body text, embedding vector(2)); CREATE INDEX text_idx ON saved USING gin(body); CREATE INDEX vector_idx ON saved USING hnsw(embedding); INSERT INTO saved VALUES(1,'original words',ARRAY[1.0,0.0]),(9,'',ARRAY[0.0,1.0]),(20,NULL,NULL)", &[]).unwrap();
    engine
}

fn table(source: &SnapshotCut, control: &StorageReadControl) -> TableOwner {
    let (_, table) = source.next_table(None).unwrap().unwrap();
    super::super::tables::retain(table, control).unwrap()
}

fn bytes(mut reader: ImageReader<'static>) -> Vec<u8> {
    loop {
        let progress = reader.prepare(1).unwrap();
        assert!(progress.positions <= 1);
        if progress.ready {
            break;
        }
    }
    let mut output = Vec::new();
    let mut window = [0; 29];
    loop {
        let count = reader.read(&mut window).unwrap();
        if count == 0 {
            return output;
        }
        output.extend_from_slice(&window[..count]);
    }
}

type DataReader = (u8, String, ImageReader<'static>);

fn readers(
    table: &TableOwner,
    cut: &MemoryCapture,
    control: &StorageReadControl,
) -> Vec<DataReader> {
    let mut result = Vec::new();
    for (kind, select) in [(0, next_document as fn(_, _, _) -> _), (1, next_text)] {
        let mut previous = None;
        while let Some((id, reader)) = select(table, previous, control).unwrap() {
            assert!(previous.is_none_or(|previous| previous < id));
            previous = Some(id);
            result.push((kind, id.to_string(), reader));
        }
    }
    let mut previous: Option<Budgeted<String>> = None;
    while let Some((field, reader)) =
        next_vector(table, cut, previous.as_deref().map(String::as_str), control).unwrap()
    {
        result.push((2, (*field).clone(), reader));
        previous = Some(field);
    }
    result
}

#[test]
fn memory_data_images_preserve_committed_rows_text_and_vectors_after_owner_retirement() {
    let engine = engine();
    let original = StorageReadControl::with_limit(16 << 20);
    let first = engine.capture_committed_memory(&original).unwrap();
    let selected = table(&first, &original);
    let output = StorageReadControl::with_limit(1 << 20);
    let expected: Vec<_> = readers(&selected, &first.cut, &output)
        .into_iter()
        .map(|(kind, key, reader)| (kind, key, bytes(reader)))
        .collect();
    assert_eq!(expected.iter().filter(|entry| entry.0 == 0).count(), 3);
    assert_eq!(expected.iter().filter(|entry| entry.0 == 1).count(), 2);
    assert_eq!(expected.iter().filter(|entry| entry.0 == 2).count(), 1);
    engine.sql("BEGIN; UPDATE saved SET body='private',embedding=ARRAY[-1.0,0.0]; DELETE FROM saved WHERE id=9; INSERT INTO saved VALUES(25,'private new',ARRAY[-1.0,0.0])", &[]).unwrap();
    let second = engine.capture_committed_memory(&original).unwrap();
    let selected_second = table(&second, &original);
    let retained = readers(&selected_second, &second.cut, &output);
    assert_eq!(retained.len(), expected.len());
    engine
        .sql(
            "ROLLBACK; DROP TABLE saved; CREATE TABLE saved(other integer)",
            &[],
        )
        .unwrap();
    drop((selected, selected_second, first, second, engine));
    std::thread::spawn(move || {
        for ((kind, key, reader), (expected_kind, expected_key, expected)) in
            retained.into_iter().zip(expected)
        {
            assert_eq!((kind, key.as_str()), (expected_kind, expected_key.as_str()));
            let actual = bytes(reader);
            assert_eq!(actual, expected);
            let input = StorageReadControl::with_limit(8 << 20);
            match kind {
                0 => {
                    let restored =
                        uqa_storage::document_store::image::decode_document_image(&actual, &input)
                            .unwrap();
                    assert!(!restored.fields().is_empty());
                    drop(restored);
                }
                1 => assert_eq!(&actual[..8], b"UQATXT01"),
                2 => {
                    let restored = uqa_storage::vector_index::image::read_vector_image(
                        &mut actual.as_slice(),
                        &input,
                    )
                    .unwrap();
                    assert_eq!(restored.count().unwrap(), 2);
                    let nearest = restored.search_knn(&[1.0, 0.0], 1).unwrap();
                    assert_eq!(nearest.len(), 1);
                    drop(restored);
                }
                _ => unreachable!(),
            }
            assert_eq!(input.memory().used(), 0);
        }
        assert_eq!(output.memory().used(), 0);
    })
    .join()
    .unwrap();
    assert_eq!(original.memory().used(), 0);
}

#[test]
fn memory_data_readers_enforce_original_capture_and_independent_output_controls() {
    for kind in 0..3 {
        for cancel_original in [false, true] {
            let engine = engine();
            let original = StorageReadControl::with_limit(16 << 20);
            let cut = engine.capture_committed_memory(&original).unwrap();
            let selected = table(&cut, &original);
            let output = StorageReadControl::with_limit(1 << 20);
            let mut retained = readers(&selected, &cut.cut, &output);
            let index = retained.iter().position(|entry| entry.0 == kind).unwrap();
            let (_, _, mut reader) = retained.swap_remove(index);
            drop((retained, selected, cut, engine));
            assert_eq!(reader.read(&mut []).unwrap(), 0);
            while !reader.prepare(1).unwrap().ready {}
            let mut one = [0; 1];
            assert_eq!(reader.read(&mut one).unwrap(), 1);
            if cancel_original {
                original.cancellation().cancel();
            } else {
                output.cancellation().cancel();
            }
            assert!(reader.read(&mut one).is_err());
            assert!(reader.read(&mut one).is_err());
            assert_eq!(output.memory().used(), 0);
            assert_eq!(original.memory().used(), 0);
        }
    }
}

#[test]
fn memory_evaluated_index_images_keep_original_keys_after_private_writes_and_name_reuse() {
    let engine = Engine::new();
    engine.sql("BEGIN; CREATE TABLE saved(id integer PRIMARY KEY, body text); INSERT INTO saved VALUES(1,'Original'); CREATE INDEX saved_expression ON saved(lower(body)); COMMIT; BEGIN; UPDATE saved SET body='Private'", &[]).unwrap();
    let original = StorageReadControl::with_limit(16 << 20);
    let cut = engine.capture_committed_memory(&original).unwrap();
    let selected = table(&cut, &original);
    let output = StorageReadControl::with_limit(1 << 20);
    let (key, index) = next_value_index(&selected, &cut.cut, None, &output)
        .unwrap()
        .unwrap();
    assert!(next_value_index(&selected, &cut.cut, Some(&key), &output)
        .unwrap()
        .is_none());
    assert_eq!(
        index.next_image_position(None, &output).unwrap(),
        Some((1, true))
    );
    assert!(index
        .next_image_position(Some(1), &output)
        .unwrap()
        .is_none());
    let expected_field = index.field().unwrap().to_owned();
    let metadata = value_index_image(&index, None, &output).unwrap();
    let value = value_index_image(&index, Some(1), &output).unwrap();
    engine.sql("ROLLBACK; DROP INDEX saved_expression; CREATE INDEX saved_expression ON saved(upper(body)); DROP TABLE saved", &[]).unwrap();
    drop((key, index, selected, cut, engine));
    let metadata = bytes(metadata);
    let input = StorageReadControl::with_limit(1 << 20);
    let restored = uqa_execution::catalog::index::value::ValueIndexImageMetadata::read_image(
        &mut metadata.as_slice(),
        &input,
    )
    .unwrap();
    assert_eq!(&*restored.field, expected_field);
    assert_eq!(restored.len, 1);
    let value = bytes(value);
    let restored_value = uqa_storage::image_io::ImageInput {
        reader: &mut value.as_slice(),
        control: &input,
    }
    .value()
    .unwrap();
    assert_eq!(
        &*restored_value,
        &uqa_core::Value::Row(vec![uqa_core::Value::Str("original".into())].into())
    );
    drop((restored_value, restored));
    assert_eq!(original.memory().used(), 0);
    assert_eq!(output.memory().used(), 0);
    assert_eq!(input.memory().used(), 0);
}
