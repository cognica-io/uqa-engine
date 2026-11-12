//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::memory_capture::MemoryCaptureRegistry;

#[test]
fn vector_handle_reinsertion_preserves_original_and_later_memory_cuts() {
    let input = StorageReadControl::with_limit(16 << 20);
    for (name, mut index) in capture_sources(&input) {
        let registry = MemoryCaptureRegistry::new();
        index.add(1, vec![1.0, 0.0]).unwrap();
        index.initialize().unwrap();
        let original = encoded(index.as_ref());
        let source = registry.cell(VectorIndexes::memory(&registry)).unwrap();
        source
            .write()
            .live_mut()
            .unwrap()
            .insert(name.into(), index)
            .unwrap();
        let control = StorageReadControl::with_limit(16 << 20);
        let first = registry.capture(&control).unwrap();
        let mut detached = source
            .write()
            .live_mut()
            .unwrap()
            .remove(name)
            .unwrap()
            .unwrap();
        detached.add(2, vec![0.0, 1.0]).unwrap();
        let renamed = encoded(detached.as_ref());
        source
            .write()
            .live_mut()
            .unwrap()
            .insert_handle("renamed".into(), detached)
            .unwrap();
        let second = registry.capture(&control).unwrap();
        source
            .write()
            .live_mut()
            .unwrap()
            .get_mut("renamed")
            .unwrap()
            .add(3, vec![0.5, 0.5])
            .unwrap();
        let captured_first = source.captured(&first).unwrap();
        let captured_second = source.captured(&second).unwrap();
        assert_eq!(captured_first.fields().unwrap().collect::<Vec<_>>(), [name]);
        assert_eq!(
            captured_second.fields().unwrap().collect::<Vec<_>>(),
            ["renamed"]
        );
        assert_eq!(encoded_capture(&captured_first, name, &first), original);
        assert_eq!(
            encoded_capture(&captured_second, "renamed", &second),
            renamed
        );
        assert_eq!(source.read()["renamed"].count().unwrap(), 3);
        drop((
            source,
            registry,
            first,
            second,
            captured_first,
            captured_second,
        ));
        assert_eq!(control.memory().used(), 0);
    }
    assert_eq!(input.memory().used(), 0);
}

#[test]
fn vector_handle_publication_rejects_uncaptured_and_foreign_sources_atomically() {
    let registry = MemoryCaptureRegistry::new();
    let foreign = MemoryCaptureRegistry::new();
    let mut indexes = VectorIndexes::memory(&registry);
    indexes
        .live_mut()
        .unwrap()
        .insert("original".into(), Box::new(MemoryVectorIndex::new(2)))
        .unwrap();
    for captured in [false, true] {
        let value: Box<dyn VectorIndex> = Box::new(MemoryVectorIndex::new(3));
        let value = if captured {
            value.into_memory_capture(&foreign).unwrap()
        } else {
            value
        };
        assert!(indexes
            .live_mut()
            .unwrap()
            .insert_handle("original".into(), value.into())
            .is_err());
        assert_eq!(indexes.len(), 1);
        assert_eq!(indexes["original"].dimensions(), 2);
    }
}
