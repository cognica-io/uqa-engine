//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Coverage for `test_analysis`. Mirrors the Lucene-style text
//! analysis pipeline tests: tokenizers, token filters, char filters,
//! analyzer composition, serialization round-trips, and the named
//! analyzer registry.

use std::collections::BTreeMap;

use uqa_analysis::{
    keyword_analyzer, standard_analyzer, standard_cjk_analyzer, whitespace_analyzer, AnalysisError,
    Analyzer, CharFilter, SynonymFileError, TokenFilter, Tokenizer,
};

// =====================================================================
// Tokenizers
// =====================================================================

#[test]
fn whitespace_tokenizer_basic() {
    let t = Tokenizer::Whitespace;
    assert_eq!(t.tokenize("hello world").unwrap(), vec!["hello", "world"]);
}

#[test]
fn whitespace_tokenizer_multiple_spaces() {
    let t = Tokenizer::Whitespace;
    assert_eq!(
        t.tokenize("  hello   world  ").unwrap(),
        vec!["hello", "world"]
    );
}

#[test]
fn whitespace_tokenizer_empty() {
    let t = Tokenizer::Whitespace;
    assert!(t.tokenize("").unwrap().is_empty());
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
    assert_eq!(t.tokenize("Hello, World!").unwrap(), vec!["Hello", "World"]);
}

#[test]
fn standard_tokenizer_unicode() {
    let t = Tokenizer::Standard;
    assert_eq!(
        t.tokenize("cafe_latte 42").unwrap(),
        vec!["cafe_latte", "42"]
    );
}

