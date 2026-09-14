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
