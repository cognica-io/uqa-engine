//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::key_value::codec::decode_retained_stored_document_value;
use uqa_core::Value;

fn parity(prefix: &[u8], input: &str) {
    let bytes = [prefix, input.as_bytes()].concat();
    let control = StorageReadControl::with_limit(1 << 20);
    let fields = [
        "a",
        "missing",
        "b",
        "a",
        "xmin",
        LEGACY_SYSTEM_XMIN,
        LEGACY_USER_XMIN_MARKER,
    ];
    let expected = decode_retained_stored_document_value(&bytes, &control);
    let actual = decode_stored_document_slots(&bytes, &fields, &control);
    match (expected, actual) {
        (Ok(expected), Ok(actual)) => {
            for (position, field) in fields.iter().enumerate() {
                assert_eq!(
                    serde_json::to_vec(&actual.get(position)).unwrap(),
                    serde_json::to_vec(&expected.fields().get(*field)).unwrap(),
                    "{input}: {field}"
                );
            }
            if let Some(first) = actual.get(0) {
                assert!(std::ptr::eq(first, actual.get(3).unwrap()));
            }
        }
        (Err(expected), Err(actual)) => {
            assert_eq!(expected.to_string(), actual.to_string(), "{input}");
        }
        (expected, actual) => panic!("{input}: {expected:?} / {actual:?}"),
    }
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn positional_durable_formats_preserve_values_metadata_and_error_order() {
    for fields in [
        "{}",
        r#"{"a":[1,2],"b":null}"#,
        r#"{"a":null,"a":{"$uqa_type":"row","values":[1,2]},"b":{"$uqa_type":"bytes","hex":"ff"}}"#,
        r#"{"a":{"$serde_json::private::Number":"bad"},"a":1}"#,
        r#"{"a":{"$serde_json::private::Number":"bad"},"b":?}"#,
        r#"{"a":1,"b":01}"#,
        r#"{"a":1,"\u0061":2,"\u0000uqa.system.xmin":42,"xmin":42,"\u0000uqa.user.xmin":true}"#,
        r#"{"a":1,"\u0000uqa.system.xmin":-1}"#,
        r#"{"a":1,"\u0000uqa.system.xmin":"bad"}"#,
        r#"{"\u0000uqa.system.xmin":-1,"\u0000uqa.system.xmin":42}"#,
        "[]",
        "null",
    ] {
        parity(&[], fields);
        parity(DOCUMENT_VALUE_V1_PREFIX, fields);
        parity(
            DOCUMENT_VALUE_V2_PREFIX,
            &format!(r#"{{"fields":{fields},"tuple_xmin":42}}"#),
        );
        parity(DOCUMENT_VALUE_V2_PREFIX, &format!("[{fields},null]"));
    }
    for input in [
        r#"{"fields":{"a":1},"fields":{"a":2}}"#,
        r#"{"fields":{},"tuple_xmin":-1}"#,
        r#"{"fields":{"a":{"$serde_json::private::Number":"bad"}},"tuple_xmin":-1}"#,
        r#"{"fields":{"a":1},"unknown":1e999999999}"#,
        "[{},null,1]",
        "{}",
    ] {
        parity(DOCUMENT_VALUE_V2_PREFIX, input);
    }
}

#[test]
fn positional_modern_fields_do_not_retain_names_or_overwritten_values() {
    let name = "a".repeat(32 << 10);
    let body = format!(r#"{{"{name}":1,"b":null}}"#);
    for prefix in [DOCUMENT_VALUE_V1_PREFIX, DOCUMENT_VALUE_V2_PREFIX] {
        let body = if prefix == DOCUMENT_VALUE_V2_PREFIX {
            format!(r#"{{"fields":{body}}}"#)
        } else {
            body.clone()
        };
        let bytes = [prefix, body.as_bytes()].concat();
        let control = StorageReadControl::with_limit(4096);
        let mut output = None;
        let allocated = allocation_counter::measure(|| {
            output = Some(
                decode_stored_document_slots(&bytes, &[&name, "b", &name, "missing"], &control)
                    .unwrap(),
            );
        });
        let output = output.unwrap();
        assert!(allocated.bytes_total < 4096, "{allocated:?}");
        assert_eq!(output.get(0), Some(&Value::Int(1)));
        assert_eq!(output.get(1), Some(&Value::Null));
        assert_eq!(output.get(3), None);
        assert!(std::ptr::eq(output.get(0).unwrap(), output.get(2).unwrap()));
        drop(output);
        assert_eq!(control.memory().used(), 0);
    }
    let input = format!(
        r#"{{{}"a":null}}"#,
        format!(r#""a":"{}","#, "x".repeat(2048)).repeat(32)
    );
    let bytes = [DOCUMENT_VALUE_V1_PREFIX, input.as_bytes()].concat();
    let control = StorageReadControl::with_limit(16 << 10);
    let output = decode_stored_document_slots(&bytes, &["a", "a"], &control).unwrap();
    assert_eq!(output.values(), &[Value::Null]);
    assert!(control.memory().used() < 1024);
    drop(output);
    assert_eq!(control.memory().used(), 0);
}
