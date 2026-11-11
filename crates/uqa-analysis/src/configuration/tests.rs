//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::work::inline;
use serde_json::{json, Value};

fn compare(value: &Value) {
    let expected = serde_json::from_value::<Analyzer>(value.clone());
    let actual = inline(decode(value, &mut ()));
    match (expected, actual) {
        (Ok(expected), Ok(actual)) => assert_eq!(
            serde_json::to_value(actual).unwrap(),
            serde_json::to_value(expected).unwrap(),
            "{value}"
        ),
        (Err(expected), Err(crate::AnalysisError::Json(actual))) => {
            assert_eq!(actual.to_string(), expected.to_string(), "{value}");
        }
        (expected, actual) => {
            panic!("configuration {value}: expected {expected:?}, got {actual:?}")
        }
    }
}

fn stages() -> Vec<(&'static str, Value)> {
    #[allow(unused_mut)]
    let mut values = vec![
        ("tokenizer", json!({"type":"whitespace"})),
        ("tokenizer", json!({"type":"standard"})),
        ("tokenizer", json!({"type":"letter"})),
        ("tokenizer", json!({"type":"keyword"})),
        ("tokenizer", json!({"type":"pattern", "pattern":"x+"})),
        (
            "tokenizer",
            json!({"type":"n_gram", "min_gram":2, "max_gram":4}),
        ),
        ("token_filters", json!({"type":"lowercase"})),
        (
            "token_filters",
            json!({"type":"stop", "language":"english", "custom_words":["a","b"]}),
        ),
        ("token_filters", json!({"type":"porter_stem"})),
        ("token_filters", json!({"type":"ascii_folding"})),
        ("token_filters", json!({"type":"a_s_c_i_i_folding"})),
        (
            "token_filters",
            json!({"type":"synonym", "synonyms":{"a":["b","c"]},"synonyms_path":null}),
        ),
        (
            "token_filters",
            json!({"type":"ngram", "min_gram":1,"max_gram":3,"keep_short":true}),
        ),
        (
            "token_filters",
            json!({"type":"edge_ngram", "min_gram":1,"max_gram":3}),
        ),
        (
            "token_filters",
            json!({"type":"length", "min_length":0,"max_length":8}),
        ),
        ("char_filters", json!({"type":"html_strip"})),
        ("char_filters", json!({"type":"h_t_m_l_strip"})),
        ("char_filters", json!({"type":"cjk_width"})),
        (
            "char_filters",
            json!({"type":"mapping", "mapping":{"a":"b"}}),
        ),
        (
            "char_filters",
            json!({"type":"pattern_replace", "pattern":"x+", "replacement":"y"}),
        ),
        ("normalization", json!({"type":"unavailable"})),
        ("normalization", json!({"type":"cjk_width"})),
    ];
    #[cfg(feature = "nori")]
    values.extend([
        ("tokenizer", json!({"type":"nori_tokenizer","dictionary":"d","decompound_mode":"mixed","output_unknown_unigrams":true,"discard_punctuation":false,"user_dictionary":"x"})),
        ("token_filters", json!({"type":"nori_part_of_speech","stop_tags":["NNG","NNP"]})),
        ("token_filters", json!({"type":"nori_readingform"})),
        ("token_filters", json!({"type":"nori_number"})),
        ("token_filters", json!({"type":"unicode_simple_lowercase","unicode_profile":{"provider":"nori","dictionary":"d"}})),
        ("normalization", json!({"type":"unicode_simple_lowercase","profile":{"provider":"nori","dictionary":"d"}})),
    ]);
    #[cfg(feature = "kuromoji")]
    values.extend([
        ("tokenizer", json!({"type":"kuromoji_tokenizer","dictionary":"d","mode":"extended","discard_punctuation":false,"discard_compound_token":true,"user_dictionary":"x","n_best_cost":-1,"n_best_examples":"y"})),
        ("token_filters", json!({"type":"kuromoji_part_of_speech","stop_tags":["名詞"],"dictionary":null})),
        ("token_filters", json!({"type":"kuromoji_stop","words":["a"],"ignore_case":true,"dictionary":"d"})),
        ("token_filters", json!({"type":"kuromoji_completion","dictionary":"d","mode":"index"})),
        ("token_filters", json!({"type":"kuromoji_baseform"})),
        ("token_filters", json!({"type":"kuromoji_stemmer","minimum_length":-1})),
        ("token_filters", json!({"type":"kuromoji_hiragana_uppercase"})),
        ("token_filters", json!({"type":"kuromoji_katakana_uppercase"})),
        ("token_filters", json!({"type":"kuromoji_readingform","use_romaji":true})),
        ("token_filters", json!({"type":"kuromoji_number"})),
        ("char_filters", json!({"type":"kuromoji_iteration_mark","normalize_kanji":false,"normalize_kana":true})),
        ("normalization", json!({"type":"cjk_width_simple_lowercase","profile":{"provider":"kuromoji","dictionary":"d"}})),
    ]);
    values
}

