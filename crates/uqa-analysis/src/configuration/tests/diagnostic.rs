//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::{Analyzer, AnalyzerDiagnostic, AnalyzerResources, CharFilter, TokenFilter, Tokenizer};
use serde_json::{json, Value};
use std::sync::Arc;
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder},
    json::{JsonReadError, JsonScalarProtocol},
    memory::MemoryBudget,
    CancellationToken,
};

struct NoRead;
impl std::io::Read for NoRead {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        panic!("diagnostic comparison borrows its original input")
    }
}

fn input(
    configuration: Arc<Analyzer>,
    text: Arc<str>,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> ImageDecoder<bool, JsonReadError> {
    ImageDecoder::new(memory, cancellation, |mut reader| async move {
        let mut work = reader.input();
        let saved = AnalyzerDiagnostic::read_from(&text, &mut work).await?;
        saved.matches_from(&configuration, &mut work).await
    })
    .unwrap()
}

fn finish(
    input: &mut ImageDecoder<bool, JsonReadError>,
    steps: usize,
) -> Result<(bool, usize), JsonReadError> {
    for polls in 1..1_000_000 {
        if let Some(value) = input.advance(&mut NoRead, ImageDecodeWindow { bytes: 1, steps })? {
            return Ok((value, polls));
        }
    }
    panic!("diagnostic comparison did not progress")
}

fn compare(configuration: &Analyzer, text: &str) {
    let expected = serde_json::from_str::<Value>(text)
        .and_then(|value| serde_json::to_value(configuration).map(|expected| value == expected));
    let memory = MemoryBudget::new(4 << 20);
    let cancellation = CancellationToken::new();
    let mut input = input(
        Arc::new(configuration.clone()),
        text.into(),
        &memory,
        &cancellation,
    );
    match (expected, finish(&mut input, 1)) {
        (Ok(expected), Ok((actual, _))) => assert_eq!(actual, expected, "{text}"),
        (Err(_), Err(JsonReadError::InvalidJson)) => (),
        (expected, actual) => panic!("{text}: {expected:?} / {actual:?}"),
    }
    assert_eq!(memory.used(), 0);
    assert!(finish(&mut input, 1).is_err());
}

#[test]
fn analyzer_diagnostics_preserve_public_field_omission_defaults_and_complete_object_equality() {
    for configuration in super::observations::configurations() {
        let expected = serde_json::to_value(&configuration).unwrap();
        let text = serde_json::to_string(&configuration).unwrap();
        compare(&configuration, &text);
        compare(
            &configuration,
            &serde_json::to_string_pretty(&configuration).unwrap(),
        );
        for field in expected.as_object().unwrap().keys() {
            let mut changed = expected.clone();
            changed.as_object_mut().unwrap().remove(field);
            compare(&configuration, &changed.to_string());
            let mut changed = expected.clone();
            changed[field] = Value::Null;
            compare(&configuration, &changed.to_string());
        }
        let mut changed = expected.clone();
        changed["ignored_by_typed_configuration"] = json!(null);
        compare(&configuration, &changed.to_string());
        compare(
            &configuration,
            &format!(r#"{{"tokenizer":null,{}}}"#, &text[1..text.len() - 1]),
        );
        compare(
            &configuration,
            &format!(r#"{{{},"tokenizer":null}}"#, &text[1..text.len() - 1]),
        );
    }
}

#[test]
fn analyzer_diagnostics_keep_generic_numeric_raw_and_overwritten_value_semantics() {
    let protocol = JsonScalarProtocol::from_values(
        &serde_json::Number::from(1u8),
        serde_json::value::RawValue::NULL,
    )
    .unwrap();
    let configuration = Analyzer::new(
        Tokenizer::NGram {
            min_gram: 0,
            max_gram: 3,
        },
        vec![],
        vec![],
    );
    let original = serde_json::to_value(&configuration).unwrap();
    let text = original.to_string();
    let mut nested = text.clone();
    for _ in 0..3 {
        nested = format!(
            r"{{{:?}:{}}}",
            protocol.raw,
            serde_json::to_string(&nested).unwrap()
        );
        compare(&configuration, &nested);
    }
    compare_maximum_integer(protocol);
    for scalar in [
        "0",
        "-0",
        "0.0",
        "0e0",
        "1",
        "-1",
        "1e99999",
        "18446744073709551616",
        "0.000000000000000000000000000000000000000",
    ] {
        let raw = format!(
            r#"{{"tokenizer":{{"type":"n_gram","min_gram":{scalar},"max_gram":3}},"token_filters":[],"char_filters":[]}}"#
        );
        compare(&configuration, &raw);
        for key in [protocol.number, protocol.raw] {
            let mut value = original.clone();
            value["tokenizer"]["min_gram"] = json!({key: scalar});
            compare(&configuration, &value.to_string());
        }
    }
    for key in [protocol.number, protocol.raw] {
        for value in [
            "null",
            "[]",
            "true",
            "{}",
            "\"not JSON\"",
            "\" 1\"",
            "\"1 \"",
            "\"01\"",
            "\"\\ud800\"",
        ] {
            compare(
                &configuration,
                &format!(
                    r#"{{"ignored":{{{key:?}:{value}}},{}}}"#,
                    &text[1..text.len() - 1]
                ),
            );
            compare(
                &configuration,
                &format!(
                    r#"{{"ignored":{{{key:?}:{value}}},"ignored":null,{}}}"#,
                    &text[1..text.len() - 1]
                ),
            );
        }
    }
    for length in 0..text.len() {
        compare(&configuration, &text[..length]);
    }
    compare(&configuration, &(text + " false"));
}

fn compare_maximum_integer(protocol: JsonScalarProtocol) {
    let configuration = Analyzer::new(
        Tokenizer::NGram {
            min_gram: usize::MAX,
            max_gram: usize::MAX,
        },
        vec![],
        vec![],
    );
    let original = serde_json::to_value(&configuration).unwrap();
    compare(&configuration, &original.to_string());
    for key in [protocol.number, protocol.raw] {
        let mut value = original.clone();
        value["tokenizer"]["min_gram"] = json!({key: usize::MAX.to_string()});
        compare(&configuration, &value.to_string());
    }
}

fn wide() -> Analyzer {
    Analyzer::new(
        Tokenizer::Pattern {
            pattern: "한🙂\\\n\0".repeat(2048),
        },
        vec![TokenFilter::Stop {
            language: "english".into(),
            custom_words: vec!["long prefix 한글".repeat(2048), String::new()],
        }],
        vec![CharFilter::Mapping {
            mapping: [("long key \0한".repeat(1024), "value\n🙂".repeat(2048))].into(),
        }],
    )
}

#[test]
fn analyzer_diagnostic_comparison_retains_long_native_fields_and_original_work_windows() {
    let configuration = Arc::new(wide());
    let text: Arc<str> = serde_json::to_string(&*configuration).unwrap().into();
    for steps in [1, 17, 1024] {
        let memory = MemoryBudget::new(4 << 20);
        let cancellation = CancellationToken::new();
        let mut input = input(configuration.clone(), text.clone(), &memory, &cancellation);
        let (matches, polls) = finish(&mut input, steps).unwrap();
        assert!(matches && polls > 1);
        assert_eq!(memory.used(), 0);
    }
    let mut modified = serde_json::to_value(&*configuration).unwrap();
    modified["token_filters"][0]["custom_words"][0] = json!("different");
    compare(&configuration, &modified.to_string());
}

#[test]
fn analyzer_diagnostic_comparison_retires_cancelled_denied_and_abandoned_native_scratch() {
    let configuration = Arc::new(wide());
    let text: Arc<str> = serde_json::to_string(&*configuration).unwrap().into();
    for steps in [1, 64, 1024] {
        for mode in ["cancel", "memory", "zero", "drop", "unwind"] {
            let memory = MemoryBudget::new(4 << 20);
            let cancellation = CancellationToken::new();
            let mut input = input(configuration.clone(), text.clone(), &memory, &cancellation);
            assert!(input
                .advance(&mut NoRead, ImageDecodeWindow { bytes: 1, steps })
                .unwrap()
                .is_none());
            let occupied = (mode == "memory").then(|| memory.reserve(memory.available()).unwrap());
            if mode == "cancel" {
                cancellation.cancel();
            }
            if mode == "unwind" {
                assert!(
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                        let _input = input;
                        panic!("abandoned analyzer diagnostic");
                    }))
                    .is_err()
                );
            } else {
                if mode != "drop" {
                    assert!(
                        finish(&mut input, if mode == "zero" { 0 } else { steps }).is_err(),
                        "{mode}"
                    );
                    assert!(finish(&mut input, 1).is_err());
                }
                drop(input);
            }
            drop(occupied);
            assert_eq!(memory.used(), 0);
        }
    }
}

