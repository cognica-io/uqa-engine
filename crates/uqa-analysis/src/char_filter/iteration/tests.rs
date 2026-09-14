//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::{AnalysisError, Analyzer, CharFilter, Tokenizer};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use uqa_core::memory::MemoryBudget;

fn input(case: &Value) -> String {
    if case["kind"] == "matrix" {
        let mut input = String::new();
        for scalar in (0..65536).filter_map(char::from_u32) {
            for mark in ['々', 'ゝ', 'ゞ', 'ヽ', 'ヾ'] {
                input.extend([scalar, mark, '。']);
            }
        }
        input
    } else if let Some(length) = case["span_size"].as_u64() {
        format!(
            "{}{}",
            "a".repeat(length as usize),
            "々".repeat(length as usize + 3)
        )
    } else {
        case["input"]
            .as_str()
            .unwrap()
            .repeat(case["repeat"].as_u64().unwrap_or(1) as usize)
    }
}
fn integer(hash: &mut Sha256, value: usize) {
    hash.update(u32::try_from(value).unwrap().to_be_bytes());
}
fn text(hash: &mut Sha256, value: &str) {
    integer(hash, value.encode_utf16().count());
    for unit in value.encode_utf16() {
        hash.update(unit.to_be_bytes());
    }
}
fn filter() -> CharFilter {
    CharFilter::KuromojiIterationMark {
        normalize_kanji: true,
        normalize_kana: true,
    }
}

#[test]
fn iteration_marks_match_complete_lucene_text_and_every_corrected_offset() {
    let cases_bytes = include_bytes!("../../../../../tests/parity/kuromoji/iteration_cases.json");
    let expected_bytes =
        include_bytes!("../../../../../tests/parity/kuromoji/iteration_expected.jsonl");
    let manifest: Value = serde_json::from_str(include_str!(
        "../../../../../tests/parity/kuromoji/iteration_manifest.json"
    ))
    .unwrap();
    assert_eq!(
        format!("{:x}", Sha256::digest(cases_bytes)),
        manifest["cases_sha256"]
    );
    assert_eq!(
        format!("{:x}", Sha256::digest(expected_bytes)),
        manifest["expected_sha256"]
    );
    let cases: Vec<Value> = serde_json::from_slice(cases_bytes).unwrap();
    let expected: Vec<Value> = std::str::from_utf8(expected_bytes)
        .unwrap()
        .lines()
        .map(|row| serde_json::from_str(row).unwrap())
        .collect();
    assert_eq!(cases.len(), expected.len());
    assert_eq!(json!(cases.len()), manifest["fixture_count"]);
    for (case, expected) in cases.iter().zip(expected) {
        let id = case["id"].as_str().unwrap();
        assert_eq!(case["id"], expected["id"]);
        let input = input(case);
        let filter = CharFilter::KuromojiIterationMark {
            normalize_kanji: case["normalize_kanji"].as_bool().unwrap_or(true),
            normalize_kana: case["normalize_kana"].as_bool().unwrap_or(true),
        };
        let output = filter.filter_with_offsets(&input).unwrap();
        let length = output.as_str().encode_utf16().count();
        assert_eq!(
            json!(input.encode_utf16().count()),
            expected["input_unit_count"],
            "{id}"
        );
        assert_eq!(json!(length), expected["output_unit_count"], "{id}");
        if let Some(units) = expected.get("output_utf16") {
            assert_eq!(
                json!(output.as_str().encode_utf16().collect::<Vec<_>>()),
                *units,
                "{id}"
            );
        }
        let mut hash = Sha256::new();
        text(&mut hash, &input);
        text(&mut hash, output.as_str());
        integer(&mut hash, length + 1);
        for offset in 0..=length {
            let corrected = output
                .source_covering_offsets_utf16(offset..offset)
                .unwrap();
            integer(&mut hash, corrected.utf16.start);
            if let Some(offsets) = expected.get("offsets") {
                assert_eq!(
                    json!(corrected.utf16.start),
                    offsets[offset],
                    "{id}: {offset}"
                );
            }
        }
        assert_eq!(format!("{:x}", hash.finalize()), expected["sha256"], "{id}");
    }
}

