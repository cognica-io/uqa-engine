//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn shared_legacy_fields_preserve_root_names_and_do_not_migrate_tuple_metadata() {
    let control = StorageReadControl::with_limit(1 << 20);
    let fields = decode_legacy_document_fields_budgeted(
        br#"{"$uqa_type":"row","values":[1,2],"xmin":7,"__uqa_system_xmin":8,"empty":[],"nested":{"bytes":[0,255]}}"#,
        &control,
    ).unwrap();
    assert_eq!(fields.get("$uqa_type"), Some(&Value::Str("row".into())));
    assert_eq!(fields.get("values"), Some(&Value::Bytes(vec![1, 2])));
    assert_eq!(fields.get("xmin"), Some(&Value::Int(7)));
    assert_eq!(fields.get("__uqa_system_xmin"), Some(&Value::Int(8)));
    assert_eq!(fields.get("empty"), Some(&Value::Bytes(Vec::new())));
    assert_eq!(
        fields.get("nested"),
        Some(&Value::Map(Document::from([(
            "bytes".into(),
            Value::Bytes(vec![0, 255])
        )])))
    );
    assert!(control.memory().used() > 0);
    drop(fields);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn shared_single_values_match_legacy_field_values_and_keep_typed_failures() {
    let control = StorageReadControl::with_limit(1 << 20);
    let unrelated = control.memory().reserve(7).unwrap();
    for input in [
        "null",
        "true",
        "18446744073709551615",
        "[]",
        "[1,2,255]",
        "[1,256]",
        r#"{"$uqa_type":"row","values":[1,2]}"#,
        r#"{"$uqa_type":"invalid","value":[1,2]}"#,
        r#"{"$serde_json::private::RawValue":"[1,2]"}"#,
    ] {
        let object = format!("{{\"field\":{input}}}");
        let fields = decode_legacy_document_fields_budgeted(object.as_bytes(), &control).unwrap();
        let value = decode_legacy_json_value_budgeted(input.as_bytes(), &control).unwrap();
        assert_eq!(fields.get("field"), Some(&*value));
    }
    assert_eq!(control.memory().used(), 7);
    for input in [b"1e400".as_slice(), b"[1,]", b"\"\\uD800\""] {
        assert!(decode_legacy_json_value_budgeted(input, &control).is_err());
        assert_eq!(control.memory().used(), 7);
    }
    let full = control
        .memory()
        .reserve(control.memory().limit() - 7)
        .unwrap();
    assert!(matches!(
        decode_legacy_json_value_budgeted(b"[1,2]", &control),
        Err(StorageBackendError::Memory(_))
    ));
    drop(full);
    control.cancellation().cancel();
    assert!(matches!(
        decode_legacy_document_fields_budgeted(b"{}", &control),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert!(matches!(
        decode_legacy_json_value_budgeted(b"null", &control),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert_eq!(control.memory().used(), 7);
    drop(unrelated);
}