#[test]
fn descriptor_diagnostic_configuration_uses_original_resolved_fields_through_retained_work() {
    let resources = AnalyzerResources::default();
    let compiled = resources
        .compile(&Analyzer::new(
            Tokenizer::Whitespace,
            vec![TokenFilter::Lowercase],
            vec![],
        ))
        .unwrap();
    let expected = serde_json::to_value(compiled.descriptor().configuration().unwrap()).unwrap();
    let memory = MemoryBudget::new(1 << 20);
    let cancellation = CancellationToken::new();
    let mut input = ImageDecoder::<_, crate::AnalysisError>::new(
        &memory,
        &cancellation,
        |mut reader| async move {
            compiled
                .descriptor()
                .configuration_with_input(&mut reader.input())
                .await
        },
    )
    .unwrap();
    let mut actual = None;
    let mut polls = 0;
    for index in 1..4096 {
        if let Some(configuration) = input
            .advance(&mut NoRead, ImageDecodeWindow { bytes: 1, steps: 1 })
            .unwrap()
        {
            actual = Some(serde_json::to_value(configuration).unwrap());
            polls = index;
            break;
        }
    }
    assert_eq!(actual.as_ref(), Some(&expected));
    assert!(polls > 1);
    assert_eq!(memory.used(), 0);
}
