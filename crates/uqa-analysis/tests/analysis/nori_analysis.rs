//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Independent filter, full-analyzer, and normalization reference comparisons.

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uqa_analysis::nori::{
    KoreanAnalyzer, KoreanFilter, KoreanTokenizer, NoriLimits, NoriOptions, NoriOutput, NoriToken,
    POSTag, UserDictionary, UserDictionaryLimits,
};
use uqa_analysis::{AnalysisError, AnalysisResult};

use super::nori_resources::{canonical, model, raw_analysis};

fn unicode_input() -> Vec<u16> {
    let mut input = Vec::new();
    for point in 0..=0x0010_ffff {
        if let Some(scalar) = char::from_u32(point) {
            input.extend_from_slice(scalar.encode_utf16(&mut [0; 2]));
        }
    }
    for unit in 0xd800..=0xdfff {
        input.extend([unit, u16::from(b'!')]);
    }
    input
}

#[test]
fn full_analyzer_matches_the_original_design_examples() {
    let mut checked = 0;
    for line in include_str!("../../../../tests/parity/nori/expected.jsonl").lines() {
        let expected: Value = serde_json::from_str(line).unwrap();
        if expected["pipeline"] != "analyzer" {
            continue;
        }
        let options = NoriOptions {
            decompound_mode: serde_json::from_value(expected["decompound_mode"].clone()).unwrap(),
            output_unknown_unigrams: expected["output_unknown_unigrams"].as_bool().unwrap(),
            discard_punctuation: expected["discard_punctuation"].as_bool().unwrap(),
        };
        let analyzer = KoreanAnalyzer::new(model().clone(), None, options).unwrap();
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
    assert_eq!(checked, 8);
}

#[test]
fn filters_analyzer_and_normalization_match_every_docker_attribute() {
    let cases_bytes = include_bytes!("../../../../tests/parity/nori/analysis_cases.json");
    let expected_bytes = include_bytes!("../../../../tests/parity/nori/analysis_expected.jsonl");
    let manifest: Value = serde_json::from_str(include_str!(
        "../../../../tests/parity/nori/analysis_manifest.json"
    ))
    .unwrap();
    assert_eq!(
        manifest["cases_sha256"],
        format!("{:x}", Sha256::digest(cases_bytes))
    );
    assert_eq!(
        manifest["expected_sha256"],
        format!("{:x}", Sha256::digest(expected_bytes))
    );
    let cases: Vec<Value> = serde_json::from_slice(cases_bytes).unwrap();
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
        let make = || -> AnalysisResult<KoreanAnalyzer> {
            let user = case["user_dictionary"]
                .as_str()
                .map(|text| UserDictionary::compile(text, model(), UserDictionaryLimits::default()))
                .transpose()?
                .flatten();
            if case["pipeline"] == "filters" {
                let filters: Vec<KoreanFilter> =
                    serde_json::from_value(case["filters"].clone()).unwrap();
                KoreanAnalyzer::with_filters(model().clone(), user, options, &filters)
            } else {
                KoreanAnalyzer::new(model().clone(), user, options)
            }
        };
        let analyzer = make();
        if expected.get("error").is_some() {
            assert!(
                matches!(analyzer, Err(AnalysisError::Dictionary(_))),
                "{id}"
            );
            continue;
        }
        let analyzer = analyzer.unwrap();
        if case["pipeline"] == "unicode_lowercase" {
            let normalized = analyzer
                .normalize_utf16(&unicode_input(), NoriLimits::default(), &mut || Ok(()))
                .unwrap();
            let mut digest = Sha256::new();
            for unit in &normalized {
                digest.update(unit.to_be_bytes());
            }
            assert_eq!(json!(normalized.len()), expected["unit_count"]);
            assert_eq!(
                format!("{:x}", digest.finalize()),
                expected["utf16be_sha256"].as_str().unwrap()
            );
            continue;
        }
        let input = case["input"].as_str().unwrap();
        if case["pipeline"] == "normalize" {
            let normalized = analyzer.normalize(input).unwrap();
            assert_eq!(
                json!(normalized.encode_utf16().collect::<Vec<_>>()),
                expected["normalized_utf16"],
                "{id}"
            );
            continue;
        }
        let output = analyzer
            .analyze(input)
            .unwrap_or_else(|error| panic!("{id}: {error}"));
        let analysis = raw_analysis(&output);
        if let Some(expected) = expected.get("analysis") {
            assert_eq!(analysis, *expected, "{id}");
        }
        let mut bytes = Vec::new();
        canonical(&analysis, &mut bytes);
        assert_eq!(json!(output.tokens.len()), expected["token_count"], "{id}");
        assert_eq!(
            format!("{:x}", Sha256::digest(bytes)),
            expected["sha256"].as_str().unwrap(),
            "{id}"
        );
    }
}

