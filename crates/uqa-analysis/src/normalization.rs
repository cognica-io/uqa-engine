//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Explicit normalization plans retain profile identity independently of analysis stages.

use serde::{Deserialize, Serialize};
use uqa_core::memory::{Budgeted, MemoryBudget};

use crate::{AnalysisError, AnalysisResult};

mod resolved;
pub(crate) mod text;
pub(crate) use resolved::PreparedNormalization;

/// Omitting this setting preserves legacy inference; an explicit unavailable plan disables it.
///
/// ```
/// use uqa_analysis::{Analyzer, NormalizationConfig};
/// let compiled = Analyzer::default()
///     .with_normalization(NormalizationConfig::CJKWidth)
///     .compile()?;
/// assert_eq!(compiled.normalize("ＵＱＡ ｶﾞ")?, "UQA ガ");
/// # Ok::<(), uqa_analysis::AnalysisError>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum NormalizationConfig {
    #[serde(deserialize_with = "deserialize_empty")]
    Unavailable,
    #[serde(rename = "cjk_width", deserialize_with = "deserialize_empty")]
    CJKWidth,
    #[cfg(any(feature = "nori", feature = "kuromoji"))]
    UnicodeSimpleLowercase { profile: UnicodeProfile },
    #[cfg(any(feature = "nori", feature = "kuromoji"))]
    #[serde(rename = "cjk_width_simple_lowercase")]
    CJKWidthSimpleLowercase { profile: UnicodeProfile },
}

fn deserialize_empty<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<(), D::Error> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Empty {}
    Empty::deserialize(deserializer).map(|_| ())
}

/// Select a language's validated Unicode table without depending on the tokenizer type.
#[cfg(any(feature = "nori", feature = "kuromoji"))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "provider", rename_all = "snake_case", deny_unknown_fields)]
pub enum UnicodeProfile {
    #[cfg(feature = "nori")]
    Nori { dictionary: String },
    #[cfg(feature = "kuromoji")]
    Kuromoji { dictionary: String },
}

impl NormalizationConfig {
    pub(crate) fn width(&self) -> bool {
        match self {
            Self::CJKWidth => true,
            #[cfg(any(feature = "nori", feature = "kuromoji"))]
            Self::CJKWidthSimpleLowercase { .. } => true,
            _ => false,
        }
    }

    pub(crate) fn stage_count(&self) -> usize {
        match self {
            Self::Unavailable => 0,
            Self::CJKWidth => 1,
            #[cfg(any(feature = "nori", feature = "kuromoji"))]
            Self::UnicodeSimpleLowercase { .. } => 1,
            #[cfg(any(feature = "nori", feature = "kuromoji"))]
            Self::CJKWidthSimpleLowercase { .. } => 2,
        }
    }

    #[cfg(any(feature = "nori", feature = "kuromoji"))]
    pub(crate) fn profile(&self) -> Option<&UnicodeProfile> {
        match self {
            Self::UnicodeSimpleLowercase { profile }
            | Self::CJKWidthSimpleLowercase { profile } => Some(profile),
            _ => None,
        }
    }

    #[cfg(any(feature = "nori", feature = "kuromoji"))]
    pub(crate) fn profile_mut(&mut self) -> Option<&mut UnicodeProfile> {
        match self {
            Self::UnicodeSimpleLowercase { profile }
            | Self::CJKWidthSimpleLowercase { profile } => Some(profile),
            _ => None,
        }
    }

    #[cfg(any(feature = "nori", feature = "kuromoji"))]
    pub(crate) fn validate(&self) -> AnalysisResult<()> {
        match self.profile() {
            #[cfg(feature = "nori")]
            Some(UnicodeProfile::Nori { dictionary }) => {
                crate::nori::NoriResources::default()
                    .load(&crate::nori::pipeline::request(dictionary)?)?;
            }
            #[cfg(feature = "kuromoji")]
            Some(UnicodeProfile::Kuromoji { dictionary }) => {
                crate::kuromoji::KuromojiResources::default()
                    .load(&crate::kuromoji::pipeline::request(dictionary)?)?;
            }
            None => {}
        }
        Ok(())
    }
}

struct WidthPolicy;
impl text::Policy for WidthPolicy {
    fn input(&self, length: usize) -> AnalysisResult<()> {
        crate::descriptor::limits::check_limit(
            "normalization input UTF-16 units",
            length,
            16 * 1024 * 1024,
        )
    }
    fn output(&self, length: usize) -> AnalysisResult<()> {
        crate::descriptor::limits::check_limit(
            "normalization output UTF-16 units",
            length,
            64 * 1024 * 1024,
        )
    }
    fn invalid_scalar(&self) -> AnalysisError {
        AnalysisError::Descriptor("normalization produced an invalid scalar result")
    }
}

fn width_budgeted(
    input: &str,
    budget: &MemoryBudget,
    poll: &mut dyn FnMut() -> AnalysisResult<()>,
) -> AnalysisResult<Budgeted<String>> {
    text::run(input, true, &WidthPolicy, budget, poll)
}
