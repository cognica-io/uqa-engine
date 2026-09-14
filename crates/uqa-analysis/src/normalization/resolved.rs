//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retained executable normalization plans select immutable profiles without tokenizer inference.

use super::NormalizationConfig;
#[cfg(any(feature = "nori", feature = "kuromoji"))]
use super::UnicodeProfile;
use crate::{AnalysisError, AnalysisResult};
#[cfg(any(feature = "nori", feature = "kuromoji"))]
use std::sync::Arc;
use uqa_core::memory::{Budgeted, MemoryBudget};

#[derive(Debug)]
pub(crate) enum PreparedNormalization {
    Unavailable,
    Width,
    #[cfg(feature = "nori")]
    Nori {
        width: bool,
        profile: Arc<crate::nori::ResolvedDictionary>,
    },
    #[cfg(feature = "kuromoji")]
    Kuromoji {
        width: bool,
        profile: Arc<crate::kuromoji::ResolvedDictionary>,
    },
}

impl PreparedNormalization {
    #[cfg_attr(
        not(any(feature = "nori", feature = "kuromoji")),
        expect(
            clippy::unnecessary_wraps,
            reason = "Language profiles make preparation fallible when enabled"
        )
    )]
    pub(crate) fn new(
        config: Option<&NormalizationConfig>,
        #[cfg(feature = "nori")] nori: Option<Arc<crate::nori::ResolvedDictionary>>,
        #[cfg(feature = "kuromoji")] kuromoji: Option<Arc<crate::kuromoji::ResolvedDictionary>>,
    ) -> AnalysisResult<Self> {
        let Some(config) = config else {
            #[cfg(feature = "nori")]
            if let Some(profile) = nori {
                return Ok(Self::Nori {
                    width: false,
                    profile,
                });
            }
            return Ok(Self::Unavailable);
        };
        Ok(match config {
            NormalizationConfig::Unavailable => Self::Unavailable,
            NormalizationConfig::CJKWidth => Self::Width,
            #[cfg(any(feature = "nori", feature = "kuromoji"))]
            NormalizationConfig::UnicodeSimpleLowercase { profile }
            | NormalizationConfig::CJKWidthSimpleLowercase { profile } => match profile {
                #[cfg(feature = "nori")]
                UnicodeProfile::Nori { .. } => Self::Nori {
                    width: config.width(),
                    profile: nori.ok_or(AnalysisError::Descriptor(
                        "missing resolved Korean normalization profile",
                    ))?,
                },
                #[cfg(feature = "kuromoji")]
                UnicodeProfile::Kuromoji { .. } => Self::Kuromoji {
                    width: config.width(),
                    profile: kuromoji.ok_or(AnalysisError::Descriptor(
                        "missing resolved Japanese normalization profile",
                    ))?,
                },
            },
        })
    }

    pub(crate) fn normalize_budgeted(
        &self,
        input: &str,
        budget: &MemoryBudget,
        poll: &mut dyn FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<Budgeted<String>> {
        match self {
            Self::Unavailable => Err(AnalysisError::NormalizationUnavailable),
            Self::Width => super::width_budgeted(input, budget, poll),
            #[cfg(feature = "nori")]
            Self::Nori { width, profile } => crate::nori::normalization::normalize_budgeted(
                input,
                *width,
                profile.model(),
                crate::nori::NoriLimits::default(),
                budget,
                poll,
            ),
            #[cfg(feature = "kuromoji")]
            Self::Kuromoji { width, profile } => {
                crate::kuromoji::normalization::normalize_budgeted(
                    input,
                    *width,
                    Some(profile.model()),
                    crate::kuromoji::KuromojiLimits::default(),
                    budget,
                    poll,
                )
            }
        }
    }
}
