//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::nori::{NoriMorpheme, NoriOrigin, NoriResources, NoriToken, POSType};
use crate::token::allocation::TokenBuffer;
use crate::{AnalysisError, TokenFilter, Tokenizer};
use std::sync::{Arc, OnceLock};
use uqa_core::memory::{MemoryBudget, MemoryError};

fn model() -> &'static Arc<NoriDictionary> {
    static MODEL: OnceLock<Arc<NoriDictionary>> = OnceLock::new();
    MODEL.get_or_init(|| {
        NoriResources::default()
            .load_default()
            .unwrap()
            .model()
            .clone()
    })
}

fn filters() -> [KoreanFilter; 4] {
    [
        KoreanFilter::PartOfSpeech {
            stop_tags: Some(vec![POSTag::NNP]),
        },
        KoreanFilter::ReadingForm,
        KoreanFilter::SimpleLowercase,
        KoreanFilter::Number,
    ]
}

fn sample(terminal: bool) -> NoriOutput {
    let terms = ["３", "．", "２", "천", "원", "韓國", "UQA"];
    let mut offset = 0;
    let mut tokens = Vec::new();
    for (index, text) in terms.into_iter().enumerate() {
        let term_utf16: Vec<_> = text.encode_utf16().collect();
        let end_utf16 = offset + term_utf16.len();
        tokens.push(NoriToken {
            term_utf16,
            start_utf16: offset,
            end_utf16,
            position_increment: 1,
            position_length: if index == 6 { 2 } else { 1 },
            keyword: index == 6,
            pos_type: POSType::Morpheme,
            left_pos: if index == 6 { POSTag::NNP } else { POSTag::NNG },
            right_pos: POSTag::NNG,
            reading: (index == 5).then(|| "한국".into()),
            morphemes: Some(vec![NoriMorpheme {
                surface_utf16: vec![0xd800, 0xdc00, 0xd800],
                pos: POSTag::NNG,
            }]),
            origin: NoriOrigin::User,
        });
        offset = end_utf16 + 1;
    }
    let mut raw = tokens.last().unwrap().clone();
    raw.term_utf16 = vec![0xdc00];
    raw.reading = Some(String::new());
    raw.morphemes = Some(Vec::new());
    raw.position_increment = 0;
    tokens.push(raw);
    let terminal = terminal.then(|| Box::new(tokens[2].clone()));
    NoriOutput {
        tokens,
        terminal,
        final_offset_utf16: offset,
        final_position_increment: 3,
    }
}

fn copy(input: &NoriOutput, budget: &MemoryBudget) -> AnalysisResult<Budgeted<NoriOutput>> {
    let mut output = TokenBuffer::new(budget);
    output.reserve_tokens(input.tokens.len())?;
    for token in &input.tokens {
        output.push(token.clone_budgeted(budget, &mut || Ok(()))?)?;
    }
    if let Some(terminal) = &input.terminal {
        output.set_terminal_token(terminal.clone_budgeted(budget, &mut || Ok(()))?)?;
    }
    let (batch, memory) = output
        .into_batch(input.final_position_increment)
        .into_parts();
    Ok(Budgeted::new(
        NoriOutput {
            tokens: batch.tokens,
            terminal: batch.terminal,
            final_offset_utf16: input.final_offset_utf16,
            final_position_increment: batch.final_position_increment,
        },
        memory,
    ))
}

fn token_bytes(token: NoriToken) -> usize {
    let mut bytes = token.term_utf16.capacity() * size_of::<u16>();
    bytes += token.reading.as_ref().map_or(0, String::capacity);
    if let Some(morphemes) = token.morphemes {
        bytes += morphemes.capacity() * size_of::<NoriMorpheme>();
        bytes += morphemes
            .iter()
            .map(|part| part.surface_utf16.capacity() * size_of::<u16>())
            .sum::<usize>();
    }
    bytes
}

fn assert_owned(output: Budgeted<NoriOutput>, budget: &MemoryBudget, other: usize) {
    assert_eq!(budget.used(), output.reserved_bytes() + other);
    let (output, memory) = output.into_parts();
    let mut bytes = output.tokens.capacity() * size_of::<NoriToken>();
    for token in output.tokens {
        bytes += token_bytes(token);
    }
    if let Some(terminal) = output.terminal {
        bytes += size_of::<NoriToken>() + token_bytes(*terminal);
    }
    assert_eq!(bytes, memory.bytes());
    assert_eq!(budget.used(), bytes + other);
    drop(memory);
    assert_eq!(budget.used(), other);
}

