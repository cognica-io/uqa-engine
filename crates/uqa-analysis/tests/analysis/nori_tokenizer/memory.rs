//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native allocation lifetimes and interruption preserve the reference token stream.

use super::{model, KoreanTokenizer, NoriOptions, UserDictionary, UserDictionaryLimits};
use uqa_analysis::nori::{DecompoundMode, NoriLimits, NoriMorpheme, NoriOutput, NoriToken};
use uqa_analysis::AnalysisError;
use uqa_core::memory::{MemoryBudget, MemoryError};

fn tokenizer() -> KoreanTokenizer {
    let user =
        UserDictionary::compile("🙂a 가 나\n", model(), UserDictionaryLimits::default()).unwrap();
    KoreanTokenizer::new(
        model().clone(),
        user,
        NoriOptions {
            decompound_mode: DecompoundMode::Mixed,
            ..NoriOptions::default()
        },
    )
    .unwrap()
}

fn retained_bytes(output: &NoriOutput) -> usize {
    output.tokens.capacity() * size_of::<NoriToken>()
        + output
            .tokens
            .iter()
            .map(|token| {
                token.term_utf16.capacity() * size_of::<u16>()
                    + token.reading.as_ref().map_or(0, String::capacity)
                    + token.morphemes.as_ref().map_or(0, |parts| {
                        parts.capacity() * size_of::<NoriMorpheme>()
                            + parts
                                .iter()
                                .map(|part| part.surface_utf16.capacity() * size_of::<u16>())
                                .sum::<usize>()
                    })
            })
            .sum::<usize>()
}

#[test]
fn completed_tokens_retain_exact_buffer_and_attribute_reservations() {
    let tokenizer = tokenizer();
    let budget = MemoryBudget::new(usize::MAX);
    let output = tokenizer
        .tokenize_budgeted(
            "韓國 감싸여 🙂a",
            NoriLimits::default(),
            &budget,
            &mut || Ok(()),
        )
        .unwrap();
    assert!(output.tokens.iter().any(|token| token.reading.is_some()));
    assert!(output.tokens.iter().any(|token| token.morphemes.is_some()));
    assert!(output
        .tokens
        .iter()
        .any(|token| token.term_utf16 == [0xd83d]));
    assert_eq!(output.reserved_bytes(), retained_bytes(&output));
    assert_eq!(budget.used(), output.reserved_bytes());
    assert!(budget.peak() > budget.used());
    drop(output);
    assert_eq!(budget.used(), 0);
}

#[test]
fn allocation_failures_return_no_partial_result_and_release_every_buffer() {
    let tokenizer = tokenizer();
    let input = "韓國 감싸여 🙂a";
    let unlimited = MemoryBudget::new(usize::MAX);
    let expected = tokenizer
        .tokenize_budgeted(input, NoriLimits::default(), &unlimited, &mut || Ok(()))
        .unwrap();
    for limit in [
        0,
        1,
        input.encode_utf16().count() * 2 - 1,
        retained_bytes(&expected) - 1,
    ] {
        let budget = MemoryBudget::new(limit);
        let result =
            tokenizer.tokenize_budgeted(input, NoriLimits::default(), &budget, &mut || Ok(()));
        assert!(
            matches!(
                result,
                Err(AnalysisError::Memory(MemoryError::Limit { .. }))
            ),
            "limit={limit}, {result:?}"
        );
        assert_eq!(budget.used(), 0);
        assert!(budget.peak() <= limit);
    }
    let budget = MemoryBudget::new(unlimited.peak());
    let actual = tokenizer
        .tokenize_budgeted(input, NoriLimits::default(), &budget, &mut || Ok(()))
        .unwrap();
    assert_eq!(*actual, *expected);
    let retained = budget.used();
    let other_owner = budget.reserve(budget.limit() - retained).unwrap();
    assert!(matches!(
        tokenizer.tokenize_budgeted(input, NoriLimits::default(), &budget, &mut || Ok(())),
        Err(AnalysisError::Memory(MemoryError::Limit { .. }))
    ));
    assert_eq!(budget.used(), budget.limit());
    drop(other_owner);
    assert_eq!(budget.used(), retained);
    drop(actual);
    assert_eq!(budget.used(), 0);
}

#[test]
fn cancellation_during_input_lattice_and_emission_releases_only_the_calls_reservations() {
    let tokenizer = tokenizer();
    let input = "韓國 감싸여 🙂a ".repeat(48);
    let baseline = MemoryBudget::new(usize::MAX);
    let mut polls = 0;
    let expected = tokenizer
        .tokenize_budgeted(&input, NoriLimits::default(), &baseline, &mut || {
            polls += 1;
            Ok(())
        })
        .unwrap();
    assert!(polls > 32);
    let budget = MemoryBudget::new(baseline.peak() + 7);
    let other_owner = budget.reserve(7).unwrap();
    for stop in [1, 2, 4, 8, 16, polls / 3, polls / 2, polls - 1, polls] {
        let mut calls = 0;
        let result =
            tokenizer.tokenize_budgeted(&input, NoriLimits::default(), &budget, &mut || {
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
        assert_eq!(budget.used(), 7);
    }
    let actual = tokenizer
        .tokenize_budgeted(&input, NoriLimits::default(), &budget, &mut || Ok(()))
        .unwrap();
    assert_eq!(*actual, *expected);
    drop(actual);
    drop(other_owner);
    assert_eq!(budget.used(), 0);
}

#[test]
fn borrowed_utf16_input_preserves_non_scalar_terms_without_owning_the_input_buffer() {
    let tokenizer = tokenizer();
    let input: Vec<u16> = "韓國 감싸여 🙂a".encode_utf16().collect();
    let budget = MemoryBudget::new(usize::MAX);
    let actual = tokenizer
        .tokenize_utf16_budgeted(&input, NoriLimits::default(), &budget, &mut || Ok(()))
        .unwrap();
    let expected = tokenizer
        .tokenize_utf16(&input, NoriLimits::default(), &mut || Ok(()))
        .unwrap();
    assert_eq!(*actual, expected);
    assert_eq!(budget.used(), retained_bytes(&actual));
    drop(actual);
    assert_eq!(budget.used(), 0);
    assert_eq!(input, "韓國 감싸여 🙂a".encode_utf16().collect::<Vec<_>>());
}
