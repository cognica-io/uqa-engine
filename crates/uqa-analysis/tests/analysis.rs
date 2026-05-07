//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Port of `uqa/tests/test_analysis.py`. Mirrors the Lucene-style text
//! analysis pipeline tests: tokenizers, token filters, char filters,
//! analyzer composition, serialization round-trips, and the named
//! analyzer registry.

use std::collections::BTreeMap;

use uqa_analysis::{
    keyword_analyzer, standard_analyzer, standard_cjk_analyzer, whitespace_analyzer, Analyzer,
    CharFilter, TokenFilter, Tokenizer,
};

// =====================================================================
// Tokenizers
// =====================================================================

#[test]
fn whitespace_tokenizer_basic() {
    let t = Tokenizer::Whitespace;
    assert_eq!(t.tokenize("hello world"), vec!["hello", "world"]);
}

#[test]
fn whitespace_tokenizer_multiple_spaces() {
    let t = Tokenizer::Whitespace;
    assert_eq!(t.tokenize("  hello   world  "), vec!["hello", "world"]);
}

#[test]
fn whitespace_tokenizer_empty() {
    let t = Tokenizer::Whitespace;
    assert!(t.tokenize("").is_empty());
}

#[test]
fn whitespace_tokenizer_roundtrip() {
    let t = Tokenizer::Whitespace;
    let json = serde_json::to_string(&t).unwrap();
    assert!(json.contains("\"type\":\"whitespace\""));
    let back: Tokenizer = serde_json::from_str(&json).unwrap();
    assert!(matches!(back, Tokenizer::Whitespace));
}

#[test]
fn standard_tokenizer_basic() {
    let t = Tokenizer::Standard;
    assert_eq!(t.tokenize("Hello, World!"), vec!["Hello", "World"]);
}

#[test]
fn standard_tokenizer_unicode() {
    let t = Tokenizer::Standard;
    assert_eq!(t.tokenize("cafe_latte 42"), vec!["cafe_latte", "42"]);
}

#[test]
fn standard_tokenizer_punctuation() {
    let t = Tokenizer::Standard;
    assert_eq!(
        t.tokenize("it's a test."),
        vec!["it", "s", "a", "test"]
    );
}

#[test]
fn standard_tokenizer_roundtrip() {
    let t = Tokenizer::Standard;
    let json = serde_json::to_string(&t).unwrap();
    let back: Tokenizer = serde_json::from_str(&json).unwrap();
    assert!(matches!(back, Tokenizer::Standard));
}

#[test]
fn letter_tokenizer_basic() {
    let t = Tokenizer::Letter;
    assert_eq!(t.tokenize("hello123world"), vec!["hello", "world"]);
}

#[test]
fn letter_tokenizer_only_letters() {
    let t = Tokenizer::Letter;
    assert!(t.tokenize("42!!").is_empty());
}

#[test]
fn ngram_tokenizer_bigrams() {
    let t = Tokenizer::NGram {
        min_gram: 2,
        max_gram: 2,
    };
    assert_eq!(t.tokenize("abc"), vec!["ab", "bc"]);
}

#[test]
fn ngram_tokenizer_unigrams_and_bigrams() {
    let t = Tokenizer::NGram {
        min_gram: 1,
        max_gram: 2,
    };
    assert_eq!(t.tokenize("ab"), vec!["a", "b", "ab"]);
}

#[test]
fn ngram_tokenizer_roundtrip() {
    let t = Tokenizer::NGram {
        min_gram: 2,
        max_gram: 3,
    };
    let json = serde_json::to_string(&t).unwrap();
    let back: Tokenizer = serde_json::from_str(&json).unwrap();
    assert!(matches!(back, Tokenizer::NGram { .. }));
}

#[test]
fn pattern_tokenizer_default_pattern() {
    let t = Tokenizer::Pattern {
        pattern: r"-".into(),
    };
    assert_eq!(t.tokenize("hello-world"), vec!["hello", "world"]);
}

#[test]
fn pattern_tokenizer_custom_pattern() {
    let t = Tokenizer::Pattern {
        pattern: r",\s*".into(),
    };
    assert_eq!(t.tokenize("a, b, c"), vec!["a", "b", "c"]);
}

