//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::inverted_index::analyze_index_field;
use uqa_analysis::{
    Analyzer, AnalyzerLimits, AnalyzerResources, CharFilter, TokenFilter, TokenLengthPolicy,
    Tokenizer,
};
use uqa_core::TokenOffsets;

#[test]
fn retained_fields_keep_graph_overlaps_normalization_and_original_end_state() {
    let config: Analyzer = serde_json::from_str(
        r#"{"tokenizer":{"type":"whitespace"},"token_filters":[{"type":"synonym","synonyms":{"a":["a","a"]}}]}"#,
    ).unwrap();
    for (policy, length) in [
        (TokenLengthPolicy::EmittedTokens, 6),
        (TokenLengthPolicy::DiscountOverlaps, 2),
    ] {
        let analyzer = AnalyzerResources::new(AnalyzerLimits::default())
            .compile_with_length_policy(&config, policy)
            .unwrap();
        let memory = MemoryBudget::new(1 << 20);
        let field = analyze_index_field_budgeted(&analyzer, "a a", &memory, || Ok(())).unwrap();
        assert_eq!(*field, analyze_index_field(&analyzer, "a a").unwrap());
        assert_eq!(field.length, length);
        assert_eq!(
            field.terms[&TokenTermKey::from_text("a")]
                .iter()
                .map(|occurrence| occurrence.position)
                .collect::<Vec<_>>(),
            [0, 0, 0, 1, 1, 1]
        );
        assert_eq!(field.final_offsets.end_utf8, 3);
        assert_eq!(field.final_offsets.end_utf16, 3);
        assert_eq!(field.final_position_increment, 0);
        assert_eq!(memory.used(), field.reserved_bytes());
        drop(field);
        assert_eq!(memory.used(), 0);
    }
    let analyzer = Analyzer::new(
        Tokenizer::Whitespace,
        vec![TokenFilter::Stop {
            language: "none".into(),
            custom_words: vec!["the".into()],
        }],
        vec![CharFilter::HTMLStrip],
    )
    .compile()
    .unwrap();
    let text = "<i>韓&amp;🙂 a the</i>";
    let memory = MemoryBudget::new(1 << 20);
    let field = analyze_index_field_budgeted(&analyzer, text, &memory, || Ok(())).unwrap();
    assert_eq!(*field, analyze_index_field(&analyzer, text).unwrap());
    assert_eq!(field.length, 2);
    assert_eq!(field.final_position_increment, 1);
    assert_eq!(
        field.final_offsets.end_utf8,
        u64::try_from(text.len()).unwrap()
    );
    assert_eq!(
        field.final_offsets.end_utf16,
        u64::try_from(text.encode_utf16().count()).unwrap()
    );
    assert_eq!(
        field.terms[&TokenTermKey::from_text("韓&🙂")][0].offsets,
        Some(TokenOffsets {
            start_utf8: 3,
            end_utf8: 15,
            start_utf16: 3,
            end_utf16: 11
        })
    );
    drop(field);
    assert_eq!(memory.used(), 0);
    for text in ["", "the"] {
        let field = analyze_index_field_budgeted(&analyzer, text, &memory, || Ok(())).unwrap();
        assert!(field.terms.is_empty());
        assert_eq!(field.length, 0);
        assert_eq!(field.final_position_increment, u32::from(!text.is_empty()));
        assert_eq!(*field, analyze_index_field(&analyzer, text).unwrap());
        drop(field);
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn field_quota_rejection_releases_analysis_projection_and_map_conversion_scratch() {
    let analyzer = uqa_analysis::whitespace_analyzer().compile().unwrap();
    let text = "a a a β β γ 🙂";
    let expected = analyze_index_field(&analyzer, text).unwrap();
    let baseline = MemoryBudget::new(1 << 20);
    let field = analyze_index_field_budgeted(&analyzer, text, &baseline, || Ok(())).unwrap();
    let live = field.terms.len() * size_of::<(TokenTermKey, Vec<TokenOccurrence>)>()
        + field
            .terms
            .iter()
            .map(|(key, values)| {
                key.as_bytes().len() + values.capacity() * size_of::<TokenOccurrence>()
            })
            .sum::<usize>();
    assert_eq!(field.reserved_bytes(), live);
    assert_eq!(baseline.used(), live);
    let peak = baseline.peak();
    drop(field);
    assert_eq!(baseline.used(), 0);
    for limit in [0, 1, 32, 128, peak / 2, peak - 1, peak] {
        let memory = MemoryBudget::new(limit + 7);
        let prior = memory.reserve(7).unwrap();
        match analyze_index_field_budgeted(&analyzer, text, &memory, || Ok(())) {
            Ok(field) => {
                assert_eq!(*field, expected);
                assert_eq!(memory.used(), field.reserved_bytes() + 7);
            }
            Err(StorageBackendError::Memory(MemoryError::Limit { .. })) => {}
            result => panic!("unexpected field result: {result:?}"),
        }
        assert_eq!(memory.used(), 7);
        assert!(memory.peak() <= memory.limit());
        drop(prior);
    }
}

#[test]
fn cancelling_any_analysis_or_projection_callback_releases_every_partial_field() {
    let analyzer = uqa_analysis::whitespace_analyzer().compile().unwrap();
    let text = "a a a beta beta gamma";
    let memory = MemoryBudget::new(1 << 20);
    let mut callbacks = 0;
    drop(
        analyze_index_field_budgeted(&analyzer, text, &memory, || {
            callbacks += 1;
            Ok(())
        })
        .unwrap(),
    );
    assert_eq!(memory.used(), 0);
    for stop in 1..=callbacks {
        let prior = memory.reserve(7).unwrap();
        let mut count = 0;
        let result = analyze_index_field_budgeted(&analyzer, text, &memory, || {
            count += 1;
            if count == stop {
                Err(AnalysisError::Cancelled)
            } else {
                Ok(())
            }
        });
        assert!(
            matches!(result, Err(StorageBackendError::Cancelled(_))),
            "callback={stop}: {result:?}"
        );
        assert_eq!(memory.used(), 7);
        drop(prior);
    }
}

#[test]
fn retained_raw_term_keys_keep_unpaired_utf16_identity_and_occurrence_order() {
    let memory = MemoryBudget::new(4096);
    let term = TokenTerm::from_utf16(vec![0xd800, 97, 0xdc00]);
    let mut terms = Terms::new(&memory);
    for position in [2, 7] {
        terms
            .push(
                &term,
                TokenOccurrence {
                    position,
                    position_length: 2,
                    offsets: None,
                },
                &mut || Ok(()),
            )
            .unwrap();
    }
    let analyzer = uqa_analysis::whitespace_analyzer().compile().unwrap();
    let empty = analyze_index_field(&analyzer, "").unwrap();
    let mut metadata = IndexedFieldMetadata::new(&analyzer, &empty);
    metadata.length = 2;
    let field = terms.finish(metadata, &mut || Ok(())).unwrap();
    assert_eq!(field.terms.len(), 1);
    assert_eq!(
        field.terms[&TokenTermKey::from_term(&term)]
            .iter()
            .map(|occurrence| occurrence.position)
            .collect::<Vec<_>>(),
        [2, 7]
    );
    assert_eq!(field.terms.keys().next().unwrap().to_term(), term);
    assert_eq!(memory.used(), field.reserved_bytes());
    drop(field);
    assert_eq!(memory.used(), 0);
}
