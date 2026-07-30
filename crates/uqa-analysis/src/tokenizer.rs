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

use crate::error::{AnalysisError, AnalysisResult};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Tokenizer {
    Whitespace,
    Standard,
    Letter,
    NGram { min_gram: usize, max_gram: usize },
    Pattern { pattern: String },
    Keyword,
}

impl Tokenizer {
    /// Validate configuration without tokenizing input. This is used when an
    /// analyzer is registered, while [`Self::tokenize`] repeats the checks so
    /// deserialized legacy values can never bypass them.
    pub fn validate(&self) -> AnalysisResult<()> {
        match self {
            Tokenizer::NGram { min_gram, max_gram } => {
                validate_gram_bounds("n-gram tokenizer", *min_gram, *max_gram)
            }
            Tokenizer::Pattern { pattern } => {
                Regex::new(pattern)
                    .map(|_| ())
                    .map_err(|source| AnalysisError::InvalidRegex {
                        component: "pattern tokenizer",
                        pattern: pattern.clone(),
                        source,
                    })
            }
            _ => Ok(()),
        }
    }

    pub fn tokenize(&self, text: &str) -> AnalysisResult<Vec<String>> {
        let tokens = match self {
            Tokenizer::Whitespace => text.split_whitespace().map(str::to_owned).collect(),
            Tokenizer::Standard => standard_word_re()?
                .find_iter(text)
                .map(|m| m.as_str().to_owned())
                .collect(),
            Tokenizer::Letter => letter_re()?
                .find_iter(text)
                .map(|m| m.as_str().to_owned())
                .collect(),
            Tokenizer::NGram { min_gram, max_gram } => {
                validate_gram_bounds("n-gram tokenizer", *min_gram, *max_gram)?;
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
            Tokenizer::Pattern { pattern } => {
                let re = Regex::new(pattern).map_err(|source| AnalysisError::InvalidRegex {
                    component: "pattern tokenizer",
                    pattern: pattern.clone(),
                    source,
                })?;
                re.split(text)
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
                    .collect()
            }
            Tokenizer::Keyword => {
                if text.is_empty() {
                    Vec::new()
                } else {
                    vec![text.to_owned()]
                }
            }
        };
        Ok(tokens)
    }
}

fn validate_gram_bounds(
    component: &'static str,
    min_gram: usize,
    max_gram: usize,
) -> AnalysisResult<()> {
    if min_gram == 0 || max_gram < min_gram {
        return Err(AnalysisError::InvalidGramBounds {
            component,
            min_gram,
            max_gram,
        });
    }
    Ok(())
}

fn standard_word_re() -> AnalysisResult<&'static Regex> {
    static RE: OnceLock<Result<Regex, String>> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\w+").map_err(|error| error.to_string()))
        .as_ref()
        .map_err(|message| AnalysisError::BuiltInRegex {
            component: "standard tokenizer",
            message: message.clone(),
        })
}

fn letter_re() -> AnalysisResult<&'static Regex> {
    static RE: OnceLock<Result<Regex, String>> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"[a-zA-Z]+").map_err(|error| error.to_string()))
        .as_ref()
        .map_err(|message| AnalysisError::BuiltInRegex {
            component: "letter tokenizer",
            message: message.clone(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whitespace_splits_on_whitespace() {
        let t = Tokenizer::Whitespace;
        assert_eq!(
            t.tokenize("hello  world\n  rust").unwrap(),
            vec!["hello", "world", "rust"]
        );
    }

    #[test]
    fn standard_extracts_unicode_words() {
        let t = Tokenizer::Standard;
        assert_eq!(
            t.tokenize("Rust 2024! Carácter.").unwrap(),
            vec!["Rust", "2024", "Carácter"]
        );
    }

    #[test]
    fn letter_extracts_ascii_letters_only() {
        let t = Tokenizer::Letter;
        assert_eq!(t.tokenize("abc123 xyz").unwrap(), vec!["abc", "xyz"]);
    }

    #[test]
    fn ngram_emits_substrings_per_word() {
        let t = Tokenizer::NGram {
            min_gram: 2,
            max_gram: 3,
        };
        // "ab" word: 2-grams [ab]
        // "abc" word: 2-grams [ab, bc], 3-grams [abc]
        assert_eq!(t.tokenize("ab abc").unwrap(), vec!["ab", "ab", "bc", "abc"]);
    }

    #[test]
    fn pattern_splits_on_regex() {
        let t = Tokenizer::Pattern {
            pattern: r"\W+".to_string(),
        };
        assert_eq!(t.tokenize("hello, world!").unwrap(), vec!["hello", "world"]);
    }

    #[test]
    fn keyword_emits_whole_input() {
        let t = Tokenizer::Keyword;
        assert_eq!(t.tokenize("a b c").unwrap(), vec!["a b c"]);
        assert!(t.tokenize("").unwrap().is_empty());
    }

    #[test]
    fn round_trips_via_serde_json() {
        let t = Tokenizer::NGram {
            min_gram: 2,
            max_gram: 4,
        };
        let s = serde_json::to_string(&t).unwrap();
        let back: Tokenizer = serde_json::from_str(&s).unwrap();
        assert_eq!(
            back.tokenize("foobar").unwrap(),
            t.tokenize("foobar").unwrap()
        );
    }
}