fn token() -> NoriToken {
    KoreanTokenizer::new(model().clone(), None, NoriOptions::default())
        .unwrap()
        .tokenize("한국")
        .unwrap()
        .tokens
        .remove(0)
}

fn stream(tokens: Vec<NoriToken>, final_increment: u32) -> NoriOutput {
    NoriOutput {
        tokens,
        final_offset_utf16: 20,
        final_position_increment: final_increment,
    }
}

#[test]
fn filters_preserve_nullable_metadata_stacked_edges_and_trailing_holes() {
    let mut original = token();
    original.term_utf16 = "UQA".encode_utf16().collect();
    original.position_length = 3;
    original.reading = Some(String::new());
    let output = KoreanFilter::ReadingForm
        .apply(stream(vec![original.clone()], 7), model())
        .unwrap();
    let mut expected = original.clone();
    expected.term_utf16.clear();
    assert_eq!(output, stream(vec![expected], 7));
    original.reading = None;
    assert_eq!(
        KoreanFilter::ReadingForm
            .apply(stream(vec![original.clone()], 0), model())
            .unwrap(),
        stream(vec![original.clone()], 0)
    );
    expected = original.clone();
    expected.term_utf16 = "uqa".encode_utf16().collect();
    assert_eq!(
        KoreanFilter::SimpleLowercase
            .apply(stream(vec![original.clone()], 7), model())
            .unwrap(),
        stream(vec![expected], 7)
    );

    let mut removed = original.clone();
    removed.left_pos = POSTag::JX;
    removed.right_pos = POSTag::NNG;
    removed.position_increment = 2;
    original.left_pos = POSTag::NNG;
    original.right_pos = POSTag::JX;
    original.position_increment = 0;
    let input = stream(vec![removed.clone(), original.clone(), removed], 4);
    let keep_all = KoreanFilter::PartOfSpeech {
        stop_tags: Some(vec![]),
    };
    assert_eq!(keep_all.apply(input.clone(), model()).unwrap(), input);
    let stop = KoreanFilter::PartOfSpeech { stop_tags: None };
    original.position_increment = 2;
    assert_eq!(
        stop.apply(input, model()).unwrap(),
        stream(vec![original], 6)
    );
}

#[test]
fn controlled_filters_and_normalization_reject_limits_overflow_and_cancellation() {
    let mut original = token();
    original.reading = Some("İUQA".repeat(4096));
    let input = stream(vec![original], 0);
    for filter in [KoreanFilter::ReadingForm, KoreanFilter::SimpleLowercase] {
        let bounded = NoriLimits {
            max_output_utf16: 1,
            ..NoriLimits::default()
        };
        assert!(filter
            .apply_controlled(input.clone(), model(), bounded, &mut || Ok(()))
            .is_err());
        assert!(matches!(
            filter.apply_controlled(input.clone(), model(), NoriLimits::default(), &mut || Err(
                AnalysisError::Cancelled
            )),
            Err(AnalysisError::Cancelled)
        ));
    }
    let mut polls = 0;
    assert!(matches!(
        KoreanFilter::ReadingForm.apply_controlled(
            input,
            model(),
            NoriLimits::default(),
            &mut || {
                polls += 1;
                if polls == 3 {
                    Err(AnalysisError::Cancelled)
                } else {
                    Ok(())
                }
            }
        ),
        Err(AnalysisError::Cancelled)
    ));
    let mut removed = token();
    removed.left_pos = POSTag::JX;
    removed.position_increment = u32::MAX;
    assert!(matches!(
        KoreanFilter::PartOfSpeech { stop_tags: None }
            .apply(stream(vec![removed, token()], 0), model()),
        Err(AnalysisError::TokenPositionOverflow)
    ));
    let analyzer = KoreanAnalyzer::new(model().clone(), None, NoriOptions::default()).unwrap();
    let text = "İUQA".repeat(4096);
    let bounded = NoriLimits {
        max_output_utf16: 1,
        ..NoriLimits::default()
    };
    assert!(analyzer
        .normalize_controlled(&text, bounded, &mut || Ok(()))
        .is_err());
    let mut polls = 0;
    assert!(matches!(
        analyzer.normalize_utf16(
            &text.encode_utf16().collect::<Vec<_>>(),
            NoriLimits::default(),
            &mut || {
                polls += 1;
                if polls == 3 {
                    Err(AnalysisError::Cancelled)
                } else {
                    Ok(())
                }
            }
        ),
        Err(AnalysisError::Cancelled)
    ));
    assert_eq!(
        analyzer.normalize("喜悲哀歡 İ UQA").unwrap(),
        "喜悲哀歡 i uqa"
    );
    for invalid in [
        r#"{"type":"nori_part_of_speech","stop_tags":["nng"]}"#,
        r#"{"type":"nori_readingform","extra":true}"#,
    ] {
        assert!(serde_json::from_str::<KoreanFilter>(invalid).is_err());
    }
}
