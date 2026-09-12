//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Tokenizer-only differential checks keep downstream filters from hiding segmentation errors.

use serde_json::{json, Value};
use uqa_analysis::nori::{KoreanTokenizer, NoriOptions, UserDictionary, UserDictionaryLimits};

use super::nori_resources::model;

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
        let mut position = -1_i64;
        let projected: Vec<_> = actual.tokens.iter().map(|token| {
            position += i64::from(token.position_increment);
            let parts = token.morphemes.as_ref().map(|parts| parts.iter().map(|part| json!({"surface": String::from_utf16(&part.surface_utf16).unwrap(), "pos": part.pos})).collect::<Vec<_>>());
            json!({"term": String::from_utf16(&token.term_utf16).unwrap(), "start_utf16": token.start_utf16, "end_utf16": token.end_utf16,
                "position": position, "position_increment": token.position_increment, "position_length": token.position_length,
                "pos_type": token.pos_type, "left_pos": token.left_pos, "right_pos": token.right_pos, "reading": token.reading, "morphemes": parts})
        }).collect();
        assert_eq!(json!(projected), expected["tokens"], "{id}");
        checked += 1;
    }
    assert!(checked >= 15);
}

fn raw_analysis(output: &uqa_analysis::nori::NoriOutput) -> Value {
    let tokens: Vec<_> = output.tokens.iter().map(|token| json!({
        "term_utf16": token.term_utf16, "start_utf16": token.start_utf16, "end_utf16": token.end_utf16,
        "position_increment": token.position_increment, "position_length": token.position_length,
        "pos_type": token.pos_type, "left_pos": token.left_pos, "right_pos": token.right_pos,
        "reading_utf16": token.reading.as_ref().map(|text| text.encode_utf16().collect::<Vec<_>>()), "morphemes": token.morphemes,
    })).collect();
    json!({"tokens": tokens, "final_offset_utf16": output.final_offset_utf16, "final_position_increment": output.final_position_increment})
}

fn canonical(value: &Value, bytes: &mut Vec<u8>) {
    match value {
        Value::Object(object) => {
            bytes.push(b'{');
            let mut keys: Vec<_> = object.keys().collect();
            keys.sort_unstable();
            for (index, key) in keys.into_iter().enumerate() {
                if index > 0 {
                    bytes.push(b',');
                }
                serde_json::to_writer(&mut *bytes, key).unwrap();
                bytes.push(b':');
                canonical(&object[key], bytes);
            }
            bytes.push(b'}');
        }
        Value::Array(array) => {
            bytes.push(b'[');
            for (index, value) in array.iter().enumerate() {
                if index > 0 {
                    bytes.push(b',');
                }
                canonical(value, bytes);
            }
            bytes.push(b']');
        }
        _ => serde_json::to_writer(bytes, value).unwrap(),
    }
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
