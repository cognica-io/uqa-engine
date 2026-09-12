//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{BM25Params, BayesianBM25Params};
use uqa_core::{memory::MemoryError, QueryCancelled};

fn stats() -> IndexStats {
    let mut stats = IndexStats::new(1000);
    stats.avg_doc_length = 37.0;
    stats.set_doc_freq("unused", "unused vocabulary", 42);
    stats
}

#[test]
fn retained_idfs_and_streamed_scores_match_native_scorer_bits_in_emitted_order() {
    for mode in [
        ScoringMode::BM25(BM25Params::default()),
        ScoringMode::BayesianBM25(BayesianBM25Params {
            alpha: 1.2,
            beta: 0.8,
            calibration_tokens: 3.0,
            beta_slope: 0.1,
            sigma_slope: 0.02,
            ..Default::default()
        }),
    ] {
        for count in [0, 1, 2, 3, 31, 32, 33, 1023, 1024, 1025] {
            let frequencies: Vec<_> = (0..count).map(|index| index % 999).collect();
            let reference = crate::text::exhaustive::build_text_scorer(
                &mode,
                Arc::new(stats()),
                frequencies.len(),
            )
            .unwrap();
            let budget = MemoryBudget::new(1 << 20);
            let candidate =
                TextCandidateScorer::new_budgeted(&mode, stats(), &frequencies, &budget, || Ok(()))
                    .unwrap();
            assert_eq!(
                budget.used(),
                size_of::<IndexStats>() + frequencies.len() * size_of::<f64>()
            );
            for length in [0, 1, 37, 1000] {
                let term_frequencies: Vec<_> = frequencies.iter().map(|value| value % 7).collect();
                let scores: Vec<_> = term_frequencies
                    .iter()
                    .zip(&frequencies)
                    .map(|(tf, df)| reference.term_score_with_idf(*tf, length, reference.idf(*df)))
                    .collect();
                let actual = candidate
                    .score_document_with_control(length, &term_frequencies, || Ok(()))
                    .unwrap();
                assert_eq!(
                    actual.to_bits(),
                    reference.finalize_score(&scores).to_bits(),
                    "count={count},length={length},mode={mode:?}"
                );
            }
            drop(candidate);
            assert_eq!(budget.used(), 0);
        }
    }
}

#[test]
fn candidate_preparation_and_scoring_failures_keep_independent_memory_and_allow_reuse() {
    let frequencies = [1, 2, 3, 1, 2, 3];
    let required = size_of::<IndexStats>() + size_of_val(&frequencies);
    for allowance in 0..=required {
        let budget = MemoryBudget::new(allowance + 7);
        let other = budget.reserve(7).unwrap();
        let result = TextCandidateScorer::new_budgeted(
            &ScoringMode::default(),
            stats(),
            &frequencies,
            &budget,
            || Ok(()),
        );
        match result {
            Ok(candidate) => {
                assert_eq!(allowance, required);
                drop(candidate);
            }
            Err(TextSearchError::Memory(MemoryError::Limit { .. })) => {
                assert!(allowance < required);
            }
            _ => panic!("unexpected preparation result"),
        }
        assert_eq!(budget.used(), 7);
        drop(other);
    }
    let budget = MemoryBudget::new(1 << 20);
    let mut calls = 0;
    let mut candidate = TextCandidateScorer::new_budgeted(
        &ScoringMode::default(),
        stats(),
        &frequencies,
        &budget,
        || {
            calls += 1;
            Ok(())
        },
    )
    .unwrap();
    let expected = candidate.score_document(37, &frequencies).unwrap();
    let retained = budget.used();
    for stop in 1..=calls {
        let mut count = 0;
        assert!(matches!(
            TextCandidateScorer::new_budgeted(
                &ScoringMode::default(),
                stats(),
                &frequencies,
                &budget,
                || {
                    count += 1;
                    if count == stop {
                        Err(QueryCancelled.into())
                    } else {
                        Ok(())
                    }
                }
            ),
            Err(TextSearchError::Cancelled(_))
        ));
        assert_eq!(budget.used(), retained);
    }
    let mut calls = 0;
    candidate
        .score_document_with_control(37, &frequencies, || {
            calls += 1;
            Ok(())
        })
        .unwrap();
    for stop in 1..=calls {
        let mut count = 0;
        assert!(matches!(
            candidate.score_document_with_control(37, &frequencies, || {
                count += 1;
                if count == stop {
                    Err(QueryCancelled.into())
                } else {
                    Ok(())
                }
            }),
            Err(TextSearchError::Cancelled(_))
        ));
        assert_eq!(
            candidate
                .score_document(37, &frequencies)
                .unwrap()
                .to_bits(),
            expected.to_bits()
        );
        assert_eq!(budget.used(), retained);
    }
    drop(candidate);
    assert_eq!(budget.used(), 0);
}
