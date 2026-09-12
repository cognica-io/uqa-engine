//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable tokenizer configuration with prevalidated bounds and prepared expressions.

use regex::Regex;

use super::{letter_re, standard_word_re, stream, validate_gram_bounds, Tokenizer};
use crate::{AnalysisError, AnalysisResult, AnalyzedText, FilteredText};

#[derive(Debug)]
pub(crate) enum PreparedTokenizer {
    Whitespace,
    Matches(&'static Regex),
    NGram {
        min_gram: usize,
        max_gram: usize,
    },
    Pattern(Regex),
    Keyword,
    #[cfg(feature = "nori")]
    Nori(crate::nori::KoreanTokenizer),
}

impl Tokenizer {
    pub(crate) fn prepare(&self) -> AnalysisResult<PreparedTokenizer> {
        Ok(match self {
            #[cfg(feature = "nori")]
            Self::Nori(config) => {
                let resources = crate::nori::NoriResources::default();
                let dictionary =
                    resources.load(&crate::nori::pipeline::request(&config.dictionary)?)?;
                PreparedTokenizer::Nori(crate::nori::pipeline::prepare_tokenizer(
                    config,
                    &dictionary,
                    &resources,
                )?)
            }
            Self::Whitespace => PreparedTokenizer::Whitespace,
            Self::Standard => PreparedTokenizer::Matches(standard_word_re()?),
            Self::Letter => PreparedTokenizer::Matches(letter_re()?),
            Self::NGram { min_gram, max_gram } => {
                validate_gram_bounds("n-gram tokenizer", *min_gram, *max_gram)?;
                PreparedTokenizer::NGram {
                    min_gram: *min_gram,
                    max_gram: *max_gram,
                }
            }
            Self::Pattern { pattern } => {
                PreparedTokenizer::Pattern(Regex::new(pattern).map_err(|source| {
                    AnalysisError::InvalidRegex {
                        component: "pattern tokenizer",
                        pattern: pattern.clone(),
                        source,
                    }
                })?)
            }
            Self::Keyword => PreparedTokenizer::Keyword,
        })
    }
}

impl PreparedTokenizer {
    pub(crate) fn tokenize_mapped(&self, text: &FilteredText<'_>) -> AnalysisResult<AnalyzedText> {
        stream::tokenize(self, text)
    }
}
