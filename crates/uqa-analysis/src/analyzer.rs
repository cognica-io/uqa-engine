//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Composable text analysis pipeline.
//!
//! ```text
//! text -> CharFilter* -> Tokenizer -> TokenFilter* -> tokens
//! ```

use serde::{Deserialize, Serialize};

use crate::char_filter::CharFilter;
use crate::token_filter::TokenFilter;
use crate::tokenizer::Tokenizer;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Analyzer {
    #[serde(default = "default_tokenizer")]
    pub tokenizer: Tokenizer,
    #[serde(default)]
    pub token_filters: Vec<TokenFilter>,
    #[serde(default)]
    pub char_filters: Vec<CharFilter>,
}

fn default_tokenizer() -> Tokenizer {
    Tokenizer::Whitespace
}

impl Default for Analyzer {
    fn default() -> Self {
        Self {
            tokenizer: default_tokenizer(),
            token_filters: Vec::new(),
            char_filters: Vec::new(),
        }
    }
}

impl Analyzer {
    pub fn new(
        tokenizer: Tokenizer,
        token_filters: Vec<TokenFilter>,
        char_filters: Vec<CharFilter>,
    ) -> Self {
        Self {
            tokenizer,
            token_filters,
            char_filters,
        }
    }

    pub fn analyze(&self, text: &str) -> Vec<String> {
        let mut filtered: String = text.to_owned();
        for cf in &self.char_filters {
            filtered = cf.filter(&filtered);
        }
        let mut tokens = self.tokenizer.tokenize(&filtered);
        for tf in &self.token_filters {
            tokens = tf.filter(tokens);
        }
        tokens
    }
}

/// `WhitespaceTokenizer` + `Lowercase`.
pub fn whitespace_analyzer() -> Analyzer {
    Analyzer::new(
        Tokenizer::Whitespace,
        vec![TokenFilter::Lowercase],
        Vec::new(),
    )
}

/// `Standard` + `Lowercase` + `AsciiFolding` + `Stop` + `PorterStem`.
pub fn standard_analyzer(language: &str) -> Analyzer {
    Analyzer::new(
        Tokenizer::Standard,
        vec![
            TokenFilter::Lowercase,
            TokenFilter::AsciiFolding,
            TokenFilter::Stop {
                language: language.to_string(),
                custom_words: Vec::new(),
            },
            TokenFilter::PorterStem,
        ],
        Vec::new(),
    )
}

/// `standard_analyzer` extended with character-level n-grams (2..=3) for
/// CJK-style text where words are not whitespace-delimited.
pub fn standard_cjk_analyzer(language: &str) -> Analyzer {
    Analyzer::new(
        Tokenizer::Standard,
        vec![
            TokenFilter::Lowercase,
            TokenFilter::AsciiFolding,
            TokenFilter::Stop {
                language: language.to_string(),
                custom_words: Vec::new(),
            },
            TokenFilter::PorterStem,
            TokenFilter::Ngram {
                min_gram: 2,
                max_gram: 3,
                keep_short: true,
            },
        ],
        Vec::new(),
    )
}

/// `Keyword` tokenizer with no filters.
pub fn keyword_analyzer() -> Analyzer {
    Analyzer::new(Tokenizer::Keyword, Vec::new(), Vec::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_pipeline_lowers_stops_and_stems() {
        let a = standard_analyzer("english");
        // "The Running" -> standard tokens ["The", "Running"]
        // -> lowercase ["the", "running"]
        // -> ascii_fold same
        // -> stop ["running"]
        // -> porter_stem ["run"]
        assert_eq!(a.analyze("The Running"), vec!["run"]);
    }

    #[test]
    fn whitespace_pipeline_just_lowers() {
        let a = whitespace_analyzer();
        assert_eq!(a.analyze("Hello WORLD"), vec!["hello", "world"]);
    }

    #[test]
    fn keyword_pipeline_emits_whole_input() {
        let a = keyword_analyzer();
        assert_eq!(a.analyze("the quick brown"), vec!["the quick brown"]);
    }

    #[test]
    fn round_trips_via_serde_json() {
        let a = standard_analyzer("english");
        let s = serde_json::to_string(&a).unwrap();
        let back: Analyzer = serde_json::from_str(&s).unwrap();
        assert_eq!(back.analyze("The Running"), a.analyze("The Running"));
    }
}
