//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    image_io::{ImageDecodeWindow, ImageDecoder},
    json::{work::inline, JsonInputError},
    memory::MemoryBudget,
    CancellationToken,
};
use serde_json::value::RawValue;

fn compare(text: &str) {
    let expected = serde_json::from_str::<Box<RawValue>>(text);
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let mut reader = JsonReader::new(text, &memory, &token).with_depth_limit(1);
    let actual = inline(async {
        let span = reader.read_raw_value_with_work(&mut ()).await?;
        assert!(reader.next_event()?.is_none());
        Ok::<_, JsonReadError>(span)
    });
    match (expected, actual) {
        (Ok(expected), Ok(actual)) => {
            assert_eq!(expected.get().as_bytes(), actual);
            assert_eq!(actual.as_ptr(), text.trim_start().as_ptr());
        }
        (Err(expected), Err(JsonReadError::InvalidJson)) => {
            let actual = reader.syntax_error().expect("retained raw syntax cause");
            assert_eq!(actual.to_string(), expected.to_string(), "{text:?}");
            assert_eq!(actual.kind.is_eof(), expected.is_eof());
        }
        (expected, actual) => panic!("{text:?}: {expected:?} != {actual:?}"),
    }
    drop(reader);
    assert_eq!(memory.used(), 0);
}

#[test]
fn raw_json_spans_preserve_original_skip_grammar_diagnostics_and_unlimited_nesting() {
    for text in [
        "null",
        "true",
        "-0",
        "1.5e400",
        "0.1",
        "\"한🙂\"",
        r#""\ud800""#,
        r#"{"\ud800":["\udc00",null,1e99999]}"#,
        r#"{"a":0,"a":1}"#,
        "[ ]",
        "{ }",
        "[1,]",
        "[1 2]",
        "{\"a\":}",
        "{\"a\" 1}",
        "{\"a\":1,}",
        "- ",
        "1.e",
        "01",
        "true x",
        r#""\u0z00""#,
        r#""\q""#,
        "\"a\nb\"",
    ] {
        let source = format!(" \n{text}\t");
        for end in (0..=source.len()).filter(|&end| source.is_char_boundary(end)) {
            compare(&source[..end]);
        }
    }
    for depth in [126, 127, 128, 1024] {
        compare(&format!("{}null{}", "[".repeat(depth), "]".repeat(depth)));
    }
    for source in [
        r#"{"a":[null,true,false,-12.5e+4,"\ud800"],"b":{}}"#,
        "[\n 1, {\"k\":\"value\"}\n]",
    ] {
        for offset in 0..source.len() {
            for byte in [0, b'"', b',', b':', b'}', b'\n'] {
                let mut changed = source.as_bytes().to_vec();
                changed[offset] = byte;
                compare(std::str::from_utf8(&changed).unwrap());
            }
        }
    }
}

