//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uqa_core::memory::MemoryError;

use super::*;
use crate::kuromoji::{DictionaryError, KuromojiResources, UserDictionaryLimits};

fn model() -> Arc<KuromojiDictionary> {
    KuromojiResources::default()
        .load_default()
        .unwrap()
        .model()
        .clone()
}

fn units(text: Option<&str>) -> Option<Vec<u16>> {
    text.map(|text| text.encode_utf16().collect())
}

fn raw_analysis(output: &KuromojiOutput) -> Value {
    let tokens: Vec<_> = output.tokens.iter().map(|token| json!({
        "term_utf16": token.term_utf16, "start_utf16": token.start_utf16, "end_utf16": token.end_utf16,
        "position_increment": token.position_increment, "position_length": token.position_length, "keyword": token.keyword,
        "part_of_speech_utf16": units(token.part_of_speech.as_deref()), "base_form_utf16": units(token.base_form.as_deref()),
        "reading_utf16": units(token.reading.as_deref()), "pronunciation_utf16": units(token.pronunciation.as_deref()),
        "inflection_type_utf16": units(token.inflection_type.as_deref()), "inflection_form_utf16": units(token.inflection_form.as_deref()),
    })).collect();
    let mut value = json!({"tokens": tokens, "final_offset_utf16": output.final_offset_utf16, "final_position_increment": output.final_position_increment});
    value.sort_all_objects();
    value
}

