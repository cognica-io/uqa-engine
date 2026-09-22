//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::super::{decode_stored_document_value, encode_stored_document_value};
use super::*;
use crate::document_store::StoredDocument;
use crate::StorageBackendError;
use uqa_core::{memory::MemoryError, Value};

fn encoded(prefix: &[u8], body: &str) -> Vec<u8> {
    [prefix, body.as_bytes()].concat()
}

fn parity(bytes: &[u8]) -> Option<RetainedStoredDocument> {
    let expected = decode_stored_document_value(bytes);
    let control = StorageReadControl::with_limit(8 * 1024 * 1024);
    let actual = decode_retained_stored_document_value(bytes, &control);
    match (expected, actual) {
        (Ok(expected), Ok(actual)) => {
            assert_eq!(actual.metadata(), expected.metadata());
            assert_eq!(
                serde_json::to_vec(actual.fields()).unwrap(),
                serde_json::to_vec(expected.fields()).unwrap(),
                "{bytes:?}"
            );
            assert!(control.memory().used() > 0);
            Some(actual)
        }
        (Err(_), Err(_)) => {
            assert_eq!(control.memory().used(), 0);
            None
        }
        (expected, actual) => {
            panic!("codec parity for {bytes:?}: ordinary={expected:?}, retained={actual:?}")
        }
    }
}

#[test]
fn current_records_preserve_user_fields_metadata_and_sequence_envelopes() {
    for body in [
        r#"{"fields":{}}"#,
        r#"{"fields":{"x":[1,2]},"tuple_xmin":null}"#,
        r#"{"fields":{"xmin":71,"\u0000uqa.system.xmin":7},"tuple_xmin":4294967295}"#,
        r#"{"tuple_xmin":0,"fields":{"$uqa_type":"void"}}"#,
        r#"[{"$uqa_type":"row","values":[1,2]},9]"#,
        "[{},null]",
        r#"{"unknown":1e999999999,"fields":{"body":"x"}}"#,
        r#"{"unknown":{"$serde_json::private::Number":false},"fields":{}}"#,
        r#"{"fields":{"x":1},"fields":{"x":2}}"#,
        r#"{"fields":{},"tuple_xmin":null,"tuple_xmin":null}"#,
        r#"{"fields":{},"tuple_xmin":-0}"#,
        r#"{"fields":{},"tuple_xmin":1.0}"#,
        r#"{"fields":{},"tuple_xmin":1e0}"#,
        r#"{"fields":{},"tuple_xmin":-1}"#,
        r#"{"fields":{},"tuple_xmin":4294967296}"#,
        r#"{"fields":[],"tuple_xmin":1}"#,
        r#"{"tuple_xmin":1}"#,
        "[]",
        "[{}]",
        "[{},null,0]",
        "null",
    ] {
        parity(&encoded(DOCUMENT_VALUE_V2_PREFIX, body));
    }
    let fields = [
        ("bytes".into(), Value::Bytes(vec![1, 2, 3])),
        ("row".into(), Value::Row(vec![Value::Int(1), Value::Null])),
        (
            "list".into(),
            Value::List(vec![Value::Int(1), Value::Int(2)]),
        ),
    ]
    .into();
    let source = StoredDocument::with_metadata(fields, DocumentMetadata::with_tuple_xmin(37));
    parity(&encode_stored_document_value(&source).unwrap()).unwrap();
}

#[test]
fn previous_formats_preserve_array_rules_and_migrate_only_storage_metadata() {
    let body = r#"{"bytes":[1,2],"empty":[],"list":[1,300],"nested":[[3,4]],"xmin":51,"\u0000uqa.system.xmin":9,"\u0000uqa.user.xmin":true}"#;
    let legacy = parity(body.as_bytes()).unwrap();
    let modern = parity(&encoded(DOCUMENT_VALUE_V1_PREFIX, body)).unwrap();
    assert!(matches!(&legacy.fields()["bytes"], Value::Bytes(_)));
    assert!(matches!(&legacy.fields()["empty"], Value::Bytes(_)));
    assert!(matches!(&modern.fields()["bytes"], Value::List(_)));
    assert!(matches!(&modern.fields()["empty"], Value::List(_)));
    for value in [legacy, modern] {
        assert_eq!(value.metadata().tuple_xmin(), Some(9));
        assert_eq!(value.fields()["xmin"], Value::Int(51));
        assert!(!value.fields().contains_key("\0uqa.system.xmin"));
        assert!(!value.fields().contains_key("\0uqa.user.xmin"));
    }
    for body in [
        r#"{"$uqa_type":"bytes","hex":"0102"}"#,
        r#"{"\u0000uqa.system.xmin":-1}"#,
        r#"{"\u0000uqa.system.xmin":4294967296}"#,
        r#"{"\u0000uqa.system.xmin":"7"}"#,
        r#"{"\u0000uqa.system.xmin":3,"xmin":3}"#,
    ] {
        parity(body.as_bytes());
        parity(&encoded(DOCUMENT_VALUE_V1_PREFIX, body));
    }
}

