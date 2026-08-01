use super::*;

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
