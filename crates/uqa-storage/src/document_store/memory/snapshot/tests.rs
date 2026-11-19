//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::document_store::{image::decode_document_image, DocumentMetadata, StoredDocument};
use uqa_core::{memory::MemoryError, CancellationToken, Value};

fn row(value: Value) -> Document {
    Document::from([("value".into(), value)])
}

fn charge(source: &MemoryDocumentStore) -> usize {
    let control = StorageReadControl::with_limit(usize::MAX);
    let captured = source.snapshot_with_control(&control).unwrap();
    let bytes = control.memory().used();
    drop(captured);
    assert_eq!(control.memory().used(), 0);
    bytes
}

#[test]
fn memory_document_capture_pins_large_generations_without_a_corpus_copy() {
    for count in [1, 20_000] {
        let mut source = MemoryDocumentStore::new();
        for id in 0..count {
            source
                .put_stored(
                    id,
                    StoredDocument::with_metadata(
                        row(Value::Str("payload".repeat(40))),
                        DocumentMetadata::with_tuple_xmin(83),
                    ),
                )
                .unwrap();
        }
        let control = StorageReadControl::with_limit(64 << 20);
        let mut captured = None;
        let measured = allocation_counter::measure(|| {
            captured = Some(source.snapshot_with_control(&control).unwrap());
        });
        assert!(measured.bytes_total < 2048, "{measured:?}");
        assert!(measured.count_total < 10, "{measured:?}");
        let captured = captured.unwrap();
        assert!(control.memory().used() >= count as usize * 280);
        source.put(0, row(Value::Int(99))).unwrap();
        source.delete(count - 1).unwrap();
        source.clear().unwrap();
        source.put(7, row(Value::Int(7))).unwrap();
        drop(source);
        assert_eq!(captured.len().unwrap(), count as usize);
        let image_control = StorageReadControl::with_limit(4096);
        let mut bytes = Vec::new();
        captured
            .write_image_document(count - 1, &mut bytes, &image_control)
            .unwrap()
            .unwrap();
        let decoded = decode_document_image(&bytes, &image_control).unwrap();
        assert_eq!(decoded.fields()["value"], Value::Str("payload".repeat(40)));
        assert_eq!(decoded.metadata().tuple_xmin(), Some(83));
        drop(decoded);
        assert_eq!(image_control.memory().used(), 0);
        drop(captured);
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn memory_document_retention_follows_replacement_delete_and_dormant_layouts() {
    let mut source = MemoryDocumentStore::new();
    let empty = charge(&source);
    let mut large = String::with_capacity(64 << 10);
    large.push('x');
    let capacity = large.capacity();
    source.put(1, row(Value::Str(large))).unwrap();
    let populated = charge(&source);
    assert!(populated >= capacity + empty);
    let previous_slots = source.state.documents[&1].values.capacity();
    source.put(1, row(Value::Int(12))).unwrap();
    let current_slots = source.state.documents[&1].values.capacity();
    assert_eq!(
        populated - charge(&source),
        capacity + (previous_slots - current_slots) * size_of::<Value>()
    );
    source.delete(1).unwrap();
    let dormant = charge(&source);
    assert!(dormant > empty);
    source.delete(1).unwrap();
    assert_eq!(charge(&source), dormant);
    source.put(1, row(Value::Int(13))).unwrap();
    let reused = charge(&source);
    source
        .patch_fields(1, &Document::from([("new-field".into(), Value::Int(17))]))
        .unwrap();
    assert!(charge(&source) > reused);
    let read = source.snapshot().unwrap();
    let mut copied = source.clone();
    copied
        .put(1, Document::from([("independent".into(), Value::Null)]))
        .unwrap();
    let before_clear = charge(&source);
    let retained = source
        .snapshot_with_control(&StorageReadControl::with_limit(before_clear))
        .unwrap();
    source.clear().unwrap();
    assert_eq!(charge(&source), empty);
    assert_eq!(
        read.get_field(1, "new-field").unwrap(),
        Some(Value::Int(17))
    );
    assert_eq!(
        retained.get_field(1, "value").unwrap(),
        Some(Value::Int(13))
    );
    assert_eq!(
        copied.get_field(1, "independent").unwrap(),
        Some(Value::Null)
    );
    assert!(charge(&copied) > empty);
}

#[test]
fn memory_document_capture_rejects_limits_and_releases_failed_admission() {
    let mut source = MemoryDocumentStore::new();
    source
        .put(1, row(Value::List(vec![Value::Bytes(vec![3; 8192]); 8])))
        .unwrap();
    let required = charge(&source);
    for limit in [0, 128, required - 1, required] {
        let control = StorageReadControl::with_limit(limit);
        let result = source.snapshot_with_control(&control);
        if limit < required {
            assert!(matches!(
                result,
                Err(StorageBackendError::Memory(MemoryError::Limit { .. }))
            ));
        } else {
            let snapshot = result.unwrap();
            assert_eq!(snapshot.next_doc_id(None).unwrap(), Some(1));
            assert_eq!(control.memory().used(), required);
            drop(snapshot);
        }
        assert_eq!(control.memory().used(), 0);
        assert_eq!(Arc::strong_count(&source.state), 1);
        assert_eq!(source.len().unwrap(), 1);
    }
    let cancelled = StorageReadControl::with_limit(0);
    cancelled.cancellation().cancel();
    assert!(matches!(
        source.snapshot_with_control(&cancelled),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert_eq!(cancelled.memory().used(), 0);
}

#[test]
fn memory_document_capture_shares_only_the_same_generation_and_allowance() {
    let mut source = MemoryDocumentStore::new();
    source.put(1, row(Value::Bytes(vec![1; 1 << 20]))).unwrap();
    let memory = MemoryBudget::new(8 << 20);
    let left_control = StorageReadControl::new(&memory, &CancellationToken::new());
    let right_control = StorageReadControl::new(&memory, &CancellationToken::new());
    let left = source.snapshot_with_control(&left_control).unwrap();
    let initial = memory.used();
    // Reject the wrapper after payload admission; keep the previous cached allowance intact.
    let rejected = StorageReadControl::with_limit(initial - 1);
    assert!(matches!(
        source.snapshot_with_control(&rejected),
        Err(StorageBackendError::Memory(_))
    ));
    assert_eq!(rejected.memory().used(), 0);
    let right = source.snapshot_with_control(&right_control).unwrap();
    assert!(memory.used() - initial < 1024);
    let independent = StorageReadControl::with_limit(initial);
    let other = source.snapshot_with_control(&independent).unwrap();
    assert_eq!(independent.memory().used(), initial);
    left_control.cancellation().cancel();
    assert!(matches!(left.len(), Err(StorageBackendError::Cancelled(_))));
    assert_eq!(right.len().unwrap(), 1);
    assert_eq!(other.len().unwrap(), 1);
    source.put(1, row(Value::Bytes(vec![2; 2 << 20]))).unwrap();
    let next = source.snapshot_with_control(&right_control).unwrap();
    assert!(memory.used() > initial + (2 << 20));
    drop((left, right));
    assert!(memory.used() > 2 << 20);
    drop(other);
    assert_eq!(independent.memory().used(), 0);
    drop(next);
    assert_eq!(memory.used(), 0);
    let again = source.snapshot_with_control(&right_control).unwrap();
    assert!(memory.used() > 2 << 20);
    drop(again);
    assert_eq!(memory.used(), 0);
}

#[test]
fn memory_document_retained_reads_keep_original_control_and_cannot_detach_writable_rows() {
    let mut source = MemoryDocumentStore::new();
    for id in 1..=10 {
        source.put(id, row(Value::Int(id as i64))).unwrap();
    }
    let original = StorageReadControl::with_limit(64 << 10);
    let invoking = StorageReadControl::with_limit(4096);
    let snapshot = source.snapshot_with_control(&original).unwrap();
    assert!(snapshot.writable_snapshot().is_err());
    assert!(snapshot
        .get_shared_fields(&[1], &["value"])
        .unwrap()
        .is_none());
    assert!(snapshot
        .next_shared_fields(None, 1, &["value"])
        .unwrap()
        .is_none());
    assert!(snapshot
        .for_each_next_shared_fields_borrowed(None, 1, &["value"], &mut |_, _| panic!())
        .is_none());
    let nested = snapshot.snapshot_with_control(&invoking).unwrap();
    let retained = snapshot.retained_snapshot().unwrap().unwrap();
    let mut alias = snapshot.snapshot().unwrap();
    assert!(Arc::get_mut(&mut alias)
        .unwrap()
        .put(2, Document::new())
        .is_err());
    let page = nested
        .get_stored_many_controlled(&[1, 1, 20], &invoking)
        .unwrap();
    assert_eq!(page.len(), 3);
    assert!(page[0].is_some() && page[1].is_some() && page[2].is_none());
    assert!(invoking.memory().used() > 0);
    drop(page);
    assert_eq!(invoking.memory().used(), 0);
    let mut visited = 0;
    assert!(matches!(
        retained.for_each_next_fields(None, 10, &["value"], &mut |_, _| {
            visited += 1;
            original.cancellation().cancel();
            true
        }),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert_eq!(visited, 1);
    assert!(matches!(
        alias.snapshot(),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert!(matches!(
        nested.next_doc_ids_controlled(None, 2, &invoking),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert!(matches!(
        snapshot.snapshot_with_control(&invoking),
        Err(StorageBackendError::Cancelled(_))
    ));
    drop((source, snapshot, alias, retained));
    assert!(original.memory().used() > 0);
    drop(nested);
    assert_eq!(original.memory().used(), 0);
}

#[test]
fn memory_document_images_separate_source_retention_from_bounded_output_and_cancellation() {
    struct Output {
        bytes: usize,
        maximum: usize,
        cancellation: Option<CancellationToken>,
    }
    impl std::io::Write for Output {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.bytes += bytes.len();
            self.maximum = self.maximum.max(bytes.len());
            if self.bytes >= 8192 {
                if let Some(token) = &self.cancellation {
                    token.cancel();
                }
            }
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut source = MemoryDocumentStore::new();
    source.put(1, row(Value::Bytes(vec![3; 4 << 20]))).unwrap();
    let original = StorageReadControl::with_limit(8 << 20);
    let invoking = StorageReadControl::with_limit(512);
    let captured = source.snapshot_with_control(&original).unwrap();
    drop(source);
    let mut output = Output {
        bytes: 0,
        maximum: 0,
        cancellation: None,
    };
    let allocated = allocation_counter::measure(|| {
        assert_eq!(
            captured
                .write_image_document(1, &mut output, &invoking)
                .unwrap(),
            Some(output.bytes as u64)
        );
    });
    assert!(allocated.bytes_total < 2048, "{allocated:?}");
    assert!(output.bytes > 4 << 20);
    assert!(output.maximum <= 16 << 10);
    assert_eq!(invoking.memory().used(), 0);
    output.bytes = 0;
    output.cancellation = Some(original.cancellation().clone());
    assert!(matches!(
        captured.write_image_document(1, &mut output, &invoking),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert!((8192..32768).contains(&output.bytes));
    assert!(!invoking.cancellation().is_cancelled());
    assert!(original.memory().used() > 4 << 20);
    drop(captured);
    assert_eq!(original.memory().used(), 0);
}
