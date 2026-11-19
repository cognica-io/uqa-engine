//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::image::tests::byte_windows::{read_windowed, WINDOWS};
use std::io::Cursor;

pub(super) fn assert_resumed<T: ImageDefinition>(
    values: &NamedDefinitions<T>,
    mut input: CatalogImageInput<NamedDefinitions<T>>,
    memory: &MemoryBudget,
) {
    let expected = encode(values);
    let (key, value) = values.iter().next().unwrap();
    let first_end = encode(&singleton(key, value.clone())).len() as u64;
    let mut reader = Cursor::new(expected.as_slice());
    assert!(input.advance(&mut reader, 1).unwrap().is_none());
    assert_eq!(reader.position(), first_end);
    let restored = input.advance(&mut reader, 1).unwrap().unwrap();
    assert_eq!(encode(&restored), expected);
    assert!(input.advance(&mut reader, 1).is_err());
    drop(restored);
    assert_eq!(memory.used(), 0);
}

#[test]
fn enum_and_shell_inputs_resume_native_identity_claims_without_rebinding() {
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let mut values = singleton("public.e", enum_value());
    let mut next = enum_value();
    next.identity.name = "f".into();
    next.array_name = "_f".into();
    next.object_id = [33; 16];
    next.oid += 100;
    next.array_oid += 100;
    for label in &mut next.labels {
        label.oid += 100;
    }
    values.insert("public.f".into(), next).unwrap();
    assert_resumed(
        &values,
        TypeDefinitions::<StoredEnum>::image_input(&memory, &memory, &token).unwrap(),
        &memory,
    );
    let mut values = singleton("public.s", shell_value());
    let mut next = shell_value();
    next.identity.name = "t".into();
    next.object_id = [34; 16];
    next.oid += 100;
    values.insert("public.t".into(), next).unwrap();
    assert_resumed(
        &values,
        TypeDefinitions::<StoredShellType>::image_input(&memory, &memory, &token).unwrap(),
        &memory,
    );
}

#[test]
fn enum_input_bounds_bytes_and_polls_through_label_fields() {
    let source = singleton("public.e", enum_value());
    let bytes = encode(&source);
    for (width, steps) in WINDOWS {
        let memory = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let restored = read_windowed(
            TypeDefinitions::<StoredEnum>::image_input(&memory, &memory, &token).unwrap(),
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
fn shell_input_bounds_bytes_and_polls_through_identity_fields() {
    let bytes = encode(&singleton("public.s", shell_value()));
    for (width, steps) in WINDOWS {
        let memory = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let restored = read_windowed(
            TypeDefinitions::<StoredShellType>::image_input(&memory, &memory, &token).unwrap(),
            &bytes,
            width,
            steps,
        );
        assert_eq!(encode(&restored), bytes);
        drop(restored);
        assert_eq!(memory.used(), 0);
    }
}
