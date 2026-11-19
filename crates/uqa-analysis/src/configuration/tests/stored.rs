//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{AnalyzerConfigurationError, AnalyzerDefinition};
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder},
    json::JsonReadError,
    memory::{Budgeted, BudgetedString, MemoryBudget, ProductionControl},
    CancellationToken, ValueRetentionError,
};

const WINDOW: ImageDecodeWindow = ImageDecodeWindow { bytes: 1, steps: 1 };

struct NoRead;
impl std::io::Read for NoRead {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        panic!("stored configuration uses its retained text")
    }
}

fn admitted(text: &str, memory: &MemoryBudget) -> Budgeted<String> {
    let mut value = BudgetedString::new(memory);
    value.push_str(text).unwrap();
    let (value, memory) = value.into_parts();
    Budgeted::new(value, memory)
}

fn begin(
    text: Budgeted<String>,
    memory: &MemoryBudget,
    token: &CancellationToken,
) -> ImageDecoder<AnalyzerDefinition, AnalyzerConfigurationError> {
    ImageDecoder::new(memory, token, |mut channel| async move {
        AnalyzerDefinition::from_stored_from(text, channel.input()).await
    })
    .unwrap()
}

fn compare(text: &str) {
    let expected = serde_json::from_str::<Analyzer>(text);
    let memory = MemoryBudget::new(16 << 20);
    let token = CancellationToken::new();
    let actual = begin(admitted(text, &memory), &memory, &token).read(&mut NoRead);
    match (expected, actual) {
        (Ok(expected), Ok(actual)) => {
            let expected = serde_json::to_vec(&expected).unwrap();
            let image = actual
                .capture_image(&ProductionControl::uncontrolled())
                .unwrap();
            assert_eq!(image.as_bytes(), expected, "{text}");
            assert_eq!(
                serde_json::to_vec(actual.configuration()).unwrap(),
                expected,
                "{text}"
            );
        }
        (Err(_), Err(AnalyzerConfigurationError::Decoding(JsonReadError::InvalidJson))) => (),
        (expected, actual) => panic!("{text}: expected {expected:?}, got {actual:?}"),
    }
    assert_eq!(memory.used(), 0, "{text}");
}

#[test]
fn stored_analyzer_input_preserves_public_values_defaults_and_feature_shapes() {
    let invalid = [
        json!(null),
        json!(true),
        json!(0),
        json!(-1),
        json!(1.5),
        json!(""),
        json!([]),
        json!({}),
        serde_json::from_str::<Value>("18446744073709551616").unwrap(),
    ];
    for value in &invalid {
        compare(&value.to_string());
    }
    for (field, stage) in stages() {
        compare(&pipeline(field, stage.clone()).to_string());
        for key in stage
            .as_object()
            .unwrap()
            .keys()
            .chain(std::iter::once(&"unknown".to_owned()))
        {
            let mut absent = stage.clone();
            absent.as_object_mut().unwrap().remove(key);
            compare(&pipeline(field, absent).to_string());
            for value in &invalid {
                let mut changed = stage.clone();
                changed[key] = value.clone();
                compare(&pipeline(field, changed).to_string());
            }
        }
    }
    for source in [
        "[]",
        "[{},[],[]]",
        r#"[{"type":"keyword"}]"#,
        r#"[{"type":"keyword"},[]]"#,
        r#"[{"type":"keyword"},[],[]]"#,
        r#"[{"type":"keyword"},[],[],null]"#,
        r#"[{"type":"keyword"},[],[],null,0]"#,
    ] {
        compare(source);
    }
}

