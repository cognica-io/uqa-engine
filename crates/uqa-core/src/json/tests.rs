//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn valid(input: &str, depth: Option<usize>) -> bool {
    let memory = MemoryBudget::new(1024 * 1024);
    let cancellation = CancellationToken::new();
    let accepted = {
        let mut reader = JsonReader::new(input, &memory, &cancellation);
        reader.depth_limit = depth;
        loop {
            match reader.next_event() {
                Ok(Some(_)) => {}
                Ok(None) => break true,
                Err(JsonReadError::InvalidJson) => break false,
                Err(error) => panic!("unexpected control failure: {error}"),
            }
        }
    };
    assert_eq!(memory.used(), 0);
    accepted
}

#[test]
fn grammar_matches_serde_for_scalars_nested_fields_and_malformed_delimiters() {
    let tokens = [
        "null",
        "true",
        "false",
        "0",
        "-0",
        "18446744073709551616",
        "1e400",
        "-1.25E-10",
        "\"한글😃\"",
        "\"a\\n\\u0000\\uD83D\\uDE03\"",
        "[]",
        "{}",
        "[1,{\"a\":true}]",
        "{\"a\":1,\"a\":2}",
        "",
        "+1",
        "01",
        "-01",
        ".1",
        "1.",
        "1e",
        "1e+",
        "--1",
        "tru",
        "True",
        "[1,]",
        "[,1]",
        "{,}",
        "{\"a\",1}",
        "{\"a\":1,}",
        "{1:2}",
        "[}",
        "\"unterminated",
        "\"\\x\"",
        "\"\\uD800\"",
        "\"\\uDC00\"",
        "\"\\uD800\\u0041\"",
        "\"\\uZZZZ\"",
        "\"\n\"",
    ];
    for token in tokens {
        for input in [
            token.to_owned(),
            format!(" \n{token}\t "),
            format!("[{token}]"),
            format!("{{\"v\":{token}}}"),
            format!("{token} null"),
        ] {
            assert_eq!(
                valid(&input, None),
                serde_json::from_str::<serde_json::Value>(&input).is_ok(),
                "{input:?}"
            );
        }
    }
}

#[test]
fn borrowed_events_preserve_token_offsets_and_duplicate_key_order() {
    let input = r#" { "a": [true, "\u0062"], "a": -1e+2 } "#;
    let memory = MemoryBudget::new(4096);
    let cancellation = CancellationToken::new();
    let mut reader = JsonReader::new(input, &memory, &cancellation);
    let mut events = Vec::new();
    while let Some(event) = reader.next_event().unwrap() {
        let raw = &input[event.range.clone()];
        match event.token {
            JsonToken::Number(text) | JsonToken::String(text) | JsonToken::Key(text) => {
                assert_eq!(text.as_ptr(), raw.as_ptr());
                assert_eq!(text, raw);
            }
            _ => {}
        }
        events.push(event.token);
    }
    assert_eq!(
        events,
        vec![
            JsonToken::StartObject,
            JsonToken::Key(r#""a""#),
            JsonToken::StartArray,
            JsonToken::Bool(true),
            JsonToken::String(r#""\u0062""#),
            JsonToken::EndArray,
            JsonToken::Key(r#""a""#),
            JsonToken::Number("-1e+2"),
            JsonToken::EndObject
        ]
    );
    drop(reader);
    assert_eq!(memory.used(), 0);
}

#[test]
fn configurable_nesting_matches_values_without_restricting_jsonb_syntax() {
    for depth in 1..=130 {
        let input = format!("{}0{}", "[".repeat(depth), "]".repeat(depth));
        assert_eq!(
            valid(&input, Some(127)),
            serde_json::from_str::<serde_json::Value>(&input).is_ok(),
            "depth {depth}"
        );
        assert!(valid(&input, None));
    }
    // Structural parsing uses an explicit stack even beyond the serde value limit.
    let input = format!("{}0{}", "[".repeat(4096), "]".repeat(4096));
    assert!(valid(&input, None));
}

#[test]
fn nesting_reserves_before_growth_and_releases_after_failure() {
    let memory = MemoryBudget::new(0);
    let cancellation = CancellationToken::new();
    let mut scalar = JsonReader::new("123456789012345678901234567890", &memory, &cancellation);
    assert!(matches!(
        scalar.next_event().unwrap().unwrap().token,
        JsonToken::Number(_)
    ));
    assert!(scalar.next_event().unwrap().is_none());
    let mut container = JsonReader::new("[]", &memory, &cancellation);
    assert!(matches!(
        container.next_event(),
        Err(JsonReadError::Memory(MemoryError::Limit { .. }))
    ));
    assert_eq!(memory.used(), 0);
}

#[test]
fn cancellation_keeps_prior_retained_strings_and_releases_reader_scratch() {
    let memory = MemoryBudget::new(4096);
    let cancellation = CancellationToken::new();
    let retained = decode_json_string(r#""prior""#, &memory, &cancellation).unwrap();
    let before = memory.used();
    let mut reader = JsonReader::new("[1,2]", &memory, &cancellation);
    reader.next_event().unwrap();
    assert!(memory.used() > before);
    cancellation.cancel();
    assert!(matches!(
        reader.next_event(),
        Err(JsonReadError::Cancelled(_))
    ));
    assert!(matches!(
        decode_json_string(r#""next""#, &memory, &cancellation),
        Err(JsonReadError::Cancelled(_))
    ));
    drop(reader);
    assert_eq!(memory.used(), before);
    assert_eq!(&**retained, "prior");
    drop(retained);
    assert_eq!(memory.used(), 0);
}

#[test]
fn string_quota_precedes_decoding_and_final_lease_charges_owned_capacity() {
    let cancellation = CancellationToken::new();
    for encoded in [r#""plain""#, r#""a\n\uD83D\uDE03\u0000""#, r#""""#] {
        let envelope = encoded.len() * 4 + 16;
        let rejected = MemoryBudget::new(envelope - 1);
        assert!(matches!(
            decode_json_string(encoded, &rejected, &cancellation),
            Err(JsonReadError::Memory(MemoryError::Limit { .. }))
        ));
        assert_eq!(rejected.used(), 0);
        let memory = MemoryBudget::new(envelope);
        let value = decode_json_string(encoded, &memory, &cancellation).unwrap();
        assert_eq!(&**value, serde_json::from_str::<String>(encoded).unwrap());
        assert_eq!(memory.used(), value.capacity());
        assert_eq!(value.reserved_bytes(), value.capacity());
        drop(value);
        assert_eq!(memory.used(), 0);
    }
    let memory = MemoryBudget::new(4096);
    assert!(matches!(
        decode_json_string(r#""\uD800""#, &memory, &cancellation),
        Err(JsonReadError::InvalidJson)
    ));
    assert_eq!(memory.used(), 0);
}
