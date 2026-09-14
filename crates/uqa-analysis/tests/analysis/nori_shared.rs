//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use uqa_analysis::nori::{
    DictionaryError, KoreanFilter, KoreanTokenizer, NoriLimits, NoriOptions, NoriOutput,
};
use uqa_analysis::{AnalysisError, Analyzer, CharFilter, TokenFilter, Tokenizer};

use super::nori_resources::model;

#[test]
fn generic_tokens_keep_absent_morphology_through_all_korean_filters() {
    let input = Tokenizer::Whitespace
        .tokenize_with_offsets("UQA 韓國 İ 𐐀")
        .unwrap();
    for filter in [
        KoreanFilter::PartOfSpeech { stop_tags: None },
        KoreanFilter::ReadingForm,
    ] {
        assert_eq!(
            filter.filter_analyzed(input.clone(), model()).unwrap(),
            input
        );
    }
    let lower = KoreanFilter::SimpleLowercase
        .filter_analyzed(input, model())
        .unwrap();
    assert_eq!(
        lower.clone().into_terms().unwrap(),
        ["uqa", "韓國", "i", "𐐨"]
    );
    assert!(lower
        .tokens()
        .iter()
        .all(|token| token.korean_morphology().is_none()));

    let numbers = KoreanFilter::Number
        .filter_analyzed(
            Tokenizer::Whitespace
                .tokenize_with_offsets("３ ． ２ 천 원 15 , 7")
                .unwrap(),
            model(),
        )
        .unwrap();
    assert_eq!(numbers.clone().into_terms().unwrap(), ["3200", "원", "157"]);
    assert_eq!(numbers.tokens()[0].offsets().unwrap().utf16, 0..7);
    assert_eq!(numbers.tokens()[0].offsets().unwrap().utf8, 0..15);
    assert!(numbers
        .tokens()
        .iter()
        .all(|token| token.korean_morphology().is_none()));
    assert_eq!(numbers.final_offsets().utf16, 16..16);
}

#[test]
fn number_composition_projects_raw_spans_after_source_and_filter_borrows_end() {
    let (generic, expected) = {
        let source = String::from("12XX");
        let filtered = CharFilter::PatternReplace {
            pattern: "XX".into(),
            replacement: String::new(),
        }
        .filter_with_offsets(&source)
        .unwrap();
        let tokenizer =
            KoreanTokenizer::new(model().clone(), None, NoriOptions::default()).unwrap();
        let mut first = tokenizer.tokenize("1").unwrap().tokens.remove(0);
        first.term_utf16.clear();
        first.start_utf16 = 2;
        first.end_utf16 = 2;
        let mut last = first.clone();
        last.term_utf16 = vec![u16::from(b'1')];
        last.start_utf16 = 0;
        let output = NoriOutput::from_tokens(vec![first, last], 2, 5);
        let generic = output.clone().into_analyzed(&filtered).unwrap();
        assert_eq!(generic.tokens()[0].offsets().unwrap().utf16, 4..4);
        assert_eq!(generic.tokens()[1].offsets().unwrap().utf16, 0..2);
        let expected = KoreanFilter::Number
            .apply(output, model())
            .unwrap()
            .into_analyzed(&filtered)
            .unwrap();
        (generic, expected)
    };
    let result = KoreanFilter::Number
        .filter_analyzed(generic, model())
        .unwrap();
    assert_eq!(result, expected);
    assert_eq!(result.clone().into_terms().unwrap(), ["1"]);
    assert_eq!(result.tokens()[0].filtered_utf16(), Some(&(2..2)));
    assert_eq!(result.tokens()[0].offsets().unwrap().utf16, 4..4);
    assert_eq!(result.final_position_increment(), 5);
}

#[test]
fn rewritten_terms_matching_the_original_source_recover_precise_gram_spans() {
    for (source, replacement, filter) in [
        ("12", "１２", KoreanFilter::Number),
        ("한국", "韓國", KoreanFilter::ReadingForm),
    ] {
        let generic = {
            let original = source.to_owned();
            let filtered = CharFilter::PatternReplace {
                pattern: source.into(),
                replacement: replacement.into(),
            }
            .filter_with_offsets(&original)
            .unwrap();
            KoreanTokenizer::new(model().clone(), None, NoriOptions::default())
                .unwrap()
                .tokenize(filtered.as_str())
                .unwrap()
                .into_analyzed(&filtered)
                .unwrap()
        };
        let rewritten = filter.filter_analyzed(generic, model()).unwrap();
        assert_eq!(rewritten.clone().into_terms().unwrap(), [source]);
        let grams = TokenFilter::Ngram {
            min_gram: 1,
            max_gram: 1,
            keep_short: false,
        }
        .filter_analyzed(rewritten)
        .unwrap();
        let width = source.len() / 2;
        assert_eq!(grams.tokens()[0].offsets().unwrap().utf8, 0..width);
        assert_eq!(
            grams.tokens()[1].offsets().unwrap().utf8,
            width..source.len()
        );
        assert_eq!(grams.tokens()[1].offsets().unwrap().utf16, 1..2);
        assert_eq!(grams.tokens()[1].filtered_utf16(), Some(&(1..2)));
    }
}

