//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Strict Japanese tokenizer inputs resolve examples once against the selected immutable model.

use serde::{Deserialize, Serialize};

use super::{KuromojiMode, KuromojiOptions, DEFAULT_KUROMOJI_DICTIONARY};

/// Japanese tokenizer inputs for a common pipeline without implicit filters or normalization.
///
/// ```
/// use uqa_analysis::{Analyzer, AnalyzerLimits, AnalyzerResources, Tokenizer};
/// use uqa_analysis::kuromoji::KuromojiTokenizerConfig;
/// let config = Analyzer::new(Tokenizer::Kuromoji(KuromojiTokenizerConfig {
///     user_dictionary: Some("東京大学,東京 大学,トウキョウ ダイガク,名詞".into()),
///     ..Default::default()
/// }), Vec::new(), Vec::new());
/// let compiled = config.compile()?;
/// assert_eq!(compiled.analyze("東京大学")?, ["東京", "大学"]);
/// let restored = AnalyzerResources::new(AnalyzerLimits::default())
///     .restore_json(compiled.descriptor().canonical_json())?;
/// assert_eq!(restored.analyze_tokens("東京大学")?, compiled.analyze_tokens("東京大学")?);
/// # Ok::<(), uqa_analysis::AnalysisError>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct KuromojiTokenizerConfig {
    pub dictionary: String,
    pub mode: KuromojiMode,
    pub discard_punctuation: bool,
    pub discard_compound_token: bool,
    pub user_dictionary: Option<String>,
    pub n_best_cost: i32,
    /// Compilation replaces example probes with their effective signed cost in the descriptor.
    pub n_best_examples: Option<String>,
}

impl Default for KuromojiTokenizerConfig {
    fn default() -> Self {
        let options = KuromojiOptions::default();
        Self {
            dictionary: DEFAULT_KUROMOJI_DICTIONARY.into(),
            mode: options.mode,
            discard_punctuation: options.discard_punctuation,
            discard_compound_token: options.discard_compound_token,
            user_dictionary: None,
            n_best_cost: options.n_best_cost,
            n_best_examples: None,
        }
    }
}

impl KuromojiTokenizerConfig {
    /// Explicit options before optional example-derived cost estimation.
    pub fn options(&self) -> KuromojiOptions {
        KuromojiOptions {
            mode: self.mode,
            discard_punctuation: self.discard_punctuation,
            discard_compound_token: self.discard_compound_token,
            n_best_cost: self.n_best_cost,
        }
    }
}
