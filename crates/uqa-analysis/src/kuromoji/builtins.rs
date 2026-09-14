//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Built-in configurations retain the pinned native analyzer's stage order and normalization.

use super::{
    KuromojiCompletionConfig, KuromojiMode, KuromojiPOSConfig, KuromojiStemConfig,
    KuromojiStopConfig, KuromojiTokenizerConfig, DEFAULT_KUROMOJI_DICTIONARY,
};
use crate::{
    Analyzer, CharFilter, EmptyFilterConfig, NormalizationConfig, SimpleLowercaseConfig,
    TokenFilter, Tokenizer, UnicodeProfile,
};

fn profile() -> UnicodeProfile {
    UnicodeProfile::Kuromoji {
        dictionary: DEFAULT_KUROMOJI_DICTIONARY.into(),
    }
}

fn lowercase() -> TokenFilter {
    TokenFilter::UnicodeSimpleLowercase(SimpleLowercaseConfig {
        unicode_profile: profile().into(),
    })
}

/// Construct default Japanese analysis without resolving a dictionary or changing the registry.
///
/// ```
/// use uqa_analysis::kuromoji::kuromoji_analyzer;
/// let compiled = kuromoji_analyzer().compile()?;
/// assert_eq!(compiled.analyze("ＵＱＡで走りました")?, ["uqa", "走る"]);
/// assert_eq!(compiled.normalize("ＵＱＡで走りました")?, "uqaで走りました");
/// # Ok::<(), uqa_analysis::AnalysisError>(())
/// ```
pub fn kuromoji_analyzer() -> Analyzer {
    Analyzer::new(
        Tokenizer::Kuromoji(KuromojiTokenizerConfig::default()),
        vec![
            TokenFilter::KuromojiBaseForm(EmptyFilterConfig::default()),
            TokenFilter::KuromojiPartOfSpeech(KuromojiPOSConfig::default()),
            TokenFilter::KuromojiStop(KuromojiStopConfig::default()),
            TokenFilter::KuromojiStem(KuromojiStemConfig::default()),
            lowercase(),
        ],
        vec![CharFilter::CJKWidth],
    )
    .with_normalization(NormalizationConfig::CJKWidthSimpleLowercase { profile: profile() })
}

/// Construct INDEX completion with NORMAL tokenization and separate width-only normalization.
///
/// ```
/// use uqa_analysis::kuromoji::kuromoji_completion_analyzer;
/// let compiled = kuromoji_completion_analyzer().compile()?;
/// assert_eq!(compiled.normalize("ＵＱＡ")?, "UQA");
/// assert_eq!(compiled.analyze("ＵＱＡ")?, ["uqa"]);
/// # Ok::<(), uqa_analysis::AnalysisError>(())
/// ```
pub fn kuromoji_completion_analyzer() -> Analyzer {
    Analyzer::new(
        Tokenizer::Kuromoji(KuromojiTokenizerConfig {
            mode: KuromojiMode::Normal,
            ..Default::default()
        }),
        vec![
            TokenFilter::KuromojiCompletion(KuromojiCompletionConfig::default()),
            lowercase(),
        ],
        vec![CharFilter::CJKWidth],
    )
    .with_normalization(NormalizationConfig::CJKWidth)
}
