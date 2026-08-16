//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

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
    assert_eq!(json, r#"{"type":"ascii_folding"}"#);
    let back: TokenFilter = serde_json::from_str(&json).unwrap();
    assert!(matches!(back, TokenFilter::ASCIIFolding));
}

#[test]
fn ascii_folding_filter_accepts_legacy_tag() {
    let legacy: TokenFilter = serde_json::from_str(r#"{"type":"a_s_c_i_i_folding"}"#).unwrap();
    assert!(matches!(legacy, TokenFilter::ASCIIFolding));
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