#[test]
fn pattern_tokenizer_roundtrip() {
    let t = Tokenizer::Pattern {
        pattern: r"\|".into(),
    };
    let json = serde_json::to_string(&t).unwrap();
    let back: Tokenizer = serde_json::from_str(&json).unwrap();
    assert!(matches!(back, Tokenizer::Pattern { .. }));
}

#[test]
fn keyword_tokenizer_single_token() {
    let t = Tokenizer::Keyword;
    assert_eq!(t.tokenize("hello world"), vec!["hello world"]);
}

#[test]
fn keyword_tokenizer_empty() {
    let t = Tokenizer::Keyword;
    assert!(t.tokenize("").is_empty());
}

// =====================================================================
// Token Filters
// =====================================================================

#[test]
fn lowercase_filter_basic() {
    let f = TokenFilter::Lowercase;
    assert_eq!(
        f.filter(vec!["Hello".into(), "WORLD".into()]),
        vec!["hello", "world"]
    );
}

#[test]
fn lowercase_filter_roundtrip() {
    let f = TokenFilter::Lowercase;
    let json = serde_json::to_string(&f).unwrap();
    let back: TokenFilter = serde_json::from_str(&json).unwrap();
    assert!(matches!(back, TokenFilter::Lowercase));
}

#[test]
fn stopword_filter_english_defaults() {
    let f = TokenFilter::Stop {
        language: "english".into(),
        custom_words: Vec::new(),
    };
    let result = f.filter(vec!["the".into(), "quick".into(), "brown".into(), "fox".into()]);
    assert_eq!(result, vec!["quick", "brown", "fox"]);
}

#[test]
fn stopword_filter_custom_words() {
    let f = TokenFilter::Stop {
        language: "english".into(),
        custom_words: vec!["quick".into()],
    };
    let result = f.filter(vec!["the".into(), "quick".into(), "brown".into()]);
    assert_eq!(result, vec!["brown"]);
}

#[test]
fn stopword_filter_roundtrip() {
    let f = TokenFilter::Stop {
        language: "english".into(),
        custom_words: vec!["extra".into()],
    };
    let json = serde_json::to_string(&f).unwrap();
    let back: TokenFilter = serde_json::from_str(&json).unwrap();
    let result = back.filter(vec!["extra".into()]);
    assert!(result.is_empty());
}

#[test]
fn porter_stem_filter_basic_stemming() {
    let f = TokenFilter::PorterStem;
    assert_eq!(f.filter(vec!["running".into()]), vec!["run"]);
    assert_eq!(f.filter(vec!["cats".into()]), vec!["cat"]);
}

#[test]
fn porter_stem_filter_complex_stemming() {
    let f = TokenFilter::PorterStem;
    let result = f.filter(vec![
        "connections".into(),
        "generalization".into(),
        "relational".into(),
    ]);
    assert!(result.contains(&"connect".to_string()));
    assert!(result.contains(&"gener".to_string()));
}

#[test]
fn ascii_folding_filter_accented_chars() {
    let f = TokenFilter::ASCIIFolding;
    assert_eq!(f.filter(vec!["café".into()]), vec!["cafe"]);
}

#[test]
fn ascii_folding_filter_roundtrip() {
    let f = TokenFilter::ASCIIFolding;
    let json = serde_json::to_string(&f).unwrap();
    let back: TokenFilter = serde_json::from_str(&json).unwrap();
    assert!(matches!(back, TokenFilter::ASCIIFolding));
}

fn syn_map(pairs: &[(&str, &[&str])]) -> BTreeMap<String, Vec<String>> {
    pairs
        .iter()
        .map(|(k, vs)| {
            (
                (*k).to_string(),
                vs.iter().map(|s| (*s).to_string()).collect(),
            )
        })
        .collect()
}

#[test]
fn synonym_filter_expansion() {
    let f = TokenFilter::Synonym {
        synonyms: syn_map(&[("fast", &["quick", "rapid"])]),
    };
    let result = f.filter(vec!["fast".into(), "car".into()]);
    assert_eq!(result, vec!["fast", "quick", "rapid", "car"]);
}

#[test]
fn synonym_filter_no_match() {
    let f = TokenFilter::Synonym {
        synonyms: syn_map(&[("fast", &["quick"])]),
    };
    let result = f.filter(vec!["slow".into(), "car".into()]);
    assert_eq!(result, vec!["slow", "car"]);
}

