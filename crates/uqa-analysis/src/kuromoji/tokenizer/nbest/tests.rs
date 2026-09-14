//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uqa_core::memory::{MemoryBudget, MemoryError};

use super::super::tests::{model, raw_analysis, retained_bytes};
use super::super::{JapaneseTokenizer, KuromojiLimits, KuromojiOptions, KuromojiOutput};
use crate::kuromoji::{UserDictionary, UserDictionaryLimits};
use crate::AnalysisError;

#[test]
fn nbest_costs_examples_ordered_attributes_and_graphs_match_the_docker_reference() {
    let case_bytes = include_bytes!("../../../../../../tests/parity/kuromoji/nbest_cases.json");
    let expected_bytes =
        include_bytes!("../../../../../../tests/parity/kuromoji/nbest_expected.jsonl");
    let manifest: Value = serde_json::from_str(include_str!(
        "../../../../../../tests/parity/kuromoji/nbest_manifest.json"
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
    for (case, expected) in cases.iter().zip(&expected) {
        assert_eq!(case["id"], expected["id"]);
        let id = case["id"].as_str().unwrap();
        let cost = case["n_best_cost"].as_i64().unwrap() as i32;
        let options = KuromojiOptions {
            mode: serde_json::from_value(case["mode"].clone()).unwrap(),
            discard_punctuation: case["discard_punctuation"].as_bool().unwrap(),
            discard_compound_token: case["discard_compound_token"].as_bool().unwrap(),
            n_best_cost: cost,
        };
        let user = case["user_dictionary"].as_str().and_then(|source| {
            UserDictionary::compile(source, &model, UserDictionaryLimits::default()).unwrap()
        });
        let tokenizer = JapaneseTokenizer::new(model.clone(), user, options).unwrap();
        let tokenizer = if let Some(examples) = case["n_best_examples"].as_str() {
            let configured = tokenizer.with_n_best_examples(examples);
            assert_eq!(tokenizer.n_best_cost(), cost);
            if expected["error_stage"] == "configure" {
                assert!(
                    matches!(configured, Err(AnalysisError::KuromojiDictionary(_))),
                    "{id}: {configured:?}"
                );
                continue;
            }
            let derived = tokenizer.calc_n_best_cost(examples).unwrap();
            assert_eq!(json!(derived), expected["example_cost"], "{id}");
            let configured = configured.unwrap_or_else(|error| panic!("{id}: {error}"));
            assert_eq!(configured.n_best_cost(), cost.max(derived), "{id}");
            configured
        } else {
            tokenizer
        };
        let input = case.get("input_utf16").map_or_else(
            || {
                case["input"]
                    .as_str()
                    .unwrap()
                    .repeat(case["repeat"].as_u64().unwrap_or(1) as usize)
                    .encode_utf16()
                    .collect::<Vec<_>>()
            },
            |raw| serde_json::from_value(raw.clone()).unwrap(),
        );
        let actual = tokenizer.tokenize_utf16(&input, KuromojiLimits::default(), &mut || Ok(()));
        if expected.get("error").is_some() {
            assert!(
                matches!(actual, Err(AnalysisError::KuromojiDictionary(_))),
                "{id}: {actual:?}"
            );
            continue;
        }
        let actual = actual.unwrap_or_else(|error| panic!("{id}: {error}"));
        let analysis = raw_analysis(&actual);
        if let Some(reference) = expected.get("analysis") {
            assert_eq!(analysis, *reference, "{id}");
        }
        assert_eq!(json!(actual.tokens.len()), expected["token_count"], "{id}");
        assert_eq!(
            format!(
                "{:x}",
                Sha256::digest(serde_json::to_vec(&analysis).unwrap())
            ),
            expected["sha256"].as_str().unwrap(),
            "{id}"
        );
        assert_common_graph(actual, &input, &analysis, id);
    }
}

fn assert_common_graph(actual: KuromojiOutput, input: &[u16], analysis: &Value, id: &str) {
    if let Ok(source) = String::from_utf16(input) {
        let mapped = actual
            .into_analyzed(&crate::FilteredText::new(&source))
            .unwrap_or_else(|error| panic!("{id}: {error}"));
        for (token, reference) in mapped
            .tokens()
            .iter()
            .zip(analysis["tokens"].as_array().unwrap())
        {
            assert_eq!(json!(token.term().utf16()), reference["term_utf16"], "{id}");
            assert_eq!(
                json!(token.position_increment()),
                reference["position_increment"],
                "{id}"
            );
            assert_eq!(
                json!(token.position_length()),
                reference["position_length"],
                "{id}"
            );
            let offsets = token.offsets().unwrap();
            assert_eq!(json!(offsets.utf16.start), reference["start_utf16"], "{id}");
            assert_eq!(json!(offsets.utf16.end), reference["end_utf16"], "{id}");
            assert!(source.is_char_boundary(offsets.utf8.start), "{id}");
            assert!(source.is_char_boundary(offsets.utf8.end), "{id}");
            assert!(token.japanese_morphology().is_some(), "{id}");
        }
        assert_eq!(
            json!(mapped.final_offsets().utf16.end),
            analysis["final_offset_utf16"],
            "{id}"
        );
        assert_eq!(
            json!(mapped.final_position_increment()),
            analysis["final_position_increment"],
            "{id}"
        );
    }
}

#[test]
fn nbest_memory_limits_and_cancellation_preserve_other_owners_and_allow_reuse() {
    let tokenizer = JapaneseTokenizer::new(
        model(),
        None,
        KuromojiOptions {
            n_best_cost: 10000,
            ..KuromojiOptions::default()
        },
    )
    .unwrap();
    let input = "関西国際空港 東京大学";
    let defaults = KuromojiLimits::default();
    let baseline = MemoryBudget::new(usize::MAX);
    let mut polls = 0;
    let expected = tokenizer
        .tokenize_budgeted(input, defaults, &baseline, &mut || {
            polls += 1;
            Ok(())
        })
        .unwrap();
    assert_eq!(expected.reserved_bytes(), retained_bytes(&expected));
    assert_eq!(baseline.used(), expected.reserved_bytes());
    assert!(expected
        .tokens
        .iter()
        .any(|token| token.position_length > 1));
    for allowance in [0, 1, expected.reserved_bytes() - 1, baseline.peak() - 1] {
        let budget = MemoryBudget::new(allowance + 7);
        let held = budget.reserve(7).unwrap();
        match tokenizer.tokenize_budgeted(input, defaults, &budget, &mut || Ok(())) {
            Ok(actual) => {
                assert_eq!(*actual, *expected);
                assert!(budget.peak() <= allowance + 7);
            }
            Err(AnalysisError::Memory(MemoryError::Limit { .. })) => {}
            Err(error) => panic!("allowance {allowance}: {error}"),
        }
        assert_eq!(budget.used(), 7);
        drop(held);
    }
    let budget = MemoryBudget::new(baseline.peak() + 7);
    let held = budget.reserve(7).unwrap();
    for limits in [
        KuromojiLimits {
            max_n_best_nodes: 1,
            ..defaults
        },
        KuromojiLimits {
            max_n_best_work: 1,
            ..defaults
        },
    ] {
        assert!(matches!(
            tokenizer.tokenize_budgeted(input, limits, &budget, &mut || Ok(())),
            Err(AnalysisError::KuromojiDictionary(_))
        ));
        assert_eq!(budget.used(), 7);
    }
    for stop in [1, 2, polls / 3, polls / 2, polls - 1, polls] {
        let mut calls = 0;
        assert!(
            matches!(
                tokenizer.tokenize_budgeted(input, defaults, &budget, &mut || {
                    calls += 1;
                    if calls == stop {
                        Err(AnalysisError::Cancelled)
                    } else {
                        Ok(())
                    }
                }),
                Err(AnalysisError::Cancelled)
            ),
            "poll {stop}"
        );
        assert_eq!(calls, stop);
        assert_eq!(budget.used(), 7);
    }
    let actual = tokenizer
        .tokenize_budgeted(input, defaults, &budget, &mut || Ok(()))
        .unwrap();
    assert_eq!(*actual, *expected);
    drop(actual);
    assert_eq!(budget.used(), 7);
    drop(held);
    drop(expected);
    assert_eq!(baseline.used(), 0);
}

#[test]
fn nonpositive_costs_allocate_no_alternative_nodes_or_work() {
    let model = model();
    let limits = KuromojiLimits {
        max_n_best_nodes: 0,
        max_n_best_work: 0,
        ..KuromojiLimits::default()
    };
    let mut expected = None;
    let mut peak = None;
    for cost in [0, -1, i32::MIN] {
        let tokenizer = JapaneseTokenizer::new(
            model.clone(),
            None,
            KuromojiOptions {
                n_best_cost: cost,
                ..KuromojiOptions::default()
            },
        )
        .unwrap();
        let budget = MemoryBudget::new(usize::MAX);
        let output = tokenizer
            .tokenize_budgeted("関西国際空港", limits, &budget, &mut || Ok(()))
            .unwrap();
        if let Some(expected) = &expected {
            assert_eq!(&*output, expected);
        }
        if let Some(peak) = peak {
            assert_eq!(budget.peak(), peak);
        }
        peak = Some(budget.peak());
        expected = Some((*output).clone());
        drop(output);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn example_preparation_is_bounded_atomic_and_retains_no_scratch() {
    let options = KuromojiOptions {
        n_best_cost: 2000,
        ..KuromojiOptions::default()
    };
    let tokenizer = JapaneseTokenizer::new(model(), None, options).unwrap();
    let examples = "関西国際空港-関西/関西国際空港-空港";
    let defaults = KuromojiLimits::default();
    let baseline = MemoryBudget::new(usize::MAX);
    let mut polls = 0;
    let actual = tokenizer
        .with_n_best_examples_budgeted(examples, defaults, &baseline, &mut || {
            polls += 1;
            Ok(())
        })
        .unwrap();
    assert_eq!(actual.n_best_cost(), 9325);
    assert_eq!(tokenizer.n_best_cost(), 2000);
    assert_eq!(baseline.used(), 0);
    let budget = MemoryBudget::new(baseline.peak() + 7);
    let held = budget.reserve(7).unwrap();
    for limits in [
        KuromojiLimits {
            max_input_utf16: 1,
            ..defaults
        },
        KuromojiLimits {
            max_n_best_examples: 1,
            ..defaults
        },
        KuromojiLimits {
            max_n_best_nodes: 1,
            ..defaults
        },
        KuromojiLimits {
            max_n_best_work: 1,
            ..defaults
        },
    ] {
        assert!(matches!(
            tokenizer.with_n_best_examples_budgeted(examples, limits, &budget, &mut || Ok(())),
            Err(AnalysisError::KuromojiDictionary(_))
        ));
        assert_eq!(budget.used(), 7);
    }
    assert!(matches!(
        tokenizer.with_n_best_examples_budgeted(
            "関西国際空港-関西/broken",
            defaults,
            &budget,
            &mut || Ok(())
        ),
        Err(AnalysisError::KuromojiDictionary(_))
    ));
    assert_eq!(budget.used(), 7);
    for stop in [1, 2, polls / 4, polls / 2, polls * 3 / 4, polls - 1, polls] {
        let mut calls = 0;
        assert!(
            matches!(
                tokenizer.with_n_best_examples_budgeted(examples, defaults, &budget, &mut || {
                    calls += 1;
                    if calls == stop {
                        Err(AnalysisError::Cancelled)
                    } else {
                        Ok(())
                    }
                }),
                Err(AnalysisError::Cancelled)
            ),
            "poll {stop}"
        );
        assert_eq!(calls, stop);
        assert_eq!(budget.used(), 7);
        assert_eq!(tokenizer.n_best_cost(), 2000);
    }
    for allowance in [0, 1, baseline.peak() / 2, baseline.peak() - 1] {
        let budget = MemoryBudget::new(allowance + 7);
        let held = budget.reserve(7).unwrap();
        match tokenizer.with_n_best_examples_budgeted(examples, defaults, &budget, &mut || Ok(())) {
            Ok(actual) => {
                assert_eq!(actual.n_best_cost(), 9325);
                assert!(budget.peak() <= allowance + 7);
            }
            Err(AnalysisError::Memory(MemoryError::Limit { .. })) => {}
            Err(error) => panic!("allowance {allowance}: {error}"),
        }
        assert_eq!(budget.used(), 7);
        drop(held);
    }
    assert_eq!(
        tokenizer
            .with_n_best_examples(examples)
            .unwrap()
            .n_best_cost(),
        9325
    );
    assert_eq!(budget.used(), 7);
    drop(held);
}
