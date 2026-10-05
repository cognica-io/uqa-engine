//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{json::JsonReadError, memory::MemoryBudget, JsonValueDecoder};

fn label(type_oid: u32, key: &[u8]) -> Value {
    Value::Enum(EnumValue::new(
        type_oid,
        EnumLabelKey::from_bytes(key.to_vec()).unwrap(),
    ))
}

fn decode_controlled(text: &str) -> Result<Value, JsonReadError> {
    let memory = MemoryBudget::new(1 << 20);
    let cancellation = CancellationToken::new();
    JsonValueDecoder::new(&memory, &cancellation)
        .value(text)
        .map(|value| value.into_parts().0)
}

#[test]
fn enum_carriers_round_trip_through_serde_and_controlled_decoding() {
    let values = [
        label(16_390, &[64]),
        label(u32::MAX, &[0, 255, 7]),
        Value::Array(
            ArrayValue::try_new(vec![label(7, &[1]), Value::Null, label(7, &[2])]).unwrap(),
        ),
        Value::Record(vec![("mood".into(), label(9, &[128]))]),
    ];
    for value in values {
        let text = serde_json::to_string(&value).unwrap();
        let decoded: Value = serde_json::from_str(&text).unwrap();
        assert!(decoded.has_same_representation(&value), "{text}");
        let controlled = decode_controlled(&text).unwrap();
        assert!(controlled.has_same_representation(&value), "{text}");
    }
    assert_eq!(
        serde_json::to_string(&label(16_390, &[0x0a, 0xff])).unwrap(),
        r#"{"$uqa_type":"enum","type_oid":16390,"key":"0aff"}"#
    );
}

#[test]
fn malformed_enum_carriers_are_typed_failures() {
    let cases = [
        (
            r#"{"$uqa_type":"enum","type_oid":-1,"key":"01"}"#,
            "type OID out of range",
        ),
        (
            r#"{"$uqa_type":"enum","type_oid":4294967296,"key":"01"}"#,
            "type OID out of range",
        ),
        (
            r#"{"$uqa_type":"enum","type_oid":"7","key":"01"}"#,
            "type OID is not an integer",
        ),
        (
            r#"{"$uqa_type":"enum","type_oid":7,"key":1}"#,
            "label key is not hexadecimal text",
        ),
        (
            r#"{"$uqa_type":"enum","type_oid":7,"key":"0"}"#,
            "label key is not hexadecimal text",
        ),
        (
            r#"{"$uqa_type":"enum","type_oid":7,"key":"zz"}"#,
            "label key is not hexadecimal text",
        ),
        (
            r#"{"$uqa_type":"enum","type_oid":7,"key":""}"#,
            "enum label key is empty",
        ),
        (
            r#"{"$uqa_type":"enum","type_oid":7,"key":"0100"}"#,
            "ends with a zero byte",
        ),
    ];
    for (text, reason) in cases {
        let error = serde_json::from_str::<Value>(text).unwrap_err().to_string();
        assert!(error.contains("malformed enum value"), "{text}: {error}");
        assert!(error.contains(reason), "{text}: {error}");
        match decode_controlled(text) {
            Err(JsonReadError::Malformed {
                kind: "enum",
                reason: actual,
            }) => {
                assert!(actual.contains(reason), "{text}: {actual}");
            }
            other => panic!("{text}: {other:?}"),
        }
    }
    let oversized = format!(
        r#"{{"$uqa_type":"enum","type_oid":7,"key":"{}"}}"#,
        "01".repeat(crate::MAX_ENUM_LABEL_KEY_BYTES + 1)
    );
    assert!(serde_json::from_str::<Value>(&oversized)
        .unwrap_err()
        .to_string()
        .contains("exceeds its length limit"));
}

#[test]
fn other_field_sets_keep_the_document_map_interpretation() {
    for text in [
        r#"{"$uqa_type":"enum","type_oid":7}"#,
        r#"{"$uqa_type":"enum","key":"01","label":"x"}"#,
        r#"{"$uqa_type":"enum","type_oid":7,"key":"01","extra":true}"#,
    ] {
        let decoded: Value = serde_json::from_str(text).unwrap();
        assert!(matches!(decoded, Value::Map(_)), "{text}");
        assert!(
            matches!(decode_controlled(text).unwrap(), Value::Map(_)),
            "{text}"
        );
    }
}
