//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::evaluate;
use uqa_core::{
    memory::{MemoryBudget, ProductionControl},
    CancellationToken, Value,
};

#[test]
fn escape_results_retain_their_entire_buffer_and_release_temporary_inputs() {
    for (similar, arguments) in [
        (
            false,
            vec![Value::Str("a!_b\\c".into()), Value::Str("!".into())],
        ),
        (
            false,
            vec![
                Value::Bytes(vec![0xff, b'!', b'_', b'\\']),
                Value::Bytes(vec![b'!']),
            ],
        ),
        (true, vec![Value::Str("a%_b".into())]),
    ] {
        let memory = MemoryBudget::new(4096);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&memory, &original, &invoking);
        let result = evaluate(similar, &arguments, &control).unwrap();
        let capacity = match &*result {
            Value::Str(text) => text.capacity(),
            Value::Bytes(bytes) => bytes.capacity(),
            value => panic!("unexpected escape result {value:?}"),
        };
        assert!(capacity > 0);
        assert_eq!(memory.used(), capacity);
        invoking.cancel();
        assert_eq!(memory.used(), capacity);
        drop(result);
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn escape_failure_nulls_and_both_cancellation_owners_release_admitted_buffers() {
    for (similar, arguments, state) in [
        (
            false,
            vec![Value::Bytes(vec![b'\\'; 1024]), Value::Bytes(Vec::new())],
            "53200",
        ),
        (
            false,
            vec![Value::Str("\\".repeat(128)), Value::Str(String::new())],
            "53200",
        ),
        (true, vec![Value::Str("%".repeat(128))], "53200"),
        (
            false,
            vec![Value::Str("a".into()), Value::Str("!!".into())],
            "22025",
        ),
        (
            false,
            vec![Value::Bytes(vec![0xff]), Value::Bytes(vec![0xff, 0xfe])],
            "22025",
        ),
    ] {
        let memory = MemoryBudget::new(256);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&memory, &token, &token);
        assert_eq!(
            evaluate(similar, &arguments, &control)
                .err()
                .unwrap()
                .sqlstate(),
            Some(state)
        );
        assert_eq!(memory.used(), 0);
    }
    for original_owner in [false, true] {
        let memory = MemoryBudget::new(0);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&memory, &original, &invoking);
        let arguments = [Value::Null, Value::Str("invalid escape".into())];
        let result = evaluate(false, &arguments, &control).unwrap();
        assert_eq!(*result, Value::Null);
        assert_eq!(memory.used(), 0);
        drop(result);
        if original_owner {
            original.cancel();
        } else {
            invoking.cancel();
        }
        assert_eq!(
            evaluate(false, &arguments, &control)
                .err()
                .unwrap()
                .sqlstate(),
            Some("57014")
        );
        assert_eq!(memory.used(), 0);
    }
}
