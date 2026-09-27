//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{key_value::MemoryKeyValueStore, VectorIndex};

#[test]
fn diskann_retained_key_value_point_reads_enforce_size_before_borrowing() {
    let store = Arc::new(MemoryKeyValueStore::new());
    let mut index = KeyValueVectorIndex::new(store, "docs", "embedding", 2);
    index.add(1, vec![1.0, 0.0]).unwrap();
    let prefix = vector_field_prefix("docs", "embedding").unwrap();
    let read = read_view(index.store.as_ref(), |read| read.retain(&[&prefix])).unwrap();
    let key = crate::key_value::codec::vector_key("docs", "embedding", 1, 0).unwrap();
    let fresh = StorageReadControl::with_limit(0);
    let mut visited = false;
    assert!(read
        .visit_value_bounded(&key, 7, &fresh, &mut |_| {
            visited = true;
            Ok(())
        })
        .is_err());
    assert!(!visited);
    read.visit_value_bounded(&key, 8, &fresh, &mut |value| {
        assert_eq!(value.unwrap().len(), 8);
        visited = true;
        Ok(())
    })
    .unwrap();
    assert!(visited);
    read.visit_value_bounded(b"missing", 0, &fresh, &mut |value| {
        assert!(value.is_none());
        Ok(())
    })
    .unwrap();
    assert_eq!(fresh.memory().used(), 0);
    read.control().cancellation().cancel();
    assert!(read
        .visit_value_bounded(&key, 8, &fresh, &mut |_| panic!(
            "cancelled source reached consumer"
        ))
        .is_err());
}

#[test]
fn diskann_raw_key_value_source_keeps_unstamped_values_and_controls_after_replacement() {
    let store = Arc::new(MemoryKeyValueStore::new());
    let mut index = KeyValueVectorIndex::new(store, "docs", "embedding", 2);
    index
        .add_many(1, vec![vec![-0.0, f32::MAX], vec![1.0, f32::from_bits(1)]])
        .unwrap();
    index.add(DocId::MAX, vec![0.0, 1.0]).unwrap();
    let control = StorageReadControl::with_limit(1 << 20);
    let source = index.vector_read_snapshot(&control).unwrap().unwrap();
    index.clear().unwrap();
    index.add(1, vec![-1.0, 0.0]).unwrap();
    drop(index);
    assert_eq!(source.next_document_after(None, &control).unwrap(), Some(1));
    assert_eq!(
        source.next_document_after(Some(1), &control).unwrap(),
        Some(DocId::MAX)
    );
    assert_eq!(
        source
            .next_document_after(Some(DocId::MAX), &control)
            .unwrap(),
        None
    );
    assert_eq!(source.document_vector_count(1, &control).unwrap(), 2);
    assert_eq!(source.document_vector_count(2, &control).unwrap(), 0);
    assert_eq!(
        source
            .read_vector(1, 0, &control)
            .unwrap()
            .unwrap()
            .iter()
            .map(|value| value.to_bits())
            .collect::<Vec<_>>(),
        [(-0.0_f32).to_bits(), f32::MAX.to_bits()]
    );
    assert_eq!(
        source.read_vector(1, 1, &control).unwrap().unwrap()[1].to_bits(),
        1
    );
    assert!(source.read_vector(1, 2, &control).unwrap().is_none());
    let held = control.memory().used();
    let full = control
        .memory()
        .reserve(control.memory().limit() - held)
        .unwrap();
    assert!(source.read_vector(1, 0, &control).is_err());
    drop(full);
    assert_eq!(control.memory().used(), held);
    let fresh = StorageReadControl::with_limit(1 << 20);
    control.cancellation().cancel();
    assert!(source.next_document_after(None, &fresh).is_err());
    assert!(source.document_vector_count(1, &fresh).is_err());
    assert!(source.read_vector(1, 0, &fresh).is_err());
    drop(source);
    assert_eq!(control.memory().used(), 0);
    assert_eq!(fresh.memory().used(), 0);
}
