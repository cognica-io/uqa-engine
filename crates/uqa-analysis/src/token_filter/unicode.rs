//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Explicit language profiles preserve the original Korean string configuration.

use serde::{Deserialize, Serialize};

#[cfg(not(feature = "nori"))]
use crate::AnalysisResult;
use crate::UnicodeProfile;

/// Java simple lowercase uses a validated dictionary's Unicode table.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SimpleLowercaseConfig {
    pub unicode_profile: UnicodeProfileSource,
}

/// Strings retain their historical Nori meaning, including the `jdk21` default. Explicit objects select the provider independently of the tokenizer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum UnicodeProfileSource {
    LegacyNori(String),
    Explicit(UnicodeProfile),
}

impl Default for UnicodeProfileSource {
    fn default() -> Self {
        Self::LegacyNori("jdk21".into())
    }
}

impl From<String> for UnicodeProfileSource {
    fn from(value: String) -> Self {
        Self::LegacyNori(value)
    }
}

impl From<&str> for UnicodeProfileSource {
    fn from(value: &str) -> Self {
        Self::LegacyNori(value.into())
    }
}

impl From<UnicodeProfile> for UnicodeProfileSource {
    fn from(value: UnicodeProfile) -> Self {
        Self::Explicit(value)
    }
}

impl UnicodeProfileSource {
    #[cfg(not(feature = "nori"))]
    pub(crate) fn validate_features(&self) -> AnalysisResult<()> {
        if matches!(self, Self::LegacyNori(_)) {
            return Err(crate::AnalysisError::Descriptor(
                "string Unicode profiles require the nori feature",
            ));
        }
        Ok(())
    }

    #[cfg(feature = "nori")]
    #[cfg_attr(
        not(feature = "kuromoji"),
        allow(
            clippy::unnecessary_wraps,
            reason = "The optional result also represents Japanese profiles when that feature is enabled."
        )
    )]
    pub(crate) fn nori_dictionary(&self) -> Option<&str> {
        match self {
            Self::LegacyNori(dictionary) | Self::Explicit(UnicodeProfile::Nori { dictionary }) => {
                Some(dictionary)
            }
            #[cfg(feature = "kuromoji")]
            _ => None,
        }
    }

    #[cfg(feature = "nori")]
    #[cfg_attr(
        not(feature = "kuromoji"),
        allow(
            clippy::unnecessary_wraps,
            reason = "The optional result also represents Japanese profiles when that feature is enabled."
        )
    )]
    pub(crate) fn nori_dictionary_mut(&mut self) -> Option<&mut String> {
        match self {
            Self::LegacyNori(dictionary) | Self::Explicit(UnicodeProfile::Nori { dictionary }) => {
                Some(dictionary)
            }
            #[cfg(feature = "kuromoji")]
            _ => None,
        }
    }

    #[cfg(feature = "kuromoji")]
    pub(crate) fn kuromoji_dictionary(&self) -> Option<&str> {
        match self {
            Self::Explicit(UnicodeProfile::Kuromoji { dictionary }) => Some(dictionary),
            _ => None,
        }
    }

    #[cfg(feature = "kuromoji")]
    pub(crate) fn kuromoji_dictionary_mut(&mut self) -> Option<&mut String> {
        match self {
            Self::Explicit(UnicodeProfile::Kuromoji { dictionary }) => Some(dictionary),
            _ => None,
        }
    }
}
