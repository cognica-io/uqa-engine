//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn flat_document_transfers_names_without_repeated_normalization_allocations() {
    let control = StorageReadControl::with_limit(1 << 20);
    let input = r#"{"id":42,"category":7,"body":"several ordinary words","price":1200,"quantity":3,"active":true}"#;
    let previous = allocation_counter::measure(|| {
        super::super::normalized::validate(input, &control).unwrap();
        let fields = super::super::document(input, &control).unwrap();
        assert_eq!(fields.get("id"), Some(&Value::Int(42)));
    });
    let current = allocation_counter::measure(|| {
        let fields = fields(input, &control).unwrap().unwrap();
        assert_eq!(fields.get("id"), Some(&Value::Int(42)));
    });
    assert!(current.count_total < previous.count_total);
    assert!(current.bytes_total < previous.bytes_total);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn flat_rows_match_historical_conversion_and_duplicate_validation() {
    let control = StorageReadControl::with_limit(1 << 20);
    for input in [
        "{}",
        r#" { "int":-9223372036854775808,"unsigned":18446744073709551615,"decimal":1.25,"exp":1e3,"s":"escaped\\ \" \uD83D\uDE00","null":null,"true":true,"false":false } "#,
        r#"{"a":1e999,"b":2,"\u0061":3}"#,
        r#"{"$uqa_type":"row","xmin":0,"__uqa_system_xmin":1,"s":"[{}]"}"#,
        r#"{"a":-0.0,"b":-0,"c":1.0,"d":18446744073709551616}"#,
        r#"{"first":0,"$serde_json::private::Number":"ordinary field"}"#,
    ] {
        super::super::normalized::validate(input, &control).unwrap();
        let expected = super::super::document(input, &control).unwrap();
        let actual = fields(input, &control).unwrap().expect("flat document");
        assert_eq!(*actual, *expected, "{input}");
        drop((actual, expected));
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn nested_values_and_private_envelopes_retain_full_normalization() {
    let control = StorageReadControl::with_limit(1 << 20);
    for input in [
        r#"{"$serde_json::private::RawValue":"{\"x\":1}"}"#,
        r#"{"$serde_json::private::Number":"1"}"#,
        r#"{"x":[],"x":1}"#,
        r#"{"x":{"$serde_json::private::Number":"bad"},"x":1}"#,
        r#"{"x":{"$uqa_type":"float","value":"NaN"}}"#,
        "[]",
        "1",
        "null",
    ] {
        assert!(fields(input, &control).unwrap().is_none(), "{input}");
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn flat_row_failures_release_every_decode_allowance() {
    let control = StorageReadControl::with_limit(1 << 20);
    for input in [
        r#"{"a":"\uD800","a":"valid"}"#,
        r#"{"a":1e999}"#,
        r#"{"a":1,}"#,
        r#"{"a":true} false"#,
        r#"{"a":01}"#,
    ] {
        assert!(fields(input, &control).is_err(), "{input}");
        assert_eq!(control.memory().used(), 0);
    }
    let input = format!("{{\"field\":\"{}\"}}", "x".repeat(1024));
    let small = StorageReadControl::with_limit(64);
    assert!(matches!(
        fields(&input, &small),
        Err(crate::StorageBackendError::Memory(_))
    ));
    assert_eq!(small.memory().used(), 0);
    control.cancellation().cancel();
    assert!(matches!(
        fields("{}", &control),
        Err(crate::StorageBackendError::Cancelled(_))
    ));
    assert_eq!(control.memory().used(), 0);
}
