//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::{collections::BTreeMap, sync::Arc};
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder},
    memory::MemoryBudget,
    CancellationToken,
};

type Input = ImageDecoder<Option<GinAnalyzerParameter>, JsonInputError>;

fn input(text: Arc<str>, memory: &MemoryBudget, token: &CancellationToken) -> Input {
    ImageDecoder::new(memory, token, |mut reader| async move {
        GinAnalyzerParameter::read_from(&text, &mut reader.input()).await
    })
    .unwrap()
}

fn finish(input: &mut Input, steps: usize) -> Result<Option<GinAnalyzerParameter>, JsonInputError> {
    for _ in 0..100_000 {
        if let Some(value) =
            input.advance(&mut std::io::empty(), ImageDecodeWindow { bytes: 1, steps })?
        {
            return Ok(value);
        }
    }
    panic!("GIN parameter input stopped progressing")
}

fn equivalent(text: &str) {
    let expected = serde_json::from_str::<BTreeMap<String, String>>(text);
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let mut read = input(text.into(), &memory, &token);
    match (expected, finish(&mut read, 1)) {
        (Ok(expected), Ok(actual)) => {
            let name = expected
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case("analyzer"))
                .map(|(_, name)| name.trim());
            assert_eq!(
                actual.as_ref().map(GinAnalyzerParameter::name),
                name,
                "{text}"
            );
        }
        (Err(expected), Err(JsonInputError::Diagnostic(actual))) => {
            assert_eq!(actual.to_string(), expected.to_string(), "{text}");
            assert_eq!(actual.position().line, expected.line(), "{text}");
            assert_eq!(actual.position().column, expected.column(), "{text}");
            assert_eq!(actual.is_syntax(), expected.is_syntax(), "{text}");
            assert_eq!(actual.is_eof(), expected.is_eof(), "{text}");
            assert_eq!(actual.is_data(), expected.is_data(), "{text}");
        }
        (expected, actual) => panic!("{text}: expected {expected:?}, success={}", actual.is_ok()),
    }
    assert!(read
        .advance(
            &mut std::io::empty(),
            ImageDecodeWindow { bytes: 1, steps: 1 }
        )
        .is_err());
    drop(read);
    assert_eq!(memory.used(), 0);
}

#[test]
fn gin_parameter_input_preserves_sorted_case_selection_duplicates_and_unicode_trim() {
    for text in [
        "{}",
        r#"{"ignored":"analyzer"}"#,
        r#"{"analyzer":"  standard  ","ANALYZER":"\t keyword\n"}"#,
        r#"{"ANALYZER":"first","anaLYZER":"ignored","ANALYZER":"last"}"#,
        r#"{"anal\u0079zer":"escaped","Analyzer":"first sorted"}"#,
        r#"{"analyzer ":"ignored","analyzer":"\u2003name\u3000"}"#,
        r#"{"analyzer":"\u2003\t\n","unrelated":"\ud83d\ude00"}"#,
        r#"{"analyzer":"","ANALYZER":"\u0000name"}"#,
    ] {
        equivalent(text);
    }
    let text = format!(
        r#"{{"analyzer":"{}name{}"}}"#,
        "\u{2003}".repeat(8192),
        "\u{3000}".repeat(8192)
    );
    equivalent(&text);
}

#[test]
fn gin_parameter_input_rejects_every_original_invalid_value_and_truncation() {
    for text in [
        "null",
        "[]",
        "true",
        "0",
        r#""name""#,
        r#"{"analyzer":null}"#,
        r#"{"unused":false,"unused":"valid","ANALYZER":"name"}"#,
        r#"{"ANALYZER":"valid","analyzer":["unused"]}"#,
        r#"{"ANALYZER":"valid","analyzer":"\ud800"}"#,
        r#"{"unused":"\ud800","analyzer":"valid"}"#,
        r#"{"unused":"valid","unused":{},"analyzer":"valid"}"#,
        r#"{"analyzer":"valid","analyzer":false}"#,
        r#"{"analyzer":"valid","unused":1e99999}"#,
        r#"{"analyzer":"valid"} null"#,
    ] {
        equivalent(text);
    }
    let text = r#"{"ANALYZER":"one","analyzer":"two","ignored":"x"}"#;
    for end in 0..text.len() {
        equivalent(&text[..end]);
    }
}

#[test]
fn gin_parameter_input_bounds_discarded_text_and_retires_interrupted_selection() {
    let text: Arc<str> = format!(
        r#"{{"ignored":"{}","analyzer":" standard "}}"#,
        "x".repeat(1 << 20)
    )
    .into();
    let memory = MemoryBudget::new(8192);
    let token = CancellationToken::new();
    let mut read = input(text, &memory, &token);
    let selected = finish(&mut read, 1).unwrap().unwrap();
    assert_eq!(selected.name(), "standard");
    assert_eq!(memory.used(), selected.text.capacity());
    drop((read, selected));
    assert_eq!(memory.used(), 0);

    let text: Arc<str> = format!(
        r#"{{"analyzer":"{}","ANALYZER":"last"}}"#,
        "name".repeat(1 << 16)
    )
    .into();
    for outcome in ["cancel", "memory", "zero", "drop", "unwind"] {
        let memory = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let mut read = input(text.clone(), &memory, &token);
        assert!(read
            .advance(
                &mut std::io::empty(),
                ImageDecodeWindow { bytes: 1, steps: 3 }
            )
            .unwrap()
            .is_none());
        let occupied = (outcome == "memory").then(|| memory.reserve(memory.available()).unwrap());
        if outcome == "cancel" {
            token.cancel();
        }
        if outcome == "unwind" {
            assert!(
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                    let _read = read;
                    panic!("abandoned GIN options");
                }))
                .is_err()
            );
        } else {
            if outcome != "drop" {
                assert!(
                    finish(&mut read, usize::from(outcome != "zero")).is_err(),
                    "{outcome}"
                );
                assert!(read
                    .advance(
                        &mut std::io::empty(),
                        ImageDecodeWindow { bytes: 1, steps: 1 }
                    )
                    .is_err());
            }
            drop(read);
        }
        drop(occupied);
        assert_eq!(memory.used(), 0);
    }
}