#[test]
fn iteration_marks_keep_mapped_source_and_immutable_compiled_configuration() {
    let config: CharFilter = serde_json::from_str(r#"{"type":"kuromoji_iteration_mark"}"#).unwrap();
    assert_eq!(config.filter("時々 なゝ").unwrap(), "時時 など");
    assert_eq!(
        serde_json::to_value(config).unwrap(),
        json!({"type":"kuromoji_iteration_mark","normalize_kanji":true,"normalize_kana":true})
    );
    let source_budget = MemoryBudget::new(1 << 20);
    let source = CharFilter::HTMLStrip
        .filter_with_offsets_budgeted("<b>?ゝ</b>", &source_budget, &mut || Ok(()))
        .unwrap();
    let mutation = MemoryBudget::new(1 << 20);
    let output = filter()
        .filter_mapped_budgeted(source, &mutation, &mut || Ok(()))
        .unwrap();
    assert_eq!(output.as_str(), " ?? ");
    assert_eq!(output.source_offsets(2..3).unwrap().utf8, 4..7);
    assert_eq!(output.source_offsets(2..3).unwrap().utf16, 4..5);
    assert_eq!(output.final_offsets().utf16.end, 9);
    assert!(source_budget.used() > 0);
    let retained = output.clone();
    drop(output);
    assert!(mutation.used() > 0);
    drop(retained);
    assert_eq!(mutation.used(), 0);
    assert_eq!(source_budget.used(), 0);
    let config = Analyzer::new(
        Tokenizer::Keyword,
        Vec::new(),
        vec![
            CharFilter::Mapping {
                mapping: BTreeMap::from([("X".into(), "ab々々".into())]),
            },
            filter(),
        ],
    );
    let plain = config.analyze_tokens("X").unwrap();
    let compiled = config.compile().unwrap().analyze_tokens("X").unwrap();
    assert_eq!(compiled, plain);
    assert_eq!(compiled.tokens()[0].term(), "abab");
    assert_eq!(compiled.tokens()[0].offsets().unwrap().utf8, 0..1);
    assert_eq!(compiled.tokens()[0].offsets().unwrap().utf16, 0..1);
    let plain_json = serde_json::to_string(&config).unwrap();
    let decoded: Analyzer = serde_json::from_str(&plain_json).unwrap();
    assert_eq!(decoded.analyze_tokens("X").unwrap(), plain);
}

#[test]
fn iteration_lookahead_edits_and_coordinates_unwind_without_releasing_other_owners() {
    let input = "東京々々。ながゝゝ?ゝ🙂々";
    let baseline = MemoryBudget::new(usize::MAX);
    let mut polls = 0;
    let output = filter()
        .filter_with_offsets_budgeted(input, &baseline, &mut || {
            polls += 1;
            Ok(())
        })
        .unwrap();
    let expected = output.as_str().to_owned();
    let peak = baseline.peak();
    drop(output);
    assert_eq!(baseline.used(), 0);
    for stop in 1..=polls {
        let budget = MemoryBudget::new(peak + 7);
        let other = budget.reserve(7).unwrap();
        let mut count = 0;
        let result = filter().filter_with_offsets_budgeted(input, &budget, &mut || {
            count += 1;
            if count == stop {
                Err(AnalysisError::Cancelled)
            } else {
                Ok(())
            }
        });
        assert!(
            matches!(result, Err(AnalysisError::Cancelled)),
            "poll {stop}/{polls}"
        );
        assert_eq!(count, stop);
        assert_eq!(budget.used(), 7);
        drop(other);
    }
    for allowance in [0, 1, peak / 2, peak - 1, peak] {
        let budget = MemoryBudget::new(allowance + 7);
        let other = budget.reserve(7).unwrap();
        match filter().filter_with_offsets_budgeted(input, &budget, &mut || Ok(())) {
            Ok(actual) => assert_eq!(actual.as_str(), expected),
            Err(AnalysisError::Memory(_)) => assert!(allowance < peak),
            result => panic!("allowance {allowance}: {result:?}"),
        }
        assert!(budget.peak() <= budget.limit());
        assert_eq!(budget.used(), 7);
        drop(other);
    }
    assert_eq!(filter().filter(input).unwrap(), expected);
}

#[test]
fn iteration_settings_survive_canonical_descriptors_with_distinct_identities() {
    use crate::{AnalyzerDescriptor, AnalyzerLimits, TokenLengthPolicy};
    let mut identities = std::collections::BTreeSet::new();
    for normalize_kanji in [false, true] {
        for normalize_kana in [false, true] {
            let config = Analyzer::new(
                Tokenizer::Keyword,
                Vec::new(),
                vec![CharFilter::KuromojiIterationMark {
                    normalize_kanji,
                    normalize_kana,
                }],
            );
            let descriptor = AnalyzerDescriptor::resolve(
                &config,
                TokenLengthPolicy::EmittedTokens,
                AnalyzerLimits::default(),
            )
            .unwrap();
            let restored = AnalyzerDescriptor::from_json(
                descriptor.canonical_json(),
                AnalyzerLimits::default(),
            )
            .unwrap();
            assert_eq!(restored.canonical_json(), descriptor.canonical_json());
            assert_eq!(restored.fingerprint(), descriptor.fingerprint());
            assert_eq!(
                restored
                    .configuration()
                    .unwrap()
                    .analyze_tokens("時々 なゝ")
                    .unwrap(),
                config.analyze_tokens("時々 なゝ").unwrap()
            );
            assert!(identities.insert(descriptor.fingerprint().to_string()));
        }
    }
}
