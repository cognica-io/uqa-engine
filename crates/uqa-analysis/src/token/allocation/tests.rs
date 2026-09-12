//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{AnalysisError, CharFilter, TokenFilter, TokenTerm, Tokenizer};
use uqa_core::memory::MemoryError;

mod common_filters;

#[test]
fn moving_a_query_term_releases_other_attributes_without_copying_its_buffer() {
    let budget = MemoryBudget::new(1 << 20);
    let other = budget.reserve(7).unwrap();
    let batch = attributes().clone_budgeted(&budget, || Ok(())).unwrap();
    let pointer = batch.tokens()[0].term().utf16().as_ptr();
    let term_bytes = batch.tokens()[0].term().allocation_bytes();
    let expected = batch.tokens()[0].term().clone();
    let input = AnalyzedText::into_token_input(batch);
    let (token, remaining) = input.next(&mut || Ok(())).unwrap();
    let term = AnalysisToken::into_term_budgeted(token.unwrap());
    assert_eq!(*term, expected);
    assert_eq!(term.utf16().as_ptr(), pointer);
    assert_eq!(term.reserved_bytes(), term_bytes);
    assert!(budget.used() > term_bytes + 7);
    drop(remaining);
    assert_eq!(budget.used(), term_bytes + 7);
    drop(term);
    assert_eq!(budget.used(), 7);
    drop(other);
}

#[test]
fn consuming_query_tokens_releases_the_unused_character_edit_projection() {
    let budget = MemoryBudget::new(1 << 20);
    let other = budget.reserve(7).unwrap();
    let filtered = CharFilter::HTMLStrip
        .filter_with_offsets_budgeted("<b>韓🙂</b>", &budget, &mut || Ok(()))
        .unwrap();
    let batch = Tokenizer::Whitespace
        .tokenize_mapped_budgeted(&filtered, &budget, || Ok(()))
        .unwrap();
    let pointer = batch.tokens()[0].term().as_str().unwrap().as_ptr();
    drop(filtered);
    let input = AnalyzedText::into_token_input(batch);
    let (token, remaining) = input.next(&mut || Ok(())).unwrap();
    let term = AnalysisToken::into_term_budgeted(token.unwrap());
    drop(remaining);
    assert_eq!(term.as_str(), Some("韓🙂"));
    assert_eq!(term.as_str().unwrap().as_ptr(), pointer);
    assert_eq!(budget.used(), term.reserved_bytes() + 7);
    drop(term);
    assert_eq!(budget.used(), 7);
    drop(other);
}

fn token_bytes(token: AnalysisToken) -> usize {
    let mut bytes = if token.term.as_str().is_some() {
        token.term.into_string().unwrap().capacity()
    } else {
        token.term.into_utf16().capacity() * size_of::<u16>()
    };
    #[cfg(feature = "nori")]
    if let Some(morphology) = token.korean_morphology {
        bytes += morphology.reading.as_ref().map_or(0, String::capacity);
        if let Some(morphemes) = morphology.morphemes {
            bytes += morphemes.capacity() * size_of::<crate::nori::NoriMorpheme>();
            bytes += morphemes
                .iter()
                .map(|value| value.surface_utf16.capacity() * size_of::<u16>())
                .sum::<usize>();
        }
    }
    #[cfg(not(feature = "nori"))]
    let _ = &mut bytes;
    bytes
}

fn owned_bytes(input: AnalyzedText) -> usize {
    let mut bytes = input.batch.tokens.capacity() * size_of::<AnalysisToken>();
    for token in input.batch.tokens {
        bytes += token_bytes(token);
    }
    if let Some(terminal) = input.batch.terminal {
        bytes += size_of::<AnalysisToken>() + token_bytes(*terminal);
    }
    bytes
}

