//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::io::{Cursor, Read};
use uqa_core::image_io::ImageDecodeWindow;

struct Reader<'a> {
    source: Cursor<&'a [u8]>,
    ended: bool,
}

impl Read for Reader<'_> {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        assert!(!self.ended, "native conversion reread a completed document");
        let count = self.source.read(bytes)?;
        self.ended = count == 0;
        Ok(count)
    }
}

fn fixture() -> (Document, Vec<u8>) {
    let fields: Document = (0..32)
        .map(|i| {
            (
                format!("{i:04}{}", "한글".repeat(1024)),
                Value::List(vec![Value::Int(i); 16]),
            )
        })
        .collect();
    let control = StorageReadControl::with_limit(8 << 20);
    let mut store = MemoryDocumentStore::new();
    store.put(7, fields.clone()).unwrap();
    let mut bytes = Vec::new();
    store.write_image_document(7, &mut bytes, &control).unwrap();
    (fields, bytes)
}

#[test]
fn document_conversion_yields_after_eof_and_preserves_native_layout_reuse() {
    let (fields, bytes) = fixture();
    let control = StorageReadControl::with_limit(8 << 20);
    let mut builder = MemoryDocumentStoreBuilder::new(&control);
    for id in [0, u64::MAX] {
        let mut input = builder.document_input(id).unwrap();
        let mut source = Reader {
            source: Cursor::new(&bytes),
            ended: false,
        };
        let mut post_input = 0;
        builder = loop {
            if source.ended {
                post_input += 1;
            }
            let before = source.source.position();
            let next = input
                .advance(
                    &mut source,
                    ImageDecodeWindow {
                        bytes: 17,
                        steps: 1,
                    },
                )
                .unwrap();
            assert!(source.source.position() - before <= 17);
            if let Some(next) = next {
                break next;
            }
        };
        assert!(
            post_input > 32 * 16,
            "conversion must retain value traversal and layout work"
        );
    }
    drop(bytes);
    let mut store = builder.finish().unwrap();
    let layout = store.state.documents[&0].layout_id;
    assert_eq!(layout, store.state.documents[&u64::MAX].layout_id);
    assert_eq!(store.get(0).unwrap(), Some(fields.clone()));
    control.cancellation().cancel();
    store.put(1, fields.clone()).unwrap();
    assert_eq!(
        store.state.documents[&1].layout_id, layout,
        "streamed hashing must match ordinary str hashing"
    );
    let old = store.snapshot().unwrap();
    store
        .put(0, Document::from([("changed".into(), Value::Int(1))]))
        .unwrap();
    drop(store);
    assert_eq!(old.get(0).unwrap(), Some(fields));
    assert!(control.memory().used() > 0);
    drop(old);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn document_conversion_retires_the_unpublished_corpus_on_failure_after_eof() {
    let (_, bytes) = fixture();
    for failure in ["cancel", "memory", "window", "drop"] {
        let control = StorageReadControl::with_limit(8 << 20);
        let first = input(&control, "previous complete tuple", 8192);
        let builder = MemoryDocumentStoreBuilder::new(&control)
            .add_document(1, first)
            .unwrap();
        let mut input = builder.document_input(2).unwrap();
        let mut source = Reader {
            source: Cursor::new(&bytes),
            ended: false,
        };
        while !source.ended {
            assert!(input
                .advance(
                    &mut source,
                    ImageDecodeWindow {
                        bytes: 4096,
                        steps: 1
                    }
                )
                .unwrap()
                .is_none());
        }
        let full = (failure == "memory").then(|| {
            control
                .memory()
                .reserve(control.memory().available())
                .unwrap()
        });
        if failure == "cancel" {
            control.cancellation().cancel();
        }
        if failure != "drop" {
            loop {
                match input.advance(
                    &mut source,
                    ImageDecodeWindow {
                        bytes: 1,
                        steps: usize::from(failure != "window"),
                    },
                ) {
                    Ok(None) => (),
                    Ok(Some(_)) => panic!("incomplete corpus published: {failure}"),
                    Err(_) => break,
                }
            }
        }
        drop((input, full));
        assert_eq!(control.memory().used(), 0, "{failure}");
    }
}

fn adoption(
    control: &StorageReadControl,
    invoking: &uqa_core::CancellationToken,
) -> uqa_core::image_io::ImageDecoder<MemoryDocumentStoreBuilder, StorageBackendError> {
    let previous = input(control, "previous tuple", 1024);
    let next = input(control, "next tuple", 8192);
    let builder = MemoryDocumentStoreBuilder::new(control)
        .add_document(1, previous)
        .unwrap();
    uqa_core::image_io::ImageDecoder::new(control.memory(), invoking, |mut reader| async move {
        builder
            .add_document_from(2, next, &mut reader.input())
            .await
    })
    .unwrap()
}

#[test]
fn document_conversion_checks_both_cancellation_owners_at_every_pending_boundary() {
    let window = ImageDecodeWindow { bytes: 1, steps: 1 };
    let control = StorageReadControl::with_limit(1 << 20);
    let invoking = uqa_core::CancellationToken::new();
    let mut baseline = adoption(&control, &invoking);
    let mut pending = 0;
    while baseline
        .advance(&mut std::io::empty(), window)
        .unwrap()
        .is_none()
    {
        pending += 1;
        assert!(pending < 128);
    }
    drop(baseline);
    assert!(pending > 4);
    assert_eq!(control.memory().used(), 0);
    for stop in 0..=pending {
        for original in [true, false] {
            let control = StorageReadControl::with_limit(1 << 20);
            let invoking = uqa_core::CancellationToken::new();
            let mut input = adoption(&control, &invoking);
            for _ in 0..stop {
                assert!(input
                    .advance(&mut std::io::empty(), window)
                    .unwrap()
                    .is_none());
            }
            if original {
                control.cancellation().cancel();
            } else {
                invoking.cancel();
            }
            assert!(
                matches!(
                    input.advance(&mut std::io::empty(), window),
                    Err(StorageBackendError::Cancelled(_))
                ),
                "stop {stop}, original {original}"
            );
            assert!(input.advance(&mut std::io::empty(), window).is_err());
            drop(input);
            assert_eq!(
                control.memory().used(),
                0,
                "stop {stop}, original {original}"
            );
        }
    }
}
