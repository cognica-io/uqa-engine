//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use uqa_analysis::nori::{
    KoreanAnalyzer, KoreanTokenizer, NoriOptions, NoriOutput, UserDictionary, UserDictionaryLimits,
};
use uqa_analysis::{AnalysisError, CharFilter, FilteredText, TokenFilter};

use super::nori_resources::model;

#[test]
fn generic_filters_preserve_unpaired_units_morphology_and_complete_word_stemming() {
    let tokenizer = KoreanTokenizer::new(model().clone(), None, NoriOptions::default()).unwrap();
    let mut token = tokenizer.tokenize("x").unwrap().tokens.remove(0);
    token.term_utf16 = [vec![0xd800], "RÚNNING".encode_utf16().collect()].concat();
    let input = FilteredText::new("x");
    let analyzed = NoriOutput::from_tokens(vec![token], 1, 3)
        .into_analyzed(&input)
        .unwrap();
    let morphology = analyzed.tokens()[0].korean_morphology().unwrap().clone();
    let mut filtered = analyzed;
    for filter in [
        TokenFilter::Lowercase,
        TokenFilter::ASCIIFolding,
        TokenFilter::PorterStem,
    ] {
        filtered = filter.filter_analyzed(filtered).unwrap();
    }
    assert_eq!(
        filtered.tokens()[0].term().utf16().as_ref(),
        [0xd800, 114, 117, 110]
    );
    assert_eq!(filtered.tokens()[0].korean_morphology(), Some(&morphology));
    assert_eq!(filtered.final_position_increment(), 3);
    assert!(matches!(
        filtered.clone().into_terms(),
        Err(AnalysisError::UnpairedTokenSurrogate { unit: 0xd800 })
    ));
    let grams = TokenFilter::Ngram {
        min_gram: 1,
        max_gram: 2,
        keep_short: false,
    }
    .filter_analyzed(filtered)
    .unwrap();
    assert_eq!(grams.tokens().len(), 7);
    assert_eq!(grams.tokens()[0].term().utf16().as_ref(), [0xd800]);
    assert_eq!(grams.tokens()[1].term().as_str(), Some("r"));
    for (index, token) in grams.tokens().iter().enumerate() {
        assert_eq!(token.position_increment(), u32::from(index == 0));
        assert_eq!(token.korean_morphology(), Some(&morphology));
        assert_eq!(token.offsets().unwrap().utf8, 0..1);
    }
    assert_eq!(grams.final_position_increment(), 3);
}

#[test]
fn native_mapped_analysis_keeps_user_surrogates_and_corrects_source_once() {
    let user =
        UserDictionary::compile("🙂a 가 나", model(), UserDictionaryLimits::default()).unwrap();
    let analyzer =
        KoreanAnalyzer::with_filters(model().clone(), user, NoriOptions::default(), &[]).unwrap();
    let filtered = CharFilter::HTMLStrip
        .filter_with_offsets("<b>🙂a</b>")
        .unwrap();
    let output = analyzer.analyze_mapped(&filtered).unwrap();
    assert_eq!(output.tokens()[0].term().utf16().as_ref(), [0xd83d]);
    assert_eq!(output.tokens()[1].term().utf16().as_ref(), [0xde42]);
    assert_eq!(output.tokens()[0].filtered_utf16(), Some(&(2..3)));
    assert_eq!(output.tokens()[0].offsets().unwrap().utf16, 4..5);
    assert_eq!(output.tokens()[0].offsets().unwrap().utf8, 3..7);
    assert_eq!(output.tokens()[1].offsets().unwrap().utf16, 5..6);
    assert_eq!(output.tokens()[1].offsets().unwrap().utf8, 7..8);
    assert_eq!(output.final_offsets().utf16, 10..10);
    assert_eq!(output.final_offsets().utf8, 12..12);
    let json = serde_json::to_value(&output).unwrap();
    assert_eq!(
        json["tokens"][0]["term"],
        serde_json::json!({"utf16": [55357]})
    );
    let lower = TokenFilter::Lowercase.filter_analyzed(output).unwrap();
    assert_eq!(lower.tokens()[1].term().utf16().as_ref(), [0xde42]);
}

#[test]
fn native_unicode_terms_keep_legacy_projection_and_exact_gram_source_ranges() {
    let analyzer = KoreanAnalyzer::new(model().clone(), None, NoriOptions::default()).unwrap();
    let output = analyzer.analyze_tokens("나물은").unwrap();
    assert_eq!(output.clone().into_terms().unwrap(), ["나물"]);
    let grams = TokenFilter::Ngram {
        min_gram: 1,
        max_gram: 1,
        keep_short: false,
    }
    .filter_analyzed(output)
    .unwrap();
    assert_eq!(grams.tokens()[0].offsets().unwrap().utf8, 0..3);
    assert_eq!(grams.tokens()[1].offsets().unwrap().utf8, 3..6);
    assert_eq!(grams.tokens()[1].filtered_utf16(), Some(&(1..2)));
    assert_eq!(grams.final_position_increment(), 1);
    assert!(matches!(
        analyzer
            .analyze("나물은")
            .unwrap()
            .into_analyzed(&FilteredText::new("다른길이")),
        Err(AnalysisError::MismatchedAnalysisInput { .. })
    ));
}
