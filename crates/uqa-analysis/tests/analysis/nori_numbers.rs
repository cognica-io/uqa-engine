//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Complete number-filter attributes, including lookahead and hidden EOF effects.

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uqa_analysis::nori::{
    normalize_number_utf16, KoreanAnalyzer, KoreanFilter, NoriLimits, NoriMorpheme, NoriOptions,
    NoriOrigin, NoriOutput, NoriToken, UserDictionary, UserDictionaryLimits,
};
use uqa_analysis::AnalysisResult;

use super::nori_resources::{canonical, model, raw_analysis};

#[path = "nori_numbers/memory.rs"]
mod memory;

fn units(value: &Value) -> Vec<u16> {
    value
        .as_array()
        .unwrap()
        .iter()
        .map(|unit| u16::try_from(unit.as_u64().unwrap()).unwrap())
        .collect()
}

fn input(case: &Value) -> Vec<u16> {
    case.get("input_utf16").map_or_else(
        || {
            case["input"]
                .as_str()
                .unwrap_or("")
                .encode_utf16()
                .collect()
        },
        units,
    )
}

fn token(value: &Value) -> NoriToken {
    NoriToken {
        term_utf16: units(&value["term_utf16"]),
        start_utf16: value["start_utf16"].as_u64().unwrap() as usize,
        end_utf16: value["end_utf16"].as_u64().unwrap() as usize,
        position_increment: u32::try_from(value["position_increment"].as_u64().unwrap()).unwrap(),
        position_length: u32::try_from(value["position_length"].as_u64().unwrap()).unwrap(),
        keyword: value["keyword"].as_bool().unwrap(),
        pos_type: serde_json::from_value(value["pos_type"].clone()).unwrap(),
        left_pos: serde_json::from_value(value["left_pos"].clone()).unwrap(),
        right_pos: serde_json::from_value(value["right_pos"].clone()).unwrap(),
        reading: value["reading_utf16"]
            .as_array()
            .map(|_| String::from_utf16(&units(&value["reading_utf16"])).unwrap()),
        morphemes: value["morphemes"].as_array().map(|parts| {
            parts
                .iter()
                .map(|part| NoriMorpheme {
                    surface_utf16: units(&part["surface_utf16"]),
                    pos: serde_json::from_value(part["pos"].clone()).unwrap(),
                })
                .collect()
        }),
        origin: NoriOrigin::Known,
    }
}

fn analyze(case: &Value) -> AnalysisResult<NoriOutput> {
    let filters: Vec<KoreanFilter> = serde_json::from_value(case["filters"].clone()).unwrap();
    if case["pipeline"] == "synthetic" {
        let tokens = case["tokens"]
            .as_array()
            .unwrap()
            .iter()
            .map(token)
            .collect();
        let mut output = NoriOutput::from_tokens(
            tokens,
            case["final_offset_utf16"].as_u64().unwrap() as usize,
            u32::try_from(case["final_position_increment"].as_u64().unwrap()).unwrap(),
        );
        for filter in filters {
            output = filter.apply(output, model())?;
        }
        return Ok(output);
    }
    let options = NoriOptions {
        decompound_mode: serde_json::from_value(
            case.get("decompound_mode")
                .cloned()
                .unwrap_or(json!("none")),
        )
        .unwrap(),
        output_unknown_unigrams: case["output_unknown_unigrams"].as_bool().unwrap_or(false),
        discard_punctuation: case["discard_punctuation"].as_bool().unwrap_or(false),
    };
    let user = case["user_dictionary"]
        .as_str()
        .map(|source| UserDictionary::compile(source, model(), UserDictionaryLimits::default()))
        .transpose()?
        .flatten();
    KoreanAnalyzer::with_filters(model().clone(), user, options, &filters)?
        .analyze(case["input"].as_str().unwrap())
}

