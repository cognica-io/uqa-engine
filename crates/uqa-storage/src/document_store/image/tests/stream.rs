//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::image_io::{ImageRead, ImageReader};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use uqa_core::{memory::Budgeted, DecimalValue};

#[test]
fn memory_document_image_readers_preserve_captured_bytes_after_source_replacement() {
    for metadata in [
        DocumentMetadata::default(),
        DocumentMetadata::with_tuple_xmin(u32::MAX),
    ] {
        let original = StorageReadControl::with_limit(8 << 20);
        let invoking = StorageReadControl::with_limit(64 << 10);
        let mut store = MemoryDocumentStore::new();
        store
            .put_stored(
                7,
                StoredDocument::with_metadata(
                    Document::from([
                        ("\0xmin".into(), Value::Void),
                        ("large".into(), Value::Bytes(vec![0x82; 65549])),
                        (
                            "lazy".into(),
                            Value::Datum(DatumValue::new(25, u32::MAX, vec![0xff; 7])),
                        ),
                        (
                            "names".into(),
                            Value::Record(RecordValue::new(vec![
                                ("x".into(), Value::Null),
                                ("x".into(), Value::Void),
                            ])),
                        ),
                    ]),
                    metadata,
                ),
            )
            .unwrap();
        store.put_stored(1, StoredDocument::default()).unwrap();
        let captured = ReadOnlySnapshot::new(store.snapshot_with_control(&original).unwrap());
        store.delete(7).unwrap();
        store
            .put(7, Document::from([("replacement".into(), Value::Int(12))]))
            .unwrap();
        store.clear().unwrap();
        drop(store);
        let expected = encode(&captured, 7, &invoking).unwrap();
        let retained = original.memory().used();
        for window in [1, 31, 32768] {
            let mut reader = captured
                .image_document_reader(7, &invoking)
                .unwrap()
                .unwrap();
            assert!(invoking.memory().used() > 0 && invoking.memory().used() < 32768);
            let mut actual = vec![0; expected.len()];
            let allocation = allocation_counter::measure(|| {
                assert_eq!(reader.read(&mut []).unwrap(), 0);
                let mut position = 0;
                while position < actual.len() {
                    let limit = (position + window).min(actual.len());
                    let count = reader.read(&mut actual[position..limit]).unwrap();
                    assert!(count > 0 && count <= window.min(16384));
                    position += count;
                }
                assert_eq!(reader.read(&mut [0; 1]).unwrap(), 0);
            });
            assert_eq!(allocation.bytes_total, 0, "{allocation:?}");
            assert_eq!(actual, expected);
            assert_eq!(invoking.memory().used(), 0);
            assert_eq!(original.memory().used(), retained);
        }
        let mut empty = captured
            .image_document_reader(1, &invoking)
            .unwrap()
            .unwrap();
        let mut bytes = [0; 128];
        let count = empty.read(&mut bytes).unwrap();
        assert_eq!(&bytes[..count], encode(&captured, 1, &invoking).unwrap());
        assert_eq!(empty.read(&mut bytes).unwrap(), 0);
        original.cancellation().cancel();
        invoking.cancellation().cancel();
        assert_eq!(empty.read(&mut bytes).unwrap(), 0);
        drop(empty);
        drop(captured);
        assert_eq!(original.memory().used(), 0);
        assert_eq!(invoking.memory().used(), 0);
    }
}

#[test]
fn memory_document_image_readers_release_current_scratch_for_both_cancellation_owners() {
    for cancel_original in [false, true] {
        let mut store = MemoryDocumentStore::new();
        store
            .put(
                1,
                Document::from([(
                    "numeric".into(),
                    Value::Decimal(
                        DecimalValue::parse(&format!("1.{}", "4".repeat(4096))).unwrap(),
                    ),
                )]),
            )
            .unwrap();
        let original = StorageReadControl::with_limit(1 << 20);
        let invoking = StorageReadControl::with_limit(64 << 10);
        let captured = store.snapshot_with_control(&original).unwrap();
        drop(store);
        let mut reader = captured
            .image_document_reader(1, &invoking)
            .unwrap()
            .unwrap();
        let header = invoking.memory().used();
        assert_eq!(reader.read(&mut [0; 128]).unwrap(), 128);
        assert!(invoking.memory().used() > header);
        if cancel_original {
            original.cancellation().cancel();
        } else {
            invoking.cancellation().cancel();
        }
        assert!(matches!(
            reader.read(&mut [0; 1]),
            Err(StorageBackendError::Cancelled(_))
        ));
        assert_eq!(invoking.memory().used(), 0);
        assert!(original.memory().used() > 0);
        assert!(reader.read(&mut [0; 1]).is_err());
        drop(reader);
        drop(captured);
        assert_eq!(original.memory().used(), 0);
    }
}

