//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::super::{parse_json, typed_json_value};
use super::*;
use uqa_core::{memory::MemoryBudget, ArrayValue, CancellationToken, DecimalValue, TemporalValue};

fn reference(value: &Value, jsonb: bool) -> Result<Value> {
    match (jsonb, value) {
        (false, Value::Json(_)) | (true, Value::JsonB(_)) => Ok(value.clone()),
        (false, Value::Str(text) | Value::FixedChar(text)) => {
            parse_json(text)?;
            Ok(Value::Json(text.clone()))
        }
        (true, Value::Json(text) | Value::Str(text) | Value::FixedChar(text)) => {
            typed_json_value(&parse_json(text)?, true)
        }
        _ => typed_json_value(&super::super::value_to_json(value), jsonb),
    }
}

#[test]
fn legacy_vectors_preserve_postgresql_json_element_categories_and_array_shape() {
    use uqa_core::{LegacyVectorKind, LegacyVectorValue};

    // PostgreSQL 18.4 to_json() emits int2vector elements as numbers and oidvector elements as strings, including vectors nested as atomic SQL array elements.
    let memory = MemoryBudget::new(4096);
    let cancellation = CancellationToken::new();
    let control = ProductionControl::new(&memory, &cancellation, &cancellation);
    for (kind, expected) in [
        (LegacyVectorKind::SmallInteger, serde_json::json!([1, 2])),
        (LegacyVectorKind::Oid, serde_json::json!(["1", "2"])),
    ] {
        let vector = Value::LegacyVector(
            LegacyVectorValue::try_new(kind, vec![Value::Int(1), Value::Int(2)]).unwrap(),
        );
        let empty = Value::LegacyVector(LegacyVectorValue::try_new(kind, Vec::new()).unwrap());
        for (value, expected) in [
            (vector.clone(), expected.clone()),
            (empty.clone(), serde_json::json!([])),
            (
                Value::Array(ArrayValue::try_new(vec![vector, empty]).unwrap()),
                serde_json::json!([expected, []]),
            ),
        ] {
            assert_eq!(
                super::super::value_to_json_text(&value),
                expected.to_string()
            );
            assert_eq!(super::super::value_to_json(&value), expected);
            let output = format_value_as_json_with_control(&value, &control).unwrap();
            assert_eq!(&**output, expected.to_string());
            assert_eq!(memory.used(), output.reserved_bytes());
            drop(output);
            assert_eq!(memory.used(), 0);
        }
    }
}

#[test]
fn admitted_json_preserves_lexical_casts_and_jsonb_canonical_number_and_key_rules() {
    let memory = MemoryBudget::new(1024 * 1024);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&memory, &original, &invoking);
    for text in [
        r#" { "long":1e0, "b":2, "a":3, "long":1.2300 } "#,
        r"[-0,-0.0,1E01,1E+001,1e-001,18446744073709551616,1.2300]",
        r#"{"\u0061":1,"a":2,"é":"\n\uD83D\uDE03"}"#,
        r#"{"a":1e999999,"a":2}"#,
        r#"{"$serde_json::private::Number":"1E02"}"#,
        r"[null,true,false,[],{}]",
    ] {
        for jsonb in [false, true] {
            for input in [Value::Str(text.into()), Value::Json(text.into())] {
                let value = cast_json_value_with_control(&input, jsonb, &control).unwrap();
                assert_eq!(
                    &*value,
                    &reference(&input, jsonb).unwrap(),
                    "{text} jsonb={jsonb}"
                );
                assert_eq!(memory.used(), value.reserved_bytes());
                drop(value);
                assert_eq!(memory.used(), 0);
            }
        }
    }
}

#[test]
fn admitted_compact_json_matches_existing_value_conversion_for_nested_carriers() {
    let memory = MemoryBudget::new(1024 * 1024);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&memory, &original, &invoking);
    let array = ArrayValue::with_lower_bounds(
        vec![
            Value::List(vec![Value::Int(1), Value::Null]),
            Value::List(vec![Value::Int(3), Value::Int(4)]),
        ],
        vec![-2, 5],
    )
    .unwrap();
    let values = [
        Value::Row((0..12).map(Value::Int).collect()),
        Value::Record(vec![
            ("z".into(), Value::Int(1)),
            ("a".into(), Value::Int(2)),
            ("z".into(), Value::Int(3)),
        ]),
        Value::Array(array),
        Value::List(vec![
            Value::Void,
            Value::FixedChar("x  ".into()),
            Value::Bytes(vec![0, 15, 255]),
            Value::Float(f64::NAN),
            Value::Float(f64::INFINITY),
            Value::Float(f64::NEG_INFINITY),
            Value::Float(-0.0),
            Value::Float(1e-100),
            Value::Decimal(DecimalValue::parse("123.4500").unwrap()),
            Value::Temporal(
                TemporalValue::parse_timestamp_tz("2024-01-01T01:02:03+09:00").unwrap(),
            ),
        ]),
        Value::Json("invalid".into()),
        Value::Str(format!("{}\n\u{0000}😃", "한글".repeat(2048))),
        Value::Map(std::collections::BTreeMap::from([(
            "x".into(),
            Value::Json(r#"{"a":-0,"b":1E1}"#.into()),
        )])),
    ];
    for value in values {
        let output = format_value_as_json_with_control(&value, &control).unwrap();
        assert_eq!(&**output, super::super::value_to_json(&value).to_string());
        assert_eq!(memory.used(), output.reserved_bytes());
        drop(output);
        assert_eq!(memory.used(), 0);
        for jsonb in [false, true] {
            let output = cast_json_value_with_control(&value, jsonb, &control);
            match (output, reference(&value, jsonb)) {
                (Ok(actual), Ok(expected)) => assert_eq!(&*actual, &expected),
                (Err(actual), Err(expected)) => assert_eq!(actual.sqlstate(), expected.sqlstate()),
                _ => panic!("conversion disagreement"),
            }
            assert_eq!(memory.used(), 0);
        }
    }
}