fn pipeline(field: &str, stage: Value) -> Value {
    let value = if field.ends_with("filters") {
        Value::Array(vec![stage])
    } else {
        stage
    };
    let mut object = serde_json::Map::new();
    object.insert(field.into(), value);
    Value::Object(object)
}

#[test]
fn native_configuration_preserves_public_serde_values_defaults_and_diagnostics() {
    let invalid = [
        Value::Null,
        json!(true),
        json!(0),
        json!(-1),
        json!(1.5),
        json!(""),
        json!([]),
        json!({}),
        serde_json::from_str("18446744073709551616").unwrap(),
        serde_json::from_str("-9223372036854775809").unwrap(),
        serde_json::from_str("1e1000").unwrap(),
    ];
    for value in &invalid {
        compare(value);
    }
    for (field, stage) in stages() {
        compare(&pipeline(field, stage.clone()));
        let object = stage.as_object().unwrap();
        for key in object.keys().chain(std::iter::once(&"unknown".to_owned())) {
            let mut absent = stage.clone();
            absent.as_object_mut().unwrap().remove(key);
            compare(&pipeline(field, absent));
            for value in &invalid {
                let mut changed = stage.clone();
                changed[key] = value.clone();
                compare(&pipeline(field, changed));
            }
        }
        for value in &invalid {
            compare(&pipeline(field, value.clone()));
        }
    }
    let complete = json!([{"type":"whitespace"},[],[],null]);
    for size in 0..=4 {
        compare(&Value::Array(complete.as_array().unwrap()[..size].to_vec()));
    }
    compare(&json!([{"type":"whitespace"},[],[],null,0]));
    compare(&json!({"char_filters":false,"token_filters":false,"tokenizer":false}));
}

mod observations;
mod retained;

#[test]
fn native_configuration_preserves_tagged_and_struct_sequence_forms() {
    for (field, values) in [
        (
            "tokenizer",
            vec![
                json!([]),
                json!(["pattern"]),
                json!(["pattern", "x"]),
                json!(["pattern",{"pattern":"x"}]),
                json!(["whitespace"]),
                json!(["whitespace", 0]),
                json!(["n_gram", 1]),
                json!(["n_gram", 1, 2]),
                json!(["n_gram", 1, 2, 3]),
            ],
        ),
        (
            "token_filters",
            vec![
                json!(["stop"]),
                json!(["stop", "", []]),
                json!(["stop", "", [], 0]),
                json!(["synonym", {}, null]),
                json!(["ngram", 1, 2]),
                json!(["edge_ngram", 1]),
                json!(["length"]),
                json!(["lowercase", 0]),
            ],
        ),
        (
            "char_filters",
            vec![
                json!(["mapping"]),
                json!(["mapping", {}]),
                json!(["pattern_replace", "x"]),
                json!(["html_strip", 0]),
            ],
        ),
        (
            "normalization",
            vec![
                json!(["unavailable"]),
                json!(["unavailable", 0]),
                json!(["cjk_width", {}]),
            ],
        ),
    ] {
        for value in values {
            compare(&pipeline(field, value));
        }
    }
    #[cfg(feature = "nori")]
    for value in [
        json!(["nori_tokenizer"]),
        json!(["nori_tokenizer", "d"]),
        json!(["nori_tokenizer", "d", "mixed", true, false, null, 0]),
    ] {
        compare(&pipeline("tokenizer", value));
    }
    #[cfg(any(feature = "nori", feature = "kuromoji"))]
    for value in [
        json!(["unicode_simple_lowercase"]),
        json!(["unicode_simple_lowercase", ["nori", "d"]]),
        json!(["unicode_simple_lowercase",{"provider":"nori","dictionary":3}]),
    ] {
        compare(&pipeline("normalization", value));
    }
}
