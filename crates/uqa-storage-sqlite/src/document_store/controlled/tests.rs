//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::document_store::{blob as ordinary, typed_value::StoredValue};
use std::collections::BTreeMap;
use uqa_core::{ArrayValue, DecimalValue, TemporalValue};

mod identities;
mod native;

fn encoded(value: Value) -> Vec<u8> {
    serde_json::to_vec(&StoredValue::from_value(value)).unwrap()
}

fn parity(input: &[u8]) {
    let expected = serde_json::from_slice::<StoredValue>(input).map(StoredValue::into_value);
    let control = StorageReadControl::with_limit(8 << 20);
    let actual = typed::decode(input, &control, 127, false);
    match (expected, actual) {
        (Ok(expected), Ok(actual)) => {
            assert_eq!(
                encoded(expected),
                encoded((*actual).clone()),
                "input={input:?}"
            );
            drop(actual);
        }
        (Err(_), Err(JsonReadError::InvalidJson)) => {}
        (expected, actual) => {
            panic!("typed parity failed for {input:?}: {expected:?} / {actual:?}")
        }
    }
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn typed_blob_round_trips_every_persisted_variant() {
    let scalars = vec![
        Value::Null,
        Value::Void,
        Value::Bool(true),
        Value::Int(i64::MIN),
        Value::Float(f64::from_bits(0x7ff8_0000_0000_0042)),
        Value::Float(-0.0),
        Value::Str("한글\u{0000}あ".into()),
        Value::FixedChar("space  ".into()),
        Value::Bytes(vec![0, 255]),
        Value::Decimal(DecimalValue::parse("1.234e1000").unwrap()),
        Value::Json("{\"a\":1}".into()),
        Value::JsonB("{\"b\":2}".into()),
        Value::Temporal(TemporalValue::Date { days: i32::MIN }),
        Value::Temporal(TemporalValue::Time { micros: i64::MAX }),
        Value::Temporal(TemporalValue::TimeTz {
            micros: -1,
            offset_minutes: -55,
        }),
        Value::Temporal(TemporalValue::Timestamp { micros: i64::MIN }),
        Value::Temporal(TemporalValue::TimestampTz { micros: i64::MAX }),
        Value::Temporal(TemporalValue::Interval {
            months: -3,
            days: 5,
            micros: 7,
        }),
        Value::Array(
            ArrayValue::with_lower_bounds(vec![Value::Int(1), Value::Int(2)], vec![-2]).unwrap(),
        ),
        Value::Array(ArrayValue::with_lower_bounds(Vec::new(), Vec::new()).unwrap()),
    ];
    for value in &scalars {
        parity(&encoded(value.clone()));
    }
    for value in [
        Value::List(scalars.clone()),
        Value::Row(scalars),
        Value::Record(vec![
            ("duplicate".into(), Value::Int(1)),
            ("duplicate".into(), Value::Int(2)),
        ]),
        Value::Map(BTreeMap::from([(
            "nested".into(),
            Value::List(vec![Value::Bytes(vec![1])]),
        )])),
    ] {
        parity(&encoded(value));
    }
}

#[test]
fn typed_envelopes_preserve_serde_sequence_duplicate_and_ignored_field_rules() {
    for input in [
        r#"{"kind":"null"}"#,
        r#"{"kind":"null","value":null}"#,
        r#"["void",null]"#,
        r#"["null"]"#,
        r#"["int",7]"#,
        r#"{"value":7,"kind":"int"}"#,
        r#"{"kind":"int","value":7,"unknown":"\uD800"}"#,
        r#"{"value":7,"unknown":"\uD800","kind":"int"}"#,
        r#"{"kind":"int","value":1,"value":2}"#,
        r#"{"kind":"int","kind":"int","value":1}"#,
        r#"{"kind":"map","value":{"a":{"kind":"str","value":"old"},"a":{"kind":"str","value":"new"}}}"#,
        r#"{"kind":"decimal","value":{"$uqa_type":"decimal","value":"1e100"}}"#,
        r#"{"kind":"decimal","value":["decimal","1e-10"]}"#,
        r#"{"kind":"decimal","value":{"$uqa_type":"decimal","value":"0","extra":"\uD800"}}"#,
        r#"{"kind":"array","value":[[1,2],[-1]]}"#,
        r#"{"kind":"array","value":{"elements":[1],"lower_bounds":[1],"unknown":"\uD800"}}"#,
        r#"{"kind":"temporal","value":["date",1]}"#,
        r#"{"kind":"temporal","value":{"$uqa_type":"date","days":1,"micros":2}}"#,
        r#"{"kind":"temporal","value":{"$uqa_type":"date","days":1,"unknown":2}}"#,
        r#"{"kind":"bytes","value":[256]}"#,
        r#"{"kind":"int","value":1.0}"#,
        r#"{"kind":"float_bits","value":18446744073709551615}"#,
    ] {
        parity(input.as_bytes());
    }
}

#[test]
fn typed_integer_fields_keep_their_declared_width_instead_of_canonical_value_casts() {
    for integer in [
        "-2147483649",
        "2147483648",
        "9223372036854775808",
        "18446744073709551615",
        "1.0",
        r#"{"$serde_json::private::Number":"1"}"#,
    ] {
        for template in [
            r#"{"kind":"array","value":{"elements":[1],"lower_bounds":[NUMBER]}}"#,
            r#"{"kind":"temporal","value":{"$uqa_type":"date","days":NUMBER}}"#,
            r#"{"kind":"temporal","value":{"$uqa_type":"time","micros":NUMBER}}"#,
            r#"{"value":{"$uqa_type":"time","micros":NUMBER},"kind":"temporal"}"#,
        ] {
            parity(template.replace("NUMBER", integer).as_bytes());
        }
    }
}

#[test]
fn typed_integer_tokens_and_recursion_limits_match_the_stored_deserializer() {
    for number in [
        "-0",
        "0",
        "-1",
        "1.0",
        "1e0",
        "1e400",
        "18446744073709551615",
    ] {
        for kind in ["int", "float_bits"] {
            parity(format!(r#"{{"kind":"{kind}","value":{number}}}"#).as_bytes());
            parity(format!(r#"{{"value":{number},"kind":"{kind}"}}"#).as_bytes());
        }
        parity(format!(r#"{{"kind":"bytes","value":[{number}]}}"#).as_bytes());
        parity(
            format!(r#"{{"kind":"temporal","value":{{"$uqa_type":"time","micros":{number}}}}}"#)
                .as_bytes(),
        );
    }
    for depth in [0, 1, 62, 63, 64, 126, 127, 128] {
        let mut encoded = r#"{"kind":"null"}"#.to_owned();
        for _ in 0..depth {
            encoded = format!(r#"{{"kind":"list","value":[{encoded}]}}"#);
        }
        parity(encoded.as_bytes());
    }
}

#[test]
fn numeric_blob_decoders_preserve_bits_and_reject_corrupt_lengths() {
    let control = StorageReadControl::with_limit(1 << 20);
    let floats = [
        0.0,
        -0.0,
        f64::INFINITY,
        f64::from_bits(0x7ff8_0000_0000_0007),
    ];
    let bytes = floats
        .iter()
        .flat_map(|n| n.to_le_bytes())
        .collect::<Vec<_>>();
    for encoding in [
        crate::document_store::VALUE_BLOB_F64_LIST,
        crate::document_store::VALUE_BLOB_F64_TENSOR,
    ] {
        let marker_value = ordinary::value_blob_marker("field".into(), encoding);
        let marker = marker(&marker_value).unwrap();
        let mut payload = bytes.clone();
        let expected = if encoding == crate::document_store::VALUE_BLOB_F64_TENSOR {
            payload = [
                2_u32.to_le_bytes().as_slice(),
                2_u32.to_le_bytes().as_slice(),
                &bytes,
            ]
            .concat();
            ordinary::decode_f64_tensor_blob(&payload).unwrap().unwrap()
        } else {
            ordinary::decode_f64_list_blob(&payload).unwrap().unwrap()
        };
        let value = decode_blob(&payload, marker, &control).unwrap();
        assert_eq!(encoded(expected), encoded((*value).clone()));
        drop(value);
        payload.pop();
        assert!(matches!(
            decode_blob(&payload, marker, &control),
            Err(JsonReadError::InvalidJson)
        ));
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn typed_and_binary_quota_failures_preserve_previous_readers_and_release_workspace() {
    let control = StorageReadControl::with_limit(1 << 20);
    let first = typed::decode(
        &encoded(Value::Str("retained".repeat(64))),
        &control,
        127,
        false,
    )
    .unwrap();
    let baseline = control.memory().used();
    let full = control
        .memory()
        .reserve(control.memory().limit() - baseline)
        .unwrap();
    assert!(matches!(
        typed::decode(&encoded(Value::Bytes(vec![1; 1024])), &control, 127, false),
        Err(JsonReadError::Memory(_))
    ));
    let marker_value = ordinary::blob_marker("field".into());
    assert!(matches!(
        decode_blob(&[1; 1024], marker(&marker_value).unwrap(), &control),
        Err(JsonReadError::Memory(_))
    ));
    drop(full);
    assert_eq!(control.memory().used(), baseline);
    control.cancellation().cancel();
    assert!(matches!(
        decode_blob(&[], marker(&marker_value).unwrap(), &control),
        Err(JsonReadError::Cancelled(_))
    ));
    assert_eq!(*first, Value::Str("retained".repeat(64)));
    drop(first);
    assert_eq!(control.memory().used(), 0);
}