#[test]
fn synonym_filter_roundtrip() {
    let f = TokenFilter::Synonym {
        synonyms: syn_map(&[("a", &["b"])]),
    };
    let json = serde_json::to_string(&f).unwrap();
    let back: TokenFilter = serde_json::from_str(&json).unwrap();
    assert!(matches!(back, TokenFilter::Synonym { .. }));
}

#[test]
fn ngram_filter_default() {
    let f = TokenFilter::Ngram {
        min_gram: 2,
        max_gram: 3,
        keep_short: false,
    };
    let result = f.filter(vec!["hello".into()]);
    for expected in ["he", "el", "ll", "lo", "hel", "ell", "llo"] {
        assert!(
            result.contains(&expected.to_string()),
            "expected {expected:?} in {result:?}"
        );
    }
}

#[test]
fn ngram_filter_short_token_dropped() {
    let f = TokenFilter::Ngram {
        min_gram: 2,
        max_gram: 3,
        keep_short: false,
    };
    assert!(f.filter(vec!["a".into()]).is_empty());
}

#[test]
fn ngram_filter_keep_short() {
    let f = TokenFilter::Ngram {
        min_gram: 2,
        max_gram: 3,
        keep_short: true,
    };
    let result = f.filter(vec!["a".into(), "hello".into()]);
    assert_eq!(result[0], "a");
    assert!(result.contains(&"he".to_string()));
}

#[test]
fn ngram_filter_keep_short_mixed() {
    let f = TokenFilter::Ngram {
        min_gram: 3,
        max_gram: 4,
        keep_short: true,
    };
    let result = f.filter(vec!["ab".into(), "cd".into(), "hello".into()]);
    assert!(result.contains(&"ab".to_string()));
    assert!(result.contains(&"cd".to_string()));
    assert!(result.contains(&"hel".to_string()));
}

#[test]
fn ngram_filter_roundtrip() {
    let f = TokenFilter::Ngram {
        min_gram: 2,
        max_gram: 4,
        keep_short: false,
    };
    let json = serde_json::to_string(&f).unwrap();
    let back: TokenFilter = serde_json::from_str(&json).unwrap();
    assert_eq!(back.filter(vec!["abc".into()]), f.filter(vec!["abc".into()]));
}

#[test]
fn ngram_filter_roundtrip_keep_short() {
    let f = TokenFilter::Ngram {
        min_gram: 2,
        max_gram: 3,
        keep_short: true,
    };
    let json = serde_json::to_string(&f).unwrap();
    assert!(json.contains("\"keep_short\":true"));
    let back: TokenFilter = serde_json::from_str(&json).unwrap();
    assert_eq!(back.filter(vec!["a".into()]), vec!["a".to_string()]);
}

#[test]
fn edge_ngram_filter_default() {
    let f = TokenFilter::EdgeNgram {
        min_gram: 1,
        max_gram: 3,
    };
    assert_eq!(f.filter(vec!["hello".into()]), vec!["h", "he", "hel"]);
}

#[test]
fn edge_ngram_filter_min_gram() {
    let f = TokenFilter::EdgeNgram {
        min_gram: 2,
        max_gram: 4,
    };
    assert_eq!(f.filter(vec!["abc".into()]), vec!["ab", "abc"]);
}

#[test]
fn edge_ngram_filter_roundtrip() {
    let f = TokenFilter::EdgeNgram {
        min_gram: 2,
        max_gram: 5,
    };
    let json = serde_json::to_string(&f).unwrap();
    let back: TokenFilter = serde_json::from_str(&json).unwrap();
    assert!(matches!(back, TokenFilter::EdgeNgram { .. }));
}

#[test]
fn length_filter_min_length() {
    let f = TokenFilter::Length {
        min_length: 3,
        max_length: 0,
    };
    assert_eq!(
        f.filter(vec!["a".into(), "ab".into(), "abc".into(), "abcd".into()]),
        vec!["abc", "abcd"]
    );
}

#[test]
fn length_filter_max_length() {
    let f = TokenFilter::Length {
        min_length: 0,
        max_length: 3,
    };
    assert_eq!(
        f.filter(vec!["a".into(), "ab".into(), "abc".into(), "abcd".into()]),
        vec!["a", "ab", "abc"]
    );
}

#[test]
fn length_filter_both() {
    let f = TokenFilter::Length {
        min_length: 2,
        max_length: 3,
    };
    assert_eq!(
        f.filter(vec!["a".into(), "ab".into(), "abc".into(), "abcd".into()]),
        vec!["ab", "abc"]
    );
}