#[test]
fn generic_stops_preserve_numeric_lookahead_holes_and_exhaustion() {
    let input = Analyzer::new(
        Tokenizer::Whitespace,
        vec![TokenFilter::Stop {
            language: "english".into(),
            custom_words: Vec::new(),
        }],
        Vec::new(),
    )
    .analyze_tokens("12 the 원 and")
    .unwrap();
    let result = KoreanFilter::Number
        .filter_analyzed(input, model())
        .unwrap();
    assert_eq!(result.clone().into_terms().unwrap(), ["12", "원"]);
    assert_eq!(result.tokens()[0].position_increment(), 2);
    assert_eq!(result.tokens()[1].position_increment(), 2);
    assert_eq!(result.final_position_increment(), 1);
    assert_eq!(result.final_offsets().utf16, 12..12);
    let grams = TokenFilter::Ngram {
        min_gram: 1,
        max_gram: 1,
        keep_short: false,
    }
    .filter_analyzed(result)
    .unwrap();
    assert_eq!(grams.tokens()[0].offsets().unwrap().utf8, 0..1);
    assert_eq!(grams.tokens()[1].offsets().unwrap().utf8, 1..2);
}

#[test]
fn shared_filters_enforce_limits_and_propagate_cancellation_without_poisoning_reuse() {
    let input = Tokenizer::Keyword
        .tokenize_with_offsets(&"1".repeat(8192))
        .unwrap();
    for filter in [
        KoreanFilter::PartOfSpeech { stop_tags: None },
        KoreanFilter::ReadingForm,
        KoreanFilter::SimpleLowercase,
        KoreanFilter::Number,
    ] {
        let expected = filter.filter_analyzed(input.clone(), model()).unwrap();
        assert!(matches!(
            filter.filter_analyzed_controlled(
                input.clone(),
                model(),
                NoriLimits::default(),
                &mut || Err(AnalysisError::Cancelled)
            ),
            Err(AnalysisError::Cancelled)
        ));
        for limits in [
            NoriLimits {
                max_tokens: 0,
                ..NoriLimits::default()
            },
            NoriLimits {
                max_output_utf16: 1,
                ..NoriLimits::default()
            },
        ] {
            assert!(matches!(
                filter.filter_analyzed_controlled(input.clone(), model(), limits, &mut || Ok(())),
                Err(AnalysisError::Dictionary(DictionaryError::Limit { .. }))
            ));
        }
        assert_eq!(
            filter.filter_analyzed(input.clone(), model()).unwrap(),
            expected
        );
    }
    for filter in [KoreanFilter::SimpleLowercase, KoreanFilter::Number] {
        let mut polls = 0;
        assert!(matches!(
            filter.filter_analyzed_controlled(
                input.clone(),
                model(),
                NoriLimits::default(),
                &mut || {
                    polls += 1;
                    if polls == 4 {
                        Err(AnalysisError::Cancelled)
                    } else {
                        Ok(())
                    }
                }
            ),
            Err(AnalysisError::Cancelled)
        ));
        assert_eq!(polls, 4);
    }
}

#[test]
fn shared_filter_input_limits_use_filtered_coordinates() {
    let input = Analyzer::new(
        Tokenizer::Keyword,
        Vec::new(),
        vec![CharFilter::PatternReplace {
            pattern: "7".into(),
            replacement: "1111".into(),
        }],
    )
    .analyze_tokens("7")
    .unwrap();
    assert_eq!(input.final_offsets().utf16, 1..1);
    assert!(matches!(
        KoreanFilter::Number.filter_analyzed_controlled(
            input,
            model(),
            NoriLimits {
                max_input_utf16: 2,
                ..NoriLimits::default()
            },
            &mut || Ok(())
        ),
        Err(AnalysisError::Dictionary(DictionaryError::Limit { .. }))
    ));
}
