//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::kuromoji::tests::analysis::common_raw;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

#[test]
fn compiled_japanese_filters_and_builtins_match_pinned_native_streams_and_normalization() {
    for (cases, expected, eligible) in [
        (
            include_str!("../../../../../../tests/parity/kuromoji/filter_cases.json"),
            include_str!("../../../../../../tests/parity/kuromoji/filter_expected.jsonl"),
            119,
        ),
        (
            include_str!("../../../../../../tests/parity/kuromoji/number_cases.json"),
            include_str!("../../../../../../tests/parity/kuromoji/number_expected.jsonl"),
            13,
        ),
        (
            include_str!("../../../../../../tests/parity/kuromoji/completion_cases.json"),
            include_str!("../../../../../../tests/parity/kuromoji/completion_expected.jsonl"),
            64,
        ),
    ] {
        let cases: Vec<Value> = serde_json::from_str(cases).unwrap();
        assert_eq!(cases.len(), expected.lines().count());
        let mut verified = 0;
        for (case, expected) in cases.iter().zip(expected.lines()) {
            if !matches!(
                case["kind"].as_str(),
                Some(
                    "native"
                        | "pipeline"
                        | "analyzer"
                        | "normalize"
                        | "completion_analyzer"
                        | "completion_normalize"
                )
            ) {
                continue;
            }
            let expected: Value = serde_json::from_str(expected).unwrap();
            assert_eq!(case["id"], expected["id"]);
            let config = filter_config(case);
            let input = case["input"]
                .as_str()
                .unwrap()
                .repeat(case["repeat"].as_u64().unwrap_or(1) as usize);
            let compiled = config.compile().unwrap();
            let restored = AnalyzerResources::new(AnalyzerLimits::default())
                .restore_json(compiled.descriptor().canonical_json())
                .unwrap();
            if matches!(
                case["kind"].as_str(),
                Some("normalize" | "completion_normalize")
            ) {
                for analyzer in [&compiled, &restored] {
                    let output = analyzer.normalize(&input).unwrap();
                    assert_eq!(
                        json!(output.encode_utf16().collect::<Vec<_>>()),
                        expected["normalized_utf16"],
                        "{}",
                        case["id"]
                    );
                }
            } else {
                for output in [
                    config.analyze_tokens(&input),
                    compiled.analyze_tokens(&input),
                    restored.analyze_tokens(&input),
                ] {
                    if expected.get("error").is_some() {
                        assert!(output.is_err(), "{}", case["id"]);
                        continue;
                    }
                    let actual = common_raw(&output.unwrap());
                    assert_eq!(
                        format!("{:x}", Sha256::digest(serde_json::to_vec(&actual).unwrap())),
                        expected["sha256"],
                        "{}",
                        case["id"]
                    );
                }
            }
            verified += 1;
        }
        assert_eq!(verified, eligible);
    }
}

fn filter_config(case: &Value) -> Analyzer {
    let dictionary = format!("sha256:{}", uqa_kuromoji_data::BUNDLE_SHA256);
    let mut config = match case["kind"].as_str().unwrap() {
        "analyzer" | "normalize" => crate::kuromoji::kuromoji_analyzer(),
        "completion_analyzer" | "completion_normalize" => {
            let mut config = crate::kuromoji::kuromoji_completion_analyzer();
            let TokenFilter::KuromojiCompletion(stage) = &mut config.token_filters[0] else {
                unreachable!()
            };
            stage.mode = serde_json::from_value(
                case.get("completion_mode")
                    .cloned()
                    .unwrap_or(json!("index")),
            )
            .unwrap();
            config
        }
        kind => {
            let mut filters = case["filters"].as_array().unwrap().clone();
            for filter in &mut filters {
                if filter["type"] == "unicode_simple_lowercase" {
                    filter["unicode_profile"] =
                        json!({"provider": "kuromoji", "dictionary": dictionary});
                }
            }
            Analyzer::new(
                Tokenizer::Kuromoji(KuromojiTokenizerConfig::default()),
                serde_json::from_value(json!(filters)).unwrap(),
                if kind == "pipeline" {
                    vec![crate::CharFilter::CJKWidth]
                } else {
                    Vec::new()
                },
            )
        }
    };
    let Tokenizer::Kuromoji(tokenizer) = &mut config.tokenizer else {
        unreachable!()
    };
    tokenizer.dictionary.clone_from(&dictionary);
    if let Some(mode) = case.get("mode") {
        tokenizer.mode = serde_json::from_value(mode.clone()).unwrap();
    }
    tokenizer.user_dictionary = case["user_dictionary"].as_str().map(str::to_owned);
    tokenizer.discard_punctuation = case["discard_punctuation"].as_bool().unwrap_or(true);
    tokenizer.discard_compound_token = case["discard_compound_token"].as_bool().unwrap_or(true);
    tokenizer.n_best_cost = case["n_best_cost"]
        .as_i64()
        .unwrap_or(0)
        .try_into()
        .unwrap();
    for filter in &mut config.token_filters {
        match filter {
            TokenFilter::KuromojiPartOfSpeech(stage) if stage.stop_tags.is_none() => {
                stage.dictionary = Some(dictionary.clone());
            }
            TokenFilter::KuromojiStop(stage) if stage.words.is_none() || stage.ignore_case => {
                stage.dictionary = Some(dictionary.clone());
            }
            TokenFilter::KuromojiCompletion(stage) => stage.dictionary.clone_from(&dictionary),
            TokenFilter::UnicodeSimpleLowercase(stage) => {
                stage
                    .unicode_profile
                    .kuromoji_dictionary_mut()
                    .unwrap()
                    .clone_from(&dictionary);
            }
            _ => {}
        }
    }
    if let Some(UnicodeProfile::Kuromoji { dictionary: name }) = config
        .normalization
        .as_mut()
        .and_then(crate::NormalizationConfig::profile_mut)
    {
        name.clone_from(&dictionary);
    }
    config
}

