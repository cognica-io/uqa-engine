//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::memory::MemoryError;

fn check_value(text: &str) {
    let memory = MemoryBudget::new(16 * 1024 * 1024);
    let cancellation = CancellationToken::new();
    let actual = JsonValueDecoder::new(&memory, &cancellation).value(text);
    let expected = serde_json::from_str::<Value>(text);
    match (actual, expected) {
        (Ok(actual), Ok(expected)) => {
            assert_eq!(*actual, expected, "{text}");
            assert_eq!(
                serde_json::to_string(&*actual).unwrap(),
                serde_json::to_string(&expected).unwrap(),
                "{text}"
            );
            let payload = actual
                .retained_payload_bytes(&MemoryBudget::new(1024 * 1024), &cancellation)
                .unwrap();
            assert_eq!(actual.reserved_bytes(), payload, "{text}");
            assert_eq!(memory.used(), payload, "{text}");
            drop(actual);
        }
        (Err(JsonReadError::InvalidJson), Err(_)) => {}
        (actual, expected) => panic!("different decoding for {text}: {actual:?} / {expected:?}"),
    }
    assert_eq!(memory.used(), 0, "{text}");
}

#[test]
fn controlled_values_preserve_ordinary_numeric_and_tagged_representations() {
    for text in [
        "null",
        "true",
        "false",
        "0",
        "-0",
        "-0.0",
        "1.00",
        "1e2",
        "1e-400",
        "9223372036854775807",
        "9223372036854775808",
        "18446744073709551615",
        "18446744073709551616",
        "-9223372036854775809",
        "1e400",
        "1e131072",
        r#""text\n\uD83D\uDE03\u0000""#,
        "[]",
        "[1,2]",
        "{}",
        r#"{"$uqa_type":"void"}"#,
        r#"{"$uqa_type":"decimal","value":"1e400"}"#,
        r#"{"$uqa_type":"decimal","value":"-0.000"}"#,
        r#"{"$uqa_type":"decimal","value":"bad"}"#,
        r#"{"$uqa_type":"decimal","value":"2.50","ignored":true}"#,
        r#"{"$uqa_type":"bytes","hex":"00ff80"}"#,
        r#"{"$uqa_type":"bytes","hex":"00fg"}"#,
        r#"{"$uqa_type":"row","values":[1,2]}"#,
        r#"{"$uqa_type":"record","fields":[["a",1],["a",2],["b",[1,2]]]}"#,
        r#"{"$uqa_type":"record","fields":[["a",1],[2,3]]}"#,
        r#"{"$uqa_type":"array","values":[[1,2],[3,4]],"lower_bounds":[-1,4]}"#,
        r#"{"$uqa_type":"array","values":[[1],[2,3]],"lower_bounds":[1,1]}"#,
        r#"{"$uqa_type":"array","values":[],"lower_bounds":[]}"#,
        r#"{"$uqa_type":"date","days":42}"#,
        r#"{"$uqa_type":"interval","months":1,"days":2,"micros":3}"#,
        r#"{"$uqa_type":"fixed_char","value":"a   "}"#,
        r#"{"$uqa_type":"json","value":"[1, 2]"}"#,
        r#"{"$uqa_type":"jsonb","value":"{\"a\":2}"}"#,
        r#"{"$uqa_type":"unknown","value":[1,2]}"#,
        r#"{"$serde_json::private::Number":"18446744073709551615"}"#,
        r#"{"$serde_json::private::Number":"1e400"}"#,
        r#"{"$serde_json::private::Number":"NaN"}"#,
        r#"{"$serde_json::private::Number":"invalid"}"#,
        r#"{"$serde_json::private::RawValue":"[1,2]"}"#,
    ] {
        check_value(text);
        check_value(&format!("[{text},{{\"nested\":{text}}}]"));
    }
}

