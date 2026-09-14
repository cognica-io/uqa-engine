//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Strict serializable inputs for Korean stages in the common analyzer pipeline.

use serde::{Deserialize, Serialize};

use super::{DecompoundMode, NoriOptions, POSTag, DEFAULT_NORI_DICTIONARY};
use crate::{Analyzer, TokenFilter, Tokenizer};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NoriTokenizerConfig {
    pub dictionary: String,
    pub decompound_mode: DecompoundMode,
    pub output_unknown_unigrams: bool,
    pub discard_punctuation: bool,
    pub user_dictionary: Option<String>,
}

impl Default for NoriTokenizerConfig {
    fn default() -> Self {
        Self {
            dictionary: DEFAULT_NORI_DICTIONARY.into(),
            decompound_mode: DecompoundMode::Discard,
            output_unknown_unigrams: false,
            discard_punctuation: true,
            user_dictionary: None,
        }
    }
}

impl NoriTokenizerConfig {
    pub fn options(&self) -> NoriOptions {
        NoriOptions {
            decompound_mode: self.decompound_mode,
            output_unknown_unigrams: self.output_unknown_unigrams,
            discard_punctuation: self.discard_punctuation,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NoriPOSConfig {
    pub stop_tags: Option<Vec<POSTag>>,
}

/// Parameterless stages reject unknown properties while preserving their tagged JSON shape.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmptyFilterConfig {}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SimpleLowercaseConfig {
    pub unicode_profile: String,
}

impl Default for SimpleLowercaseConfig {
    fn default() -> Self {
        Self {
            unicode_profile: "jdk21".into(),
        }
    }
}

/// Default Korean analysis configuration; constructing it changes no named registry or catalog.
///
/// ```
/// use uqa_analysis::{AnalyzerLimits, AnalyzerResources, Tokenizer};
/// use uqa_analysis::nori::nori_analyzer;
/// let mut config = nori_analyzer();
/// if let Tokenizer::Nori(tokenizer) = &mut config.tokenizer {
///     tokenizer.user_dictionary = Some("세종시 세종 시".into());
/// }
/// let compiled = config.compile()?;
/// assert_eq!(compiled.analyze("세종시")?, ["세종", "시"]);
/// assert_eq!(compiled.normalize("喜悲哀歡 İ UQA")?, "喜悲哀歡 i uqa");
/// let restored = AnalyzerResources::new(AnalyzerLimits::default())
///     .restore_json(compiled.descriptor().canonical_json())?;
/// assert_eq!(restored.analyze_tokens("세종시")?, compiled.analyze_tokens("세종시")?);
/// # Ok::<(), uqa_analysis::AnalysisError>(())
/// ```
pub fn nori_analyzer() -> Analyzer {
    Analyzer::new(
        Tokenizer::Nori(NoriTokenizerConfig::default()),
        vec![
            TokenFilter::NoriPartOfSpeech(NoriPOSConfig::default()),
            TokenFilter::NoriReadingForm(EmptyFilterConfig::default()),
            TokenFilter::UnicodeSimpleLowercase(SimpleLowercaseConfig::default()),
        ],
        Vec::new(),
    )
}
