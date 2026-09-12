//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_analysis::nori::{
    DecompoundMode, KoreanTokenizer, NoriMorpheme, NoriTokenizerConfig, UserDictionary,
    UserDictionaryLimits,
};
use uqa_analysis::AnalysisToken;

fn config() -> NoriTokenizerConfig {
    NoriTokenizerConfig {
        user_dictionary: Some("🙂a 가 나".into()),
        decompound_mode: DecompoundMode::Mixed,
        ..Default::default()
    }
}

#[test]
fn native_conversion_transfers_exact_morphology_and_term_reservations() {
    let config = config();
    let model = crate::nori_resources::model();
    let user = UserDictionary::compile(
        config.user_dictionary.as_deref().unwrap(),
        model,
        UserDictionaryLimits::default(),
    )
    .unwrap();
    let native = KoreanTokenizer::new(model.clone(), user, config.options()).unwrap();
    let budget = MemoryBudget::new(1 << 20);
    let filtered = CharFilter::HTMLStrip
        .filter_with_offsets_budgeted("<b>韓國 감싸여 🙂a</b>", &budget, &mut || Ok(()))
        .unwrap();
    let raw = native.tokenize(filtered.as_str()).unwrap();
    let attributes: usize = raw
        .tokens
        .iter()
        .map(|token| {
            String::from_utf16(&token.term_utf16).map_or_else(
                |_| token.term_utf16.capacity() * size_of::<u16>(),
                |term| term.len(),
            ) + token.reading.as_ref().map_or(0, String::capacity)
                + token.morphemes.as_ref().map_or(0, |parts| {
                    parts.capacity() * size_of::<NoriMorpheme>()
                        + parts
                            .iter()
                            .map(|part| part.surface_utf16.capacity() * size_of::<u16>())
                            .sum::<usize>()
                })
        })
        .sum();
    let expected = raw.into_analyzed(&filtered).unwrap();
    let actual = Tokenizer::Nori(config)
        .tokenize_mapped_budgeted(&filtered, &budget, || Ok(()))
        .unwrap();
    assert_eq!(*actual, expected);
    assert!(actual
        .tokens()
        .iter()
        .any(|token| token.term().as_str().is_none()));
    assert!(actual.tokens().iter().any(|token| token
        .korean_morphology()
        .unwrap()
        .reading
        .is_some()));
    assert!(actual.tokens().iter().any(|token| token
        .korean_morphology()
        .unwrap()
        .morphemes
        .is_some()));
    drop(expected);
    drop(filtered);
    let (actual, memory) = actual.into_parts();
    let tokens = actual.into_tokens();
    assert_eq!(
        memory.bytes(),
        tokens.capacity() * size_of::<AnalysisToken>() + attributes
    );
    assert_eq!(budget.used(), memory.bytes());
    drop(tokens);
    drop(memory);
    assert_eq!(budget.used(), 0);
}

#[test]
fn cancellation_and_limits_release_native_and_converted_token_buffers() {
    let tokenizer = Tokenizer::Nori(config());
    let input = "韓國 감싸여 🙂a";
    let baseline = MemoryBudget::new(1 << 20);
    let mut polls = 0;
    let expected = tokenizer
        .tokenize_with_offsets_budgeted(input, &baseline, || {
            polls += 1;
            Ok(())
        })
        .unwrap();
    for stop in [1, 2, polls / 2, polls - 6, polls - 3, polls - 1, polls] {
        let budget = MemoryBudget::new(1 << 20);
        let other = budget.reserve(7).unwrap();
        let mut count = 0;
        let actual = tokenizer.tokenize_with_offsets_budgeted(input, &budget, || {
            count += 1;
            if count == stop {
                Err(AnalysisError::Cancelled)
            } else {
                Ok(())
            }
        });
        assert!(
            matches!(actual, Err(AnalysisError::Cancelled)),
            "poll {stop}: {actual:?}"
        );
        assert_eq!(budget.used(), 7);
        drop(other);
    }
    for allowance in [0, 128, 512, expected.reserved_bytes() - 1, baseline.peak()] {
        let budget = MemoryBudget::new(allowance + 7);
        let other = budget.reserve(7).unwrap();
        match tokenizer.tokenize_with_offsets_budgeted(input, &budget, || Ok(())) {
            Ok(actual) => assert_eq!(*actual, *expected),
            Err(AnalysisError::Memory(MemoryError::Limit { .. })) => {
                assert!(allowance < baseline.peak());
            }
            other => panic!("allowance {allowance}: {other:?}"),
        }
        assert_eq!(budget.used(), 7);
        assert!(budget.peak() <= budget.limit());
        drop(other);
        assert_eq!(budget.used(), 0);
    }
}
