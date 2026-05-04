//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Tokenizers for the analysis pipeline. An [`Analyzer`] owns exactly one
//! tokenizer.
//!
//! [`Analyzer`]: crate::analyzer::Analyzer

use std::sync::OnceLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Tokenizer {
    Whitespace,
    Standard,
    Letter,
    NGram {
        min_gram: usize,
        max_gram: usize,
    },
    Pattern {
        pattern: String,
    },
    Keyword,
}

impl Tokenizer {
    pub fn tokenize(&self, text: &str) -> Vec<String> {
        match self {
            Tokenizer::Whitespace => text.split_whitespace().map(str::to_owned).collect(),
            Tokenizer::Standard => standard_word_re()
                .find_iter(text)
                .map(|m| m.as_str().to_owned())
                .collect(),
            Tokenizer::Letter => letter_re()
                .find_iter(text)
                .map(|m| m.as_str().to_owned())
                .collect(),
            Tokenizer::NGram { min_gram, max_gram } => {
                debug_assert!(*min_gram >= 1, "min_gram must be >= 1");
                debug_assert!(max_gram >= min_gram, "max_gram must be >= min_gram");
                let mut out = Vec::new();
                for word in text.split_whitespace() {
                    let chars: Vec<char> = word.chars().collect();
                    for n in *min_gram..=*max_gram {
                        if chars.len() < n {
                            continue;
                        }
                        for i in 0..=(chars.len() - n) {
                            out.push(chars[i..i + n].iter().collect());
                        }
                    }
                }
                out
            }
            Tokenizer::Pattern { pattern } => Regex::new(pattern)
                .map(|re| re.split(text).filter(|s| !s.is_empty()).map(str::to_owned).collect())
                .unwrap_or_default(),
            Tokenizer::Keyword => {
                if text.is_empty() {
                    Vec::new()
                } else {
                    vec![text.to_owned()]
                }
            }
        }
    }
}

fn standard_word_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\w+").expect("standard tokenizer regex compiles"))
}

fn letter_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"[a-zA-Z]+").expect("letter tokenizer regex compiles"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whitespace_splits_on_whitespace() {
        let t = Tokenizer::Whitespace;
        assert_eq!(t.tokenize("hello  world\n  rust"), vec!["hello", "world", "rust"]);
    }

    #[test]
    fn standard_extracts_unicode_words() {
        let t = Tokenizer::Standard;
        assert_eq!(
            t.tokenize("Rust 2024! Carácter."),
            vec!["Rust", "2024", "Carácter"]
        );
    }

    #[test]
    fn letter_extracts_ascii_letters_only() {
        let t = Tokenizer::Letter;
        assert_eq!(t.tokenize("abc123 xyz"), vec!["abc", "xyz"]);
    }

    #[test]
    fn ngram_emits_substrings_per_word() {
        let t = Tokenizer::NGram {
            min_gram: 2,
            max_gram: 3,
        };
        // "ab" word: 2-grams [ab]
        // "abc" word: 2-grams [ab, bc], 3-grams [abc]
        assert_eq!(t.tokenize("ab abc"), vec!["ab", "ab", "bc", "abc"]);
    }

    #[test]
    fn pattern_splits_on_regex() {
        let t = Tokenizer::Pattern {
            pattern: r"\W+".to_string(),
        };
        assert_eq!(t.tokenize("hello, world!"), vec!["hello", "world"]);
    }

    #[test]
    fn keyword_emits_whole_input() {
        let t = Tokenizer::Keyword;
        assert_eq!(t.tokenize("a b c"), vec!["a b c"]);
        assert!(t.tokenize("").is_empty());
    }

    #[test]
    fn round_trips_via_serde_json() {
        let t = Tokenizer::NGram {
            min_gram: 2,
            max_gram: 4,
        };
        let s = serde_json::to_string(&t).unwrap();
        let back: Tokenizer = serde_json::from_str(&s).unwrap();
        assert_eq!(back.tokenize("foobar"), t.tokenize("foobar"));
    }
}
