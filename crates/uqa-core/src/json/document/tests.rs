//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::json::JsonInputError;

mod output;
mod projection;
mod resources;

fn protocol() -> JsonScalarProtocol {
    JsonScalarProtocol::from_values(
        &serde_json::Number::from(1u8),
        serde_json::value::RawValue::NULL,
    )
    .unwrap()
}

fn compare(bytes: &[u8]) {
    let expected = serde_json::from_slice::<serde_json::Value>(bytes);
    let memory = MemoryBudget::new(8 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let actual = JsonDocument::read_with_control(bytes, &control, protocol())
        .and_then(|value| value.root().to_json_with_control(&control));
    match (expected, actual) {
        (Ok(expected), Ok(actual)) => assert_eq!(
            &*actual,
            &serde_json::to_string(&expected).unwrap(),
            "{}",
            String::from_utf8_lossy(bytes)
        ),
        (Err(expected), Err(JsonInputError::Diagnostic(actual))) => {
            assert_eq!(
                actual.to_string(),
                expected.to_string(),
                "{}",
                String::from_utf8_lossy(bytes)
            );
            assert_eq!(actual.is_data(), expected.is_data());
            assert_eq!(actual.is_syntax(), expected.is_syntax());
            assert_eq!(actual.is_eof(), expected.is_eof());
        }
        (expected, actual) => panic!(
            "{}: {expected:?} / {actual:?}",
            String::from_utf8_lossy(bytes)
        ),
    }
    assert_eq!(memory.used(), 0);
}

#[test]
fn native_json_document_preserves_complete_generic_values_and_number_projection() {
    for text in [
        "null",
        "true",
        "false",
        "[]",
        "{}",
        "0",
        "-0",
        "-0.0",
        "1.00",
        "1e3",
        "1E+003",
        "1E-003",
        "1e999999",
        "18446744073709551615",
        "18446744073709551616",
        "-9223372036854775809",
        r#""한🙂\u0000\b\f\n\r\t\\\"""#,
        r#"{"z":[true,null,{"b":2,"a":1}],"z":{"later":3},"a":1.00}"#,
        r#"{"a":1,"\u0061":2}"#,
    ] {
        compare(text.as_bytes());
    }
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let value = JsonDocument::read_with_control(
        br#"{"n":-0,"s":"text","b":true,"a":[null,1]}"#,
        &control,
        protocol(),
    )
    .unwrap();
    assert_eq!(value.root().kind(), JsonValueKind::Object);
    assert_eq!(value.root().get("n").unwrap().number_text(), Some("0"));
    assert_eq!(value.root().get("s").unwrap().as_str(), Some("text"));
    assert_eq!(value.root().get("b").unwrap().as_bool(), Some(true));
    let elements: Vec<_> = value
        .root()
        .get("a")
        .unwrap()
        .elements()
        .unwrap()
        .map(JsonValue::kind)
        .collect();
    assert_eq!(elements, [JsonValueKind::Null, JsonValueKind::Number]);
    assert!(value.root().get("missing").is_none());
    assert_eq!(
        value
            .root()
            .fields()
            .unwrap()
            .map(|(name, _)| name)
            .collect::<Vec<_>>(),
        ["a", "b", "n", "s"]
    );
    drop(value);
    assert_eq!(memory.used(), 0);
}

#[test]
fn native_json_document_preserves_scalar_protocols_and_first_key_error_order() {
    let protocol = protocol();
    for key in [protocol.number, protocol.raw] {
        for value in [
            "1",
            "-0",
            "1.00",
            "1e3",
            "1e999999",
            "01",
            " 1",
            "1 ",
            "true",
            "null",
            "[]",
            "{}",
            "{\"a\":1,\"a\":2}",
            "[1,]",
            "\"\\ud800\"",
        ] {
            let payload = serde_json::to_string(value).unwrap();
            for suffix in ["", ",\"later\":0", ",", ",\"later\":"] {
                compare(format!("{{{key:?}:{payload}{suffix}}}").as_bytes());
            }
            compare(format!("{{\"first\":0,{key:?}:{payload}}}").as_bytes());
        }
        for value in ["null", "true", "2", "[]", "{}"] {
            compare(format!("{{{key:?}:{value}}}").as_bytes());
        }
    }
    let mut nested = "{\"z\":2,\"a\":1}".to_owned();
    for _ in 0..8 {
        nested = format!(
            "{{{:?}:{}}}",
            protocol.raw,
            serde_json::to_string(&nested).unwrap()
        );
        compare(nested.as_bytes());
    }
}

#[test]
fn native_json_document_preserves_invalid_prefixes_overwritten_values_and_depth() {
    let text = r#"{"a":["한🙂",{"b":null}],"z":true}"#;
    for end in 0..=text.len() {
        compare(&text.as_bytes()[..end]);
    }
    for text in [
        r#"{"x":"\ud800","x":null}"#,
        r#"{"x":{"a":"\udc00"},"x":null}"#,
        r#"{"x":00,"x":null}"#,
        r#"{"x":[1,]}"#,
        "{} false",
        "01",
        "1e",
        "NaN",
        r#"{"a":true,"\ud800":null}"#,
    ] {
        compare(text.as_bytes());
    }
    compare(b"{\"a\":\"\xff\"}");
    for depth in [15, 16, 125, 126, 127, 128, 129] {
        compare(format!("{}null{}", "[".repeat(depth), "]".repeat(depth)).as_bytes());
    }
}

#[test]
fn native_json_document_preserves_scalar_depth_and_number_boundaries() {
    for depth in [0, 125, 126, 127, 128] {
        for key in [protocol().number, protocol().raw] {
            for payload in [
                "[]", "{}", "[invalid", "{invalid", "null", "\"1\"", "\"[1]\"",
            ] {
                let text = format!(
                    "{}{{{key:?}:{payload}}}{}",
                    "[".repeat(depth),
                    "]".repeat(depth)
                );
                compare(text.as_bytes());
            }
        }
    }
    for base in [
        "0",
        "-0",
        "0.0",
        "-0.0",
        "1",
        "-1",
        "18446744073709551615",
        "18446744073709551616",
        "-9223372036854775808",
        "-9223372036854775809",
    ] {
        for suffix in [
            "",
            "e0",
            "E-0",
            "e+00000",
            "e00023",
            "E-00023",
            "e309",
            "e-99999999999999999999999999",
        ] {
            let text = format!("{base}{suffix}");
            compare(text.as_bytes());
            compare(format!("{{{:?}:{text:?}}}", protocol().number).as_bytes());
        }
    }
    let inner = format!("{}null{}", "[".repeat(126), "]".repeat(126));
    let raw = format!("{{{:?}:{inner:?}}}", protocol().raw);
    let expanded = format!("{}{raw}{}", "[".repeat(126), "]".repeat(126));
    compare(expanded.as_bytes());
    let long = format!("1{}E31", "0".repeat(4094));
    compare(long.as_bytes());
}
