//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Token-level filters that run after tokenization.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{AnalysisError, AnalysisResult};

mod ascii;
mod compiled;
mod lowercase;
mod stream;
mod synonyms;
pub(crate) use compiled::PreparedTokenFilter;
use synonyms::parse_synonym_body;
pub(crate) use synonyms::parse_synonym_body_bounded;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TokenFilter {
    #[cfg(feature = "nori")]
    #[serde(rename = "nori_part_of_speech")]
    NoriPartOfSpeech(crate::nori::NoriPOSConfig),
    #[cfg(feature = "nori")]
    #[serde(rename = "nori_readingform")]
    NoriReadingForm(crate::nori::EmptyFilterConfig),
    #[cfg(feature = "nori")]
    #[serde(rename = "unicode_simple_lowercase")]
    UnicodeSimpleLowercase(crate::nori::SimpleLowercaseConfig),
    #[cfg(feature = "nori")]
    #[serde(rename = "nori_number")]
    NoriNumber(crate::nori::EmptyFilterConfig),
    Lowercase,
    Stop {
        #[serde(default = "default_stop_language")]
        language: String,
        #[serde(default)]
        custom_words: Vec<String>,
    },
    PorterStem,
    // The alias keeps catalogs persisted before the stable tag existed
    // deserializable: releases up to 0.1.2 wrote the derived spelling.
    #[serde(rename = "ascii_folding", alias = "a_s_c_i_i_folding")]
    ASCIIFolding,
    Synonym {
        /// Inline `term -> [expansion, ...]` mapping. Empty when the
        /// filter sources its mappings from `synonyms_path` instead.
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        synonyms: BTreeMap<String, Vec<String>>,
        /// Path to a Solr / Elasticsearch-style synonym file. The file
        /// is parsed every time the filter runs so reload-on-edit is
        /// free; for production use cache the parsed map upstream.
        /// Optional path to a reloadable synonym map.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        synonyms_path: Option<PathBuf>,
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

/// Errors raised when constructing a `Synonym` filter from a file.
#[derive(Debug, thiserror::Error)]
pub enum SynonymFileError {
    #[error("synonym file not found: {0}")]
    NotFound(PathBuf),
    #[error("failed to read synonym file `{path}`: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

impl TokenFilter {
    /// Validate configuration without filtering tokens. File-backed synonym
    /// filters are read here so registration rejects missing/unreadable paths;
    /// [`Self::filter`] reads them again on every execution to detect later
    /// deletion, permission changes, and edits.
    pub fn validate(&self) -> AnalysisResult<()> {
        match self {
            #[cfg(feature = "nori")]
            TokenFilter::UnicodeSimpleLowercase(_) => self.prepare().map(|_| ()),
            TokenFilter::Synonym {
                synonyms_path: Some(_),
                ..
            }
            | TokenFilter::Ngram { .. }
            | TokenFilter::EdgeNgram { .. } => self.prepare().map(|_| ()),
            _ => Ok(()),
        }
    }

    /// Build a `Synonym` filter from a Solr or Elasticsearch synonym file.
    /// In this format,
    /// blank lines and `#` comments are skipped, `a => b, c` defines a
    /// one-way mapping, and `a, b, c` defines an equivalent group
    /// where every term expands to the other group members.
    pub fn synonym_from_path<P: AsRef<Path>>(path: P) -> Result<Self, SynonymFileError> {
        let path = path.as_ref();
        if !path.exists() {
            return Err(SynonymFileError::NotFound(path.to_path_buf()));
        }
        // Read once at construction so unreadable paths fail before the
        // analyzer is registered. Execution reads it again to support reloads
        // and to make deletion/revocation visible to callers.
        read_synonym_file(path)?;
        Ok(TokenFilter::Synonym {
            synonyms: BTreeMap::new(),
            synonyms_path: Some(path.to_path_buf()),
        })
    }

    /// Parse a synonym file into the same shape `Synonym::synonyms`
    /// uses. Public so engines can pre-resolve a path to an inline map.
    pub fn parse_synonym_file(
        path: &Path,
    ) -> Result<BTreeMap<String, Vec<String>>, SynonymFileError> {
        let body = read_synonym_file(path)?;
        Ok(parse_synonym_body(&body))
    }
}

fn default_stop_language() -> String {
    "english".to_string()
}

impl TokenFilter {
    pub fn filter(&self, tokens: Vec<String>) -> AnalysisResult<Vec<String>> {
        stream::filter(
            &self.prepare()?,
            crate::token::TokenBatch::from_terms(tokens),
        )?
        .into_terms()
    }

    /// Transform tokens while retaining their source spans and graph end state.
    pub fn filter_analyzed(
        &self,
        input: crate::AnalyzedText,
    ) -> AnalysisResult<crate::AnalyzedText> {
        self.prepare()?.filter_analyzed(input)
    }

    /// Consume a reserved stream and retain its allowance through every common or Korean token filter.
    ///
    /// The returned tokens retain their own terms, morphology, terminal state and vector reservations, and share existing source leases. Removed buffers release their reservations after destruction; replacements and copies reserve before allocation. Byte-limit and callback errors return no partial result. Immutable filter preparation and caller-owned configuration have separate ownership.
    ///
    /// ```
    /// use uqa_analysis::{TokenFilter, Tokenizer};
    /// use uqa_core::memory::MemoryBudget;
    /// let budget = MemoryBudget::new(64 * 1024);
    /// let tokens = Tokenizer::Whitespace.tokenize_with_offsets_budgeted(
    ///     "UQA AND", &budget, || Ok(()),
    /// )?;
    /// let output = TokenFilter::Lowercase.filter_analyzed_budgeted(tokens, || Ok(()))?;
    /// assert_eq!(output.tokens()[0].term(), "uqa");
    /// drop(output);
    /// assert_eq!(budget.used(), 0);
    /// # Ok::<(), uqa_analysis::AnalysisError>(())
    /// ```
    pub fn filter_analyzed_budgeted(
        &self,
        input: uqa_core::memory::Budgeted<crate::AnalyzedText>,
        mut poll: impl FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<uqa_core::memory::Budgeted<crate::AnalyzedText>> {
        poll()?;
        self.prepare()?.filter_analyzed_budgeted(input, &mut poll)
    }
}

fn read_synonym_file(path: &Path) -> Result<String, SynonymFileError> {
    fs::read_to_string(path).map_err(|source| {
        if source.kind() == io::ErrorKind::NotFound {
            SynonymFileError::NotFound(path.to_path_buf())
        } else {
            SynonymFileError::Io {
                path: path.to_path_buf(),
                source,
            }
        }
    })
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

const ENGLISH_STOP_WORDS: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "but", "by", "for", "if", "in", "into", "is", "it",
    "no", "not", "of", "on", "or", "such", "that", "the", "their", "then", "there", "these",
    "they", "this", "to", "was", "were", "will", "with", "would", "can", "could", "do", "does",
    "did", "had", "has", "have", "he", "her", "him", "his", "how", "i", "its", "may", "me", "my",
    "nor", "our", "own", "she", "should", "so", "some", "than", "too", "us", "very", "we", "what",
    "when", "which", "who", "whom", "why", "you", "your",
];

pub(crate) fn builtin_stop_words(language: &str) -> &'static [&'static str] {
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
        assert_eq!(
            f.filter(v(&["Hello", "WORLD"])).unwrap(),
            v(&["hello", "world"])
        );
    }

    #[test]
    fn stop_removes_english_stop_words() {
        let f = TokenFilter::Stop {
            language: "english".to_string(),
            custom_words: vec![],
        };
        assert_eq!(
            f.filter(v(&["the", "rust", "is", "fast"])).unwrap(),
            v(&["rust", "fast"])
        );
    }

    #[test]
    fn stop_includes_custom_words() {
        let f = TokenFilter::Stop {
            language: "english".to_string(),
            custom_words: vec!["foo".to_string()],
        };
        assert_eq!(f.filter(v(&["foo", "bar", "the"])).unwrap(), v(&["bar"]));
    }

    #[test]
    fn porter_stem_runs() {
        let f = TokenFilter::PorterStem;
        assert_eq!(
            f.filter(v(&["caresses", "ponies"])).unwrap(),
            v(&["caress", "poni"])
        );
    }

    #[test]
    fn ascii_folding_strips_diacritics() {
        let f = TokenFilter::ASCIIFolding;
        assert_eq!(
            f.filter(v(&["café", "naïve"])).unwrap(),
            v(&["cafe", "naive"])
        );
    }

    #[test]
    fn ascii_folding_preserves_cjk() {
        let f = TokenFilter::ASCIIFolding;
        assert_eq!(f.filter(v(&["한글"])).unwrap(), v(&["한글"]));
    }

    #[test]
    fn synonym_appends_alternatives() {
        let mut m: BTreeMap<String, Vec<String>> = BTreeMap::new();
        m.insert(
            "car".to_string(),
            vec!["auto".to_string(), "vehicle".to_string()],
        );
        let f = TokenFilter::Synonym {
            synonyms: m,
            synonyms_path: None,
        };
        assert_eq!(
            f.filter(v(&["fast", "car"])).unwrap(),
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
        assert_eq!(f.filter(v(&["abc"])).unwrap(), v(&["ab", "bc", "abc"]));
    }

    #[test]
    fn ngram_drops_short_unless_keep_set() {
        let f_drop = TokenFilter::Ngram {
            min_gram: 3,
            max_gram: 4,
            keep_short: false,
        };
        assert!(f_drop.filter(v(&["ab"])).unwrap().is_empty());

        let f_keep = TokenFilter::Ngram {
            min_gram: 3,
            max_gram: 4,
            keep_short: true,
        };
        assert_eq!(f_keep.filter(v(&["ab"])).unwrap(), v(&["ab"]));
    }

    #[test]
    fn edge_ngram_emits_prefixes() {
        let f = TokenFilter::EdgeNgram {
            min_gram: 1,
            max_gram: 3,
        };
        assert_eq!(f.filter(v(&["abcd"])).unwrap(), v(&["a", "ab", "abc"]));
    }

    #[test]
    fn length_bounds_token_size() {
        let f = TokenFilter::Length {
            min_length: 2,
            max_length: 4,
        };
        assert_eq!(
            f.filter(v(&["a", "ab", "abcd", "abcde"])).unwrap(),
            v(&["ab", "abcd"])
        );
    }
}
