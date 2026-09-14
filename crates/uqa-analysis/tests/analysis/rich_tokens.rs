//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use uqa_analysis::{AnalyzedText, SourceOffsets};

use super::*;

fn terms(text: &AnalyzedText) -> Vec<&str> {
    text.tokens()
        .iter()
        .map(|token| token.term().as_str().unwrap())
        .collect()
}

fn increments(text: &AnalyzedText) -> Vec<u32> {
    text.tokens()
        .iter()
        .map(uqa_analysis::AnalysisToken::position_increment)
        .collect()
}

#[test]
fn source_spans_and_gaps_survive_removal_and_duplicate_synonym_edges() {
    let input = "<p>The CAR and bike the</p>";
    let analyzer = Analyzer::new(
        Tokenizer::Standard,
        vec![
            TokenFilter::Lowercase,
            TokenFilter::Stop {
                language: "english".into(),
                custom_words: vec![],
            },
            TokenFilter::Synonym {
                synonyms: BTreeMap::from([(
                    "car".into(),
                    vec!["automobile".into(), "auto".into(), "auto".into()],
                )]),
                synonyms_path: None,
            },
            TokenFilter::Length {
                min_length: 0,
                max_length: 4,
            },
        ],
        vec![CharFilter::HTMLStrip],
    );
    let result = analyzer.analyze_tokens(input).unwrap();
    assert_eq!(terms(&result), ["car", "auto", "auto", "bike"]);
    assert_eq!(increments(&result), [2, 0, 0, 2]);
    for token in &result.tokens()[..3] {
        assert_eq!(token.offsets().unwrap().utf8, 7..10);
        assert_eq!(&input[token.offsets().unwrap().utf8.clone()], "CAR");
        assert_eq!(token.position_length(), 1);
    }
    assert_eq!(result.tokens()[3].offsets().unwrap().utf8, 15..19);
    assert_eq!(result.final_position_increment(), 1);
    assert_eq!(result.final_offsets().utf8, input.len()..input.len());

    let stopped = TokenFilter::Stop {
        language: "none".into(),
        custom_words: vec!["car".into()],
    }
    .filter_analyzed(result)
    .unwrap();
    assert_eq!(terms(&stopped), ["auto", "auto", "bike"]);
    assert_eq!(increments(&stopped), [2, 0, 2]);
    assert_eq!(stopped.final_position_increment(), 1);
}

#[test]
fn token_filter_grams_stack_at_the_input_position_with_exact_unicode_spans() {
    let analyzer = Analyzer::new(
        Tokenizer::Whitespace,
        vec![TokenFilter::Ngram {
            min_gram: 2,
            max_gram: 3,
            keep_short: false,
        }],
        vec![],
    );
    let result = analyzer.analyze_tokens("한🙂글 x").unwrap();
    assert_eq!(terms(&result), ["한🙂", "🙂글", "한🙂글"]);
    assert_eq!(increments(&result), [1, 0, 0]);
    let expected = [
        SourceOffsets {
            utf8: 0..7,
            utf16: 0..3,
        },
        SourceOffsets {
            utf8: 3..10,
            utf16: 1..4,
        },
        SourceOffsets {
            utf8: 0..10,
            utf16: 0..4,
        },
    ];
    for (token, offsets) in result.tokens().iter().zip(expected.iter()) {
        assert_eq!(token.offsets(), Some(offsets));
    }
    assert_eq!(result.final_position_increment(), 1);
    assert_eq!(
        result.final_offsets(),
        &SourceOffsets {
            utf8: 12..12,
            utf16: 6..6
        }
    );
}

#[test]
fn rewritten_token_grams_retain_the_complete_source_span() {
    let analyzer = Analyzer::new(
        Tokenizer::Keyword,
        vec![
            TokenFilter::Lowercase,
            TokenFilter::PorterStem,
            TokenFilter::Ngram {
                min_gram: 2,
                max_gram: 2,
                keep_short: false,
            },
        ],
        vec![],
    );
    let result = analyzer.analyze_tokens("RUNNING").unwrap();
    assert_eq!(terms(&result), ["ru", "un"]);
    assert_eq!(increments(&result), [1, 0]);
    for token in result.tokens() {
        assert_eq!(token.offsets().unwrap().utf8, 0..7);
    }
}

#[test]
fn repeated_words_keep_their_individual_source_locations() {
    let result = Tokenizer::Whitespace
        .tokenize_with_offsets(" 한🙂\t한🙂 ")
        .unwrap();
    assert_eq!(terms(&result), ["한🙂", "한🙂"]);
    assert_eq!(
        result.tokens()[0].offsets(),
        Some(&SourceOffsets {
            utf8: 1..8,
            utf16: 1..4
        })
    );
    assert_eq!(
        result.tokens()[1].offsets(),
        Some(&SourceOffsets {
            utf8: 9..16,
            utf16: 5..8
        })
    );
    assert_eq!(
        result.final_offsets(),
        &SourceOffsets {
            utf8: 17..17,
            utf16: 9..9
        }
    );

    let standard = Tokenizer::Standard
        .tokenize_with_offsets("서울 서울")
        .unwrap();
    assert_eq!(
        standard.tokens()[1].offsets(),
        Some(&SourceOffsets {
            utf8: 7..13,
            utf16: 3..5
        })
    );
}

