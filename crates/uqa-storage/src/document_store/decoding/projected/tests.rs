//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::document_store::decoding::{
    decode_legacy_document_fields_budgeted, decode_legacy_document_projection_budgeted,
};

#[test]
fn projected_primitives_preserve_values_and_borrow_unselected_strings() {
    let input = format!(
        "{{\"body\":\"{}\",\"null\":null,\"value\":42}}",
        "x".repeat(1 << 20)
    );
    let control = StorageReadControl::with_limit(2048);
    let selected = decode_legacy_document_projection_budgeted(
        input.as_bytes(),
        &["value", "null", "missing", "value"],
        &control,
    )
    .unwrap();
    assert_eq!(
        *selected,
        Document::from([
            ("null".into(), Value::Null),
            ("value".into(), Value::Int(42))
        ])
    );
    assert!(control.memory().used() < 2048);
    drop(selected);
    assert_eq!(control.memory().used(), 0);
    assert!(matches!(
        decode_legacy_document_fields_budgeted(input.as_bytes(), &control),
        Err(crate::StorageBackendError::Memory(_))
    ));
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn projection_preserves_historical_duplicates_tags_and_normalization() {
    let control = StorageReadControl::with_limit(1 << 20);
    for input in [
        "{}",
        r#"{"a":1,"b":18446744073709551615,"c":-0.0,"d":1e3,"e":"escaped\\ \" \uD83D\uDE00","f":true}"#,
        r#"{"b":1,"a":2,"b":3}"#,
        r#"{"a":1e999,"\u0061":3,"b":"text"}"#,
        r#"{"a":[1,2,3],"b":{"$uqa_type":"float","value":"NaN"}}"#,
        r#"{"$serde_json::private::RawValue":"{\"a\":1}"}"#,
        r#"{"a":1,"$serde_json::private::Number":"ordinary field"}"#,
        r#"{"a":{"$uqa_type":"value_blob","encoding":"f64_list","field":"a"},"b":2}"#,
    ] {
        let full = decode_legacy_document_fields_budgeted(input.as_bytes(), &control).unwrap();
        for selected in [&[][..], &["a", "missing", "b", "a"][..], &["e", "f"][..]] {
            let actual =
                decode_legacy_document_projection_budgeted(input.as_bytes(), selected, &control)
                    .unwrap();
            let expected: Document = full
                .iter()
                .filter(|(name, _)| selected.contains(&name.as_str()))
                .map(|(name, value)| (name.clone(), value.clone()))
                .collect();
            assert_eq!(*actual, expected, "{input}");
        }
        drop(full);
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn projection_checks_unselected_values_and_releases_failures() {
    let control = StorageReadControl::with_limit(1 << 20);
    for input in [
        r#"{"a":1e999,"b":2}"#,
        r#"{"a":"\uD800","b":2}"#,
        r#"{"a":1,"b":{"$serde_json::private::Number":"bad"}}"#,
        r#"{"a":1,"b":{"$serde_json::private::Number":"bad"},"b":2}"#,
        r#"{"a":1,"b":01}"#,
        r#"{"a":1,}"#,
        r#"{"a":1} false"#,
    ] {
        let expected =
            decode_legacy_document_fields_budgeted(input.as_bytes(), &control).unwrap_err();
        let actual =
            decode_legacy_document_projection_budgeted(input.as_bytes(), &["missing"], &control)
                .unwrap_err();
        assert_eq!(actual.to_string(), expected.to_string(), "{input}");
        assert_eq!(control.memory().used(), 0);
    }
    control.cancellation().cancel();
    assert!(matches!(
        decode_legacy_document_projection_budgeted(b"{}", &[], &control),
        Err(crate::StorageBackendError::Cancelled(_))
    ));
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn canonical_projection_avoids_unused_name_and_string_allocations() {
    let input = format!(
        "{{\"a\":\"{}\",\"b\":1,\"c\":true,\"d\":42}}",
        "x".repeat(8192)
    );
    let control = StorageReadControl::with_limit(1 << 20);
    let full = allocation_counter::measure(|| {
        let row = decode_legacy_document_fields_budgeted(input.as_bytes(), &control).unwrap();
        assert_eq!(row.get("d"), Some(&Value::Int(42)));
    });
    let projected = allocation_counter::measure(|| {
        let row =
            decode_legacy_document_projection_budgeted(input.as_bytes(), &["d"], &control).unwrap();
        assert_eq!(row.get("d"), Some(&Value::Int(42)));
    });
    assert!(projected.count_total < full.count_total);
    assert!(projected.bytes_total + 8192 < full.bytes_total);
    assert_eq!(control.memory().used(), 0);
}
