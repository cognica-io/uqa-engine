//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::sequence::registry::SequenceCatalogInput;

#[test]
fn ordered_sequence_input_retires_readers_and_keeps_original_records_through_runtime_mutation() {
    let source = fixture();
    let relation = RelationIdentity::new("s", "q");
    let expected = *source.catalog.get(&relation).unwrap();
    let (metadata, authority) = encode(&source);
    drop(source);
    let input_memory = MemoryBudget::new(65536);
    let runtime = MemoryBudget::new(65536);
    let cancellation = CancellationToken::new();
    let input =
        SequenceCatalogInput::read_metadata(&mut metadata.as_slice(), &input_memory, &cancellation)
            .unwrap();
    drop(metadata);
    assert!(input_memory.used() > 0);
    let mut restored = input
        .read_authority(&mut authority.as_slice(), &runtime)
        .unwrap();
    drop(authority);
    assert_eq!(restored.get(&relation), Some(&expected));
    let retained = restored.clone();
    let name = retained.keys().next().unwrap().name.as_ptr();
    assert_eq!(restored.keys().next().unwrap().name.as_ptr(), name);
    let original = input_memory.used();
    let full = input_memory.reserve(65536 - original).unwrap();
    cancellation.cancel();
    let replacement = RelationIdentity::new("s", "next");
    restored
        .insert(
            replacement.clone(),
            SequenceRecord {
                object_id: [19; 16],
                catalog_oid: Some(31_019),
                ..expected
            },
            BoundSequenceSecurity::owner(RoleIdentity::BOOTSTRAP),
        )
        .unwrap();
    assert!(runtime.used() > 0);
    restored.remove(&relation).unwrap();
    assert_eq!(restored.len(), 1);
    assert!(restored.contains_key(&replacement));
    assert_eq!(retained.get(&relation), Some(&expected));
    assert_eq!(retained.keys().next().unwrap().name.as_ptr(), name);
    drop(restored);
    assert_eq!(runtime.used(), 0);
    drop(full);
    assert_eq!(input_memory.used(), original);
    drop(retained);
    assert_eq!(input_memory.used(), 0);
}

#[test]
fn ordered_sequence_input_failure_consumes_metadata_before_any_catalog_is_exposed() {
    let (metadata, authority) = encode(&fixture());
    for failure in 0..3 {
        let memory = MemoryBudget::new(65536);
        let runtime = MemoryBudget::new(65536);
        let cancellation = CancellationToken::new();
        let input =
            SequenceCatalogInput::read_metadata(&mut metadata.as_slice(), &memory, &cancellation)
                .unwrap();
        let mut bytes = authority.clone();
        let full = match failure {
            0 => {
                cancellation.cancel();
                None
            }
            1 => {
                bytes.push(0);
                None
            }
            _ => Some(memory.reserve(65536 - memory.used()).unwrap()),
        };
        assert!(input
            .read_authority(&mut bytes.as_slice(), &runtime)
            .is_err());
        drop(full);
        assert_eq!(memory.used(), 0);
        assert_eq!(runtime.used(), 0);
    }
}