#[test]
fn root_fields_preserve_reserved_names_and_recognize_only_nested_tags() {
    let text = r#"{"$uqa_type":"bytes","hex":"ff","child":{"$uqa_type":"row","values":[1,2]},"$serde_json::private::Number":"1e400"}"#;
    let memory = MemoryBudget::new(64 * 1024);
    let cancellation = CancellationToken::new();
    let actual = JsonValueDecoder::new(&memory, &cancellation)
        .fields(text)
        .unwrap();
    assert_eq!(
        *actual,
        serde_json::from_str::<BTreeMap<String, Value>>(text).unwrap()
    );
    assert!(matches!(actual["child"], Value::Row(_)));
    assert!(matches!(actual["$uqa_type"], Value::Str(_)));
    drop(actual);
    assert_eq!(memory.used(), 0);
    for invalid in ["null", "[]", "1", r#""text""#] {
        assert!(matches!(
            JsonValueDecoder::new(&memory, &cancellation).fields(invalid),
            Err(JsonReadError::InvalidJson)
        ));
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn duplicate_fields_release_overwritten_payloads_and_keep_the_last_value() {
    let repeated = format!("\"a\":\"{}\",", "x".repeat(2048)).repeat(20);
    let text = format!("{{{repeated}\"\\u0061\":null}}");
    let memory = MemoryBudget::new(32 * 1024);
    let cancellation = CancellationToken::new();
    let value = JsonValueDecoder::new(&memory, &cancellation)
        .value(&text)
        .unwrap();
    assert_eq!(*value, Value::Map([("a".into(), Value::Null)].into()));
    assert_eq!(memory.used(), size_of::<(String, Value)>() + 1);
    assert_eq!(value.reserved_bytes(), memory.used());
    drop(value);
    assert_eq!(memory.used(), 0);
}

#[test]
fn malformed_input_and_container_limits_match_the_existing_decoder() {
    for text in [
        "",
        "[1,]",
        "{\"a\":}",
        "true false",
        "1e",
        "01",
        r#""\uD800""#,
    ] {
        check_value(text);
    }
    for depth in [1, 126, 127, 128] {
        check_value(&format!("{}0{}", "[".repeat(depth), "]".repeat(depth)));
    }
    let memory = MemoryBudget::new(4096);
    let cancellation = CancellationToken::new();
    let decoder = JsonValueDecoder::new(&memory, &cancellation).with_depth_limit(1);
    assert!(decoder.value("[1]").is_ok());
    assert!(matches!(
        decoder.value("[[1]]"),
        Err(JsonReadError::InvalidJson)
    ));
    assert_eq!(memory.used(), 0);
}

#[test]
fn quota_failures_release_partial_values_and_preserve_prior_results() {
    let text = r#"{"nested":[{"$uqa_type":"record","fields":[["a","a\nlonger"],["b",{"$uqa_type":"decimal","value":"1e100"}]]}],"bytes":{"$uqa_type":"bytes","hex":"abcdef00"}}"#;
    let cancellation = CancellationToken::new();
    let accepted = MemoryBudget::new(1024 * 1024);
    drop(
        JsonValueDecoder::new(&accepted, &cancellation)
            .value(text)
            .unwrap(),
    );
    let expected: Value = serde_json::from_str(text).unwrap();
    let mut rejected = 0;
    for limit in (0..accepted.peak()).step_by(97) {
        let memory = MemoryBudget::new(limit);
        let result = JsonValueDecoder::new(&memory, &cancellation).value(text);
        match result {
            Err(JsonReadError::Memory(MemoryError::Limit { .. })) => rejected += 1,
            Ok(value) => {
                // BudgetedVec may choose its required capacity when geometric growth would exceed the allowance.
                assert_eq!(*value, expected, "limit {limit}");
                assert_eq!(value.reserved_bytes(), memory.used());
                assert!(memory.peak() <= limit);
                drop(value);
            }
            Err(error) => panic!("unexpected failure at limit {limit}: {error}"),
        }
        assert_eq!(memory.used(), 0, "limit {limit}");
    }
    assert!(rejected > 0);
    let memory = MemoryBudget::new(4096);
    let prior = JsonValueDecoder::new(&memory, &cancellation)
        .value(r#""prior""#)
        .unwrap();
    let retained = memory.used();
    let large = format!("[\"{}\"]", "x".repeat(8192));
    assert!(matches!(
        JsonValueDecoder::new(&memory, &cancellation).value(&large),
        Err(JsonReadError::Memory(_))
    ));
    assert_eq!(memory.used(), retained);
    assert_eq!(*prior, Value::Str("prior".into()));
    cancellation.cancel();
    assert!(matches!(
        JsonValueDecoder::new(&memory, &cancellation).value(text),
        Err(JsonReadError::Cancelled(_))
    ));
    assert_eq!(memory.used(), retained);
    drop(prior);
    assert_eq!(memory.used(), 0);
}