#[test]
fn memory_document_image_reader_admission_and_early_drop_preserve_the_snapshot() {
    let mut store = MemoryDocumentStore::new();
    store
        .put(
            1,
            Document::from([("bytes".into(), Value::Bytes(vec![3; 2 << 20]))]),
        )
        .unwrap();
    let original = StorageReadControl::with_limit(4 << 20);
    let captured = store.snapshot_with_control(&original).unwrap();
    drop(store);
    let invoking = StorageReadControl::with_limit(0);
    let allocation = allocation_counter::measure(|| {
        assert!(matches!(
            captured.image_document_reader(1, &invoking),
            Err(StorageBackendError::Memory(_))
        ));
        assert!(captured
            .image_document_reader(99, &invoking)
            .unwrap()
            .is_none());
    });
    assert_eq!(allocation.bytes_total, 0, "{allocation:?}");
    let invoking = StorageReadControl::with_limit(64 << 10);
    let mut reader = captured
        .image_document_reader(1, &invoking)
        .unwrap()
        .unwrap();
    assert_eq!(reader.read(&mut [0; 1]).unwrap(), 1);
    drop(reader);
    assert_eq!(invoking.memory().used(), 0);
    let retained = original.memory().used();
    let mut independent = captured
        .image_document_reader(1, &invoking)
        .unwrap()
        .unwrap();
    assert_eq!(independent.read(&mut [0; 64]).unwrap(), 64);
    assert_eq!(original.memory().used(), retained);
    drop(independent);
    drop(captured);
    assert_eq!(invoking.memory().used(), 0);
    assert_eq!(original.memory().used(), 0);
}

struct Source {
    payload: Budgeted<Vec<u8>>,
    mode: usize,
    dropped: Arc<AtomicUsize>,
    token: uqa_core::CancellationToken,
}

impl ImageRead for Source {
    fn read(&mut self, output: &mut [u8]) -> StorageBackendResult<usize> {
        assert_eq!(self.payload.len(), 4096);
        match self.mode {
            0 => Err(StorageBackendError::Other("source failed".into())),
            1 => panic!("source unwind"),
            2 => Ok(output.len() + 1),
            3 => {
                self.token.cancel();
                Ok(output.len())
            }
            _ => Ok(0),
        }
    }
}

impl Drop for Source {
    fn drop(&mut self) {
        self.dropped.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn admitted_image_readers_release_sources_on_failure_unwind_and_complete_only_nonempty_reads() {
    for mode in 0..5 {
        let control = StorageReadControl::with_limit(8192);
        let memory = control.memory().reserve(4096).unwrap();
        let dropped = Arc::new(AtomicUsize::new(0));
        let source = Source {
            payload: Budgeted::new(vec![0; 4096], memory),
            mode,
            dropped: Arc::clone(&dropped),
            token: control.cancellation().clone(),
        };
        let mut reader = ImageReader::new(source, &control).unwrap();
        if mode == 4 {
            assert_eq!(reader.read(&mut []).unwrap(), 0);
            assert_eq!(dropped.load(Ordering::SeqCst), 0);
            assert!(control.memory().used() >= 4096);
        }
        let result =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| reader.read(&mut [0; 1])));
        match mode {
            1 => assert!(result.is_err()),
            4 => assert_eq!(result.unwrap().unwrap(), 0),
            _ => assert!(result.unwrap().is_err()),
        }
        assert_eq!(dropped.load(Ordering::SeqCst), 1);
        assert_eq!(control.memory().used(), 0);
        control.cancellation().cancel();
        if mode == 4 {
            assert_eq!(reader.read(&mut [0; 1]).unwrap(), 0);
        } else {
            assert!(reader.read(&mut [0; 1]).is_err());
        }
        assert_eq!(dropped.load(Ordering::SeqCst), 1);
    }
}
