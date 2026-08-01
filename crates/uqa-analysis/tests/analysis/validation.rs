use super::*;

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
