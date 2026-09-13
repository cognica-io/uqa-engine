//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use sha2::{Digest, Sha256};
use uqa_analysis::AnalyzedText;

use super::*;

fn snapshot(output: &AnalyzedText, keyword: bool) -> Value {
    let tokens: Vec<_> = output.tokens().iter().map(|token| {
        let span = token.filtered_utf16().unwrap();
        let morphology = token.korean_morphology().unwrap();
        let mut value = json!({
            "term_utf16": token.term().utf16(), "start_utf16": span.start, "end_utf16": span.end,
            "position_increment": token.position_increment(), "position_length": token.position_length(),
            "pos_type": morphology.pos_type, "left_pos": morphology.left_pos, "right_pos": morphology.right_pos,
            "reading_utf16": morphology.reading.as_ref().map(|text| text.encode_utf16().collect::<Vec<_>>()),
            "morphemes": morphology.morphemes,
        });
        if keyword { value["keyword"] = json!(token.is_keyword()); }
        value
    }).collect();
    json!({"tokens":tokens, "final_offset_utf16":output.final_offsets().utf16.end, "final_position_increment":output.final_position_increment()})
}

#[test]
fn compiled_and_restored_korean_pipelines_match_all_text_reference_streams() {
    let resources = AnalyzerResources::new(AnalyzerLimits::default());
    let restored_resources = AnalyzerResources::new(AnalyzerLimits::default());
    let mut checked = 0;
    let mut normalized = 0;
    for (case_bytes, expected_bytes, keyword) in [
        (
            include_str!("../../../../../tests/parity/nori/tokenizer_cases.json"),
            include_str!("../../../../../tests/parity/nori/tokenizer_expected.jsonl"),
            false,
        ),
        (
            include_str!("../../../../../tests/parity/nori/analysis_cases.json"),
            include_str!("../../../../../tests/parity/nori/analysis_expected.jsonl"),
            false,
        ),
        (
            include_str!("../../../../../tests/parity/nori/number_cases.json"),
            include_str!("../../../../../tests/parity/nori/number_expected.jsonl"),
            true,
        ),
    ] {
        let cases: Vec<Value> = serde_json::from_str(case_bytes).unwrap();
        let expected: Vec<Value> = expected_bytes
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(cases.len(), expected.len());
        for (case, expected) in cases.iter().zip(expected) {
            if case["pipeline"] == "unicode_lowercase"
                || (keyword && case["pipeline"] != "tokenizer")
            {
                continue;
            }
            assert_eq!(case["id"], expected["id"]);
            let input = case["input"].as_str().unwrap();
            let mut config = nori_analyzer();
            config.tokenizer = Tokenizer::Nori(NoriTokenizerConfig {
                dictionary: exact_dictionary(),
                decompound_mode: serde_json::from_value(
                    case.get("decompound_mode")
                        .cloned()
                        .unwrap_or(json!("none")),
                )
                .unwrap(),
                output_unknown_unigrams: case["output_unknown_unigrams"].as_bool().unwrap_or(false),
                discard_punctuation: case["discard_punctuation"].as_bool().unwrap_or(false),
                user_dictionary: case["user_dictionary"].as_str().map(str::to_owned),
            });
            if case["pipeline"] == "filters" || keyword {
                config.token_filters = serde_json::from_value(case["filters"].clone()).unwrap();
            } else if case["pipeline"].is_null() {
                config.token_filters.clear();
            }
            let compiled = resources.compile(&config);
            if expected.get("error").is_some() {
                assert!(compiled.is_err(), "{}", case["id"]);
                continue;
            }
            let compiled = compiled.unwrap_or_else(|error| panic!("{}: {error}", case["id"]));
            let restored = restored_resources
                .restore_json(compiled.descriptor().canonical_json())
                .unwrap();
            if case["pipeline"] == "normalize" {
                let output = compiled.normalize(input).unwrap();
                assert_eq!(
                    json!(output.encode_utf16().collect::<Vec<_>>()),
                    expected["normalized_utf16"],
                    "{}",
                    case["id"]
                );
                assert_eq!(restored.normalize(input).unwrap(), output);
                normalized += 1;
                continue;
            }
            let output = compiled.analyze_tokens(input).unwrap();
            assert_eq!(
                restored.analyze_tokens(input).unwrap(),
                output,
                "{}",
                case["id"]
            );
            let value = snapshot(&output, keyword);
            if let Some(expected) = expected.get("analysis") {
                assert_eq!(&value, expected, "{}", case["id"]);
            }
            let mut bytes = Vec::new();
            super::super::nori_resources::canonical(&value, &mut bytes);
            assert_eq!(
                format!("{:x}", Sha256::digest(bytes)),
                expected["sha256"].as_str().unwrap(),
                "{}",
                case["id"]
            );
            assert_eq!(json!(output.tokens().len()), expected["token_count"]);
            checked += 1;
        }
    }
    assert_eq!(checked, 803);
    assert_eq!(normalized, 14);
}

#[test]
fn compiled_korean_filters_preserve_existing_generic_tokenizer_contracts() {
    let cases: Vec<Value> = serde_json::from_str(include_str!(
        "../../../../../tests/parity/nori/generic_cases.json"
    ))
    .unwrap();
    let resources = AnalyzerResources::new(AnalyzerLimits::default());
    let mut checked = 0;
    for case in cases {
        let tokenizer = match case["pipeline"].as_str().unwrap() {
            "keyword" => Tokenizer::Keyword,
            "whitespace" => Tokenizer::Whitespace,
            _ => continue,
        };
        let input = case["input"].as_str().unwrap();
        let filters: Vec<KoreanFilter> = serde_json::from_value(case["filters"].clone()).unwrap();
        let mut expected = tokenizer.tokenize_with_offsets(input).unwrap();
        for filter in filters {
            expected = filter.filter_analyzed(expected, model()).unwrap();
        }
        let config = Analyzer::new(
            tokenizer,
            serde_json::from_value(case["filters"].clone()).unwrap(),
            Vec::new(),
        );
        let compiled = resources.compile(&config).unwrap();
        assert_eq!(
            compiled.analyze_tokens(input).unwrap(),
            expected,
            "{}",
            case["id"]
        );
        assert_eq!(
            config.analyze_tokens(input).unwrap(),
            expected,
            "{}",
            case["id"]
        );
        checked += 1;
    }
    assert_eq!(checked, 90);
}