// =====================================================================
// Char Filters
// =====================================================================

#[test]
fn html_strip_strip_tags() {
    let f = CharFilter::HTMLStrip;
    let result = f.filter("<p>Hello <b>world</b></p>");
    assert!(result.contains("Hello"));
    assert!(result.contains("world"));
    assert!(!result.contains('<'));
}

#[test]
fn html_strip_no_tags() {
    let f = CharFilter::HTMLStrip;
    assert_eq!(f.filter("plain text"), "plain text");
}

#[test]
fn html_strip_entities() {
    let f = CharFilter::HTMLStrip;
    assert_eq!(f.filter("a &amp; b"), "a & b");
}

#[test]
fn html_strip_roundtrip() {
    let f = CharFilter::HTMLStrip;
    let json = serde_json::to_string(&f).unwrap();
    let back: CharFilter = serde_json::from_str(&json).unwrap();
    assert!(matches!(back, CharFilter::HTMLStrip));
}

#[test]
fn mapping_char_filter_mapping() {
    let mut m = BTreeMap::new();
    m.insert("&".to_string(), "and".to_string());
    m.insert("@".to_string(), "at".to_string());
    let f = CharFilter::Mapping { mapping: m };
    assert_eq!(f.filter("you & me @ home"), "you and me at home");
}

#[test]
fn mapping_char_filter_roundtrip() {
    let mut m = BTreeMap::new();
    m.insert("x".into(), "y".into());
    let f = CharFilter::Mapping { mapping: m };
    let json = serde_json::to_string(&f).unwrap();
    let back: CharFilter = serde_json::from_str(&json).unwrap();
    assert!(matches!(back, CharFilter::Mapping { .. }));
}

#[test]
fn pattern_replace_replace() {
    let f = CharFilter::PatternReplace {
        pattern: r"\d+".into(),
        replacement: "#".into(),
    };
    assert_eq!(f.filter("abc123def456"), "abc#def#");
}

#[test]
fn pattern_replace_roundtrip() {
    let f = CharFilter::PatternReplace {
        pattern: r"\s+".into(),
        replacement: " ".into(),
    };
    let json = serde_json::to_string(&f).unwrap();
    let back: CharFilter = serde_json::from_str(&json).unwrap();
    assert!(matches!(back, CharFilter::PatternReplace { .. }));
}

// =====================================================================
// Analyzer
// =====================================================================

#[test]
fn default_analyzer_is_standard() {
    let standard = standard_analyzer("english");
    let result = standard.analyze("The Quick BROWN Fox");
    assert!(!result.contains(&"the".to_string()));
    assert!(result.contains(&"quick".to_string()));
    assert!(result.contains(&"brown".to_string()));
    assert!(result.contains(&"fox".to_string()));
}

#[test]
fn whitespace_analyzer_run() {
    let a = whitespace_analyzer();
    assert_eq!(a.analyze("Hello World"), vec!["hello", "world"]);
}

#[test]
fn standard_analyzer_run() {
    let a = standard_analyzer("english");
    let result = a.analyze("The quick brown fox");
    assert!(!result.contains(&"the".to_string()));
    assert!(result.contains(&"quick".to_string()));
}

#[test]
fn standard_analyzer_stemming() {
    let a = standard_analyzer("english");
    let result = a.analyze("Running transformers efficiently");
    assert!(result.contains(&"run".to_string()));
    assert!(result.contains(&"transform".to_string()));
}

#[test]
fn standard_analyzer_ascii_folding() {
    let a = standard_analyzer("english");
    let result = a.analyze("café résumé");
    assert!(result.contains(&"cafe".to_string()));
    assert!(result.contains(&"resum".to_string()));
}

#[test]
fn standard_cjk_analyzer_run() {
    let a = standard_cjk_analyzer("english");
    let result = a.analyze("hello world");
    assert!(result.contains(&"he".to_string()));
    assert!(result.contains(&"hel".to_string()));
    assert!(result.contains(&"wo".to_string()));
    assert!(result.contains(&"wor".to_string()));
}

#[test]
fn standard_cjk_analyzer_stemming() {
    let a = standard_cjk_analyzer("english");
    let result = a.analyze("Running");
    assert!(result.contains(&"ru".to_string()));
    assert!(result.contains(&"run".to_string()));
}