#[test]
fn admitted_json_failures_release_scratch_and_preserve_prior_results() {
    let value = Value::Str(r#"{"long":[1,2,3],"a":"escaped\nvalue"}"#.into());
    for allowance in [8, 32, 128, 512, 2048, 16384] {
        let memory = MemoryBudget::new(allowance);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&memory, &original, &invoking);
        let prior = control.copy_text("prior").unwrap();
        let before = memory.used();
        match cast_json_value_with_control(&value, true, &control) {
            Ok(output) => {
                drop(output);
            }
            Err(error) => assert_eq!(error.sqlstate(), Some("53200")),
        }
        assert_eq!(memory.used(), before);
        for cancellation in [&original, &invoking] {
            cancellation.cancel();
            assert_eq!(
                cast_json_value_with_control(&value, true, &control)
                    .unwrap_err()
                    .sqlstate(),
                Some("57014")
            );
            assert_eq!(
                format_value_as_json_with_control(&value, &control)
                    .unwrap_err()
                    .sqlstate(),
                Some("57014")
            );
            assert_eq!(memory.used(), before);
            cancellation.reset();
        }
        assert_eq!(&**prior, "prior");
        drop(prior);
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn admitted_json_keeps_existing_invalid_input_and_nesting_boundaries() {
    let memory = MemoryBudget::new(1024 * 1024);
    let cancellation = CancellationToken::new();
    let control = ProductionControl::new(&memory, &cancellation, &cancellation);
    for text in [
        "[1,]",
        r#""\uD800""#,
        r#"{"$serde_json::private::Number":1}"#,
        r#"{"$serde_json::private::Number":" 1"}"#,
        r#"{"$serde_json::private::Number":"1","a":2}"#,
    ] {
        let input = Value::Str(text.into());
        let actual = cast_json_value_with_control(&input, false, &control).unwrap_err();
        assert_eq!(
            actual.sqlstate(),
            reference(&input, false).unwrap_err().sqlstate()
        );
        assert_eq!(memory.used(), 0);
    }
    for depth in [126, 127, 128] {
        let input = Value::Str(format!("{}0{}", "[".repeat(depth), "]".repeat(depth)));
        assert_eq!(
            cast_json_value_with_control(&input, false, &control).is_ok(),
            reference(&input, false).is_ok()
        );
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn assignment_carrier_keeps_json_strings_float_conversion_and_lossy_bytes_distinct() {
    use crate::expr::json_carrier::{core_value_to_json, value_to_text_with_control};

    let memory = MemoryBudget::new(1024 * 1024);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&memory, &original, &invoking);
    let nested = Value::List(vec![
        Value::Str("{\"a\":1}".into()),
        Value::Decimal(DecimalValue::parse("1.2300").unwrap()),
        Value::Bytes(vec![b'a', 0xff, b'b']),
        Value::List(vec![Value::Str("true".into()), Value::Str("plain".into())]),
    ]);
    let expected = core_value_to_json(&nested).to_string();
    let output = value_to_text_with_control(&nested, &control).unwrap();
    assert_eq!(&**output, expected);
    assert_ne!(&**output, super::super::value_to_json(&nested).to_string());
    assert_eq!(memory.used(), output.reserved_bytes());
    drop(output);
    for bytes in [
        vec![0xf0, 0x9f],
        vec![0xe1, 0x80, b'a', 0xff, 0xc0, 0xaf],
        [vec![b'a'; 4095], "😃".as_bytes().to_vec(), vec![0xff]].concat(),
    ] {
        let expected = String::from_utf8_lossy(&bytes).into_owned();
        let output = value_to_text_with_control(&Value::Bytes(bytes), &control).unwrap();
        assert_eq!(&**output, expected);
        assert_eq!(memory.used(), output.reserved_bytes());
        drop(output);
        assert_eq!(memory.used(), 0);
    }
    for value in [0.0, -0.0, 1e100, f64::NAN] {
        let output = value_to_text_with_control(&Value::Float(value), &control).unwrap();
        assert_eq!(&**output, value.to_string());
    }
    assert_eq!(memory.used(), 0);
    for token in [&original, &invoking] {
        token.cancel();
        assert_eq!(
            value_to_text_with_control(&nested, &control)
                .unwrap_err()
                .sqlstate(),
            Some("57014")
        );
        assert_eq!(memory.used(), 0);
        token.reset();
    }
}