fn attributes() -> AnalyzedText {
    let mut input = Tokenizer::Whitespace
        .tokenize_with_offsets("韓🙂 UQA")
        .unwrap();
    let first = &mut input.batch.tokens[0];
    first.term = TokenTerm::from_utf16(vec![0xd800, 0xd83d, 0xde42, 0xdc00]);
    first.verbatim = false;
    first.keyword = true;
    first.position_increment = 3;
    first.position_length = 2;
    #[cfg(feature = "nori")]
    {
        use crate::nori::{KoreanMorphology, NoriMorpheme, NoriOrigin, POSTag, POSType};
        first.korean_morphology = Some(KoreanMorphology {
            pos_type: POSType::Compound,
            left_pos: POSTag::NNG,
            right_pos: POSTag::NNP,
            reading: Some("韓🙂 UQA".into()),
            morphemes: Some(vec![
                NoriMorpheme {
                    surface_utf16: vec![0xd800],
                    pos: POSTag::NNG,
                },
                NoriMorpheme {
                    surface_utf16: vec![0xdc00],
                    pos: POSTag::NNP,
                },
            ]),
            origin: NoriOrigin::User,
        });
        input.batch.tokens[1].korean_morphology = Some(KoreanMorphology {
            pos_type: POSType::Morpheme,
            left_pos: POSTag::NNG,
            right_pos: POSTag::NNG,
            reading: Some(String::new()),
            morphemes: Some(Vec::new()),
            origin: NoriOrigin::Known,
        });
    }
    input.batch.terminal = Some(Box::new(input.batch.tokens[0].clone()));
    input.batch.final_position_increment = 7;
    input
}

#[test]
fn independent_token_copies_reserve_all_owned_buffers_and_share_source_lifetime() {
    let budget = MemoryBudget::new(1 << 20);
    let filtered = CharFilter::HTMLStrip
        .filter_with_offsets_budgeted("<b>韓🙂 UQA</b>", &budget, &mut || Ok(()))
        .unwrap();
    let original = Tokenizer::Whitespace
        .tokenize_mapped_budgeted(&filtered, &budget, || Ok(()))
        .unwrap();
    let original_bytes = original.reserved_bytes();
    drop(filtered);
    let source_and_tokens = budget.used();
    let copied = original.clone_budgeted(&budget, || Ok(())).unwrap();
    assert_eq!(*copied, *original);
    assert_eq!(budget.used(), source_and_tokens + copied.reserved_bytes());
    assert!(!std::ptr::eq(
        copied.tokens().as_ptr(),
        original.tokens().as_ptr()
    ));
    assert!(!std::ptr::eq(
        copied.tokens()[0].term().as_str().unwrap().as_ptr(),
        original.tokens()[0].term().as_str().unwrap().as_ptr()
    ));
    #[cfg(feature = "nori")]
    assert!(std::sync::Arc::ptr_eq(
        &copied.projection,
        &original.projection
    ));
    drop(original);
    assert_eq!(
        budget.used(),
        source_and_tokens - original_bytes + copied.reserved_bytes()
    );
    assert_eq!(copied.tokens()[0].offsets().unwrap().utf8, 3..10);
    let (copied, memory) = copied.into_parts();
    assert_eq!(owned_bytes(copied), memory.bytes());
    assert_eq!(budget.used(), memory.bytes());
    drop(memory);
    assert_eq!(budget.used(), 0);
}

#[test]
fn raw_attributes_and_hidden_terminal_copies_own_exact_independent_reservations() {
    let input = attributes();
    let original = input.clone();
    let budget = MemoryBudget::new(1 << 20);
    let copied = input.clone_budgeted(&budget, || Ok(())).unwrap();
    assert_eq!(*copied, input);
    assert_eq!(budget.used(), copied.reserved_bytes());
    assert!(!std::ptr::eq(
        copied.batch.terminal.as_deref().unwrap(),
        input.batch.terminal.as_deref().unwrap()
    ));
    let (copied, memory) = copied.into_parts();
    assert_eq!(owned_bytes(copied), memory.bytes());
    assert_eq!(budget.used(), memory.bytes());
    drop(memory);
    assert_eq!(budget.used(), 0);
    assert_eq!(input, original);
    let token = input.tokens()[0]
        .clone_budgeted(&budget, || Ok(()))
        .unwrap();
    assert_eq!(*token, input.tokens()[0]);
    let (token, memory) = token.into_parts();
    assert_eq!(token_bytes(token), memory.bytes());
    drop(memory);
    assert_eq!(budget.used(), 0);
}

