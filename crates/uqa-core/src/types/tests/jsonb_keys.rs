//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Logical JSONB keys preserve native comparison and release shared parsing reservations.

use super::*;
use crate::{
    memory::{BudgetedVec, MemoryBudget},
    CancellationToken,
};

fn key(text: &str) -> Vec<u8> {
    let memory = MemoryBudget::new(1 << 20);
    let mut key = BudgetedVec::new(&memory);
    write_jsonb_comparison_key(text, &mut key, &CancellationToken::new()).unwrap();
    assert_eq!(memory.used(), key.capacity());
    let output = key.to_vec();
    drop(key);
    assert_eq!(memory.used(), 0);
    output
}

#[test]
fn jsonb_comparison_keys_follow_native_structural_and_numeric_order() {
    let values = [
        "null",
        "false",
        "true",
        "0",
        "-0.00",
        "0.1",
        "0.01",
        "-0.1",
        "1",
        "1.00",
        "1e2",
        "100",
        "1.0001",
        "-1",
        "-1.0001",
        "1e-100000",
        "-1e-100000",
        "1e100000",
        "-1e100000",
        "123456789012345678901234567890.1234567890123456789",
        "123456789012345678901234567890.12345678901234567891",
        "\"\"",
        "\"a\"",
        "\"aa\"",
        "\"a\\u0000\"",
        "\"a\\u0000b\"",
        "\"한글\\n\"",
        "[]",
        "[null]",
        "[[]]",
        "[1,9]",
        "[1,10]",
        "[1]",
        "[[1],null]",
        "{}",
        "{\"b\":1,\"zz\":1}",
        "{\"c\":1,\"aa\":1}",
        "{\"a\":1,\"a\":2}",
        "{\"a\":2}",
        "{\"a\":{\"z\":null},\"b\":[2,1]}",
        "{\"b\":[2.0,1],\"a\":{\"z\":null}}",
    ];
    let keys: Vec<_> = values.iter().map(|text| key(text)).collect();
    for (left, left_key) in values.iter().zip(&keys) {
        for (right, right_key) in values.iter().zip(&keys) {
            assert_eq!(
                left_key.cmp(right_key),
                Value::JsonB((*left).into()).cmp(&Value::JsonB((*right).into())),
                "{left}, {right}"
            );
        }
    }
}

#[test]
fn jsonb_parser_preserves_last_duplicate_fields_and_existing_equality_keys() {
    for (left, right) in [
        (
            "{\"b\":1,\"a\":0,\"b\":2,\"a\":3,\"b\":4}",
            "{\"a\":3,\"b\":4}",
        ),
        ("{\"a\":[1,2],\"a\":{\"b\":1,\"b\":2}}", "{\"a\":{\"b\":2}}"),
        ("{\"a\":1,\"\\u0061\":2}", "{\"a\":2.00}"),
        ("[{\"long\":1,\"b\":2},{}]", "[{\"b\":2.0,\"long\":1.0},{}]"),
    ] {
        assert_eq!(Value::JsonB(left.into()), Value::JsonB(right.into()));
        assert_eq!(
            jsonb_equality_key(left).unwrap(),
            jsonb_equality_key(right).unwrap()
        );
        assert_eq!(key(left), key(right));
    }
    let expected_null = vec![0];
    let expected_empty_array = vec![4, 0, 0, 0, 0, 0, 0, 0, 0];
    assert_eq!(jsonb_equality_key("null").unwrap(), expected_null);
    assert_eq!(jsonb_equality_key("[]").unwrap(), expected_empty_array);
    assert_ne!(key("null"), jsonb_equality_key("null").unwrap());
}

#[test]
fn jsonb_key_failures_preserve_prefixes_and_release_original_allowance() {
    let text = "{\"escaped\\nkey\":[1,2,{\"nested\":\"日本語\\u0000\"}]}";
    for limit in [1, 8, 32, 96, 256] {
        let memory = MemoryBudget::new(limit);
        let mut output = BudgetedVec::new(&memory);
        output.push(99).unwrap();
        assert!(matches!(
            write_jsonb_comparison_key(text, &mut output, &CancellationToken::new()),
            Err(JsonbKeyError::Memory(_))
        ));
        assert_eq!(&*output, &[99]);
        assert_eq!(memory.used(), output.capacity());
        drop(output);
        assert_eq!(memory.used(), 0);
    }
    let memory = MemoryBudget::new(16 << 10);
    let mut output = BudgetedVec::new(&memory);
    output.push(99).unwrap();
    write_jsonb_comparison_key(text, &mut output, &CancellationToken::new()).unwrap();
    assert_eq!(memory.used(), output.capacity());
    assert!(memory.peak() > memory.used());
    output.truncate(1);
    for invalid in [
        "",
        "null true",
        "[1,]",
        "{\"a\":1,}",
        "\"\\uD800\"",
        "01",
        "1e",
        "1e9223372036854775808",
    ] {
        assert!(
            matches!(
                write_jsonb_comparison_key(invalid, &mut output, &CancellationToken::new()),
                Err(JsonbKeyError::InvalidJson)
            ),
            "{invalid}"
        );
        assert_eq!(&*output, &[99]);
        assert!(jsonb_equality_key(invalid).is_none());
        assert_eq!(memory.used(), output.capacity());
    }
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    assert!(matches!(
        write_jsonb_comparison_key(text, &mut output, &cancellation),
        Err(JsonbKeyError::Cancelled(_))
    ));
    assert_eq!(&*output, &[99]);
    assert_eq!(memory.used(), output.capacity());
    drop(output);
    assert_eq!(memory.used(), 0);

    let memory = MemoryBudget::new(1);
    let mut output = BudgetedVec::new(&memory);
    output.push(99).unwrap();
    assert!(matches!(
        write_jsonb_comparison_key("true", &mut output, &CancellationToken::new()),
        Err(JsonbKeyError::Memory(_))
    ));
    assert_eq!(&*output, &[99]);
    assert_eq!(memory.used(), 1);
}
