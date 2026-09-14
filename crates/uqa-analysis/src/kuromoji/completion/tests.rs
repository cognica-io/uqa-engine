//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::kuromoji::tokenizer::tests::model;
use crate::kuromoji::DictionaryError;
use crate::AnalysisError;
use uqa_core::memory::MemoryError;

fn retained(output: &Vec<Vec<u16>>) -> usize {
    output.capacity() * size_of::<Vec<u16>>()
        + output
            .iter()
            .map(|term| term.capacity() * size_of::<u16>())
            .sum::<usize>()
}

#[test]
fn completion_reserves_every_candidate_and_cancels_lookup_counting_and_emission() {
    let model = model();
    for text in [
        "シン".repeat(5),
        "キョ".repeat(2049),
        "カ?".repeat(2049),
        String::new(),
    ] {
        let input: Vec<_> = text.encode_utf16().collect();
        let run = |budget: &MemoryBudget, poll: &mut dyn FnMut() -> AnalysisResult<()>| {
            romanize_completion_utf16_budgeted(
                &input,
                &model,
                KuromojiLimits::default(),
                budget,
                &mut || poll(),
            )
        };
        let baseline = MemoryBudget::new(usize::MAX);
        let mut calls = 0;
        let expected = run(&baseline, &mut || {
            calls += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!(expected.reserved_bytes(), retained(&expected));
        assert_eq!(baseline.used(), expected.reserved_bytes());
        let peak = baseline.peak();
        for stop in 1..=calls {
            let budget = MemoryBudget::new(peak + 7);
            let other = budget.reserve(7).unwrap();
            let mut count = 0;
            let actual = run(&budget, &mut || {
                count += 1;
                if count == stop {
                    Err(AnalysisError::Cancelled)
                } else {
                    Ok(())
                }
            });
            assert!(
                matches!(actual, Err(AnalysisError::Cancelled)),
                "{stop}/{calls}: {actual:?}"
            );
            assert_eq!(count, stop);
            assert_eq!(budget.used(), 7);
            drop(other);
        }
        for allowance in [0, 1, peak / 2, peak.saturating_sub(1), peak] {
            let budget = MemoryBudget::new(allowance + 7);
            let other = budget.reserve(7).unwrap();
            match run(&budget, &mut || Ok(())) {
                Ok(actual) => {
                    assert_eq!(*actual, *expected);
                    assert_eq!(actual.reserved_bytes(), retained(&actual));
                }
                Err(AnalysisError::Memory(MemoryError::Limit { .. })) => assert!(allowance < peak),
                result => panic!("allowance {allowance}/{peak}: {result:?}"),
            }
            assert_eq!(budget.used(), 7);
            assert!(budget.peak() <= budget.limit());
            drop(other);
        }
        drop(expected);
        assert_eq!(baseline.used(), 0);
        drop(run(&baseline, &mut || Ok(())).unwrap());
        assert_eq!(baseline.used(), 0);
    }
}

#[test]
fn completion_checks_the_complete_product_and_work_before_unbounded_expansion() {
    let model = model();
    let input: Vec<_> = "シン".encode_utf16().collect();
    let output =
        romanize_completion_utf16(&input, &model, KuromojiLimits::default(), &mut || Ok(()))
            .unwrap();
    let units = output.iter().map(Vec::len).sum();
    let exact = KuromojiLimits {
        max_input_utf16: input.len(),
        max_tokens: output.len(),
        max_output_utf16: units,
        ..KuromojiLimits::default()
    };
    assert_eq!(
        romanize_completion_utf16(&input, &model, exact, &mut || Ok(())).unwrap(),
        output
    );
    for (limits, expected_resource) in [
        (
            KuromojiLimits {
                max_input_utf16: input.len() - 1,
                ..exact
            },
            "Kuromoji input UTF-16 units",
        ),
        (
            KuromojiLimits {
                max_tokens: output.len() - 1,
                ..exact
            },
            "Kuromoji output tokens",
        ),
        (
            KuromojiLimits {
                max_output_utf16: units - 1,
                ..exact
            },
            "Kuromoji output UTF-16 units",
        ),
        (
            KuromojiLimits {
                max_completion_work: 0,
                ..exact
            },
            "Kuromoji completion work",
        ),
    ] {
        let budget = MemoryBudget::new(usize::MAX);
        let error =
            romanize_completion_utf16_budgeted(&input, &model, limits, &budget, &mut || Ok(()))
                .unwrap_err();
        assert!(
            matches!(error, AnalysisError::KuromojiDictionary(DictionaryError::Limit { resource, .. }) if resource == expected_resource),
            "{error:?}"
        );
        assert_eq!(budget.used(), 0);
    }
    let explosive: Vec<_> = "シ".repeat(128).encode_utf16().collect();
    let budget = MemoryBudget::new(32 * 1024);
    let error = romanize_completion_utf16_budgeted(
        &explosive,
        &model,
        KuromojiLimits {
            max_tokens: 1024,
            ..KuromojiLimits::default()
        },
        &budget,
        &mut || Ok(()),
    )
    .unwrap_err();
    assert!(
        matches!(
            error,
            AnalysisError::KuromojiDictionary(DictionaryError::Limit {
                resource: "Kuromoji output tokens",
                ..
            })
        ),
        "{error:?}"
    );
    assert!(
        budget.peak() < 1024,
        "rejected while only the borrowed plan is retained"
    );
    assert_eq!(budget.used(), 0);
    assert!(matches!(
        multiply(usize::MAX, 2),
        Err(AnalysisError::KuromojiDictionary(_))
    ));
    assert!(matches!(
        add(usize::MAX, 1),
        Err(AnalysisError::KuromojiDictionary(_))
    ));
}

#[test]
fn completion_model_prefix_ranks_and_serialized_identity_remain_exact() {
    let model = model();
    for (rank, mapping) in model.completion_mappings().iter().enumerate() {
        assert_eq!(
            model
                .analysis
                .completion_lexicon
                .lookup(mapping.key().encode_utf16()),
            Some(rank as u32)
        );
    }
    assert_eq!(model.id().to_string(), uqa_kuromoji_data::DICTIONARY_ID);
}
