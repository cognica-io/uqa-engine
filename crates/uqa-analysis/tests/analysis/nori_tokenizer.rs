//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Tokenizer-only differential checks keep downstream filters from hiding segmentation errors.

use serde_json::{json, Value};
use uqa_analysis::nori::{KoreanTokenizer, NoriOptions, UserDictionary, UserDictionaryLimits};

use super::nori_resources::{canonical, model, raw_analysis};

#[test]
fn tokenizer_matches_all_original_tokenizer_fixtures() {
    let mut checked = 0;
    for row in include_str!("../../../../tests/parity/nori/expected.jsonl").lines() {
        let expected: Value = serde_json::from_str(row).unwrap();
        if expected["pipeline"] != "tokenizer" {
            continue;
        }
        let id = expected["id"].as_str().unwrap();
        let user = expected["user_dictionary"]
            .as_str()
            .map(|source| UserDictionary::compile(source, model(), UserDictionaryLimits::default()))
            .transpose();
        if expected.get("error").is_some() {
            assert!(user.is_err(), "{id}");
            continue;
        }
        let options = NoriOptions {
            decompound_mode: serde_json::from_value(expected["decompound_mode"].clone()).unwrap(),
            output_unknown_unigrams: expected["output_unknown_unigrams"].as_bool().unwrap(),
            discard_punctuation: expected["discard_punctuation"].as_bool().unwrap(),
        };
        let tokenizer =
            KoreanTokenizer::new(model().clone(), user.unwrap().flatten(), options).unwrap();
        let actual = tokenizer
            .tokenize(expected["input"].as_str().unwrap())
            .unwrap_or_else(|error| panic!("{id}: {error}"));
        assert_eq!(
            json!(actual.final_offset_utf16),
            expected["final_offset_utf16"],
            "{id}"
        );
        assert_eq!(
            json!(actual.final_position_increment),
            expected["final_position_increment"],
            "{id}"
        );
        assert_eq!(
            super::nori_resources::string_tokens(&actual),
            expected["tokens"],
            "{id}"
        );
        checked += 1;
    }
    assert!(checked >= 15);
}

#[test]
fn tokenizer_matches_complete_docker_snapshots_across_unicode_and_lattice_boundaries() {
    use sha2::{Digest, Sha256};
    use uqa_analysis::{AnalysisError, AnalysisResult};
    let case_bytes = include_bytes!("../../../../tests/parity/nori/tokenizer_cases.json");
    let expected_bytes = include_bytes!("../../../../tests/parity/nori/tokenizer_expected.jsonl");
    let manifest: Value = serde_json::from_str(include_str!(
        "../../../../tests/parity/nori/tokenizer_manifest.json"
    ))
    .unwrap();
    assert_eq!(
        manifest["cases_sha256"],
        format!("{:x}", Sha256::digest(case_bytes))
    );
    assert_eq!(
        manifest["expected_sha256"],
        format!("{:x}", Sha256::digest(expected_bytes))
    );
    let cases: Vec<Value> = serde_json::from_slice(case_bytes).unwrap();
    let expected: Vec<Value> = std::str::from_utf8(expected_bytes)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(cases.len(), expected.len());
    assert_eq!(json!(cases.len()), manifest["fixture_count"]);
    for (case, expected) in cases.into_iter().zip(expected) {
        assert_eq!(case["id"], expected["id"]);
        let id = case["id"].as_str().unwrap();
        let options = NoriOptions {
            decompound_mode: serde_json::from_value(case["decompound_mode"].clone()).unwrap(),
            output_unknown_unigrams: case["output_unknown_unigrams"].as_bool().unwrap(),
            discard_punctuation: case["discard_punctuation"].as_bool().unwrap(),
        };
        let run = || -> AnalysisResult<_> {
            let user = case["user_dictionary"]
                .as_str()
                .map(|source| {
                    UserDictionary::compile(source, model(), UserDictionaryLimits::default())
                })
                .transpose()?
                .flatten();
            KoreanTokenizer::new(model().clone(), user, options)?
                .tokenize(case["input"].as_str().unwrap())
        };
        let actual = run();
        if expected.get("error").is_some() {
            assert!(matches!(actual, Err(AnalysisError::Dictionary(_))), "{id}");
            continue;
        }
        let actual = actual.unwrap_or_else(|error| panic!("{id}: {error}"));
        let analysis = raw_analysis(&actual);
        if let Some(expected) = expected.get("analysis") {
            assert_eq!(analysis, *expected, "{id}");
        }
        let mut bytes = Vec::new();
        canonical(&analysis, &mut bytes);
        assert_eq!(json!(actual.tokens.len()), expected["token_count"], "{id}");
        assert_eq!(
            format!("{:x}", Sha256::digest(&bytes)),
            expected["sha256"].as_str().unwrap(),
            "{id}"
        );
    }
}

#[test]
fn tokenizer_limits_and_cancellation_return_no_partial_result_and_allow_reuse() {
    use uqa_analysis::nori::{DictionaryError, NoriLimits};
    use uqa_analysis::AnalysisError;
    let tokenizer = KoreanTokenizer::new(model().clone(), None, NoriOptions::default()).unwrap();
    let input = "가".repeat(2050);
    let expected = tokenizer.tokenize(&input).unwrap();
    let limits = NoriLimits::default();
    for bounded in [
        NoriLimits {
            max_input_utf16: 10,
            ..limits
        },
        NoriLimits {
            max_lattice_positions: 1,
            ..limits
        },
        NoriLimits {
            max_lattice_candidates: 1,
            ..limits
        },
        NoriLimits {
            max_tokens: 1,
            ..limits
        },
        NoriLimits {
            max_output_utf16: 1,
            ..limits
        },
    ] {
        assert!(matches!(
            tokenizer.tokenize_controlled(&input, bounded, &mut || Ok(())),
            Err(AnalysisError::Dictionary(DictionaryError::Limit { .. }))
        ));
    }
    for stop in [1, 4, 10] {
        let mut polls = 0;
        let actual = tokenizer.tokenize_controlled(&input, limits, &mut || {
            polls += 1;
            if polls == stop {
                Err(AnalysisError::Cancelled)
            } else {
                Ok(())
            }
        });
        assert!(matches!(actual, Err(AnalysisError::Cancelled)));
        assert_eq!(polls, stop);
    }
    assert_eq!(tokenizer.clone().tokenize(&input).unwrap(), expected);
}