#[test]
fn native_filter_batches_keep_exact_term_morphology_and_terminal_reservations() {
    for terminal in [false, true] {
        let original = sample(terminal);
        for filter in filters() {
            let expected = filter.apply(original.clone(), model()).unwrap();
            let budget = MemoryBudget::new(1 << 20);
            let other = budget.reserve(7).unwrap();
            let input = copy(&original, &budget).unwrap();
            let output = filter
                .apply_budgeted(input, model(), NoriLimits::default(), &mut || Ok(()))
                .unwrap();
            assert_eq!(*output, expected, "{filter:?}, terminal {terminal}");
            assert_owned(output, &budget, 7);
            drop(other);
        }
    }
}

#[test]
fn native_filter_byte_limits_and_every_callback_failure_preserve_other_owners() {
    let original = sample(true);
    for filter in filters() {
        let baseline = MemoryBudget::new(1 << 20);
        let input = copy(&original, &baseline).unwrap();
        let mut calls = 0;
        let expected = filter
            .apply_budgeted(input, model(), NoriLimits::default(), &mut || {
                calls += 1;
                Ok(())
            })
            .unwrap();
        let mut succeeded = false;
        for allowance in 0..=baseline.peak() {
            let budget = MemoryBudget::new(allowance + 7);
            let other = budget.reserve(7).unwrap();
            let result = copy(&original, &budget).and_then(|input| {
                filter.apply_budgeted(input, model(), NoriLimits::default(), &mut || Ok(()))
            });
            match result {
                Ok(output) => {
                    assert_eq!(*output, *expected);
                    assert_owned(output, &budget, 7);
                    succeeded = true;
                }
                Err(AnalysisError::Memory(MemoryError::Limit { .. })) => {}
                result => panic!("{filter:?}, allowance {allowance}: {result:?}"),
            }
            assert_eq!(budget.used(), 7);
            assert!(budget.peak() <= budget.limit());
            drop(other);
        }
        assert!(succeeded);
        for stop in 1..=calls {
            let budget = MemoryBudget::new(1 << 20);
            let other = budget.reserve(7).unwrap();
            let input = copy(&original, &budget).unwrap();
            let mut count = 0;
            let result = filter.apply_budgeted(input, model(), NoriLimits::default(), &mut || {
                count += 1;
                if count == stop {
                    Err(AnalysisError::Cancelled)
                } else {
                    Ok(())
                }
            });
            assert!(
                matches!(result, Err(AnalysisError::Cancelled)),
                "{filter:?}, callback {stop}: {result:?}"
            );
            assert_eq!(budget.used(), 7);
            drop(other);
        }
    }
}

#[test]
fn native_lowercase_and_reading_reuse_existing_capacity_with_a_full_allowance() {
    for filter in [KoreanFilter::SimpleLowercase, KoreanFilter::ReadingForm] {
        let original = sample(true);
        let budget = MemoryBudget::new(1 << 20);
        let input = copy(&original, &budget).unwrap();
        let vector = input.tokens.as_ptr();
        let terms: Vec<_> = input
            .tokens
            .iter()
            .map(|token| token.term_utf16.as_ptr())
            .collect();
        let terminal = std::ptr::from_ref(input.terminal.as_deref().unwrap());
        let bytes = input.reserved_bytes();
        let blocker = budget.reserve(budget.limit() - budget.used()).unwrap();
        let output = filter
            .apply_budgeted(input, model(), NoriLimits::default(), &mut || Ok(()))
            .unwrap();
        assert_eq!(output.tokens.as_ptr(), vector);
        assert_eq!(
            std::ptr::from_ref(output.terminal.as_deref().unwrap()),
            terminal
        );
        for (token, pointer) in output.tokens.iter().zip(terms) {
            assert_eq!(token.term_utf16.as_ptr(), pointer);
        }
        assert_eq!(output.reserved_bytes(), bytes);
        assert_owned(output, &budget, blocker.bytes());
    }
}

#[test]
fn common_korean_filters_release_long_copies_source_comparisons_and_numeric_scratch_on_cancel() {
    let source = format!("{}{}", "9".repeat(4096), "십".repeat(4096));
    let original = Tokenizer::Keyword.tokenize_with_offsets(&source).unwrap();
    for filter in filters() {
        let filter: TokenFilter =
            serde_json::from_value(serde_json::to_value(filter).unwrap()).unwrap();
        let budget = MemoryBudget::new(1 << 22);
        let input = original.clone_budgeted(&budget, || Ok(())).unwrap();
        let mut calls = 0;
        let output = filter
            .filter_analyzed_budgeted(input, || {
                calls += 1;
                Ok(())
            })
            .unwrap();
        assert!(calls > 10, "{filter:?}");
        drop(output);
        assert_eq!(budget.used(), 0);
        for stop in 1..=calls {
            let other = budget.reserve(7).unwrap();
            let input = original.clone_budgeted(&budget, || Ok(())).unwrap();
            let mut count = 0;
            let result = filter.filter_analyzed_budgeted(input, || {
                count += 1;
                if count == stop {
                    Err(AnalysisError::Cancelled)
                } else {
                    Ok(())
                }
            });
            assert!(
                matches!(result, Err(AnalysisError::Cancelled)),
                "{filter:?}, callback {stop}: {result:?}"
            );
            assert_eq!(budget.used(), 7);
            drop(other);
        }
    }
}