#[test]
fn every_copy_limit_and_cancellation_preserves_input_and_unrelated_owners() {
    let input = attributes();
    let original = input.clone();
    let baseline = MemoryBudget::new(1 << 20);
    let mut calls = 0;
    let expected = input
        .clone_budgeted(&baseline, || {
            calls += 1;
            Ok(())
        })
        .unwrap();
    for allowance in 0..=baseline.peak() {
        let budget = MemoryBudget::new(allowance + 7);
        let other = budget.reserve(7).unwrap();
        match input.clone_budgeted(&budget, || Ok(())) {
            Ok(copied) => assert_eq!(*copied, *expected),
            Err(AnalysisError::Memory(MemoryError::Limit { .. })) => {}
            result => panic!("allowance {allowance}: {result:?}"),
        }
        assert_eq!(budget.used(), 7);
        assert!(budget.peak() <= budget.limit());
        drop(other);
    }
    for stop in 1..=calls {
        let budget = MemoryBudget::new(1 << 20);
        let other = budget.reserve(7).unwrap();
        let mut count = 0;
        let result = input.clone_budgeted(&budget, || {
            count += 1;
            if count == stop {
                Err(AnalysisError::Cancelled)
            } else {
                Ok(())
            }
        });
        assert!(
            matches!(result, Err(AnalysisError::Cancelled)),
            "callback {stop}: {result:?}"
        );
        assert_eq!(budget.used(), 7);
        drop(other);
    }
    assert_eq!(input, original);
}

#[test]
fn copied_removal_terminal_preserves_final_gaps_and_upstream_attributes() {
    let input = Tokenizer::Whitespace
        .tokenize_with_offsets("retained a bb")
        .unwrap();
    let input = TokenFilter::Length {
        min_length: 4,
        max_length: 0,
    }
    .filter_analyzed(input)
    .unwrap();
    assert_eq!(input.batch.final_position_increment, 2);
    assert_eq!(
        input.batch.terminal.as_ref().unwrap().term.as_str(),
        Some("bb")
    );
    let budget = MemoryBudget::new(4096);
    let copied = input.clone_budgeted(&budget, || Ok(())).unwrap();
    assert_eq!(*copied, input);
    drop(input);
    assert_eq!(
        copied.batch.terminal.as_ref().unwrap().term.as_str(),
        Some("bb")
    );
    let (copied, memory) = copied.into_parts();
    assert_eq!(owned_bytes(copied), memory.bytes());
    drop(memory);
    assert_eq!(budget.used(), 0);
}

#[test]
fn reserved_substrings_match_original_scalar_raw_and_rewritten_source_attributes() {
    let mut inputs = attributes().into_tokens();
    inputs.extend(
        Tokenizer::Keyword
            .tokenize_with_offsets("a韓🙂z")
            .unwrap()
            .into_tokens(),
    );
    let mut rewritten = inputs.last().unwrap().clone();
    rewritten.replace_term("other🙂".into());
    inputs.push(rewritten);
    let budget = MemoryBudget::new(1 << 20);
    for input in inputs {
        let boundaries = input
            .term
            .boundaries_budgeted(&budget, &mut || Ok(()))
            .unwrap();
        let complete = *boundaries.last().unwrap();
        for (start, left) in boundaries.iter().enumerate() {
            for right in &boundaries[start..] {
                let expected = input.substring(left.offset..right.offset);
                let output = input
                    .substring_budgeted(*left, *right, complete, &budget, &mut || Ok(()))
                    .unwrap();
                assert_eq!(*output, expected);
                let (output, memory) = output.into_parts();
                assert_eq!(token_bytes(output), memory.bytes());
            }
        }
        drop(boundaries);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn synonym_rewrites_copy_only_replacement_terms_and_preserve_verbatim_equality() {
    let input = Tokenizer::Keyword
        .tokenize_with_offsets(&"A".repeat(8192))
        .unwrap();
    let token = &input.tokens()[0];
    let budget = MemoryBudget::new(64);
    let (term, memory) = crate::allocation::copy_text("x", &budget, &mut || Ok(()))
        .unwrap()
        .into_parts();
    let rewritten = token
        .rewrite_budgeted(Budgeted::new(term.into(), memory), &budget, &mut || Ok(()))
        .unwrap();
    assert_eq!(rewritten.term.as_str(), Some("x"));
    assert!(!rewritten.verbatim);
    drop(rewritten);
    assert_eq!(budget.used(), 0);
    let budget = MemoryBudget::new(1 << 20);
    let term = token.term.clone_budgeted(&budget, || Ok(())).unwrap();
    let same = token
        .rewrite_budgeted(term, &budget, &mut || Ok(()))
        .unwrap();
    assert_eq!(*same, *token);
    assert!(same.verbatim);
}
