//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::collections::BTreeMap;

use super::*;
use crate::TokenFilter;

fn compound_graph() -> AnalyzedText {
    let input = FilteredText::new("AB");
    let mut original = AnalysisToken::from_source(&input, 0..2).unwrap();
    original.position_length = 2;
    let mut first = AnalysisToken::from_source(&input, 0..1).unwrap();
    first.position_increment = 0;
    let second = AnalysisToken::from_source(&input, 1..2).unwrap();
    AnalyzedText::from_source(vec![original, first, second], &input).unwrap()
}

#[test]
fn filters_preserve_long_edges_and_shared_start_positions() {
    let synonym = TokenFilter::Synonym {
        synonyms: BTreeMap::from([("AB".into(), vec!["pair".into()])]),
        synonyms_path: None,
    };
    let stream = synonym.filter_analyzed(compound_graph()).unwrap();
    let stop = TokenFilter::Stop {
        language: "none".into(),
        custom_words: vec!["AB".into()],
    };
    let stream = stop.filter_analyzed(stream).unwrap();
    let stream = TokenFilter::Lowercase.filter_analyzed(stream).unwrap();
    assert_eq!(
        stream
            .tokens()
            .iter()
            .map(AnalysisToken::term)
            .collect::<Vec<_>>(),
        ["pair", "a", "b"]
    );
    assert_eq!(
        stream
            .tokens()
            .iter()
            .map(AnalysisToken::position_increment)
            .collect::<Vec<_>>(),
        [1, 0, 1]
    );
    assert_eq!(
        stream
            .tokens()
            .iter()
            .map(AnalysisToken::position_length)
            .collect::<Vec<_>>(),
        [2, 1, 1]
    );
    assert_eq!(stream.tokens()[0].offsets().unwrap().utf8, 0..2);
    assert_eq!(stream.tokens()[1].offsets().unwrap().utf8, 0..1);
}

#[test]
fn token_graph_validation_rejects_invalid_starts_and_edge_lengths() {
    let mut stream = compound_graph();
    stream.batch.tokens[0].position_increment = 0;
    assert!(matches!(
        stream.batch.validate_positions(),
        Err(AnalysisError::InvalidTokenPosition)
    ));
    stream.batch.tokens[0].position_increment = 1;
    stream.batch.tokens[0].position_length = 0;
    assert!(matches!(
        stream.batch.validate_positions(),
        Err(AnalysisError::InvalidTokenPosition)
    ));
    stream.batch.tokens[0].position_length = 2;
    stream.batch.tokens[0].position_increment = u32::MAX;
    assert!(matches!(
        stream.batch.validate_positions(),
        Err(AnalysisError::TokenPositionOverflow)
    ));
}

#[test]
fn removed_position_arithmetic_fails_instead_of_wrapping() {
    let mut stream = compound_graph();
    stream.batch.tokens[0].position_increment = u32::MAX;
    stream.batch.tokens[1].position_increment = 2;
    let stop = TokenFilter::Stop {
        language: "none".into(),
        custom_words: vec!["AB".into(), "A".into()],
    };
    assert!(matches!(
        stop.filter_analyzed(stream),
        Err(AnalysisError::TokenPositionOverflow)
    ));
}

#[test]
fn porter_stemming_preserves_keyword_tokens() {
    let input = FilteredText::new("running");
    let mut token = AnalysisToken::from_source(&input, 0..7).unwrap();
    token.keyword = true;
    let stream = AnalyzedText::from_source(vec![token], &input).unwrap();
    let stream = TokenFilter::PorterStem.filter_analyzed(stream).unwrap();
    assert_eq!(stream.tokens()[0].term(), "running");
    assert!(stream.tokens()[0].is_keyword());
}
