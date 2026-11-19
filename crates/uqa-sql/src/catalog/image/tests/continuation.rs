//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::io::Cursor;

#[test]
fn catalog_input_limits_each_call_and_preserves_original_mutation_ownership() {
    let expected = encode_roles(&catalog());
    let mut first = RoleDefinitions::new();
    first.insert("alpha".into(), role("alpha", 42)).unwrap();
    let first_end = encode_roles(&first).len() as u64;
    let memory = MemoryBudget::new(1 << 20);
    let mutations = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let mut input = RoleDefinitions::image_input(&memory, &mutations, &token).unwrap();
    let mut reader = Cursor::new(expected.as_slice());
    assert!(input.advance(&mut reader, 1).unwrap().is_none());
    assert_eq!(reader.position(), first_end);
    assert!(memory.used() > 0);
    assert_eq!(mutations.used(), 0);
    let mut restored = input.advance(&mut reader, 1).unwrap().unwrap();
    assert_eq!(reader.position(), expected.len() as u64);
    assert_eq!(encode_roles(&restored), expected);
    assert!(input.advance(&mut reader, 1).is_err());
    token.cancel();
    let full = memory.reserve(memory.available()).unwrap();
    restored.insert("later".into(), role("later", 101)).unwrap();
    assert!(mutations.used() > 0);
    assert_eq!(restored.get("omega").unwrap().revision, 9);
    drop((restored, full));
    assert_eq!(memory.used(), 0);
    assert_eq!(mutations.used(), 0);
}

struct PanickingInput;
impl Read for PanickingInput {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        panic!("input failed between catalog entries")
    }
}

#[test]
fn catalog_input_retires_partial_state_on_every_terminal_path() {
    for failure in [
        "zero",
        "cancel",
        "memory",
        "short",
        "trailing",
        "duplicate",
        "unwind",
    ] {
        let mut source = catalog();
        if failure == "duplicate" {
            source.insert("omega".into(), role("omega", 42)).unwrap();
        }
        let mut bytes = encode_roles(&source);
        if failure == "short" {
            bytes.pop();
        }
        if failure == "trailing" {
            bytes.push(0);
        }
        let memory = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let mut input = RoleDefinitions::image_input(&memory, &memory, &token).unwrap();
        let mut reader = Cursor::new(bytes);
        assert!(input.advance(&mut reader, 1).unwrap().is_none());
        let full = (failure == "memory").then(|| memory.reserve(memory.available()).unwrap());
        if failure == "cancel" {
            token.cancel();
        }
        if failure == "unwind" {
            assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _ = input.advance(&mut PanickingInput, 1);
            }))
            .is_err());
        } else {
            assert!(
                input
                    .advance(&mut reader, usize::from(failure != "zero"))
                    .is_err(),
                "{failure}"
            );
        }
        drop(full);
        assert_eq!(memory.used(), 0, "{failure}");
        let position = reader.position();
        assert!(input.advance(&mut reader, 1).is_err());
        assert_eq!(reader.position(), position);
    }
    let token = CancellationToken::new();
    let empty = MemoryBudget::new(0);
    assert!(RoleDefinitions::image_input(&empty, &empty, &token).is_err());
    assert_eq!(empty.used(), 0);
}

#[test]
fn schema_input_retains_cross_entry_claims_and_exact_acl_images() {
    let source = schema_registry();
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let captured = source
        .capture(&ProductionControl::new(&memory, &token, &token))
        .unwrap();
    let bytes = streamed(captured.image_reader(&token).unwrap(), 7);
    drop(captured);
    let mut input = SchemaRegistry::image_input(&memory, &memory, &token).unwrap();
    let mut reader = Cursor::new(bytes.as_slice());
    for _ in 1..source.len() {
        assert!(input.advance(&mut reader, 1).unwrap().is_none());
    }
    let restored = input.advance(&mut reader, 1).unwrap().unwrap();
    let captured = restored
        .capture(&ProductionControl::new(&memory, &token, &token))
        .unwrap();
    assert_eq!(streamed(captured.image_reader(&token).unwrap(), 7), bytes);
    drop((captured, restored));
    assert_eq!(memory.used(), 0);
}