#[test]
fn raw_json_completion_restores_enclosing_typed_string_and_depth_rules() {
    for text in ["-", "1.", "1e+", "[1,]", "{\"a\":1,}", "\"a\nb\""] {
        let expected = serde_json::from_str::<serde_json::Value>(text).unwrap_err();
        let memory = MemoryBudget::new(65536);
        let token = CancellationToken::new();
        let mut reader = JsonReader::new(text, &memory, &token);
        assert!(matches!(
            inline(reader.read_value_span_with_work(&mut ())),
            Err(JsonReadError::InvalidJson)
        ));
        assert_eq!(
            reader.syntax_error().unwrap().to_string(),
            expected.to_string()
        );
    }
    for after in [r#""\ud800""#, r#""\udc00""#, r#""\q""#] {
        let raw = format!("{}\"\\ud800\"{}", "[".repeat(128), "]".repeat(128));
        let text = format!("[{raw},\n {after}]");
        let expected = serde_json::from_str::<(Box<RawValue>, String)>(&text).unwrap_err();
        let memory = MemoryBudget::new(65536);
        let token = CancellationToken::new();
        let mut reader = JsonReader::new(&text, &memory, &token).with_depth_limit(1);
        assert_eq!(
            reader.next_event().unwrap().unwrap().token,
            JsonToken::StartArray
        );
        assert_eq!(
            inline(reader.read_raw_value_with_work(&mut ())).unwrap(),
            raw.as_bytes()
        );
        assert!(matches!(
            reader.next_event(),
            Err(JsonReadError::InvalidJson)
        ));
        assert_eq!(
            reader.syntax_error().unwrap().to_string(),
            expected.to_string()
        );
        drop(reader);
        assert_eq!(memory.used(), 0);
    }
    let memory = MemoryBudget::new(65536);
    let token = CancellationToken::new();
    let mut reader = JsonReader::new("[[[0]],[]]", &memory, &token).with_depth_limit(1);
    reader.next_event().unwrap();
    assert_eq!(
        inline(reader.read_raw_value_with_work(&mut ())).unwrap(),
        b"[[0]]"
    );
    assert!(matches!(
        reader.next_event(),
        Err(JsonReadError::InvalidJson)
    ));
    assert_eq!(
        reader.syntax_error().unwrap().kind,
        JsonSyntaxKind::DepthLimit
    );
}

fn decoder(
    text: String,
    memory: &MemoryBudget,
    token: &CancellationToken,
) -> ImageDecoder<usize, JsonInputError> {
    ImageDecoder::new(memory, token, |mut channel| async move {
        let mut work = channel.input();
        let mut reader = JsonReader::new(&text, work.memory, work.cancellation);
        let raw = reader
            .read_raw_value_with_work(&mut work)
            .await
            .map_err(|error| JsonInputError::from_reader(&reader, error))?;
        Ok(raw.len())
    })
    .unwrap()
}

#[test]
fn raw_json_work_windows_keep_admission_cancellation_and_retirement_terminal() {
    struct NoRead;
    impl std::io::Read for NoRead {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            panic!("raw span reread source");
        }
    }
    let source = format!("[\"{}\"]", "\\ud800".repeat(8192));
    let window = ImageDecodeWindow { bytes: 1, steps: 1 };
    let memory = MemoryBudget::new(65536);
    let token = CancellationToken::new();
    let mut read = decoder(source.clone(), &memory, &token);
    let mut polls = 0;
    loop {
        polls += 1;
        if let Some(length) = read.advance(&mut NoRead, window).unwrap() {
            assert_eq!(length, source.len());
            break;
        }
    }
    assert!(polls > source.len() / 4096);
    drop(read);
    assert_eq!(memory.used(), 0);
    for boundary in 1..polls {
        let token = CancellationToken::new();
        let mut read = decoder(source.clone(), &memory, &token);
        for _ in 0..boundary {
            assert!(read.advance(&mut NoRead, window).unwrap().is_none());
        }
        token.cancel();
        assert!(matches!(
            read.advance(&mut NoRead, window),
            Err(JsonInputError::Cancelled(_))
        ));
        assert_eq!(memory.used(), 0);
        assert!(read.advance(&mut NoRead, window).is_err());
        let mut abandoned = decoder(source.clone(), &memory, &CancellationToken::new());
        for _ in 0..boundary {
            assert!(abandoned.advance(&mut NoRead, window).unwrap().is_none());
        }
        drop(abandoned);
        assert_eq!(memory.used(), 0);
    }
    let mut read = decoder(
        format!("{}null{}", "[".repeat(65536), "]".repeat(65536)),
        &memory,
        &CancellationToken::new(),
    );
    loop {
        match read.advance(&mut NoRead, window) {
            Ok(None) => (),
            Err(JsonInputError::Memory(_)) => break,
            value => panic!("unexpected deep raw result: {value:?}"),
        }
    }
    assert_eq!(memory.used(), 0);
}
