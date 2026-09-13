//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::sync::Arc;

use serde_json::{json, Value};
use uqa_analysis::nori::{
    nori_analyzer, DecompoundMode, EmptyFilterConfig, KoreanAnalyzer, KoreanFilter, NoriPOSConfig,
    NoriTokenizerConfig, POSTag, SimpleLowercaseConfig, UserDictionary, UserDictionaryLimits,
};
use uqa_analysis::{
    AnalysisError, Analyzer, AnalyzerDescriptor, AnalyzerLimits, AnalyzerResources, CharFilter,
    TokenFilter, Tokenizer,
};

use super::nori_resources::model;

#[path = "nori_compiled/corpus.rs"]
mod corpus;
#[path = "nori_compiled/memory.rs"]
mod memory;
#[path = "nori_compiled/resources.rs"]
mod resources;

fn exact_dictionary() -> String {
    format!("sha256:{}", uqa_nori_data::BUNDLE_SHA256)
}

#[test]
fn default_korean_pipeline_freezes_explicit_resources_and_normalizes_independently() {
    let mut config = nori_analyzer();
    let original = serde_json::to_value(&config).unwrap();
    let resources = AnalyzerResources::new(AnalyzerLimits::default());
    let compiled = config.compile_with_resources(&resources).unwrap();
    let descriptor = compiled.descriptor();
    assert_eq!(
        descriptor.length_policy(),
        uqa_analysis::TokenLengthPolicy::DiscountOverlaps
    );
    assert_eq!(serde_json::to_value(&config).unwrap(), original);
    let wire: Value = serde_json::from_str(descriptor.canonical_json()).unwrap();
    let pipeline = &wire["descriptor"]["pipeline"];
    assert_eq!(pipeline["tokenizer"]["dictionary"], exact_dictionary());
    assert_eq!(pipeline["tokenizer"]["decompound_mode"], "discard");
    assert_eq!(pipeline["tokenizer"]["user_dictionary"], Value::Null);
    assert_eq!(
        pipeline["token_filters"][2]["unicode_profile"],
        exact_dictionary()
    );
    assert!(
        pipeline["token_filters"][0]["stop_tags"]
            .as_array()
            .unwrap()
            .len()
            > 20
    );
    let native = KoreanAnalyzer::new(
        model().clone(),
        None,
        uqa_analysis::nori::NoriOptions::default(),
    )
    .unwrap();
    for input in ["", "나물은", "喜悲哀歡 İ UQA", "세종시 한국어"] {
        assert_eq!(
            compiled.analyze_tokens(input).unwrap(),
            native.analyze_tokens(input).unwrap()
        );
        assert_eq!(
            compiled.normalize(input).unwrap(),
            native.normalize(input).unwrap()
        );
    }
    config.char_filters.push(CharFilter::PatternReplace {
        pattern: ".+".into(),
        replacement: "다른말".into(),
    });
    config.token_filters.clear();
    let changed = resources.compile(&config).unwrap();
    assert_eq!(
        changed.normalize("喜悲哀歡 İ UQA").unwrap(),
        "喜悲哀歡 i uqa"
    );
    assert_ne!(
        changed.analyze("喜悲哀歡 İ UQA").unwrap(),
        compiled.analyze("喜悲哀歡 İ UQA").unwrap()
    );
    assert!(Arc::ptr_eq(
        &compiled,
        &resources.restore_json(descriptor.canonical_json()).unwrap()
    ));
    assert!(matches!(
        resources
            .compile(&Analyzer::default())
            .unwrap()
            .normalize("x"),
        Err(AnalysisError::NormalizationUnavailable)
    ));
}

