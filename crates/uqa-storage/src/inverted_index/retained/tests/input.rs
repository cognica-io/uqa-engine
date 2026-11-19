//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::io::Read;
use uqa_core::image_io::{ImageDecodeWindow, ImageDecoder};

struct Reader<'a> {
    bytes: &'a [u8],
    position: usize,
    eof: bool,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self {
            bytes,
            position: 0,
            eof: false,
        }
    }
}

impl Read for Reader<'_> {
    fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
        assert!(!self.eof, "publication must never reread source EOF");
        let count = output.len().min(self.bytes.len() - self.position);
        output[..count].copy_from_slice(&self.bytes[self.position..self.position + count]);
        self.position += count;
        self.eof = count == 0;
        Ok(count)
    }
}

fn window(bytes: usize) -> ImageDecodeWindow {
    ImageDecodeWindow { bytes, steps: 1 }
}

fn complete(
    mut input: ImageDecoder<RetainedInvertedIndexBuilder, StorageBackendError>,
    bytes: &[u8],
) -> (RetainedInvertedIndexBuilder, usize) {
    let mut reader = Reader::new(bytes);
    let mut calls = 0;
    let mut post_input = 0;
    loop {
        calls += 1;
        let maximum = if calls % 2 == 0 { 1 } else { 17 };
        let before = reader.position;
        post_input += usize::from(reader.eof);
        let next = input.advance(&mut reader, window(maximum)).unwrap();
        assert!(reader.position - before <= maximum);
        if let Some(owner) = next {
            assert!(reader.eof);
            return (owner, post_input);
        }
        assert!(calls < bytes.len() * 4 + 4096);
    }
}

#[test]
fn retained_text_input_keeps_current_windows_exact_corpus_charges_and_writable_snapshots() {
    let mut source = MemoryInvertedIndex::new(uqa_analysis::whitespace_analyzer());
    let field = "한🙂".repeat(600);
    let text = format!("{}a {}b", "한🙂".repeat(800), "한🙂".repeat(800));
    source
        .add_document(
            1,
            BTreeMap::from([(field.clone(), text), ("empty".into(), String::new())]),
        )
        .unwrap();
    let bytes = crate::inverted_index::image::tests::encode(&source, 1).unwrap();
    drop(source);
    let control = StorageReadControl::with_limit(8 << 20);
    let original = builder(&control);
    let bindings = control.memory().used() - corpus_bytes(&original.index.state);
    let (mut owner, polls) = complete(original.document_input(1).unwrap(), &bytes);
    assert!(polls > 15);
    assert_eq!(
        control.memory().used(),
        corpus_bytes(&owner.index.state) + bindings
    );
    let raw = crate::inverted_index::image::tests::fixture(true);
    owner = complete(owner.document_input(2).unwrap(), &raw).0;
    assert_eq!(
        control.memory().used(),
        corpus_bytes(&owner.index.state) + bindings
    );
    owner
        .index
        .state
        .retention
        .with_retained(&StorageReadControl::with_limit(8 << 20), |memory| {
            assert_eq!(
                memory.bytes(),
                corpus_bytes(&owner.index.state) + size_of::<MemoryReservation>()
            );
            Ok(())
        })
        .unwrap();
    let mut index = owner.finish_writable().unwrap();
    control.cancellation().cancel();
    let snapshot = index.snapshot().unwrap();
    assert_eq!(
        crate::inverted_index::image::tests::encode(&*snapshot, 1).as_ref(),
        Some(&bytes)
    );
    assert_eq!(
        crate::inverted_index::image::tests::encode(&*snapshot, 2).as_ref(),
        Some(&raw)
    );
    assert_eq!(snapshot.total_field_length(&field).unwrap(), 2);
    index.remove_document(1).unwrap();
    index
        .add_document(3, BTreeMap::from([("x".into(), "later".into())]))
        .unwrap();
    assert_eq!(index.get_term_freq(3, "x", "later").unwrap(), 1);
    assert_eq!(snapshot.doc_count().unwrap(), 2);
    drop(index);
    assert!(control.memory().used() > 0);
    drop(snapshot);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn retained_text_input_retires_partial_native_publication_at_every_post_eof_poll() {
    let bytes = crate::inverted_index::image::tests::fixture(false);
    let control = StorageReadControl::with_limit(1 << 20);
    let (owner, polls) = complete(builder(&control).document_input(2).unwrap(), &bytes);
    drop(owner);
    assert!(polls > 6);
    assert_eq!(control.memory().used(), 0);
    for step in 0..polls - 1 {
        for failure in ["cancel", "window", "drop"] {
            let control = StorageReadControl::with_limit(1 << 20);
            let mut owner = builder(&control);
            owner.add_image_document(1, &mut bytes.as_slice()).unwrap();
            let mut input = owner.document_input(2).unwrap();
            let mut reader = Reader::new(&bytes);
            while !reader.eof {
                assert!(input.advance(&mut reader, window(4096)).unwrap().is_none());
            }
            for _ in 0..step {
                assert!(input.advance(&mut reader, window(4096)).unwrap().is_none());
            }
            if failure == "drop" {
                drop(input);
            } else {
                if failure == "cancel" {
                    control.cancellation().cancel();
                }
                let result = input.advance(
                    &mut reader,
                    window(if failure == "window" { 0 } else { 4096 }),
                );
                assert!(result.is_err(), "{step} {failure}");
                assert!(input.advance(&mut reader, window(4096)).is_err());
            }
            assert_eq!(control.memory().used(), 0, "{step} {failure}");
        }
    }
}

#[test]
fn retained_text_input_rejects_malformed_units_and_late_admission_without_retaining_a_corpus() {
    let bytes = crate::inverted_index::image::tests::fixture(false);
    for length in [0, 1, 16, 30, 100, bytes.len() - 1, bytes.len() + 1] {
        let mut corrupt = bytes.clone();
        corrupt.resize(length, 0);
        let control = StorageReadControl::with_limit(1 << 20);
        let mut owner = builder(&control);
        owner.add_image_document(1, &mut bytes.as_slice()).unwrap();
        assert!(owner
            .document_input(2)
            .unwrap()
            .read(&mut corrupt.as_slice())
            .is_err());
        assert_eq!(control.memory().used(), 0);
    }
    let mut successes = 0;
    let mut failures = 0;
    for available in [0, 1, 64, 256, 1024, 4096, 8192, 32768] {
        let control = StorageReadControl::with_limit(1 << 20);
        let mut input = builder(&control).document_input(1).unwrap();
        let mut reader = Reader::new(&bytes);
        assert!(input.advance(&mut reader, window(1)).unwrap().is_none());
        let held = control
            .memory()
            .reserve(control.memory().available() - available)
            .unwrap();
        match input.read(&mut reader) {
            Ok(owner) => {
                successes += 1;
                drop(owner);
            }
            Err(StorageBackendError::Memory(_)) => failures += 1,
            Err(error) => panic!("unexpected input failure: {error}"),
        }
        assert_eq!(control.memory().used(), held.bytes());
        drop(held);
        assert_eq!(control.memory().used(), 0);
    }
    assert!(successes > 0 && failures > 0);
}
