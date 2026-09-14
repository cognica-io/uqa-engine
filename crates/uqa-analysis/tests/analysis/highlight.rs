//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use uqa_analysis::{
    highlight, highlight_compiled, Analyzer, CharFilter, HighlightOptions, Tokenizer,
};

fn render(text: &str, query: &str, analyzer: &Analyzer) -> String {
    highlight(
        text,
        &[query.into()],
        Some(analyzer),
        &HighlightOptions::default(),
    )
    .unwrap()
}

#[test]
fn explicit_keyword_analysis_keeps_punctuation_and_complete_query_inputs() {
    let analyzer = uqa_analysis::keyword_analyzer();
    assert_eq!(render("c++", "c++", &analyzer), "<b>c++</b>");
    assert_eq!(render("new york", "new york", &analyzer), "<b>new york</b>");
    assert_eq!(render("new york", "new", &analyzer), "new york");
}

#[test]
fn source_character_edits_apply_before_matching_and_preserve_original_spelling() {
    let analyzer = Analyzer::new(
        Tokenizer::Whitespace,
        Vec::new(),
        vec![CharFilter::PatternReplace {
            pattern: "New York".into(),
            replacement: "NY".into(),
        }],
    );
    assert_eq!(
        render("visit New York today", "NY", &analyzer),
        "visit <b>New York</b> today"
    );
    assert_eq!(
        render("visit New York today", "New York", &analyzer),
        "visit <b>New York</b> today"
    );
}

#[test]
fn html_entities_and_multibyte_offsets_address_the_original_string() {
    let analyzer = Analyzer::new(
        Tokenizer::Whitespace,
        Vec::new(),
        vec![CharFilter::HTMLStrip],
    );
    assert_eq!(
        render("<i>경&amp;궁</i>", "경&궁", &analyzer),
        "<i><b>경&amp;궁</b></i>"
    );
}

#[test]
fn overlapping_grams_merge_and_partial_word_matches_keep_their_exact_span() {
    let analyzer = Analyzer::new(
        Tokenizer::NGram {
            min_gram: 2,
            max_gram: 3,
        },
        Vec::new(),
        Vec::new(),
    );
    assert_eq!(render("abcd", "abcd", &analyzer), "<b>abcd</b>");
    assert_eq!(render("abcde", "ab", &analyzer), "<b>ab</b>cde");
}

#[test]
fn fragments_keep_a_complete_match_even_when_smaller_than_the_match() {
    let analyzer = uqa_analysis::whitespace_analyzer().compile().unwrap();
    let options = HighlightOptions {
        max_fragments: 1,
        fragment_size: 1,
        ..Default::default()
    };
    for text in ["앞 경복궁 뒤", "앞쪽 경복궁 뒤쪽", "경복궁"] {
        let result = highlight_compiled(text, &["경복궁".into()], &analyzer, &options).unwrap();
        assert!(result.contains("<b>경복궁</b>"), "{result}");
    }
    let word_result = uqa_analysis::highlight::highlight_words(
        "before running after",
        &["running".into()],
        None,
        &options,
    )
    .unwrap();
    assert!(word_result.contains("<b>running</b>"), "{word_result}");
}

#[test]
fn compiled_revision_keeps_resolved_synonyms_after_source_file_changes() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("highlight-synonyms.txt");
    std::fs::write(&path, "nyc, metropolitan").unwrap();
    let config = Analyzer::new(
        Tokenizer::Whitespace,
        vec![uqa_analysis::TokenFilter::synonym_from_path(&path).unwrap()],
        Vec::new(),
    );
    let compiled = config.compile().unwrap();
    std::fs::write(&path, "nyc, urban").unwrap();
    assert_eq!(
        highlight_compiled(
            "nyc",
            &["metropolitan".into()],
            &compiled,
            &HighlightOptions::default()
        )
        .unwrap(),
        "<b>nyc</b>"
    );
    assert_eq!(render("nyc", "metropolitan", &config), "nyc");
}

#[cfg(feature = "nori")]
#[test]
fn korean_readings_highlight_hanja_and_user_entries_keep_punctuation() {
    let analyzer = uqa_analysis::nori::nori_analyzer();
    assert_eq!(render("韓國 경제", "한국", &analyzer), "<b>韓國</b> 경제");
    let analyzer = Analyzer::new(
        Tokenizer::Nori(uqa_analysis::nori::NoriTokenizerConfig {
            user_dictionary: Some("c++".into()),
            ..Default::default()
        }),
        Vec::new(),
        Vec::new(),
    );
    assert_eq!(render("c++ 개발", "c++", &analyzer), "<b>c++</b> 개발");
}

#[cfg(feature = "nori")]
#[test]
fn raw_surrogate_terms_keep_distinct_identity_and_cover_complete_source_scalars() {
    let analyzer = Analyzer::new(
        Tokenizer::Nori(uqa_analysis::nori::NoriTokenizerConfig {
            user_dictionary: Some("🙂a 가 나".into()),
            decompound_mode: uqa_analysis::nori::DecompoundMode::Discard,
            ..Default::default()
        }),
        Vec::new(),
        vec![CharFilter::HTMLStrip],
    );
    // The pinned Lucene user_split_surrogate_discard fixture assigns these raw terms UTF-16 ranges 1..2 and 2..3, covering adjacent original scalars rather than one overlapping span.
    assert_eq!(
        render("<i>🙂a</i> �a", "🙂a", &analyzer),
        "<i><b>🙂</b><b>a</b></i> �a"
    );
}

#[cfg(feature = "nori")]
#[test]
fn compound_paths_and_inflected_terms_highlight_original_spelling() {
    let mut analyzer = uqa_analysis::nori::nori_analyzer();
    let Tokenizer::Nori(config) = &mut analyzer.tokenizer else {
        unreachable!()
    };
    config.decompound_mode = uqa_analysis::nori::DecompoundMode::Mixed;
    assert_eq!(
        render("가락지나물", "가락지나물", &analyzer),
        "<b>가락지나물</b>"
    );
    assert_eq!(render("가락지나물", "나물", &analyzer), "가락지<b>나물</b>");
    // The pinned filters_4_discard_0 fixture maps the inflection's term 감싸이 to the complete source range 0..3 of 감싸여.
    let analyzer = uqa_analysis::nori::nori_analyzer();
    assert_eq!(render("감싸여", "감싸이", &analyzer), "<b>감싸여</b>");
}
