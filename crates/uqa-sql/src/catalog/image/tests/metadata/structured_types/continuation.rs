//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::super::continuation::assert_resumed;
use super::*;
use crate::catalog::image::tests::byte_windows::{read_windowed, WINDOWS};

fn ranges() -> TypeDefinitions<StoredRange> {
    let mut values = singleton("public.r", range());
    let mut next = range();
    next.identity.name = "s".into();
    next.array_name = "_s".into();
    next.object_id = [40; 16];
    next.oid += 100;
    next.array_oid += 100;
    next.multirange.identity.name = "n".into();
    next.multirange.array_name = "_n".into();
    next.multirange.object_id = [41; 16];
    next.multirange.oid += 100;
    next.multirange.array_oid += 100;
    values.insert("public.s".into(), next).unwrap();
    values
}

#[test]
fn composite_and_range_inputs_resume_native_attributes_and_name_claims() {
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let mut values = singleton("public.c", composite());
    let mut next = composite();
    next.identity.name = "d".into();
    next.array_name = "_d".into();
    next.object_id = [50; 16];
    next.oid += 100;
    next.relation_oid += 100;
    next.array_oid += 100;
    values.insert("public.d".into(), next).unwrap();
    assert_resumed(
        &values,
        TypeDefinitions::<StoredComposite>::image_input(&memory, &memory, &token).unwrap(),
        &memory,
    );
    assert_resumed(
        &ranges(),
        TypeDefinitions::<StoredRange>::image_input(&memory, &memory, &token).unwrap(),
        &memory,
    );
}

#[test]
fn range_input_rejects_name_collisions_from_previous_calls() {
    let mut source = ranges();
    let mut next = source.get("public.s").unwrap().clone();
    next.multirange.array_name = "_m".into();
    source.insert("public.s".into(), next).unwrap();
    let bytes = encode(&source);
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let mut input = TypeDefinitions::<StoredRange>::image_input(&memory, &memory, &token).unwrap();
    let mut reader = bytes.as_slice();
    assert!(input.advance(&mut reader, 1).unwrap().is_none());
    assert!(input
        .advance(&mut reader, 1)
        .err()
        .unwrap()
        .to_string()
        .contains("duplicate range type name"));
    assert_eq!(memory.used(), 0);
}

#[test]
fn composite_input_bounds_bytes_and_polls_through_attribute_fields() {
    let bytes = encode(&singleton("public.c", composite()));
    for (width, steps) in WINDOWS {
        let memory = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let restored = read_windowed(
            TypeDefinitions::<StoredComposite>::image_input(&memory, &memory, &token).unwrap(),
            &bytes,
            width,
            steps,
        );
        assert_eq!(encode(&restored), bytes);
        drop(restored);
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn range_input_bounds_bytes_and_polls_through_subtypes_and_name_claims() {
    let bytes = encode(&ranges());
    for (width, steps) in WINDOWS {
        let memory = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let restored = read_windowed(
            TypeDefinitions::<StoredRange>::image_input(&memory, &memory, &token).unwrap(),
            &bytes,
            width,
            steps,
        );
        assert_eq!(encode(&restored), bytes);
        drop(restored);
        assert_eq!(memory.used(), 0);
    }
}