#[test]
fn native_and_common_korean_chains_preserve_morphology_and_retained_source_ownership() {
    use crate::nori::{
        DecompoundMode, KoreanTokenizer, NoriOptions, UserDictionary, UserDictionaryLimits,
    };
    let budget = MemoryBudget::new(1 << 24);
    let other = budget.reserve(7).unwrap();
    let source = crate::CharFilter::HTMLStrip
        .filter_with_offsets_budgeted(
            "<b>３．２천 원 韓國 감싸여 🙂a UQA</b>",
            &budget,
            &mut || Ok(()),
        )
        .unwrap();
    source.prepare_coordinates(&budget, &mut || Ok(())).unwrap();
    let user =
        UserDictionary::compile("🙂a 가 나", model(), UserDictionaryLimits::default()).unwrap();
    let tokenizer = KoreanTokenizer::new(
        model().clone(),
        user,
        NoriOptions {
            decompound_mode: DecompoundMode::Mixed,
            discard_punctuation: false,
            ..NoriOptions::default()
        },
    )
    .unwrap();
    let native = tokenizer
        .tokenize_budgeted(source.as_str(), NoriLimits::default(), &budget, &mut || {
            Ok(())
        })
        .unwrap();
    let mut expected = (*native).clone();
    let mut actual =
        crate::AnalyzedText::from_nori_budgeted(native, &source, &mut || Ok(())).unwrap();
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
    let source_bytes = budget.used() - actual.reserved_bytes() - 7;
    for filter in [
        KoreanFilter::Number,
        KoreanFilter::ReadingForm,
        KoreanFilter::SimpleLowercase,
        filters()[0].clone(),
    ] {
        expected = filter.apply(expected, model()).unwrap();
        let configuration: TokenFilter =
            serde_json::from_value(serde_json::to_value(filter).unwrap()).unwrap();
        actual = configuration
            .filter_analyzed_budgeted(actual, || Ok(()))
            .unwrap();
        let comparison = expected.clone().into_analyzed(&source).unwrap();
        assert_eq!(*actual, comparison);
        assert_eq!(budget.used(), actual.reserved_bytes() + source_bytes + 7);
    }
    drop(source);
    assert!(budget.used() > actual.reserved_bytes() + 7);
    assert!(actual
        .tokens()
        .iter()
        .all(|token| token.korean_morphology().is_some()));
    drop(actual);
    assert_eq!(budget.used(), 7);
    drop(other);
}

#[test]
fn common_korean_filter_allocation_failures_release_copied_morphology_and_terminal_state() {
    let native = sample(true);
    let source = " ".repeat(native.final_offset_utf16);
    let original = native
        .into_analyzed(&crate::FilteredText::new(&source))
        .unwrap();
    for filter in filters() {
        let baseline = MemoryBudget::new(1 << 20);
        let input = original.clone_budgeted(&baseline, || Ok(())).unwrap();
        let expected = filter
            .filter_analyzed_budgeted(input, model(), NoriLimits::default(), &mut || Ok(()))
            .unwrap();
        let mut succeeded = false;
        for allowance in 0..=baseline.peak() {
            let budget = MemoryBudget::new(allowance + 7);
            let other = budget.reserve(7).unwrap();
            let result = original
                .clone_budgeted(&budget, || Ok(()))
                .and_then(|input| {
                    filter.filter_analyzed_budgeted(
                        input,
                        model(),
                        NoriLimits::default(),
                        &mut || Ok(()),
                    )
                });
            match result {
                Ok(output) => {
                    assert_eq!(*output, *expected);
                    assert_eq!(budget.used(), output.reserved_bytes() + 7);
                    succeeded = true;
                }
                Err(AnalysisError::Memory(MemoryError::Limit { .. })) => {}
                result => panic!("{filter:?}, allowance {allowance}: {result:?}"),
            }
            assert_eq!(budget.used(), 7);
            assert!(budget.peak() <= budget.limit());
            drop(other);
        }
        assert!(succeeded);
    }
}
