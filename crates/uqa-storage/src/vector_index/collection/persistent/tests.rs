//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    key_value::{KeyValueStorageBackend, MemoryKeyValueStore},
    vector_index::MemoryVectorIndex,
    PersistentStorageBackend, StorageName,
};
use std::sync::Arc;
use uqa_core::memory::{BudgetedString, MemoryReservation};

fn field(name: &str, control: &StorageReadControl) -> Budgeted<String> {
    let mut value = BudgetedString::new(control.memory());
    value.push_str(name).unwrap();
    let (value, memory) = value.into_parts();
    Budgeted::new(value, memory)
}

fn index(
    backend: &KeyValueStorageBackend,
    name: &str,
    control: &StorageReadControl,
) -> StorageHandle<dyn VectorIndex> {
    backend
        .canonical_vector_index_with_control(
            StorageName::from_admitted(field("public.notes", control)).unwrap(),
            StorageName::from_admitted(field(name, control)).unwrap(),
            2,
            control,
        )
        .unwrap()
}

#[test]
fn persistent_vector_collection_preserves_original_fields_and_independent_handle_leases() {
    let backend = KeyValueStorageBackend::new(Arc::new(MemoryKeyValueStore::new()));
    let input = StorageReadControl::with_limit(1 << 20);
    let original = index(&backend, "embedding", &input);
    let handle_bytes = input.memory().used();
    let name = field("embedding", &input);
    let name_bytes = name.reserved_bytes();
    let name_pointer = name.as_ptr();
    let mut indexes = PersistentVectorIndexesBuilder::new(&input)
        .add_index(name, original)
        .unwrap()
        .finish()
        .unwrap();
    assert_eq!(indexes.keys().next().unwrap().as_ptr(), name_pointer);
    let collection_bytes = handle_bytes + name_bytes + NativeIndexes::entry_bytes();
    assert_eq!(input.memory().used(), collection_bytes);
    let occupied = input.memory().reserve(input.memory().available()).unwrap();
    input.cancellation().cancel();
    indexes
        .live_mut()
        .unwrap()
        .get_mut("embedding")
        .unwrap()
        .add(1, vec![1.0, 0.0])
        .unwrap();
    let replacement_control = StorageReadControl::with_limit(1 << 20);
    let replacement = index(&backend, "embedding", &replacement_control);
    let replacement_bytes = replacement_control.memory().used();
    let mut original = indexes
        .live_mut()
        .unwrap()
        .insert_handle("embedding".into(), replacement)
        .unwrap()
        .unwrap();
    assert_eq!(indexes.keys().next().unwrap().as_ptr(), name_pointer);
    assert_eq!(input.memory().used(), occupied.bytes() + collection_bytes);
    original.add(2, vec![0.0, 1.0]).unwrap();
    assert_eq!(indexes["embedding"].count().unwrap(), 2);
    let replacement = indexes
        .live_mut()
        .unwrap()
        .remove("embedding")
        .unwrap()
        .unwrap();
    assert_eq!(input.memory().used(), occupied.bytes() + handle_bytes);
    assert_eq!(replacement_control.memory().used(), replacement_bytes);
    indexes
        .live_mut()
        .unwrap()
        .insert_handle("renamed".into(), original)
        .unwrap();
    assert_eq!(input.memory().used(), occupied.bytes() + handle_bytes);
    drop(replacement);
    assert_eq!(replacement_control.memory().used(), 0);
    assert!(indexes["renamed"].contains_document(2).unwrap());
    drop(indexes);
    assert_eq!(input.memory().used(), occupied.bytes());
    drop(occupied);
    assert_eq!(input.memory().used(), 0);
}

#[test]
fn persistent_vector_collection_failure_retires_the_complete_unpublished_candidate() {
    let backend = KeyValueStorageBackend::new(Arc::new(MemoryKeyValueStore::new()));
    for invalid in 0..8 {
        let input = StorageReadControl::with_limit(1 << 20);
        let foreign = StorageReadControl::with_limit(1 << 20);
        let builder = PersistentVectorIndexesBuilder::new(&input)
            .add_index(field("first", &input), index(&backend, "first", &input))
            .unwrap();
        let name = match invalid {
            0 => field("first", &input),
            1 => field("second", &foreign),
            2 => Budgeted::new("second".into(), input.memory().empty_reservation()),
            _ => field("second", &input),
        };
        let value = match invalid {
            3 => index(&backend, "second", &foreign),
            4 => {
                let value: Box<dyn VectorIndex> = Box::new(MemoryVectorIndex::new(2));
                Budgeted::new(value, input.memory().empty_reservation()).into()
            }
            5 => {
                let value: Box<dyn VectorIndex> = Box::new(MemoryVectorIndex::new(2));
                value.into()
            }
            _ => index(&backend, "second", &input),
        };
        let occupied =
            (invalid == 7).then(|| input.memory().reserve(input.memory().available()).unwrap());
        if invalid == 6 {
            input.cancellation().cancel();
        }
        let error = builder.add_index(name, value).err().unwrap();
        assert!(match invalid {
            6 => matches!(error, StorageBackendError::Cancelled(_)),
            7 => matches!(error, StorageBackendError::Memory(_)),
            _ => matches!(error, StorageBackendError::Other(_)),
        });
        assert_eq!(
            input.memory().used(),
            occupied.as_ref().map_or(0, MemoryReservation::bytes)
        );
        assert_eq!(foreign.memory().used(), 0);
        drop(occupied);
        assert_eq!(input.memory().used(), 0);
    }
}

#[test]
fn persistent_vector_collection_completion_preserves_cancellation_and_empty_writability() {
    let input = StorageReadControl::with_limit(0);
    let mut indexes = PersistentVectorIndexesBuilder::new(&input)
        .finish()
        .unwrap();
    input.cancellation().cancel();
    indexes
        .live_mut()
        .unwrap()
        .insert("later".into(), Box::new(MemoryVectorIndex::new(2)))
        .unwrap();
    assert_eq!(indexes.len(), 1);
    assert_eq!(input.memory().used(), 0);
    assert!(matches!(
        PersistentVectorIndexesBuilder::new(&input).finish(),
        Err(StorageBackendError::Cancelled(_))
    ));
    let backend = KeyValueStorageBackend::new(Arc::new(MemoryKeyValueStore::new()));
    let input = StorageReadControl::with_limit(1 << 20);
    let builder = PersistentVectorIndexesBuilder::new(&input)
        .add_index(field("first", &input), index(&backend, "first", &input))
        .unwrap();
    input.cancellation().cancel();
    assert!(matches!(
        builder.finish(),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert_eq!(input.memory().used(), 0);
}
