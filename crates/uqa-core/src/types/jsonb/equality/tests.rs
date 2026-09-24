//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::memory::{MemoryBudget, MemoryError};

#[test]
fn equality_keys_keep_the_persisted_bytes_and_native_duplicate_numeric_semantics() {
    let memory = MemoryBudget::new(1024 * 1024);
    let cancellation = CancellationToken::new();
    for (text, expected) in [
        ("null", vec![0]),
        ("false", vec![3, 0]),
        ("true", vec![3, 1]),
        ("\"x\"", vec![1, 0, 0, 0, 0, 0, 0, 0, 1, b'x']),
        ("[true,null]", vec![4, 0, 0, 0, 0, 0, 0, 0, 2, 3, 1, 0]),
        (
            "1.00",
            vec![2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, b'1'],
        ),
    ] {
        let mut output = BudgetedVec::new(&memory);
        write_jsonb_equality_key(text, &mut output, &cancellation).unwrap();
        assert_eq!(&*output, expected);
        assert_eq!(jsonb_equality_key(text).unwrap(), expected);
        assert_eq!(memory.used(), output.capacity());
    }
    let mut first = BudgetedVec::new(&memory);
    let mut second = BudgetedVec::new(&memory);
    write_jsonb_equality_key(
        r#"{"b":[1e2,-0.0],"a":"discarded","a":"é"}"#,
        &mut first,
        &cancellation,
    )
    .unwrap();
    write_jsonb_equality_key(
        r#"{"a":"\u00e9","b":[100.00,0]}"#,
        &mut second,
        &cancellation,
    )
    .unwrap();
    assert_eq!(&*first, &*second);
    drop((first, second));
    assert_eq!(memory.used(), 0);
}

#[test]
fn failed_equality_construction_keeps_the_prefix_and_releases_parsing_workspace() {
    let memory = MemoryBudget::new(1024);
    let cancellation = CancellationToken::new();
    let mut output = BudgetedVec::new(&memory);
    output.extend_from_slice(b"prefix").unwrap();
    let error = write_jsonb_equality_key(
        &format!("[\"{}\"]", "x".repeat(2048)),
        &mut output,
        &cancellation,
    )
    .unwrap_err();
    assert!(matches!(
        error,
        JsonbKeyError::Memory(MemoryError::Limit { .. })
    ));
    assert_eq!(&*output, b"prefix");
    assert_eq!(memory.used(), output.capacity());
    assert!(matches!(
        write_jsonb_equality_key("[true,", &mut output, &cancellation),
        Err(JsonbKeyError::InvalidJson)
    ));
    cancellation.cancel();
    assert!(matches!(
        write_jsonb_equality_key("null", &mut output, &cancellation),
        Err(JsonbKeyError::Cancelled(_))
    ));
    assert_eq!(&*output, b"prefix");
    drop(output);
    assert_eq!(memory.used(), 0);
}

#[test]
fn nested_equality_traversal_keeps_only_the_output_after_completion() {
    let memory = MemoryBudget::new(1024 * 1024);
    let mut output = BudgetedVec::new(&memory);
    let text = format!("{}null{}", "[".repeat(256), "]".repeat(256));
    write_jsonb_equality_key(&text, &mut output, &CancellationToken::new()).unwrap();
    assert_eq!(output.len(), 256 * 9 + 1);
    assert_eq!(memory.used(), output.capacity());
    assert_eq!(&*output, jsonb_equality_key(&text).unwrap());
}

#[test]
fn controlled_equality_owns_only_destination_and_preserves_invalid_result() {
    let memory = MemoryBudget::new(1024 * 1024);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&memory, &original, &invoking);
    for input in [r#"{"z":[1.00,-0.0],"a":"é","a":"x"}"#, "true", "[]"] {
        let output = jsonb_equality_key_with_control(input, &control)
            .unwrap()
            .unwrap();
        assert_eq!(&*output, &jsonb_equality_key(input).unwrap());
        assert_eq!(output.reserved_bytes(), output.capacity());
        assert_eq!(memory.used(), output.capacity());
        drop(output);
        assert_eq!(memory.used(), 0);
    }
    for input in ["[true,", "1e9223372036854775808"] {
        assert!(jsonb_equality_key_with_control(input, &control)
            .unwrap()
            .is_none());
    }
    assert_eq!(memory.used(), 0);
}

#[test]
fn controlled_equality_checks_both_tokens_and_releases_failed_output() {
    for cancel_original in [false, true] {
        let memory = MemoryBudget::new(1024);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        if cancel_original {
            original.cancel();
        } else {
            invoking.cancel();
        }
        let control = ProductionControl::new(&memory, &original, &invoking);
        assert!(matches!(
            jsonb_equality_key_with_control("null", &control),
            Err(ValueRetentionError::Cancelled(_))
        ));
        assert_eq!(memory.used(), 0);
    }
    let memory = MemoryBudget::new(16);
    let cancellation = CancellationToken::new();
    let control = ProductionControl::new(&memory, &cancellation, &cancellation);
    assert!(matches!(
        jsonb_equality_key_with_control("[1,2,3]", &control),
        Err(ValueRetentionError::Memory(_))
    ));
    assert_eq!(memory.used(), 0);
}
