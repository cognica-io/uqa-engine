//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Tokenizers for the analysis pipeline. An [`Analyzer`] owns exactly one
//! tokenizer.
//!
//! [`Analyzer`]: crate::analyzer::Analyzer

use serde::{Deserialize, Serialize};
use std::sync::OnceLock;
use uqa_core::memory::{Budgeted, MemoryBudget};

use crate::character_class::class;
use crate::error::{AnalysisError, AnalysisResult};

mod compiled;
mod stream;
pub(crate) use compiled::PreparedTokenizer;

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
    #[cfg(feature = "nori")]
    #[serde(rename = "nori_tokenizer")]
    Nori(crate::nori::NoriTokenizerConfig),
}

impl Tokenizer {
    /// Validate configuration without tokenizing input. This is used when an
    /// analyzer is registered, while [`Self::tokenize`] repeats the checks so
    /// deserialized legacy values can never bypass them.
    pub fn validate(&self) -> AnalysisResult<()> {
        match self {
            #[cfg(feature = "nori")]
            Tokenizer::Nori(_) => self.prepare().map(|_| ()),
            Tokenizer::NGram { .. } | Tokenizer::Pattern { .. } => self.prepare().map(|_| ()),
            _ => Ok(()),
        }
    }

    pub fn tokenize(&self, text: &str) -> AnalysisResult<Vec<String>> {
        self.tokenize_with_offsets(text)?.into_terms()
    }

    /// Tokenize source text with explicit offsets, positions, and final source coordinates.
    pub fn tokenize_with_offsets(&self, text: &str) -> AnalysisResult<crate::AnalyzedText> {
        self.tokenize_mapped(&crate::FilteredText::new(text))
    }

    /// Tokenize with reservations for owned buffers and retained source provenance.
    ///
    /// Configuration resources and library regex workspaces remain separately managed. Built-in word tokenizers poll while scanning; configured pattern tokenizers poll between library searches, and loops owned by analysis poll while emitting. Cloning the underlying analyzed value creates separate, unreserved token buffers.
    pub fn tokenize_with_offsets_budgeted(
        &self,
        text: &str,
        budget: &MemoryBudget,
        poll: impl FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<Budgeted<crate::AnalyzedText>> {
        self.tokenize_mapped_budgeted(&crate::FilteredText::new(text), budget, poll)
    }

    /// Tokenize a retained character-filter result under a caller allowance.
    ///
    /// Previously prepared source allocations keep their existing owner. Newly built coordinate indexes, token buffers, terms, and retained source copies use `budget`; failed calls leave the input's coordinate caches unchanged.
    ///
    /// ```
    /// use uqa_analysis::{CharFilter, Tokenizer};
    /// use uqa_core::memory::MemoryBudget;
    /// let budget = MemoryBudget::new(16 * 1024);
    /// let filtered = CharFilter::HTMLStrip.filter_with_offsets_budgeted(
    ///     "<b>韓🙂</b>", &budget, &mut || Ok(()),
    /// )?;
    /// let tokens = Tokenizer::Whitespace.tokenize_mapped_budgeted(
    ///     &filtered, &budget, || Ok(()),
    /// )?;
    /// assert_eq!(tokens.tokens()[0].term(), "韓🙂");
    /// assert_eq!(tokens.tokens()[0].offsets().unwrap().utf8, 3..10);
    /// drop(filtered);
    /// assert!(budget.used() > 0);
    /// drop(tokens);
    /// assert_eq!(budget.used(), 0);
    /// # Ok::<(), uqa_analysis::AnalysisError>(())
    /// ```
    pub fn tokenize_mapped_budgeted(
        &self,
        text: &crate::FilteredText<'_>,
        budget: &MemoryBudget,
        mut poll: impl FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<Budgeted<crate::AnalyzedText>> {
        poll()?;
        self.prepare()?
            .tokenize_mapped_budgeted(text, budget, &mut poll)
    }

    pub(crate) fn tokenize_mapped(
        &self,
        text: &crate::FilteredText<'_>,
    ) -> AnalysisResult<crate::AnalyzedText> {
        self.prepare()?.tokenize_mapped(text)
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

pub(super) fn standard_word_class() -> AnalysisResult<&'static regex_syntax::hir::ClassUnicode> {
    static CLASS: OnceLock<Result<regex_syntax::hir::ClassUnicode, String>> = OnceLock::new();
    CLASS
        .get_or_init(|| class(r"\w"))
        .as_ref()
        .map_err(|message| AnalysisError::BuiltInRegex {
            component: "standard tokenizer",
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
