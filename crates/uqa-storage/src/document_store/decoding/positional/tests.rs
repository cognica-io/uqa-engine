//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::document_store::decoding::decode_legacy_document_fields_budgeted;
use crate::StorageBackendError;

#[test]
fn positional_decoding_shares_duplicate_fields_and_omits_field_name_allocations() {
    let name = "a".repeat(32 * 1024);
    let input = format!(
        r#"{{"{name}":"selected","null":null,"unused":"{}"}}"#,
        "x".repeat(64 * 1024)
    );
    let control = StorageReadControl::with_limit(2048);
    let mut output = None;
    let allocated = allocation_counter::measure(|| {
        output = Some(
            decode_legacy_document_slots_budgeted(
                input.as_bytes(),
                &[&name, "null", "missing", &name],
                &control,
            )
            .unwrap(),
        );
    });
    let output = output.unwrap();
    assert!(allocated.bytes_total < 2048, "{allocated:?}");
    assert_eq!(output.len(), 4);
    assert_eq!(output.values().len(), 2);
    assert_eq!(output.get(0), Some(&Value::Str("selected".into())));
    assert_eq!(output.get(1), Some(&Value::Null));
    assert_eq!(output.get(2), None);
    assert!(std::ptr::eq(output.get(0).unwrap(), output.get(3).unwrap()));
    assert_eq!(output.get(4), None);
    drop(output);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn positional_decoding_preserves_legacy_values_duplicates_and_normalization() {
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
        r#"{"a":{"$uqa_type":"row","values":[1,2]},"b":null}"#,
        r#"{"a":{"$uqa_type":"invalid","value":[1,2]},"b":{"nested":[0,255]}}"#,
    ] {
        let full = decode_legacy_document_fields_budgeted(input.as_bytes(), &control).unwrap();
        for fields in [
            &[][..],
            &["a", "missing", "b", "a"][..],
            &["c", "d", "e", "f"][..],
        ] {
            let projected =
                decode_legacy_document_slots_budgeted(input.as_bytes(), fields, &control).unwrap();
            assert_eq!(projected.len(), fields.len());
            assert_eq!(projected.is_empty(), fields.is_empty());
            for (position, field) in fields.iter().enumerate() {
                assert_eq!(
                    projected.get(position),
                    full.get(*field),
                    "{input}: {field}"
                );
            }
            if fields.len() == 4 && fields[0] == fields[3] {
                if let Some(first) = projected.get(0) {
                    assert!(std::ptr::eq(first, projected.get(3).unwrap()));
                }
            }
        }
        drop(full);
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn positional_decoding_preserves_unselected_errors_and_releases_resources() {
    let control = StorageReadControl::with_limit(1 << 20);
    let unrelated = control.memory().reserve(7).unwrap();
    for input in [
        "[]",
        "null",
        "false",
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
        for fields in [&[][..], &["missing"][..], &["b", "a", "b"][..]] {
            let actual = decode_legacy_document_slots_budgeted(input.as_bytes(), fields, &control)
                .unwrap_err();
            assert_eq!(actual.to_string(), expected.to_string(), "{input}");
            assert_eq!(control.memory().used(), 7);
        }
    }
    let full = control
        .memory()
        .reserve(control.memory().limit() - 7)
        .unwrap();
    assert!(matches!(
        decode_legacy_document_slots_budgeted(b"{}", &["a"], &control),
        Err(StorageBackendError::Memory(_))
    ));
    drop(full);
    assert_eq!(control.memory().used(), 7);
    control.cancellation().cancel();
    assert!(matches!(
        decode_legacy_document_slots_budgeted(b"{}", &[], &control),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert_eq!(control.memory().used(), 7);
    drop(unrelated);
}
