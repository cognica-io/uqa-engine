//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Token-level filters that run after tokenization.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;

use crate::porter;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TokenFilter {
    Lowercase,
    Stop {
        #[serde(default = "default_stop_language")]
        language: String,
        #[serde(default)]
        custom_words: Vec<String>,
    },
    PorterStem,
    AsciiFolding,
    Synonym {
        synonyms: BTreeMap<String, Vec<String>>,
    },
    Ngram {
        min_gram: usize,
        max_gram: usize,
        #[serde(default)]
        keep_short: bool,
    },
    EdgeNgram {
        min_gram: usize,
        max_gram: usize,
    },
    Length {
        #[serde(default)]
        min_length: usize,
        #[serde(default)]
        max_length: usize,
    },
}

fn default_stop_language() -> String {
    "english".to_string()
}

impl TokenFilter {
    pub fn filter(&self, tokens: Vec<String>) -> Vec<String> {
        match self {
            TokenFilter::Lowercase => tokens.into_iter().map(|t| t.to_lowercase()).collect(),
            TokenFilter::Stop {
                language,
                custom_words,
            } => {
                let mut words: BTreeSet<&str> =
                    builtin_stop_words(language).iter().copied().collect();
                let custom: Vec<&str> = custom_words.iter().map(String::as_str).collect();
                words.extend(custom);
                tokens
                    .into_iter()
                    .filter(|t| !words.contains(t.as_str()))
                    .collect()
            }
            TokenFilter::PorterStem => tokens.into_iter().map(|t| porter::stem(&t)).collect(),
            TokenFilter::AsciiFolding => tokens.into_iter().map(|t| ascii_fold(&t)).collect(),
            TokenFilter::Synonym { synonyms } => {
                let mut out = Vec::with_capacity(tokens.len());
                for t in tokens {
                    if let Some(extra) = synonyms.get(&t) {
                        out.push(t);
                        out.extend(extra.iter().cloned());
                    } else {
                        out.push(t);
                    }
                }
                out
            }
            TokenFilter::Ngram {
                min_gram,
                max_gram,
                keep_short,
            } => {
                debug_assert!(*min_gram >= 1, "min_gram must be >= 1");
                debug_assert!(max_gram >= min_gram, "max_gram must be >= min_gram");
                let mut out = Vec::new();
                for t in tokens {
                    let chars: Vec<char> = t.chars().collect();
                    if chars.len() < *min_gram {
                        if *keep_short {
                            out.push(t);
                        }
                        continue;
                    }
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
            TokenFilter::EdgeNgram { min_gram, max_gram } => {
                let mut out = Vec::new();
                for t in tokens {
                    let chars: Vec<char> = t.chars().collect();
                    let upper = (*max_gram).min(chars.len());
                    for n in *min_gram..=upper {
                        out.push(chars[..n].iter().collect());
                    }
                }
                out
            }
            TokenFilter::Length {
                min_length,
                max_length,
            } => tokens
                .into_iter()
                .filter(|t| {
                    let len = t.chars().count();
                    if len < *min_length {
                        return false;
                    }
                    if *max_length > 0 && len > *max_length {
                        return false;
                    }
                    true
                })
                .collect(),
        }
    }
}

fn ascii_fold(token: &str) -> String {
    if token.is_ascii() {
        return token.to_owned();
    }
    let mut out = String::with_capacity(token.len());
    for ch in token.chars() {
        if ch.is_ascii() {
            out.push(ch);
            continue;
        }
        let folded: String = ch.nfkd().filter(char::is_ascii).collect();
        if folded.is_empty() {
            // No ASCII equivalent (CJK, Korean, Arabic, etc.) — keep original.
            out.push(ch);
        } else {
            out.push_str(&folded);
        }
    }
    out
}

const ENGLISH_STOP_WORDS: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "but", "by", "for", "if", "in", "into", "is", "it",
    "no", "not", "of", "on", "or", "such", "that", "the", "their", "then", "there", "these",
    "they", "this", "to", "was", "were", "will", "with", "would", "can", "could", "do", "does",
    "did", "had", "has", "have", "he", "her", "him", "his", "how", "i", "its", "may", "me", "my",
    "nor", "our", "own", "she", "should", "so", "some", "than", "too", "us", "very", "we", "what",
    "when", "which", "who", "whom", "why", "you", "your",
];

fn builtin_stop_words(language: &str) -> &'static [&'static str] {
    match language {
        "english" => ENGLISH_STOP_WORDS,
        _ => &[],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &[&str]) -> Vec<String> {
        s.iter().map(|t| (*t).to_string()).collect()
    }

    #[test]
    fn lowercase_lowers_each_token() {
        let f = TokenFilter::Lowercase;
        assert_eq!(f.filter(v(&["Hello", "WORLD"])), v(&["hello", "world"]));
    }

    #[test]
    fn stop_removes_english_stop_words() {
        let f = TokenFilter::Stop {
            language: "english".to_string(),
            custom_words: vec![],
        };
        assert_eq!(
            f.filter(v(&["the", "rust", "is", "fast"])),
            v(&["rust", "fast"])
        );
    }

    #[test]
    fn stop_includes_custom_words() {
        let f = TokenFilter::Stop {
            language: "english".to_string(),
            custom_words: vec!["foo".to_string()],
        };
        assert_eq!(f.filter(v(&["foo", "bar", "the"])), v(&["bar"]));
    }

    #[test]
    fn porter_stem_runs() {
        let f = TokenFilter::PorterStem;
        assert_eq!(f.filter(v(&["caresses", "ponies"])), v(&["caress", "poni"]));
    }

    #[test]
    fn ascii_folding_strips_diacritics() {
        let f = TokenFilter::AsciiFolding;
        assert_eq!(f.filter(v(&["café", "naïve"])), v(&["cafe", "naive"]));
    }

    #[test]
    fn ascii_folding_preserves_cjk() {
        let f = TokenFilter::AsciiFolding;
        assert_eq!(f.filter(v(&["한글"])), v(&["한글"]));
    }

    #[test]
    fn synonym_appends_alternatives() {
        let mut m: BTreeMap<String, Vec<String>> = BTreeMap::new();
        m.insert(
            "car".to_string(),
            vec!["auto".to_string(), "vehicle".to_string()],
        );
        let f = TokenFilter::Synonym { synonyms: m };
        assert_eq!(
            f.filter(v(&["fast", "car"])),
            v(&["fast", "car", "auto", "vehicle"])
        );
    }

    #[test]
    fn ngram_emits_substrings() {
        let f = TokenFilter::Ngram {
            min_gram: 2,
            max_gram: 3,
            keep_short: false,
        };
        assert_eq!(f.filter(v(&["abc"])), v(&["ab", "bc", "abc"]));
    }

    #[test]
    fn ngram_drops_short_unless_keep_set() {
        let f_drop = TokenFilter::Ngram {
            min_gram: 3,
            max_gram: 4,
            keep_short: false,
        };
        assert!(f_drop.filter(v(&["ab"])).is_empty());

        let f_keep = TokenFilter::Ngram {
            min_gram: 3,
            max_gram: 4,
            keep_short: true,
        };
        assert_eq!(f_keep.filter(v(&["ab"])), v(&["ab"]));
    }

    #[test]
    fn edge_ngram_emits_prefixes() {
        let f = TokenFilter::EdgeNgram {
            min_gram: 1,
            max_gram: 3,
        };
        assert_eq!(f.filter(v(&["abcd"])), v(&["a", "ab", "abc"]));
    }

    #[test]
    fn length_bounds_token_size() {
        let f = TokenFilter::Length {
            min_length: 2,
            max_length: 4,
        };
        assert_eq!(
            f.filter(v(&["a", "ab", "abcd", "abcde"])),
            v(&["ab", "abcd"])
        );
    }
}