#[test]
fn number_parsing_and_composition_match_the_complete_docker_snapshots() {
    let case_bytes = include_bytes!("../../../../tests/parity/nori/number_cases.json");
    let expected_bytes = include_bytes!("../../../../tests/parity/nori/number_expected.jsonl");
    let manifest: Value = serde_json::from_str(include_str!(
        "../../../../tests/parity/nori/number_manifest.json"
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
        .map(|row| serde_json::from_str(row).unwrap())
        .collect();
    assert_eq!(cases.len(), expected.len());
    assert_eq!(json!(cases.len()), manifest["fixture_count"]);
    for (case, expected) in cases.into_iter().zip(expected) {
        let id = case["id"].as_str().unwrap();
        assert_eq!(case["id"], expected["id"]);
        if case["pipeline"] == "normalize" {
            let normalized =
                normalize_number_utf16(&input(&case), NoriLimits::default(), &mut || Ok(()))
                    .unwrap();
            let mut hash = Sha256::new();
            for unit in &normalized {
                hash.update(unit.to_be_bytes());
            }
            assert_eq!(json!(normalized.len()), expected["unit_count"], "{id}");
            if let Some(expected) = expected.get("normalized_utf16") {
                assert_eq!(json!(normalized), *expected, "{id}");
            }
            assert_eq!(
                format!("{:x}", hash.finalize()),
                expected["utf16be_sha256"].as_str().unwrap(),
                "{id}"
            );
            continue;
        }
        let result = analyze(&case);
        if expected.get("error").is_some() {
            assert!(result.is_err(), "{id}");
            continue;
        }
        let result = result.unwrap_or_else(|error| panic!("{id}: {error}"));
        if case["pipeline"] == "tokenizer" {
            super::nori_resources::assert_generic_bridge(&result, case["input"].as_str().unwrap());
        }
        let mut analysis = raw_analysis(&result);
        for (value, token) in analysis["tokens"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .zip(&result.tokens)
        {
            value["keyword"] = json!(token.keyword);
        }
        if let Some(expected) = expected.get("analysis") {
            assert_eq!(analysis, *expected, "{id}");
        }
        let mut bytes = Vec::new();
        canonical(&analysis, &mut bytes);
        assert_eq!(json!(result.tokens.len()), expected["token_count"], "{id}");
        assert_eq!(
            format!("{:x}", Sha256::digest(bytes)),
            expected["sha256"].as_str().unwrap(),
            "{id}"
        );
    }
}

#[test]
fn numeric_bounds_cancellation_and_reuse_do_not_turn_errors_into_verbatim_success() {
    use uqa_analysis::nori::{normalize_number, DictionaryError};
    use uqa_analysis::AnalysisError;
    let text = format!("{}{}", "9".repeat(4096), "십".repeat(4096));
    let input: Vec<_> = text.encode_utf16().collect();
    let mut polls = 0;
    let expected = normalize_number_utf16(&input, NoriLimits::default(), &mut || {
        polls += 1;
        Ok(())
    })
    .unwrap();
    assert!(polls > 10);
    // Adding many small values must not repeatedly scan every unchanged digit of the long sum.
    assert!(
        polls < 250,
        "unexpected repeated coefficient scans: {polls}"
    );
    for stop in [1, 3, polls / 2, polls] {
        let mut calls = 0;
        let result = normalize_number_utf16(&input, NoriLimits::default(), &mut || {
            calls += 1;
            if calls == stop {
                Err(AnalysisError::Cancelled)
            } else {
                Ok(())
            }
        });
        assert!(matches!(result, Err(AnalysisError::Cancelled)));
        assert_eq!(calls, stop);
    }
    for (source, limits) in [
        (
            "1.2.3",
            NoriLimits {
                max_output_utf16: 2,
                ..NoriLimits::default()
            },
        ),
        (
            "해",
            NoriLimits {
                max_output_utf16: 1,
                ..NoriLimits::default()
            },
        ),
        (
            "100",
            NoriLimits {
                max_input_utf16: 1,
                ..NoriLimits::default()
            },
        ),
    ] {
        assert!(matches!(
            normalize_number_utf16(
                &source.encode_utf16().collect::<Vec<_>>(),
                limits,
                &mut || Ok(())
            ),
            Err(AnalysisError::Dictionary(DictionaryError::Limit { .. }))
        ));
    }
    assert_eq!(
        normalize_number_utf16(&input, NoriLimits::default(), &mut || Ok(())).unwrap(),
        expected
    );
    assert_eq!(normalize_number("３．２천").unwrap(), "3200");
    assert_eq!(normalize_number("1.2.3").unwrap(), "1.2.3");
}

#[test]
fn numeric_filter_bounds_cancellation_and_reuse_preserve_failure_atomicity() {
    use uqa_analysis::nori::{DictionaryError, KoreanTokenizer};
    use uqa_analysis::AnalysisError;
    let tokenizer = KoreanTokenizer::new(model().clone(), None, NoriOptions::default()).unwrap();
    let mut source = tokenizer.tokenize("a").unwrap().tokens.remove(0);
    source.term_utf16 = vec![u16::from(b'1'); 8192];
    source.start_utf16 = 1;
    source.end_utf16 = 8193;
    let output = NoriOutput::from_tokens(vec![source], 8193, 0);
    let number = KoreanFilter::Number;
    let expected = number.apply(output.clone(), model()).unwrap();
    let mut calls = 0;
    assert!(matches!(
        number.apply_controlled(output.clone(), model(), NoriLimits::default(), &mut || {
            calls += 1;
            if calls == 8 {
                Err(AnalysisError::Cancelled)
            } else {
                Ok(())
            }
        }),
        Err(AnalysisError::Cancelled)
    ));
    for limits in [
        NoriLimits {
            max_tokens: 0,
            ..NoriLimits::default()
        },
        NoriLimits {
            max_output_utf16: 1,
            ..NoriLimits::default()
        },
    ] {
        assert!(matches!(
            number.apply_controlled(output.clone(), model(), limits, &mut || Ok(())),
            Err(AnalysisError::Dictionary(DictionaryError::Limit { .. }))
        ));
    }
    assert_eq!(number.apply(output, model()).unwrap(), expected);
    assert!(
        serde_json::from_str::<KoreanFilter>(r#"{"type":"nori_number","extra":true}"#).is_err()
    );
}

#[test]
fn original_number_examples_preserve_positions_offsets_and_metadata() {
    let mut checked = 0;
    for line in include_str!("../../../../tests/parity/nori/expected.jsonl").lines() {
        let expected: Value = serde_json::from_str(line).unwrap();
        if expected["pipeline"] != "number" {
            continue;
        }
        let options = NoriOptions {
            decompound_mode: serde_json::from_value(expected["decompound_mode"].clone()).unwrap(),
            output_unknown_unigrams: expected["output_unknown_unigrams"].as_bool().unwrap(),
            discard_punctuation: expected["discard_punctuation"].as_bool().unwrap(),
        };
        let analyzer =
            KoreanAnalyzer::with_filters(model().clone(), None, options, &[KoreanFilter::Number])
                .unwrap();
        let output = analyzer
            .analyze(expected["input"].as_str().unwrap())
            .unwrap();
        assert_eq!(
            super::nori_resources::string_tokens(&output),
            expected["tokens"],
            "{}",
            expected["id"]
        );
        assert_eq!(
            json!(output.final_offset_utf16),
            expected["final_offset_utf16"]
        );
        assert_eq!(
            json!(output.final_position_increment),
            expected["final_position_increment"]
        );
        checked += 1;
    }
    assert_eq!(checked, 4);
}