#[test]
fn compiled_character_edits_preserve_surrogate_splits_and_generic_stage_metadata() {
    let config = Analyzer::new(
        Tokenizer::Nori(NoriTokenizerConfig {
            user_dictionary: Some("🙂a 가 나".into()),
            decompound_mode: DecompoundMode::Mixed,
            ..Default::default()
        }),
        vec![
            TokenFilter::NoriReadingForm(EmptyFilterConfig::default()),
            TokenFilter::Lowercase,
        ],
        vec![CharFilter::HTMLStrip],
    );
    let compiled = config.compile().unwrap();
    let input = "<b>🙂a</b>";
    let mapped = CharFilter::HTMLStrip.filter_with_offsets(input).unwrap();
    let user =
        UserDictionary::compile("🙂a 가 나", model(), UserDictionaryLimits::default()).unwrap();
    let native = KoreanAnalyzer::with_filters(
        model().clone(),
        user,
        uqa_analysis::nori::NoriOptions {
            decompound_mode: DecompoundMode::Mixed,
            ..Default::default()
        },
        &[KoreanFilter::ReadingForm],
    )
    .unwrap();
    let expected = TokenFilter::Lowercase
        .filter_analyzed(native.analyze_mapped(&mapped).unwrap())
        .unwrap();
    assert_eq!(compiled.analyze_tokens(input).unwrap(), expected);
    assert!(matches!(
        compiled.analyze(input),
        Err(AnalysisError::UnpairedTokenSurrogate { .. })
    ));
    let reopened = AnalyzerResources::new(AnalyzerLimits::default())
        .restore_json(compiled.descriptor().canonical_json())
        .unwrap();
    assert_eq!(reopened.analyze_tokens(input).unwrap(), expected);
    assert!(expected
        .tokens()
        .iter()
        .any(|token| token.position_length() > 1));
    for token in expected.tokens() {
        assert!(input.get(token.offsets().unwrap().utf8.clone()).is_some());
    }
}

#[test]
fn common_number_composition_retains_source_spans_and_absent_morphology() {
    let mut config = Analyzer::new(
        Tokenizer::Whitespace,
        vec![
            TokenFilter::NoriPartOfSpeech(NoriPOSConfig {
                stop_tags: Some(vec![POSTag::SP]),
            }),
            TokenFilter::NoriNumber(EmptyFilterConfig::default()),
            TokenFilter::NoriReadingForm(EmptyFilterConfig::default()),
            TokenFilter::UnicodeSimpleLowercase(SimpleLowercaseConfig::default()),
        ],
        Vec::new(),
    );
    let compiled = config.compile().unwrap();
    let input = "３ ． ２ 천 원 15 , 7";
    let output = compiled.analyze_tokens(input).unwrap();
    assert_eq!(output.clone().into_terms().unwrap(), ["3200", "원", "157"]);
    assert_eq!(output.tokens()[0].offsets().unwrap().utf16, 0..7);
    assert!(output
        .tokens()
        .iter()
        .all(|token| token.korean_morphology().is_none()));
    assert_eq!(config.analyze_tokens(input).unwrap(), output);
    assert_eq!(
        TokenFilter::NoriNumber(EmptyFilterConfig::default())
            .filter(input.split_whitespace().map(str::to_owned).collect())
            .unwrap(),
        ["3200", "원", "157"]
    );
    config.char_filters.push(CharFilter::PatternReplace {
        pattern: "한국".into(),
        replacement: "韓國".into(),
    });
    config.tokenizer = Tokenizer::Nori(NoriTokenizerConfig::default());
    config.token_filters.push(TokenFilter::Ngram {
        min_gram: 1,
        max_gram: 1,
        keep_short: false,
    });
    let output = config.compile().unwrap().analyze_tokens("한국").unwrap();
    assert_eq!(output.clone().into_terms().unwrap(), ["한", "국"]);
    assert_eq!(output.tokens()[0].offsets().unwrap().utf8, 0..3);
    assert_eq!(output.tokens()[1].offsets().unwrap().utf8, 3..6);
}

#[test]
fn new_component_json_rejects_unknown_properties_and_invalid_enum_values() {
    for tokenizer in [
        json!({"type":"nori_tokenizer","unknown":true}),
        json!({"type":"nori_tokenizer","decompound_mode":"MIXED"}),
        json!({"type":"nori_tokenizer","discard_punctuation":0}),
    ] {
        assert!(serde_json::from_value::<Analyzer>(json!({"tokenizer":tokenizer})).is_err());
    }
    for filter in [
        json!({"type":"nori_part_of_speech","stop_tags":["unknown"]}),
        json!({"type":"nori_part_of_speech","unknown":true}),
        json!({"type":"nori_readingform","unknown":true}),
        json!({"type":"nori_number","unknown":true}),
        json!({"type":"unicode_simple_lowercase","unknown":true}),
    ] {
        assert!(serde_json::from_value::<Analyzer>(json!({"token_filters":[filter]})).is_err());
    }
    let config: Analyzer = serde_json::from_value(json!({"tokenizer":{"type":"nori_tokenizer"}, "token_filters":[{"type":"nori_part_of_speech"}, {"type":"nori_readingform"}, {"type":"unicode_simple_lowercase"}]})).unwrap();
    assert_eq!(
        serde_json::to_value(&config).unwrap(),
        serde_json::to_value(nori_analyzer()).unwrap()
    );
    assert!(config.uses_korean_stages());
    assert!(!Analyzer::default().uses_korean_stages());
}
