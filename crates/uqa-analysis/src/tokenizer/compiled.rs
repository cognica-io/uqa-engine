//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable tokenizer configuration with prevalidated bounds and prepared expressions.

use regex::Regex;
use uqa_core::memory::{Budgeted, MemoryBudget};

use super::{stream, validate_gram_bounds, Tokenizer};
use crate::cooperative_regex::CooperativeRegex;
use crate::{AnalysisError, AnalysisResult, AnalyzedText, FilteredText};

#[derive(Debug)]
pub(crate) enum PreparedTokenizer {
    Whitespace,
    Standard,
    Letter,
    NGram {
        min_gram: usize,
        max_gram: usize,
    },
    Pattern {
        expression: Box<CooperativeRegex>,
    },
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
            Self::Standard => PreparedTokenizer::Standard,
            Self::Letter => PreparedTokenizer::Letter,
            Self::NGram { min_gram, max_gram } => {
                validate_gram_bounds("n-gram tokenizer", *min_gram, *max_gram)?;
                PreparedTokenizer::NGram {
                    min_gram: *min_gram,
                    max_gram: *max_gram,
                }
            }
            Self::Pattern { pattern } => {
                Regex::new(pattern).map_err(|source| AnalysisError::InvalidRegex {
                    component: "pattern tokenizer",
                    pattern: pattern.clone(),
                    source,
                })?;
                let expression = CooperativeRegex::compile(pattern).map_err(|source| {
                    AnalysisError::InvalidRegex {
                        component: "pattern tokenizer",
                        pattern: pattern.clone(),
                        source,
                    }
                })?;
                PreparedTokenizer::Pattern {
                    expression: Box::new(expression),
                }
            }
            Self::Keyword => PreparedTokenizer::Keyword,
        })
    }
}

impl PreparedTokenizer {
    pub(crate) fn tokenize_mapped(&self, text: &FilteredText<'_>) -> AnalysisResult<AnalyzedText> {
        stream::tokenize(self, text)
    }

    pub(crate) fn tokenize_mapped_budgeted(
        &self,
        text: &FilteredText<'_>,
        budget: &MemoryBudget,
        poll: &mut dyn FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<Budgeted<AnalyzedText>> {
        stream::tokenize_budgeted(self, text, budget, poll)
    }
}