#[test]
fn legacy_tags_convert_modern_children_before_falling_back_to_original_subtrees() {
    for value in [
        r#"{"$uqa_type":"row","values":[1,2]}"#,
        r#"{"$uqa_type":"row","values":[[1,2],[]]}"#,
        r#"{"$uqa_type":"record","fields":[["a",[1,2]],["b",[]]]}"#,
        r#"{"$uqa_type":"array","lower_bounds":[2],"values":[1,2]}"#,
        r#"{"$uqa_type":"array","lower_bounds":[0,1],"values":[[],[]]}"#,
        r#"{"$uqa_type":"row","values":[1,2],"extra":true}"#,
        r#"{"$uqa_type":"bytes","hex":"not hex","values":[1,2]}"#,
        r#"{"$uqa_type":"unknown","values":[[1,2],[]]}"#,
        r#"{"$uqa_type":"row","values":[18446744073709551616]}"#,
        r#"{"$uqa_type":"row","values":[-9223372036854775809]}"#,
        r#"{"$uqa_type":"row","values":[1e400]}"#,
        r#"{"$uqa_type":"unknown","values":[1e400]}"#,
        r#"{"$uqa_type":"row","values":[1e200000],"values":[1,2]}"#,
    ] {
        parity(format!(r#"{{"v":{value}}}"#).as_bytes());
    }
}

#[test]
fn legacy_normalization_preserves_private_envelopes_and_duplicate_validation_order() {
    for body in [
        r#"{"v":{"$serde_json::private::Number":"1"}}"#,
        r#"{"v":{"$serde_json::private::Number":"1e400"}}"#,
        r#"{"v":{"$serde_json::private::Number":" 1"}}"#,
        r#"{"v":{"$serde_json::private::Number":"1 "}}"#,
        r#"{"v":{"$serde_json::private::Number":"01"}}"#,
        r#"{"v":{"$serde_json::private::Number":1}}"#,
        r#"{"v":{"$serde_json::private::Number":"1","x":2}}"#,
        r#"{"v":{"x":2,"$serde_json::private::Number":"1"}}"#,
        r#"{"v":{"$serde_json::private::RawValue":"[1,2]"}}"#,
        r#"{"v":{"$serde_json::private::RawValue":"invalid"}}"#,
        r#"{"$serde_json::private::RawValue":"{\"x\":[1,2]}"}"#,
        r#"{"$serde_json::private::RawValue":"3"}"#,
        r#"{"$serde_json::private::Number":"1"}"#,
        r#"{"v":{"$uqa_type":"row","values":[{"$serde_json::private::RawValue":"[1,2]"}]}}"#,
        r#"{"v":1e400,"v":[1,2]}"#,
        r#"{"v":1e400,"\u0076":[1,2]}"#,
        r#"{"v":{"$serde_json::private::Number":"invalid"},"v":1}"#,
        r#"{"v":{"$serde_json::private::RawValue":"invalid"},"v":1}"#,
    ] {
        parity(body.as_bytes());
    }
}

#[test]
fn numeric_semantics_and_invalid_inputs_match_each_document_format() {
    for number in [
        "0",
        "-0",
        "1.0",
        "1e0",
        "-1e-99999",
        "9223372036854775807",
        "9223372036854775808",
        "18446744073709551615",
        "18446744073709551616",
        "-9223372036854775809",
        "340282366920938463463374607431768211456",
        "1e400",
        "1e131072",
    ] {
        let body = format!(r#"{{"v":{number}}}"#);
        parity(body.as_bytes());
        parity(&encoded(DOCUMENT_VALUE_V1_PREFIX, &body));
        parity(&encoded(
            DOCUMENT_VALUE_V2_PREFIX,
            &format!(r#"{{"fields":{body}}}"#),
        ));
    }
    for body in [
        "",
        " ",
        "[]",
        "[1,2]",
        "false",
        "{",
        r#"{"x":1,}"#,
        r#"{"x":"\uD800"}"#,
    ] {
        parity(body.as_bytes());
        parity(&encoded(DOCUMENT_VALUE_V1_PREFIX, body));
        parity(&encoded(DOCUMENT_VALUE_V2_PREFIX, body));
    }
    parity(&[0xff]);
}

#[test]
fn known_fields_keep_the_old_depth_limit_while_ignored_envelopes_are_iterative() {
    for depth in [125, 126, 127, 128] {
        let body = format!(r#"{{"v":{}0{}}}"#, "[".repeat(depth), "]".repeat(depth));
        parity(body.as_bytes());
        parity(&encoded(DOCUMENT_VALUE_V1_PREFIX, &body));
        parity(&encoded(
            DOCUMENT_VALUE_V2_PREFIX,
            &format!(r#"{{"fields":{body}}}"#),
        ));
    }
    let ignored = format!(
        r#"{{"unknown":{}0{},"fields":{{}}}}"#,
        "[".repeat(1024),
        "]".repeat(1024)
    );
    parity(&encoded(DOCUMENT_VALUE_V2_PREFIX, &ignored)).unwrap();
    parity(&encoded(
        DOCUMENT_VALUE_V2_PREFIX,
        r#"{"unknown":"\uD800","fields":{}}"#,
    ))
    .unwrap();
}

#[test]
fn current_unknown_strings_preserve_the_byte_input_compatibility_boundary() {
    for body in [
        &b"{\"unknown\":\"\xff\",\"fields\":{}}"[..],
        &b"{\"unknown\":{\"\xff\":[\"\xc3\"]},\"fields\":{}}"[..],
        &b"{\"\xff\":null,\"fields\":{}}"[..],
        &b"{\"fields\":{\"value\":\"\xff\"}}"[..],
        &b"{\"fields\":{},\"tuple_xmin\":\"\xff\"}"[..],
        &b"{\"unknown\":\"\\\xff\",\"fields\":{}}"[..],
        &b"{\"unknown\":\"\x1f\",\"fields\":{}}"[..],
        &b"{\"unknown\":\xff,\"fields\":{}}"[..],
    ] {
        parity(&[DOCUMENT_VALUE_V2_PREFIX, body].concat());
    }
}

#[test]
fn quota_and_cancellation_failures_release_scratch_and_preserve_existing_readers() {
    let control = StorageReadControl::with_limit(4096);
    let prior = decode_retained_stored_document_value(br#"{"body":"prior"}"#, &control).unwrap();
    let sibling = prior.clone();
    let retained = control.memory().used();
    let large = format!(r#"{{"body":"{}"}}"#, "x".repeat(8192));
    for prefix in [&[][..], DOCUMENT_VALUE_V1_PREFIX, DOCUMENT_VALUE_V2_PREFIX] {
        let body = if prefix == DOCUMENT_VALUE_V2_PREFIX {
            format!(r#"{{"fields":{large}}}"#)
        } else {
            large.clone()
        };
        assert!(matches!(
            decode_retained_stored_document_value(&encoded(prefix, &body), &control),
            Err(StorageBackendError::Memory(MemoryError::Limit { .. }))
        ));
        assert_eq!(control.memory().used(), retained);
    }
    assert!(decode_retained_stored_document_value(br#"{"bad":{"x":[1,2],}}"#, &control).is_err());
    assert_eq!(control.memory().used(), retained);
    control.cancellation().cancel();
    assert!(matches!(
        decode_retained_stored_document_value(b"{}", &control),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert_eq!(control.memory().used(), retained);
    drop(prior);
    assert_eq!(control.memory().used(), retained);
    assert_eq!(sibling.fields()["body"], Value::Str("prior".into()));
    drop(sibling);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn legacy_migration_releases_removed_payload_and_unique_outputs_move_storage_fields() {
    let control = StorageReadControl::with_limit(1 << 20);
    let input = format!(
        r#"{{"body":"kept","\u0000uqa.user.xmin":"{}","\u0000uqa.system.xmin":7}}"#,
        "x".repeat(8192)
    );
    let retained = decode_retained_stored_document_value(input.as_bytes(), &control).unwrap();
    assert!(control.memory().used() < 8192);
    assert_eq!(retained.metadata().tuple_xmin(), Some(7));
    let Value::Str(text) = &retained.fields()["body"] else {
        panic!()
    };
    let address = text.as_ptr();
    let output = retained.into_stored();
    let Value::Str(text) = &output.fields()["body"] else {
        panic!()
    };
    assert_eq!(text.as_ptr(), address);
    assert_eq!(output.metadata().tuple_xmin(), Some(7));
    assert_eq!(control.memory().used(), 0);
}
