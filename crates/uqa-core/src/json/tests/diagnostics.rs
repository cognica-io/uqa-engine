//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::image_io::{ImageDecodeWindow, ImageDecoder};

fn compare(bytes: &[u8]) {
    let expected = serde_json::from_slice::<serde_json::Value>(bytes);
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let mut reader = JsonReader::from_slice(bytes, &memory, &token).with_depth_limit(127);
    let actual = loop {
        match reader.next_event() {
            Ok(Some(_)) => (),
            Ok(None) => break Ok(()),
            Err(JsonReadError::InvalidJson) => {
                break Err(reader.syntax_error().expect("syntax cause"))
            }
            Err(error) => panic!("unexpected resource failure: {error}"),
        }
    };
    match (expected, actual) {
        (Ok(_), Ok(())) => assert!(reader.syntax_error().is_none()),
        (Err(expected), Err(actual)) => {
            assert_eq!(actual.to_string(), expected.to_string(), "{bytes:?}");
            assert_eq!(actual.position.line, expected.line());
            assert_eq!(actual.position.column, expected.column());
            assert_eq!(actual.kind.is_eof(), expected.is_eof(), "{bytes:?}");
            assert!(actual.position.byte_offset <= bytes.len());
        }
        (expected, actual) => panic!("{bytes:?}: {expected:?} != {actual:?}"),
    }
    drop(reader);
    assert_eq!(memory.used(), 0);
}

#[test]
fn native_json_diagnostics_preserve_original_syntax_messages_positions_and_eof() {
    for text in [
        "",
        " ",
        "[",
        "{",
        "[1,",
        "[1,]",
        "{\"a\":1,}",
        "{bad",
        "{\"a\" 1}",
        "{\"a\":}",
        "[true false]",
        "{} x",
        "-",
        "- ",
        "01",
        "1.",
        "1.e",
        "1e",
        "1e+",
        "tr",
        "tx",
        "truex",
        "\"a",
        "\"a\\",
        "\"\\q\"",
        "\"\\u\"",
        "\"\\u0X00\"",
        "\"\\uD800\"",
        "\"\\uD800",
        "\"\\uD800\\x\"",
        "\"\\uD800\\u0000\"",
        "\"\\uDC00\"",
        "\"a\nb\"",
        "[\n  1,\n]",
        "{\n\"a\"\n1}",
    ] {
        compare(text.as_bytes());
    }
    for depth in [126, 127, 128, 129] {
        compare(format!("{}0{}", "[".repeat(depth), "]".repeat(depth)).as_bytes());
    }
}

#[test]
fn native_json_diagnostics_match_truncated_mutated_and_multibyte_inputs() {
    for text in [
        r#"{"a":[null,true,false,-12.5e+4,"한🙂\uD83D\uDE03"],"empty":{}}"#,
        "{\n\"a\":\n[1,\n2],\n\"b\":\"c\"\n}",
        r#"["\u0000","\uD800\uDC00",12345678901234567890123456789]"#,
    ] {
        let bytes = text.as_bytes();
        for length in 0..=bytes.len() {
            compare(&bytes[..length]);
        }
        for index in 0..bytes.len() {
            for byte in [0, b'"', b',', 0xff, b'{', b'\n'] {
                let mut changed = bytes.to_vec();
                changed[index] = byte;
                compare(&changed);
            }
        }
    }
}

#[test]
fn native_json_diagnostics_retain_locations_across_bounded_input_without_replay() {
    struct NoRead;
    impl std::io::Read for NoRead {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            panic!("source reread")
        }
    }
    let text = format!("[{}\"{}\\q\"]", " \n".repeat(32768), "한🙂".repeat(32768));
    let expected = serde_json::from_str::<serde_json::Value>(&text).unwrap_err();
    let length = text.len();
    let memory = MemoryBudget::new(16384);
    let token = CancellationToken::new();
    let mut input =
        ImageDecoder::<_, JsonReadError>::new(&memory, &token, |mut reader| async move {
            let mut work = reader.input();
            let mut json = JsonReader::new(&text, work.memory, work.cancellation);
            loop {
                match json.next_event_from(&mut work).await {
                    Ok(Some(_)) => (),
                    Ok(None) => panic!("malformed escape was accepted"),
                    Err(JsonReadError::InvalidJson) => return Ok(json.syntax_error().unwrap()),
                    Err(error) => return Err(error),
                }
            }
        })
        .unwrap();
    let mut polls = 0;
    let actual = loop {
        polls += 1;
        if let Some(result) = input
            .advance(&mut NoRead, ImageDecodeWindow { bytes: 1, steps: 1 })
            .unwrap()
        {
            break result;
        }
    };
    assert_eq!(actual.to_string(), expected.to_string());
    assert_eq!(actual.position.line, expected.line());
    assert_eq!(actual.position.column, expected.column());
    assert!(polls > length / 4096);
    drop(input);
    assert_eq!(memory.used(), 0);
}

#[test]
fn native_json_resource_failures_do_not_become_syntax_diagnostics() {
    let memory = MemoryBudget::new(0);
    let token = CancellationToken::new();
    let text = "[".repeat(32);
    let mut reader = JsonReader::new(&text, &memory, &token);
    loop {
        match reader.next_event() {
            Ok(Some(_)) => (),
            Err(JsonReadError::Memory(_)) => break,
            result => panic!("expected native admission failure: {result:?}"),
        }
    }
    assert!(reader.syntax_error().is_none());
    let mut reader = JsonReader::new("[", &memory, &token);
    token.cancel();
    assert!(matches!(
        reader.next_event(),
        Err(JsonReadError::Cancelled(_))
    ));
    assert!(reader.syntax_error().is_none());
}