#[test]
fn stored_analyzer_input_preserves_raw_field_order_duplicates_and_ignored_values() {
    for source in [
        r#"{"tokenizer":{"pattern":"[","type":"pattern"},"unknown":"\ud800"}"#,
        r#"{"unknown":"\ud800","tokenizer":{"type":"keyword"}}"#,
        r#"{"tokenizer":{"unknown":"\ud800","type":"keyword"}}"#,
        r#"{"tokenizer":{"type":"keyword","unknown":"\ud800"}}"#,
        r#"{"tokenizer":{"type":"keyword","type":"keyword"}}"#,
        r#"{"tokenizer":{"type":"keyword"},"tokenizer":{"type":"keyword"}}"#,
        r#"{"tokenizer":{"type":"pattern","pattern":"a","pattern":"b"}}"#,
        r#"{"tokenizer":{"type":"pattern","pat\u0074ern":"a","pattern":"b"}}"#,
        r#"{"normalization":null,"normalization":null}"#,
        r#"{"unknown":0,"unknown":1}"#,
        r#"{"token_filters":[{"type":"synonym","synonyms":{"a":["first"],"a":["last"]}}]}"#,
        r#"{"char_filters":[{"mapping":{"a":"first","a":"last"},"type":"mapping"}]}"#,
        r#"{"char_filters":[{"type":"h_t_m_l_strip"}],"token_filters":[{"type":"a_s_c_i_i_folding"}]}"#,
        r#"{"tokenizer":["n_gram",1,2]}"#,
        r#"{"tokenizer":["pattern","a"]}"#,
        r#"{"tokenizer":["whitespace",0]}"#,
        r#"{"token_filters":[["length"],["stop","english",[]]]}"#,
        r#"{"char_filters":[["mapping",{"a":"b"}]]}"#,
        r#"{"tokenizer":{"type":"n_gram","min_gram":-0,"max_gram":3}}"#,
        r#"{"tokenizer":{"type":"n_gram","min_gram":0.0,"max_gram":3}}"#,
    ] {
        compare(source);
    }
    let valid = r#"{"char_filters":[{"mapping":{"a":"b"},"type":"mapping"}]}"#;
    for end in 0..valid.len() {
        compare(&valid[..end]);
    }
    compare(&format!("{valid}0"));
    for depth in [126, 127, 128, 129, 256] {
        let nested = format!("{}0{}", "[".repeat(depth), "]".repeat(depth));
        compare(&format!(r#"{{"unknown":{nested}}}"#));
        compare(&format!(
            r#"{{"tokenizer":{{"type":"keyword","unknown":{nested}}}}}"#
        ));
    }
}

#[test]
fn stored_analyzer_input_preserves_generic_numeric_map_content() {
    for value in [
        "0",
        "-0",
        "1.5",
        "1E2",
        "1e-00",
        "18446744073709551616",
        "-9223372036854775809",
        "1e1000",
    ] {
        compare(&format!(
            r#"{{"char_filters":[{{"type":"mapping","mapping":{value}}}]}}"#
        ));
        compare(&format!(
            r#"{{"token_filters":[{{"type":"synonym","synonyms":{value}}}]}}"#
        ));
    }
    compare(&format!(
        r#"{{"char_filters":[{{"mapping":1E{},"type":"mapping"}}]}}"#,
        "1".repeat(16384)
    ));
    compare(&format!(
        r#"{{"char_filters":[{{"type":"mapping","mapping":{}}}]}}"#,
        "9".repeat(16384)
    ));
}

#[cfg(feature = "nori")]
#[test]
fn stored_analyzer_input_preserves_korean_default_and_scalar_enum_forms() {
    for value in [
        r#"{"type":"nori_tokenizer"}"#,
        r#"["nori_tokenizer"]"#,
        r#"{"type":"nori_tokenizer","decompound_mode":{"mixed":null}}"#,
    ] {
        compare(&format!(
            r#"{{"tokenizer":{value},"token_filters":[{{"type":"unicode_simple_lowercase"}}]}}"#
        ));
    }
}

#[cfg(feature = "kuromoji")]
#[test]
fn stored_analyzer_input_preserves_japanese_default_and_scalar_enum_forms() {
    for value in [
        r#"{"type":"kuromoji_tokenizer"}"#,
        r#"["kuromoji_tokenizer"]"#,
        r#"{"type":"kuromoji_tokenizer","mode":{"search":null}}"#,
    ] {
        compare(&format!(
            r#"{{"tokenizer":{value},"token_filters":[{{"type":"kuromoji_completion"}},{{"type":"unicode_simple_lowercase"}}]}}"#
        ));
    }
}

mod lifecycle;