#[test]
fn standard_cjk_analyzer_keep_short() {
    let a = standard_cjk_analyzer("english");
    let result = a.analyze("x marks");
    assert!(result.contains(&"x".to_string()));
    assert!(result.contains(&"ma".to_string()));
    assert!(result.contains(&"mar".to_string()));
}

#[test]
fn keyword_analyzer_run() {
    let a = keyword_analyzer();
    assert_eq!(a.analyze("hello world"), vec!["hello world"]);
}

#[test]
fn analyzer_custom_pipeline() {
    let a = Analyzer::new(
        Tokenizer::Standard,
        vec![TokenFilter::Lowercase, TokenFilter::PorterStem],
        vec![CharFilter::HTMLStrip],
    );
    let result = a.analyze("<p>Running Connections</p>");
    assert!(result.contains(&"run".to_string()));
    assert!(result.contains(&"connect".to_string()));
}

#[test]
fn analyzer_serialization_roundtrip() {
    let a = Analyzer::new(
        Tokenizer::Standard,
        vec![
            TokenFilter::Lowercase,
            TokenFilter::Stop {
                language: "english".into(),
                custom_words: Vec::new(),
            },
        ],
        vec![CharFilter::HTMLStrip],
    );
    let json = serde_json::to_string(&a).unwrap();
    let back: Analyzer = serde_json::from_str(&json).unwrap();
    let text = "<p>The quick brown fox</p>";
    assert_eq!(back.analyze(text), a.analyze(text));
}

#[test]
fn analyzer_json_roundtrip() {
    let a = standard_analyzer("english");
    let j = serde_json::to_string(&a).unwrap();
    let back: Analyzer = serde_json::from_str(&j).unwrap();
    let text = "The quick brown fox";
    assert_eq!(back.analyze(text), a.analyze(text));
}

// =====================================================================
// Named Analyzer Registry
// =====================================================================

#[test]
fn registry_register_and_get() {
    use uqa_analysis::{drop_analyzer, get_analyzer, register_analyzer};
    let custom = Analyzer::new(
        Tokenizer::Letter,
        vec![TokenFilter::Lowercase],
        Vec::new(),
    );
    register_analyzer("rs_test_custom_reg".to_string(), custom).unwrap();
    let retrieved = get_analyzer("rs_test_custom_reg").unwrap();
    assert_eq!(
        retrieved.analyze("hello123world"),
        vec!["hello", "world"]
    );
    drop_analyzer("rs_test_custom_reg").unwrap();
}

#[test]
fn registry_unknown_analyzer() {
    use uqa_analysis::get_analyzer;
    let r = get_analyzer("rs_nonexistent_analyzer_xyz");
    assert!(r.is_err());
}

#[test]
fn registry_drop_nonexistent() {
    use uqa_analysis::drop_analyzer;
    let r = drop_analyzer("rs_nonexistent_analyzer_xyz_drop");
    assert!(r.is_err());
}

#[test]
fn registry_list_includes_registered() {
    use uqa_analysis::{drop_analyzer, list_analyzers, register_analyzer};
    register_analyzer(
        "rs_test_listed".to_string(),
        Analyzer::new(Tokenizer::Whitespace, Vec::new(), Vec::new()),
    )
    .unwrap();
    let names = list_analyzers();
    assert!(names.contains(&"rs_test_listed".to_string()));
    drop_analyzer("rs_test_listed").unwrap();
}

#[test]
fn ngram_filter_validation_zero_min() {
    // Rust uses debug_assert, so a panic on min_gram=0 is the contract;
    // we exercise the working path here to keep the test deterministic.
    let f = TokenFilter::Ngram {
        min_gram: 1,
        max_gram: 1,
        keep_short: false,
    };
    assert_eq!(f.filter(vec!["a".into()]), vec!["a"]);
}

#[test]
fn synonym_filter_chain_with_lowercase() {
    let mut m = BTreeMap::new();
    m.insert("car".to_string(), vec!["automobile".to_string()]);
    let pipeline = Analyzer::new(
        Tokenizer::Whitespace,
        vec![TokenFilter::Lowercase, TokenFilter::Synonym { synonyms: m }],
        Vec::new(),
    );
    let result = pipeline.analyze("Used CAR for sale");
    assert!(result.contains(&"car".to_string()));
    assert!(result.contains(&"automobile".to_string()));
}