#[test]
fn tokenizer_grams_have_independent_positions_and_source_spans() {
    let result = Tokenizer::NGram {
        min_gram: 2,
        max_gram: 3,
    }
    .tokenize_with_offsets("한🙂글")
    .unwrap();
    assert_eq!(terms(&result), ["한🙂", "🙂글", "한🙂글"]);
    assert_eq!(increments(&result), [1, 1, 1]);
    assert_eq!(
        result.tokens()[1].offsets(),
        Some(&SourceOffsets {
            utf8: 3..10,
            utf16: 1..4
        })
    );
}

#[test]
fn pattern_and_letter_tokenizers_report_their_actual_ranges() {
    let result = Tokenizer::Pattern {
        pattern: "[,;]+".into(),
    }
    .tokenize_with_offsets("한,한;;🙂")
    .unwrap();
    assert_eq!(terms(&result), ["한", "한", "🙂"]);
    assert_eq!(
        result.tokens()[1].offsets(),
        Some(&SourceOffsets {
            utf8: 4..7,
            utf16: 2..3
        })
    );
    assert_eq!(
        result.tokens()[2].offsets(),
        Some(&SourceOffsets {
            utf8: 9..13,
            utf16: 5..7
        })
    );
    let letters = Tokenizer::Letter.tokenize_with_offsets("a1 b2").unwrap();
    assert_eq!(terms(&letters), ["a", "b"]);
    assert_eq!(letters.tokens()[1].offsets().unwrap().utf8, 3..4);
}

#[test]
fn zero_width_tokenizer_separators_do_not_split_surrogates() {
    let result = Tokenizer::Pattern {
        pattern: String::new(),
    }
    .tokenize_with_offsets("한🙂")
    .unwrap();
    assert_eq!(terms(&result), ["한", "🙂"]);
    assert_eq!(
        result.tokens()[1].offsets(),
        Some(&SourceOffsets {
            utf8: 3..7,
            utf16: 1..3
        })
    );
}

#[test]
fn all_removed_tokens_preserve_trailing_positions_through_later_filters() {
    let analyzer = Analyzer::new(
        Tokenizer::Whitespace,
        vec![
            TokenFilter::Stop {
                language: "english".into(),
                custom_words: vec![],
            },
            TokenFilter::Ngram {
                min_gram: 2,
                max_gram: 3,
                keep_short: false,
            },
        ],
        vec![],
    );
    let result = analyzer.analyze_tokens("the and").unwrap();
    assert!(result.tokens().is_empty());
    assert_eq!(result.final_position_increment(), 2);
    assert_eq!(result.final_offsets().utf8, 7..7);
}

#[test]
fn configured_gram_maximum_is_bounded_by_actual_token_length() {
    let tokenizer = Tokenizer::NGram {
        min_gram: 2,
        max_gram: usize::MAX,
    };
    assert_eq!(tokenizer.tokenize("abc").unwrap(), ["ab", "bc", "abc"]);
    let filter = TokenFilter::Ngram {
        min_gram: 2,
        max_gram: usize::MAX,
        keep_short: false,
    };
    assert_eq!(
        filter.filter(vec!["abc".into()]).unwrap(),
        ["ab", "bc", "abc"]
    );
}

#[test]
fn edge_grams_preserve_skipped_positions_and_cover_unicode_prefixes() {
    let analyzer = Analyzer::new(
        Tokenizer::Whitespace,
        vec![TokenFilter::EdgeNgram {
            min_gram: 2,
            max_gram: 3,
        }],
        vec![],
    );
    let result = analyzer.analyze_tokens("x 한🙂글 y").unwrap();
    assert_eq!(terms(&result), ["한🙂", "한🙂글"]);
    assert_eq!(increments(&result), [2, 0]);
    assert_eq!(
        result.tokens()[0].offsets(),
        Some(&SourceOffsets {
            utf8: 2..9,
            utf16: 2..5
        })
    );
    assert_eq!(result.final_position_increment(), 1);
}

#[test]
fn complete_character_filter_chain_retains_original_token_spans() {
    let analyzer = Analyzer::new(
        Tokenizer::Standard,
        vec![TokenFilter::Lowercase],
        vec![
            CharFilter::HTMLStrip,
            CharFilter::PatternReplace {
                pattern: "서울".into(),
                replacement: "SEOUL".into(),
            },
        ],
    );
    let result = analyzer.analyze_tokens("<b>서울</b>").unwrap();
    assert_eq!(terms(&result), ["seoul"]);
    assert_eq!(
        result.tokens()[0].offsets(),
        Some(&SourceOffsets {
            utf8: 3..9,
            utf16: 3..5
        })
    );
    assert_eq!(
        result.final_offsets(),
        &SourceOffsets {
            utf8: 13..13,
            utf16: 9..9
        }
    );
}