#[test]
fn compiled_and_restored_japanese_tokenizers_match_pinned_scalar_input_graphs_and_failures() {
    for (cases, expected, eligible) in [
        (
            include_str!("../../../../../../tests/parity/kuromoji/tokenizer_cases.json"),
            include_str!("../../../../../../tests/parity/kuromoji/tokenizer_expected.jsonl"),
            194,
        ),
        (
            include_str!("../../../../../../tests/parity/kuromoji/nbest_cases.json"),
            include_str!("../../../../../../tests/parity/kuromoji/nbest_expected.jsonl"),
            95,
        ),
    ] {
        let cases: Vec<Value> = serde_json::from_str(cases).unwrap();
        assert_eq!(cases.len(), expected.lines().count());
        let resources = AnalyzerResources::new(AnalyzerLimits::default());
        let mut verified = 0;
        for (case, expected) in cases.iter().zip(expected.lines()) {
            let expected: Value = serde_json::from_str(expected).unwrap();
            let id = case["id"].as_str().unwrap();
            assert_eq!(case["id"], expected["id"]);
            let config = Analyzer::new(
                Tokenizer::Kuromoji(KuromojiTokenizerConfig {
                    dictionary: format!("sha256:{}", uqa_kuromoji_data::BUNDLE_SHA256),
                    mode: serde_json::from_value(case["mode"].clone()).unwrap(),
                    discard_punctuation: case["discard_punctuation"].as_bool().unwrap(),
                    discard_compound_token: case["discard_compound_token"].as_bool().unwrap(),
                    user_dictionary: case["user_dictionary"].as_str().map(str::to_owned),
                    n_best_cost: case["n_best_cost"]
                        .as_i64()
                        .unwrap_or(0)
                        .try_into()
                        .unwrap(),
                    n_best_examples: case["n_best_examples"].as_str().map(str::to_owned),
                }),
                Vec::new(),
                Vec::new(),
            );
            let compiled = resources.compile(&config);
            if matches!(
                expected["error_stage"].as_str(),
                Some("compile" | "configure")
            ) {
                assert!(compiled.is_err(), "{id}");
                verified += 1;
                continue;
            }
            let compiled = compiled.unwrap_or_else(|error| panic!("{id}: {error}"));
            let input = if let Some(raw) = case.get("input_utf16") {
                let units: Vec<u16> = serde_json::from_value(raw.clone()).unwrap();
                let Ok(text) = String::from_utf16(&units) else {
                    continue;
                };
                text
            } else {
                case["input"]
                    .as_str()
                    .unwrap()
                    .repeat(case["repeat"].as_u64().unwrap_or(1) as usize)
            };
            let reopened = AnalyzerResources::new(AnalyzerLimits::default())
                .restore_json(compiled.descriptor().canonical_json())
                .unwrap();
            for compiled in [&compiled, &reopened] {
                let output = compiled.analyze_tokens(&input);
                if expected.get("error").is_some() {
                    assert!(output.is_err(), "{id}");
                    continue;
                }
                let output = output.unwrap_or_else(|error| panic!("{id}: {error}"));
                let actual = common_raw(&output);
                assert_eq!(
                    json!(output.tokens().len()),
                    expected["token_count"],
                    "{id}"
                );
                if let Some(analysis) = expected.get("analysis") {
                    assert_eq!(&actual, analysis, "{id}");
                }
                assert_eq!(
                    format!("{:x}", Sha256::digest(serde_json::to_vec(&actual).unwrap())),
                    expected["sha256"],
                    "{id}"
                );
            }
            verified += 1;
        }
        assert_eq!(verified, eligible);
    }
}
