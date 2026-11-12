//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{encode, scored};
use crate::{
    read_control::StorageReadControl, vector_index::image::vector_image_input, VectorIndex,
};
use std::io::{Cursor, Read};
use uqa_core::image_io::ImageDecodeWindow;

struct Wire<'a> {
    cursor: Cursor<&'a [u8]>,
    end_reads: usize,
}

impl Read for Wire<'_> {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        assert_eq!(self.end_reads, 0, "a completed input was read again");
        let count = self.cursor.read(bytes)?;
        self.end_reads += usize::from(count == 0);
        Ok(count)
    }
}

fn read(bytes: &[u8], control: &StorageReadControl) -> Box<dyn VectorIndex> {
    let mut input = vector_image_input(control, None).unwrap();
    let mut wire = Wire {
        cursor: Cursor::new(bytes),
        end_reads: 0,
    };
    let mut calls = 0;
    let result = loop {
        calls += 1;
        let before = wire.cursor.position();
        let window = ImageDecodeWindow {
            bytes: if calls % 2 == 0 { 1 } else { 17 },
            steps: 1,
        };
        let result = input.advance(&mut wire, window).unwrap();
        assert!(wire.cursor.position() - before <= window.bytes as u64);
        if let Some(result) = result {
            break result;
        }
    };
    assert!(calls > 10);
    assert_eq!(wire.end_reads, 1);
    result
}

#[test]
fn vector_input_preserves_all_native_images_queries_and_original_ownership() {
    for source in super::ownership::owners() {
        let expected = scored(&*source, &[1.0, 0.0], 4);
        let bytes = encode(&*source);
        drop(source);
        let control = StorageReadControl::with_limit(32 << 20);
        let mut restored = read(&bytes, &control);
        assert!(control.memory().used() > 0);
        assert_eq!(encode(&*restored), bytes);
        assert_eq!(scored(&*restored, &[1.0, 0.0], 4), expected);
        let snapshot = restored.snapshot().unwrap();
        let capture = StorageReadControl::with_limit(32 << 20);
        let image = restored.capture_image_with_control(&capture).unwrap();
        control.cancellation().cancel();
        restored.add(19, vec![-1.0, 0.0]).unwrap();
        restored.delete(1).unwrap();
        assert!(restored.contains_document(19).unwrap());
        assert_eq!(scored(&*snapshot, &[1.0, 0.0], 4), expected);
        drop((restored, snapshot));
        // A capture may retain the original allocation or own an admitted copy.
        assert!(capture.memory().used() > 0);
        let mut actual = vec![];
        image.write_image(&mut actual, &capture).unwrap();
        assert_eq!(actual, bytes);
        drop(image);
        assert_eq!(control.memory().used(), 0);
        assert_eq!(capture.memory().used(), 0);
    }
}

#[test]
fn vector_input_retires_truncated_cancelled_and_unfunded_native_candidates() {
    for source in super::ownership::owners() {
        let bytes = encode(&*source);
        drop(source);
        for failure in [
            "truncated",
            "trailing",
            "cancel",
            "window",
            "memory",
            "drop",
        ] {
            let mut bytes = bytes.clone();
            if failure == "truncated" {
                bytes.pop();
            }
            if failure == "trailing" {
                bytes.push(0);
            }
            let control = StorageReadControl::with_limit(32 << 20);
            let mut input = vector_image_input(&control, None).unwrap();
            let mut wire = Cursor::new(&bytes);
            assert!(input
                .advance(
                    &mut wire,
                    ImageDecodeWindow {
                        bytes: 13,
                        steps: 32
                    }
                )
                .unwrap()
                .is_none());
            if failure == "drop" {
                drop(input);
            } else {
                if failure == "cancel" {
                    control.cancellation().cancel();
                }
                let held = (failure == "memory").then(|| {
                    control
                        .memory()
                        .reserve(control.memory().available())
                        .unwrap()
                });
                let window = ImageDecodeWindow {
                    bytes: 17,
                    steps: usize::from(failure != "window"),
                };
                loop {
                    match input.advance(&mut wire, window) {
                        Ok(None) => (),
                        Ok(Some(_)) => panic!("invalid vector input completed: {failure}"),
                        Err(_) => break,
                    }
                }
                drop((input, held));
            }
            assert_eq!(control.memory().used(), 0, "{failure}");
        }
    }
}

#[test]
fn vector_input_preserves_wide_spilled_hnsw_and_exact_tensor_bits() {
    let dimensions = 8192;
    let mut owners: Vec<Box<dyn VectorIndex>> = vec![
        Box::new(crate::vector_index::MemoryVectorIndex::new(dimensions)),
        Box::new(crate::HNSWIndex::new(dimensions)),
    ];
    for owner in &mut owners {
        owner
            .add_many(
                7,
                vec![
                    vec![-0.0; dimensions as usize],
                    vec![1.0; dimensions as usize],
                ],
            )
            .unwrap();
        owner.add(9, vec![-1.0; dimensions as usize]).unwrap();
    }
    for owner in owners {
        let bytes = encode(&*owner);
        drop(owner);
        let control = StorageReadControl::with_limit(512 << 10);
        let restored = read(&bytes, &control);
        assert_eq!(encode(&*restored), bytes);
        assert_eq!(restored.count().unwrap(), 3);
        drop(restored);
        assert_eq!(control.memory().used(), 0);
    }
}