#[test]
fn japanese_tokenizer_matches_pinned_modes_graphs_attributes_and_raw_units() {
    let case_bytes = include_bytes!("../../../../../tests/parity/kuromoji/tokenizer_cases.json");
    let expected_bytes =
        include_bytes!("../../../../../tests/parity/kuromoji/tokenizer_expected.jsonl");
    let manifest: Value = serde_json::from_str(include_str!(
        "../../../../../tests/parity/kuromoji/tokenizer_manifest.json"
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
    let model = model();
    for (case, expected) in cases.into_iter().zip(expected) {
        assert_eq!(case["id"], expected["id"]);
        let id = case["id"].as_str().unwrap();
        let options = KuromojiOptions {
            mode: serde_json::from_value(case["mode"].clone()).unwrap(),
            discard_punctuation: case["discard_punctuation"].as_bool().unwrap(),
            discard_compound_token: case["discard_compound_token"].as_bool().unwrap(),
        };
        let input = if let Some(raw) = case.get("input_utf16") {
            serde_json::from_value(raw.clone()).unwrap()
        } else {
            case["input"]
                .as_str()
                .unwrap()
                .repeat(case["repeat"].as_u64().unwrap_or(1) as usize)
                .encode_utf16()
                .collect::<Vec<_>>()
        };
        let user = case["user_dictionary"]
            .as_str()
            .map(|source| UserDictionary::compile(source, &model, UserDictionaryLimits::default()))
            .transpose();
        if expected["error_stage"] == "compile" {
            assert!(user.is_err(), "{id}");
            continue;
        }
        let tokenizer =
            JapaneseTokenizer::new(model.clone(), user.unwrap().flatten(), options).unwrap();
        let actual = tokenizer.tokenize_utf16(&input, KuromojiLimits::default(), &mut || Ok(()));
        if expected.get("error").is_some() {
            assert!(
                matches!(actual, Err(AnalysisError::KuromojiDictionary(_))),
                "{id}: {actual:?}"
            );
            continue;
        }
        let output = actual.unwrap_or_else(|error| panic!("{id}: {error}"));
        let analysis = raw_analysis(&output);
        if let Some(expected) = expected.get("analysis") {
            assert_eq!(analysis, *expected, "{id}");
        }
        assert_eq!(json!(output.tokens.len()), expected["token_count"], "{id}");
        assert_eq!(
            format!(
                "{:x}",
                Sha256::digest(serde_json::to_vec(&analysis).unwrap())
            ),
            expected["sha256"].as_str().unwrap(),
            "{id}"
        );
    }
}

fn tokenizer() -> JapaneseTokenizer {
    JapaneseTokenizer::new(
        model(),
        None,
        KuromojiOptions {
            discard_compound_token: false,
            ..KuromojiOptions::default()
        },
    )
    .unwrap()
}

#[test]
fn japanese_user_failures_preserve_selected_models_and_release_partial_output() {
    let model = model();
    let user = UserDictionary::compile(
        "東京,東京,トウキョウ,名詞",
        &model,
        UserDictionaryLimits::default(),
    )
    .unwrap();
    let valid =
        JapaneseTokenizer::new(model.clone(), user.clone(), KuromojiOptions::default()).unwrap();
    let other = KuromojiDictionary::from_bytes(
        &crate::kuromoji::tests::fixtures::bundle(),
        super::super::DictionaryLimits::default(),
    )
    .unwrap();
    assert!(matches!(
        JapaneseTokenizer::new(other, user, KuromojiOptions::default()),
        Err(AnalysisError::KuromojiDictionary(
            DictionaryError::Invalid { .. }
        ))
    ));
    let malformed = UserDictionary::compile(
        "東京,東京,トウキョウ,",
        &model,
        UserDictionaryLimits::default(),
    )
    .unwrap();
    let invalid = JapaneseTokenizer::new(model, malformed, KuromojiOptions::default()).unwrap();
    let budget = MemoryBudget::new(usize::MAX);
    let held = budget.reserve(7).unwrap();
    assert!(matches!(
        invalid.tokenize_budgeted(
            "大阪 東京",
            KuromojiLimits::default(),
            &budget,
            &mut || Ok(())
        ),
        Err(AnalysisError::KuromojiDictionary(
            DictionaryError::Invalid { .. }
        ))
    ));
    assert_eq!(budget.used(), 7);
    let output = valid.tokenize("東京").unwrap();
    assert_eq!(output.tokens[0].origin, KuromojiOrigin::User);
    assert_eq!(output.tokens[0].reading.as_deref(), Some("トウキョウ"));
    drop(held);
}

fn retained_bytes(output: &KuromojiOutput) -> usize {
    output.tokens.capacity() * size_of::<KuromojiToken>()
        + output
            .tokens
            .iter()
            .map(|token| {
                token.term_utf16.capacity() * size_of::<u16>()
                    + [
                        &token.part_of_speech,
                        &token.base_form,
                        &token.reading,
                        &token.pronunciation,
                        &token.inflection_type,
                        &token.inflection_form,
                    ]
                    .into_iter()
                    .flatten()
                    .map(String::capacity)
                    .sum::<usize>()
            })
            .sum::<usize>()
}

#[test]
fn japanese_resegmentation_and_output_retain_exact_reservations() {
    let tokenizer = tokenizer();
    let input = "関西国際空港に行きました。";
    let baseline = MemoryBudget::new(usize::MAX);
    let output = tokenizer
        .tokenize_budgeted(input, KuromojiLimits::default(), &baseline, &mut || Ok(()))
        .unwrap();
    assert!(output.tokens.iter().any(|token| token.position_length > 1));
    assert!(output.tokens.iter().any(|token| token.base_form.is_some()));
    assert_eq!(output.reserved_bytes(), retained_bytes(&output));
    assert_eq!(baseline.used(), output.reserved_bytes());
    let peak = baseline.peak();
    assert!(peak > baseline.used());
    for limit in [
        0,
        1,
        input.encode_utf16().count() * 2 - 1,
        output.reserved_bytes() - 1,
        peak - 1,
    ] {
        let budget = MemoryBudget::new(limit + 7);
        let held = budget.reserve(7).unwrap();
        let result =
            tokenizer.tokenize_budgeted(input, KuromojiLimits::default(), &budget, &mut || Ok(()));
        assert!(
            matches!(
                result,
                Err(AnalysisError::Memory(MemoryError::Limit { .. }))
            ),
            "{limit}: {result:?}"
        );
        assert_eq!(budget.used(), 7);
        drop(held);
        assert_eq!(budget.used(), 0);
    }
    let budget = MemoryBudget::new(peak);
    let actual = tokenizer
        .tokenize_budgeted(input, KuromojiLimits::default(), &budget, &mut || Ok(()))
        .unwrap();
    assert_eq!(*output, *actual);
    drop(actual);
    assert_eq!(budget.used(), 0);
    drop(output);
    assert_eq!(baseline.used(), 0);
}

#[test]
fn japanese_limits_and_cancellation_preserve_other_owners_and_allow_reuse() {
    let tokenizer = tokenizer();
    let input = "関西国際空港に行きました。".repeat(24);
    let defaults = KuromojiLimits::default();
    let budget = MemoryBudget::new(usize::MAX);
    let held = budget.reserve(7).unwrap();
    for limits in [
        KuromojiLimits {
            max_input_utf16: 1,
            ..defaults
        },
        KuromojiLimits {
            max_lattice_positions: 1,
            ..defaults
        },
        KuromojiLimits {
            max_lattice_candidates: 1,
            ..defaults
        },
        KuromojiLimits {
            max_tokens: 0,
            ..defaults
        },
        KuromojiLimits {
            max_output_utf16: 1,
            ..defaults
        },
        KuromojiLimits {
            max_resegmentation_arcs: 0,
            ..defaults
        },
        KuromojiLimits {
            max_resegmentation_work: 0,
            ..defaults
        },
    ] {
        let result = tokenizer.tokenize_budgeted(&input, limits, &budget, &mut || Ok(()));
        assert!(
            matches!(
                result,
                Err(AnalysisError::KuromojiDictionary(
                    DictionaryError::Limit { .. }
                ))
            ),
            "{limits:?}: {result:?}"
        );
        assert_eq!(budget.used(), 7);
    }
    let mut polls = 0;
    let expected = tokenizer
        .tokenize_budgeted(&input, defaults, &budget, &mut || {
            polls += 1;
            Ok(())
        })
        .unwrap();
    let retained = budget.used();
    for stop in [1, 2, 4, 8, polls / 3, polls / 2, polls - 1, polls] {
        let mut calls = 0;
        let result = tokenizer.tokenize_budgeted(&input, defaults, &budget, &mut || {
            calls += 1;
            if calls == stop {
                Err(AnalysisError::Cancelled)
            } else {
                Ok(())
            }
        });
        assert!(
            matches!(result, Err(AnalysisError::Cancelled)),
            "poll {stop}: {result:?}"
        );
        assert_eq!(calls, stop);
        assert_eq!(budget.used(), retained);
    }
    let actual = tokenizer
        .tokenize_budgeted(&input, defaults, &budget, &mut || Ok(()))
        .unwrap();
    assert_eq!(*actual, *expected);
    drop(actual);
    drop(expected);
    assert_eq!(budget.used(), 7);
    drop(held);
}