#[test]
fn standard_tokenizer_punctuation() {
    let t = Tokenizer::Standard;
    assert_eq!(
        t.tokenize("it's a test.").unwrap(),
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
    assert_eq!(t.tokenize("hello123world").unwrap(), vec!["hello", "world"]);
}

#[test]
fn letter_tokenizer_only_letters() {
    let t = Tokenizer::Letter;
    assert!(t.tokenize("42!!").unwrap().is_empty());
}

#[test]
fn ngram_tokenizer_bigrams() {
    let t = Tokenizer::NGram {
        min_gram: 2,
        max_gram: 2,
    };
    assert_eq!(t.tokenize("abc").unwrap(), vec!["ab", "bc"]);
}

#[test]
fn ngram_tokenizer_unigrams_and_bigrams() {
    let t = Tokenizer::NGram {
        min_gram: 1,
        max_gram: 2,
    };
    assert_eq!(t.tokenize("ab").unwrap(), vec!["a", "b", "ab"]);
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
    assert_eq!(t.tokenize("hello-world").unwrap(), vec!["hello", "world"]);
}

#[test]
fn pattern_tokenizer_custom_pattern() {
    let t = Tokenizer::Pattern {
        pattern: r",\s*".into(),
    };
    assert_eq!(t.tokenize("a, b, c").unwrap(), vec!["a", "b", "c"]);
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
    assert_eq!(t.tokenize("hello world").unwrap(), vec!["hello world"]);
}

#[test]
fn keyword_tokenizer_empty() {
    let t = Tokenizer::Keyword;
    assert!(t.tokenize("").unwrap().is_empty());
}

// =====================================================================
// Token Filters
// =====================================================================

#[test]
fn lowercase_filter_basic() {
    let f = TokenFilter::Lowercase;
    assert_eq!(
        f.filter(vec!["Hello".into(), "WORLD".into()]).unwrap(),
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
    let result = f
        .filter(vec![
            "the".into(),
            "quick".into(),
            "brown".into(),
            "fox".into(),
        ])
        .unwrap();
    assert_eq!(result, vec!["quick", "brown", "fox"]);
}

#[test]
fn stopword_filter_custom_words() {
    let f = TokenFilter::Stop {
        language: "english".into(),
        custom_words: vec!["quick".into()],
    };
    let result = f
        .filter(vec!["the".into(), "quick".into(), "brown".into()])
        .unwrap();
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
    let result = back.filter(vec!["extra".into()]).unwrap();
    assert!(result.is_empty());
}

#[test]
fn porter_stem_filter_basic_stemming() {
    let f = TokenFilter::PorterStem;
    assert_eq!(f.filter(vec!["running".into()]).unwrap(), vec!["run"]);
    assert_eq!(f.filter(vec!["cats".into()]).unwrap(), vec!["cat"]);
}

#[test]
fn porter_stem_filter_complex_stemming() {
    let f = TokenFilter::PorterStem;
    let result = f
        .filter(vec![
            "connections".into(),
            "generalization".into(),
            "relational".into(),
        ])
        .unwrap();
    assert!(result.contains(&"connect".to_string()));
    assert!(result.contains(&"gener".to_string()));
}

#[test]
fn ascii_folding_filter_accented_chars() {
    let f = TokenFilter::ASCIIFolding;
    assert_eq!(f.filter(vec!["café".into()]).unwrap(), vec!["cafe"]);
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
        synonyms_path: None,
    };
    let result = f.filter(vec!["fast".into(), "car".into()]).unwrap();
    assert_eq!(result, vec!["fast", "quick", "rapid", "car"]);
}

#[test]
fn synonym_filter_no_match() {
    let f = TokenFilter::Synonym {
        synonyms: syn_map(&[("fast", &["quick"])]),
        synonyms_path: None,
    };
    let result = f.filter(vec!["slow".into(), "car".into()]).unwrap();
    assert_eq!(result, vec!["slow", "car"]);
}

#[test]
fn synonym_filter_roundtrip() {
    let f = TokenFilter::Synonym {
        synonyms: syn_map(&[("a", &["b"])]),
        synonyms_path: None,
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
    let result = f.filter(vec!["hello".into()]).unwrap();
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
    assert!(f.filter(vec!["a".into()]).unwrap().is_empty());
}

#[test]
fn ngram_filter_keep_short() {
    let f = TokenFilter::Ngram {
        min_gram: 2,
        max_gram: 3,
        keep_short: true,
    };
    let result = f.filter(vec!["a".into(), "hello".into()]).unwrap();
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
    let result = f
        .filter(vec!["ab".into(), "cd".into(), "hello".into()])
        .unwrap();
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
    assert_eq!(
        back.filter(vec!["abc".into()]).unwrap(),
        f.filter(vec!["abc".into()]).unwrap()
    );
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
    assert_eq!(
        back.filter(vec!["a".into()]).unwrap(),
        vec!["a".to_string()]
    );
}

#[test]
fn edge_ngram_filter_default() {
    let f = TokenFilter::EdgeNgram {
        min_gram: 1,
        max_gram: 3,
    };
    assert_eq!(
        f.filter(vec!["hello".into()]).unwrap(),
        vec!["h", "he", "hel"]
    );
}

#[test]
fn edge_ngram_filter_min_gram() {
    let f = TokenFilter::EdgeNgram {
        min_gram: 2,
        max_gram: 4,
    };
    assert_eq!(f.filter(vec!["abc".into()]).unwrap(), vec!["ab", "abc"]);
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
        f.filter(vec!["a".into(), "ab".into(), "abc".into(), "abcd".into()])
            .unwrap(),
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
        f.filter(vec!["a".into(), "ab".into(), "abc".into(), "abcd".into()])
            .unwrap(),
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
        f.filter(vec!["a".into(), "ab".into(), "abc".into(), "abcd".into()])
            .unwrap(),
        vec!["ab", "abc"]
    );
}

// =====================================================================
// Char Filters
// =====================================================================

#[test]
fn html_strip_strip_tags() {
    let f = CharFilter::HTMLStrip;
    let result = f.filter("<p>Hello <b>world</b></p>").unwrap();
    assert!(result.contains("Hello"));
    assert!(result.contains("world"));
    assert!(!result.contains('<'));
}

#[test]
fn html_strip_no_tags() {
    let f = CharFilter::HTMLStrip;
    assert_eq!(f.filter("plain text").unwrap(), "plain text");
}

#[test]
fn html_strip_entities() {
    let f = CharFilter::HTMLStrip;
    assert_eq!(f.filter("a &amp; b").unwrap(), "a & b");
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
    assert_eq!(f.filter("you & me @ home").unwrap(), "you and me at home");
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
    assert_eq!(f.filter("abc123def456").unwrap(), "abc#def#");
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
    let result = standard.analyze("The Quick BROWN Fox").unwrap();
    assert!(!result.contains(&"the".to_string()));
    assert!(result.contains(&"quick".to_string()));
    assert!(result.contains(&"brown".to_string()));
    assert!(result.contains(&"fox".to_string()));
}

#[test]
fn whitespace_analyzer_run() {
    let a = whitespace_analyzer();
    assert_eq!(a.analyze("Hello World").unwrap(), vec!["hello", "world"]);
}

#[test]
fn standard_analyzer_run() {
    let a = standard_analyzer("english");
    let result = a.analyze("The quick brown fox").unwrap();
    assert!(!result.contains(&"the".to_string()));
    assert!(result.contains(&"quick".to_string()));
}

#[test]
fn standard_analyzer_stemming() {
    let a = standard_analyzer("english");
    let result = a.analyze("Running transformers efficiently").unwrap();
    assert!(result.contains(&"run".to_string()));
    assert!(result.contains(&"transform".to_string()));
}

#[test]
fn standard_analyzer_ascii_folding() {
    let a = standard_analyzer("english");
    let result = a.analyze("café résumé").unwrap();
    assert!(result.contains(&"cafe".to_string()));
    assert!(result.contains(&"resum".to_string()));
}

#[test]
fn standard_cjk_analyzer_run() {
    let a = standard_cjk_analyzer("english");
    let result = a.analyze("hello world").unwrap();
    assert!(result.contains(&"he".to_string()));
    assert!(result.contains(&"hel".to_string()));
    assert!(result.contains(&"wo".to_string()));
    assert!(result.contains(&"wor".to_string()));
}

#[test]
fn standard_cjk_analyzer_stemming() {
    let a = standard_cjk_analyzer("english");
    let result = a.analyze("Running").unwrap();
    assert!(result.contains(&"ru".to_string()));
    assert!(result.contains(&"run".to_string()));
}

#[test]
fn standard_cjk_analyzer_keep_short() {
    let a = standard_cjk_analyzer("english");
    let result = a.analyze("x marks").unwrap();
    assert!(result.contains(&"x".to_string()));
    assert!(result.contains(&"ma".to_string()));
    assert!(result.contains(&"mar".to_string()));
}

#[test]
fn keyword_analyzer_run() {
    let a = keyword_analyzer();
    assert_eq!(a.analyze("hello world").unwrap(), vec!["hello world"]);
}

#[test]
fn analyzer_custom_pipeline() {
    let a = Analyzer::new(
        Tokenizer::Standard,
        vec![TokenFilter::Lowercase, TokenFilter::PorterStem],
        vec![CharFilter::HTMLStrip],
    );
    let result = a.analyze("<p>Running Connections</p>").unwrap();
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
    assert_eq!(back.analyze(text).unwrap(), a.analyze(text).unwrap());
}

#[test]
fn analyzer_json_roundtrip() {
    let a = standard_analyzer("english");
    let j = serde_json::to_string(&a).unwrap();
    let back: Analyzer = serde_json::from_str(&j).unwrap();
    let text = "The quick brown fox";
    assert_eq!(back.analyze(text).unwrap(), a.analyze(text).unwrap());
}

// =====================================================================
// Named Analyzer Registry
// =====================================================================

#[test]
fn registry_register_and_get() {
    use uqa_analysis::{drop_analyzer, get_analyzer, register_analyzer};
    let custom = Analyzer::new(Tokenizer::Letter, vec![TokenFilter::Lowercase], Vec::new());
    register_analyzer("rs_test_custom_reg".to_string(), custom).unwrap();
    let retrieved = get_analyzer("rs_test_custom_reg").unwrap();
    assert_eq!(
        retrieved.analyze("hello123world").unwrap(),
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
    let f = TokenFilter::Ngram {
        min_gram: 0,
        max_gram: 1,
        keep_short: false,
    };
    assert!(matches!(
        f.filter(vec!["a".into()]),
        Err(AnalysisError::InvalidGramBounds {
            component: "n-gram token filter",
            min_gram: 0,
            max_gram: 1,
        })
    ));
}

#[test]
fn synonym_filter_chain_with_lowercase() {
    let mut m = BTreeMap::new();
    m.insert("car".to_string(), vec!["automobile".to_string()]);
    let pipeline = Analyzer::new(
        Tokenizer::Whitespace,
        vec![
            TokenFilter::Lowercase,
            TokenFilter::Synonym {
                synonyms: m,
                synonyms_path: None,
            },
        ],
        Vec::new(),
    );
    let result = pipeline.analyze("Used CAR for sale").unwrap();
    assert!(result.contains(&"car".to_string()));
    assert!(result.contains(&"automobile".to_string()));
}

// =====================================================================
// Synonym file loader (TestSynonymFilterFile)
// =====================================================================

mod synonym_file {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn write_synonyms(dir: &TempDir, body: &str) -> std::path::PathBuf {
        let path = dir.path().join("synonyms.txt");
        fs::write(&path, body).unwrap();
        path
    }

    #[test]
    fn explicit_mapping() {
        let dir = TempDir::new().unwrap();
        let path = write_synonyms(
            &dir,
            "# vehicle synonyms\ncar => automobile, vehicle\nfast => quick, speedy\n",
        );
        let f = TokenFilter::synonym_from_path(&path).unwrap();
        assert_eq!(
            f.filter(vec!["car".into()]).unwrap(),
            vec!["car", "automobile", "vehicle"]
        );
        assert_eq!(
            f.filter(vec!["fast".into()]).unwrap(),
            vec!["fast", "quick", "speedy"]
        );
        assert_eq!(f.filter(vec!["slow".into()]).unwrap(), vec!["slow"]);
    }

    #[test]
    fn equivalent_synonyms() {
        let dir = TempDir::new().unwrap();
        let path = write_synonyms(&dir, "car, automobile, vehicle\n");
        let f = TokenFilter::synonym_from_path(&path).unwrap();
        assert_eq!(
            f.filter(vec!["car".into()]).unwrap(),
            vec!["car", "automobile", "vehicle"]
        );
        assert_eq!(
            f.filter(vec!["automobile".into()]).unwrap(),
            vec!["automobile", "car", "vehicle"]
        );
        assert_eq!(
            f.filter(vec!["vehicle".into()]).unwrap(),
            vec!["vehicle", "car", "automobile"]
        );
    }

    #[test]
    fn mixed_formats() {
        let dir = TempDir::new().unwrap();
        let path = write_synonyms(
            &dir,
            "# explicit\nbig => large\n\n# equivalent\nfast, quick, speedy\n",
        );
        let f = TokenFilter::synonym_from_path(&path).unwrap();
        assert_eq!(f.filter(vec!["big".into()]).unwrap(), vec!["big", "large"]);
        // explicit one-way: large does not expand
        assert_eq!(f.filter(vec!["large".into()]).unwrap(), vec!["large"]);
        assert_eq!(
            f.filter(vec!["fast".into()]).unwrap(),
            vec!["fast", "quick", "speedy"]
        );
        assert_eq!(
            f.filter(vec!["quick".into()]).unwrap(),
            vec!["quick", "fast", "speedy"]
        );
    }

    #[test]
    fn blank_lines_and_comments() {
        let dir = TempDir::new().unwrap();
        let path = write_synonyms(
            &dir,
            "# this is a comment\n\n   \n# another comment\na => b\n",
        );
        let f = TokenFilter::synonym_from_path(&path).unwrap();
        assert_eq!(f.filter(vec!["a".into()]).unwrap(), vec!["a", "b"]);
    }

    #[test]
    fn deduplication() {
        let dir = TempDir::new().unwrap();
        let path = write_synonyms(&dir, "car => automobile\ncar => automobile, vehicle\n");
        let f = TokenFilter::synonym_from_path(&path).unwrap();
        assert_eq!(
            f.filter(vec!["car".into()]).unwrap(),
            vec!["car", "automobile", "vehicle"]
        );
    }

    #[test]
    fn serialization_roundtrip() {
        let dir = TempDir::new().unwrap();
        let path = write_synonyms(&dir, "car => automobile\n");
        let f = TokenFilter::synonym_from_path(&path).unwrap();
        let json = serde_json::to_string(&f).unwrap();
        assert!(json.contains("synonyms_path"));
        // Inline synonyms map is empty when sourced from file, so it's
        // omitted by the skip-if-empty serde attribute.
        assert!(!json.contains("\"synonyms\":{"));
        let back: TokenFilter = serde_json::from_str(&json).unwrap();
        assert_eq!(
            back.filter(vec!["car".into()]).unwrap(),
            vec!["car", "automobile"]
        );
    }

    #[test]
    fn inline_does_not_serialize_path() {
        let mut m = BTreeMap::new();
        m.insert("a".to_string(), vec!["b".to_string()]);
        let f = TokenFilter::Synonym {
            synonyms: m,
            synonyms_path: None,
        };
        let json = serde_json::to_string(&f).unwrap();
        assert!(json.contains("\"synonyms\""));
        assert!(!json.contains("synonyms_path"));
    }

    #[test]
    fn file_not_found() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("nonexistent.txt");
        let r = TokenFilter::synonym_from_path(&path);
        assert!(r.is_err());
    }

    #[test]
    fn single_term_line_ignored() {
        let dir = TempDir::new().unwrap();
        let path = write_synonyms(&dir, "lonely\ncar, automobile\n");
        let f = TokenFilter::synonym_from_path(&path).unwrap();
        assert_eq!(f.filter(vec!["lonely".into()]).unwrap(), vec!["lonely"]);
        assert_eq!(
            f.filter(vec!["car".into()]).unwrap(),
            vec!["car", "automobile"]
        );
    }

    #[test]
    fn parse_helper_returns_map() {
        let dir = TempDir::new().unwrap();
        let path = write_synonyms(&dir, "car => automobile\n");
        let map = TokenFilter::parse_synonym_file(&path).unwrap();
        assert_eq!(map.get("car"), Some(&vec!["automobile".to_string()]));
    }

    #[test]
    fn file_path_filter_in_pipeline() {
        let dir = TempDir::new().unwrap();
        let path = write_synonyms(&dir, "fast, quick\n");
        let synonym = TokenFilter::synonym_from_path(&path).unwrap();
        let pipe = Analyzer::new(
            Tokenizer::Whitespace,
            vec![TokenFilter::Lowercase, synonym],
            Vec::new(),
        );
        let result = pipe.analyze("Fast running").unwrap();
        assert!(result.contains(&"fast".to_string()));
        assert!(result.contains(&"quick".to_string()));
    }

    #[test]
    fn deleting_synonym_file_is_an_execution_error() {
        let dir = TempDir::new().unwrap();
        let path = write_synonyms(&dir, "fast, quick\n");
        let filter = TokenFilter::synonym_from_path(&path).unwrap();
        fs::remove_file(&path).unwrap();

        let error = filter.filter(vec!["fast".into()]).unwrap_err();
        assert!(matches!(
            error,
            AnalysisError::SynonymFile(SynonymFileError::NotFound(missing))
                if missing == path
        ));
    }
}

// =====================================================================
// Validation surface (NGram requires_synonyms_or_path style errors)
// =====================================================================

#[test]
fn deserialized_zero_ngram_filter_is_an_execution_error() {
    let f: TokenFilter =
        serde_json::from_str(r#"{"type":"ngram","min_gram":0,"max_gram":1,"keep_short":false}"#)
            .unwrap();
    assert!(matches!(
        f.filter(vec!["a".into()]),
        Err(AnalysisError::InvalidGramBounds { .. })
    ));
}

#[test]
fn ngram_filter_max_smaller_than_min_is_an_error() {
    let f = TokenFilter::Ngram {
        min_gram: 3,
        max_gram: 2,
        keep_short: false,
    };
    assert!(matches!(
        f.filter(vec!["abc".into()]),
        Err(AnalysisError::InvalidGramBounds { .. })
    ));
}

#[test]
fn ngram_tokenizer_max_smaller_than_min_is_an_error() {
    let t = Tokenizer::NGram {
        min_gram: 3,
        max_gram: 2,
    };
    assert!(matches!(
        t.tokenize("abc"),
        Err(AnalysisError::InvalidGramBounds { .. })
    ));
}

#[test]
fn ngram_tokenizer_min_zero_is_an_error() {
    let t = Tokenizer::NGram {
        min_gram: 0,
        max_gram: 2,
    };
    assert!(matches!(
        t.tokenize("abc"),
        Err(AnalysisError::InvalidGramBounds { .. })
    ));
}

#[test]
fn deserialized_invalid_pattern_tokenizer_is_an_execution_error() {
    let t: Tokenizer = serde_json::from_str(r#"{"type":"pattern","pattern":"["}"#).unwrap();
    assert!(matches!(
        t.tokenize("abc"),
        Err(AnalysisError::InvalidRegex {
            component: "pattern tokenizer",
            ..
        })
    ));
}

#[test]
fn invalid_pattern_character_filter_is_an_execution_error() {
    let f = CharFilter::PatternReplace {
        pattern: "[".into(),
        replacement: "x".into(),
    };
    assert!(matches!(
        f.filter("abc"),
        Err(AnalysisError::InvalidRegex {
            component: "pattern-replace character filter",
            ..
        })
    ));
}

#[test]
fn edge_ngram_invalid_bounds_are_errors() {
    for (min_gram, max_gram) in [(0, 2), (3, 2)] {
        let f = TokenFilter::EdgeNgram { min_gram, max_gram };
        assert!(matches!(
            f.filter(vec!["abc".into()]),
            Err(AnalysisError::InvalidGramBounds {
                component: "edge n-gram token filter",
                ..
            })
        ));
    }
}

#[test]
fn registry_builtin_analyzers_present() {
    use uqa_analysis::list_analyzers;
    let names = list_analyzers();
    assert!(names.contains(&"whitespace".to_string()));
    assert!(names.contains(&"standard".to_string()));
    assert!(names.contains(&"standard_cjk".to_string()));
    assert!(names.contains(&"keyword".to_string()));
}

#[test]
fn registry_cannot_overwrite_builtin() {
    use uqa_analysis::register_analyzer;
    let r = register_analyzer("standard".to_string(), whitespace_analyzer());
    assert!(r.is_err());
}

#[test]
fn registry_cannot_drop_builtin() {
    use uqa_analysis::drop_analyzer;
    let r = drop_analyzer("standard");
    assert!(r.is_err());
}
