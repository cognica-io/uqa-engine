//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::image::tests::byte_windows::{assert_retirement, read_windowed, WINDOWS};

#[test]
fn domain_input_bounds_nested_expressions_and_preserves_original_mutation_ownership() {
    let source_memory = MemoryBudget::new(16 << 20);
    let token = CancellationToken::new();
    let mut source = TypeDefinitions::<StoredDomain>::read_image(
        &mut fixture().as_slice(),
        &source_memory,
        &token,
    )
    .unwrap();
    let mut domain = source.get("s.d").unwrap().clone();
    domain.definition.base = crate::ColumnType::Text;
    domain.definition.default = Some(Expr::Literal(Value::Str("한🙂".repeat(1025))));
    source.insert("s.d".into(), domain).unwrap();
    let expected = encode_domains(&source);
    drop(source);
    assert_eq!(source_memory.used(), 0);
    for (width, steps) in WINDOWS {
        let memory = MemoryBudget::new(16 << 20);
        let mutations = MemoryBudget::new(16 << 20);
        let token = CancellationToken::new();
        let mut restored = read_windowed(
            TypeDefinitions::<StoredDomain>::image_input(&memory, &mutations, &token).unwrap(),
            &expected,
            width,
            steps,
        );
        assert_eq!(encode_domains(&restored), expected);
        let retained = restored.clone();
        let mut replacement = restored.get("s.d").unwrap().clone();
        replacement.definition.default = Some(Expr::Literal(Value::Str("changed".into())));
        let full = memory.reserve(memory.available()).unwrap();
        token.cancel();
        restored.insert("s.d".into(), replacement).unwrap();
        assert_eq!(encode_domains(&retained), expected);
        drop((restored, full));
        assert_eq!(mutations.used(), 0);
        assert!(memory.used() > 0);
        drop(retained);
        assert_eq!(memory.used(), 0);
    }
    assert_retirement(&expected, TypeDefinitions::<StoredDomain>::image_input);
}

#[test]
fn domain_input_keeps_constraint_identity_claims_across_entry_windows() {
    let source_memory = MemoryBudget::new(16 << 20);
    let token = CancellationToken::new();
    let mut source = TypeDefinitions::<StoredDomain>::read_image(
        &mut fixture().as_slice(),
        &source_memory,
        &token,
    )
    .unwrap();
    let first = encode_domains(&source);
    let mut second = source.get("s.d").unwrap().clone();
    second.object_id = [9; 16];
    second.oid = 65539;
    second.identity.name = "e".into();
    second.definition.name = "s.e".into();
    source.insert("s.e".into(), second).unwrap();
    let expected = encode_domains(&source);
    drop(source);
    assert_eq!(source_memory.used(), 0);
    let memory = MemoryBudget::new(16 << 20);
    let mut input = TypeDefinitions::<StoredDomain>::image_input(&memory, &memory, &token).unwrap();
    let mut reader = std::io::Cursor::new(expected);
    assert!(input.advance(&mut reader, 1).unwrap().is_none());
    assert_eq!(reader.position(), first.len() as u64);
    assert!(input.advance(&mut reader, 1).is_err());
    assert_eq!(memory.used(), 0);
    let position = reader.position();
    assert!(input.advance(&mut reader, 1).is_err());
    assert_eq!(reader.position(), position);
}
