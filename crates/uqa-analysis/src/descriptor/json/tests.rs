//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{work::inline, AnalyzerDescriptor, AnalyzerLimits, TokenLengthPolicy};
use serde_json::{json, Value};
use std::sync::Arc;
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder, ImageIoError},
    memory::MemoryBudget,
    CancellationToken,
};

fn original() -> Value {
    let descriptor = AnalyzerDescriptor::resolve(
        &crate::keyword_analyzer(),
        TokenLengthPolicy::EmittedTokens,
        AnalyzerLimits::default(),
    )
    .unwrap();
    serde_json::from_str(descriptor.canonical_json()).unwrap()
}

#[test]
fn native_descriptor_wire_preserves_reference_struct_forms_and_values() {
    let object = original();
    let expected: Wire = serde_json::from_value(object.clone()).unwrap();
    let expected = serde_json::to_value(expected).unwrap();
    let mut profile_sequence = object.clone();
    let profile = &object["descriptor"]["runtime_profiles"];
    profile_sequence["descriptor"]["runtime_profiles"] = json!([
        profile["rust_unicode"],
        profile["normalization_unicode"],
        profile["expressions"],
    ]);
    let mut data_sequence = object.clone();
    data_sequence["descriptor"] = [
        "format",
        "format_version",
        "algorithm_revision",
        "source_mapping_revision",
        "length_policy",
        "pipeline",
        "runtime_profiles",
    ]
    .into_iter()
    .map(|key| object["descriptor"][key].clone())
    .collect();
    let wire_sequence = json!([object["descriptor"], object["fingerprint"]]);
    for value in [object, profile_sequence, data_sequence, wire_sequence] {
        let text = serde_json::to_string_pretty(&value).unwrap();
        let reference: Wire = serde_json::from_str(&text).unwrap();
        let native = inline(decode(&text, &mut ())).unwrap();
        assert_eq!(serde_json::to_value(reference).unwrap(), expected);
        assert_eq!(serde_json::to_value(native).unwrap(), expected);
    }
}

fn message(error: &serde_json::Error) -> String {
    error
        .to_string()
        .split(" at line ")
        .next()
        .unwrap()
        .to_owned()
}

#[test]
fn native_descriptor_wire_preserves_schema_and_duplicate_key_error_order() {
    let invalid = [
        "42",
        "true",
        "null",
        r#""text""#,
        "[]",
        "[null]",
        "{}",
        r#"{"fingerprint":true,"descriptor":null}"#,
        r#"{"descriptor":null,"fingerprint":true}"#,
        r#"{"descriptor":{"format":12,"bad":true}}"#,
        r#"{"descriptor":{"bad":true,"format":12}}"#,
        r#"{"descriptor":{"runtime_profiles":{"expressions":[false]}}}"#,
        r#"{"descriptor":{"runtime_profiles":[]}}"#,
        r#"{"descriptor":{"runtime_profiles":{"rust_unicode":[1,2,3,4]}}}"#,
        r#"{"descriptor":{"runtime_profiles":{"rust_unicode":[1,2]}}}"#,
        r#"{"descriptor":{"runtime_profiles":{"rust_unicode":[1,2.0,3]}}}"#,
        r#"{"descriptor":{"runtime_profiles":{"rust_unicode":[1,256,3]}}}"#,
        r#"{"descriptor":{"runtime_profiles":{"rust_unicode":12}}}"#,
    ];
    for text in invalid {
        let Err(reference) = serde_json::from_str::<Wire>(text) else {
            panic!("invalid reference accepted {text}");
        };
        let Err(AnalysisError::Json(actual)) = inline(decode(text, &mut ())) else {
            panic!("invalid native input accepted {text}");
        };
        assert_eq!(message(&actual), message(&reference), "{text}");
    }
    // Duplicate admission completes before any typed field or fingerprint validation.
    for text in [
        r#"{"fingerprint":false,"descriptor":{"pipeline":{"x":1,"x":2}}}"#,
        r#"{"descriptor":{"pipeline":{"x":1,"\u0078":2}},"fingerprint":false}"#,
    ] {
        let Err(AnalysisError::Json(error)) = inline(decode(text, &mut ())) else {
            panic!("duplicate native input accepted");
        };
        assert_eq!(message(&error), "duplicate analyzer descriptor property");
        assert_eq!(error.line(), 1);
    }
}

struct NoRead;
impl std::io::Read for NoRead {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        panic!("wire input borrows its original bytes")
    }
}
const WINDOW: ImageDecodeWindow = ImageDecodeWindow { bytes: 1, steps: 1 };

fn decoder(
    text: Arc<str>,
    memory: &MemoryBudget,
    token: &CancellationToken,
) -> ImageDecoder<Wire, AnalysisError> {
    ImageDecoder::new(memory, token, |mut channel| async move {
        decode(&text, &mut channel.input()).await
    })
    .unwrap()
}

#[test]
fn native_descriptor_wire_retires_candidates_during_each_syntax_pass() {
    let mut value = original();
    value["descriptor"]["runtime_profiles"]["expressions"] = (0..80)
        .map(|i| Value::String(format!("{i}{}", "a中𠮷".repeat(1024))))
        .collect();
    let text: Arc<str> = serde_json::to_string(&value).unwrap().into();
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let mut input = decoder(text.clone(), &memory, &token);
    let mut calls = 0;
    loop {
        calls += 1;
        if let Some(wire) = input.advance(&mut NoRead, WINDOW).unwrap() {
            assert_eq!(serde_json::to_value(wire).unwrap(), value);
            break;
        }
        assert!(calls < 100_000);
    }
    drop(input);
    for stop in [0, 1, calls / 2, calls - 1] {
        for cancel in [false, true] {
            let measured = allocation_counter::measure(|| {
                let memory = MemoryBudget::new(1 << 20);
                let token = CancellationToken::new();
                let mut input = decoder(text.clone(), &memory, &token);
                for _ in 0..stop {
                    assert!(input.advance(&mut NoRead, WINDOW).unwrap().is_none());
                }
                if cancel {
                    token.cancel();
                    assert!(matches!(
                        input.advance(&mut NoRead, WINDOW),
                        Err(AnalysisError::Input(ImageIoError::Cancelled(_)))
                    ));
                }
                drop(input);
                assert_eq!(memory.used(), 0);
            });
            assert_eq!(measured.bytes_current, 0, "stop {stop}");
        }
    }
}
