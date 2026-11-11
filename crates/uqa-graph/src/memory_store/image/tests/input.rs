//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::image_work::tests::Reader;
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder},
    memory::MemoryBudget,
};

fn input(control: &StorageReadControl) -> ImageDecoder<MemoryGraphStore, StorageBackendError> {
    ImageDecoder::new(
        control.memory(),
        control.cancellation(),
        |mut channel| async move { MemoryGraphStore::read_image_from(&mut channel.input()).await },
    )
    .unwrap()
}

fn wide_image() -> Vec<u8> {
    let mut source = source();
    let mut vertex = Vertex::new(1, "node\0한글".repeat(1025));
    vertex.properties.insert(
        "nested".into(),
        Value::List(
            (0..64)
                .map(|_| {
                    Value::Map(BTreeMap::from([(
                        "field".into(),
                        Value::List(vec![Value::Str("saved".into()); 8]),
                    )]))
                })
                .collect(),
        ),
    );
    source.add_vertex(vertex, "g").unwrap();
    source.copy_graph("g", &"graph\0한글".repeat(1025)).unwrap();
    source.allocate_vertex_id("retired", "g").unwrap();
    source.drop_label("g", "retired").unwrap();
    encode(&source)
}

#[test]
fn graph_input_retains_current_windows_and_validation_after_single_eof() {
    for bytes in [fixture([1, 2], [1, 2], 4, 3), wide_image()] {
        let control = StorageReadControl::with_limit(8 << 20);
        let mut input = input(&control);
        let mut reader = Reader::new(&bytes);
        let mut iterations = 0;
        let mut after_eof = 0;
        let restored = loop {
            let window = ImageDecodeWindow {
                bytes: [1, 7, 4096][iterations % 3],
                steps: 1,
            };
            let before = (reader.bytes, reader.calls);
            after_eof += usize::from(reader.eof);
            let result = input.advance(&mut reader, window).unwrap();
            assert!(reader.bytes - before.0 <= window.bytes);
            assert!(reader.calls - before.1 <= window.steps);
            iterations += 1;
            assert!(iterations < bytes.len() * 16);
            if let Some(store) = result {
                break store;
            }
        };
        assert!(reader.eof && after_eof > 8);
        assert_eq!(reader.bytes, bytes.len());
        assert_eq!(encode(&restored), bytes);
        assert!(input
            .advance(&mut reader, ImageDecodeWindow { bytes: 1, steps: 1 })
            .is_err());
        let ordinary = StorageReadControl::with_limit(8 << 20);
        let synchronous = decode(&bytes, &ordinary);
        assert_eq!(control.memory().used(), ordinary.memory().used());
        drop(synchronous);
        assert_eq!(ordinary.memory().used(), 0);
        let saved = restored.clone();
        let mutations = MemoryBudget::new(8 << 20);
        let mut restored = restored.into_mutation_budget(&mutations);
        control.cancellation().cancel();
        restored.add_vertex(Vertex::new(99, "later"), "g").unwrap();
        assert_eq!(encode(&saved), bytes);
        assert_ne!(encode(&restored), bytes);
        drop(restored);
        assert_eq!(mutations.used(), 0);
        assert!(control.memory().used() > 0);
        drop(saved);
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn graph_input_retires_partial_fields_and_post_eof_candidates() {
    let bytes = fixture([1, 2], [1, 2], 4, 3);
    for after_eof in [false, true] {
        for mode in 0..3 {
            let control = StorageReadControl::with_limit(1 << 20);
            let mut decoder = input(&control);
            let mut reader = Reader::new(&bytes);
            loop {
                assert!(decoder
                    .advance(&mut reader, ImageDecodeWindow { bytes: 7, steps: 1 })
                    .unwrap()
                    .is_none());
                if if after_eof {
                    reader.eof
                } else {
                    reader.bytes >= 80
                } {
                    break;
                }
            }
            let calls = reader.calls;
            assert!(control.memory().used() > 0);
            if mode == 0 {
                control.cancellation().cancel();
            }
            if mode < 2 {
                assert!(decoder
                    .advance(
                        &mut reader,
                        ImageDecodeWindow {
                            bytes: usize::from(mode != 1),
                            steps: 1
                        }
                    )
                    .is_err());
                assert_eq!(control.memory().used(), 0);
                assert!(decoder
                    .advance(&mut reader, ImageDecodeWindow { bytes: 1, steps: 1 })
                    .is_err());
            }
            drop(decoder);
            assert_eq!(reader.calls, calls);
            assert_eq!(control.memory().used(), 0);
        }
    }
}

#[test]
fn graph_input_errors_and_denied_admission_retire_the_original_continuation() {
    let valid = fixture([1, 2], [1, 2], 4, 3);
    for bytes in [
        valid[..80].to_vec(),
        fixture([1, 2], [1, 3], 4, 3),
        fixture([1, 2], [1, 2], 4, 2),
    ] {
        let control = StorageReadControl::with_limit(1 << 20);
        let mut decoder = input(&control);
        let mut reader = Reader::new(&bytes);
        loop {
            match decoder.advance(&mut reader, ImageDecodeWindow { bytes: 1, steps: 1 }) {
                Ok(None) => (),
                Ok(Some(_)) => panic!("invalid graph was published"),
                Err(_) => break,
            }
        }
        assert_eq!(control.memory().used(), 0);
        let calls = reader.calls;
        assert!(decoder
            .advance(&mut reader, ImageDecodeWindow { bytes: 1, steps: 1 })
            .is_err());
        assert_eq!(reader.calls, calls);
    }
    let control = StorageReadControl::with_limit(1 << 20);
    let decoder = input(&control);
    let occupied = control
        .memory()
        .reserve(control.memory().available())
        .unwrap();
    assert!(matches!(
        decoder.read(&mut valid.as_slice()),
        Err(StorageBackendError::Memory(_))
    ));
    assert_eq!(control.memory().used(), occupied.bytes());
    drop(occupied);
    assert_eq!(control.memory().used(), 0);
}
