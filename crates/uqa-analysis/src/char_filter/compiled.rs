//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Prepared character stages share the existing ordered source-edit semantics.

use regex::Regex;
use uqa_core::memory::MemoryBudget;

use super::literals::{replace_entities, LiteralMappings};
use super::replacement::Replacement;
use super::stream::{replace_html, replace_pattern};
use super::CharFilter;
use crate::cooperative_regex::CooperativeRegex;
use crate::{AnalysisError, AnalysisResult, FilteredText};

#[derive(Debug)]
pub(crate) enum PreparedCharFilter<'a> {
    HTMLStrip,
    CJKWidth,
    #[cfg(feature = "kuromoji")]
    KuromojiIterationMark {
        normalize_kanji: bool,
        normalize_kana: bool,
    },
    Mapping(LiteralMappings),
    PatternReplace {
        expression: Box<CooperativeRegex>,
        replacement: Replacement<'a>,
    },
}

impl CharFilter {
    pub(crate) fn prepare(&self) -> AnalysisResult<PreparedCharFilter<'_>> {
        crate::work::inline(self.prepare_with(&mut ()))
    }

    pub(crate) async fn prepare_with(
        &self,
        work: &mut impl crate::work::Work<AnalysisError>,
    ) -> AnalysisResult<PreparedCharFilter<'_>> {
        work.checkpoint().await?;
        Ok(match self {
            Self::HTMLStrip => PreparedCharFilter::HTMLStrip,
            Self::CJKWidth => PreparedCharFilter::CJKWidth,
            #[cfg(feature = "kuromoji")]
            Self::KuromojiIterationMark {
                normalize_kanji,
                normalize_kana,
            } => PreparedCharFilter::KuromojiIterationMark {
                normalize_kanji: *normalize_kanji,
                normalize_kana: *normalize_kana,
            },
            Self::Mapping { mapping } => {
                PreparedCharFilter::Mapping(LiteralMappings::prepare(mapping, work).await?)
            }
            Self::PatternReplace {
                pattern,
                replacement,
            } => {
                let expression =
                    Regex::new(pattern).map_err(|source| AnalysisError::InvalidRegex {
                        component: "pattern-replace character filter",
                        pattern: pattern.clone(),
                        source,
                    })?;
                let replacement = Replacement::prepare_with(replacement, &expression, work).await?;
                let expression = CooperativeRegex::compile(pattern).map_err(|source| {
                    AnalysisError::InvalidRegex {
                        component: "pattern-replace character filter",
                        pattern: pattern.clone(),
                        source,
                    }
                })?;
                PreparedCharFilter::PatternReplace {
                    expression: Box::new(expression),
                    replacement,
                }
            }
        })
    }
}

impl PreparedCharFilter<'_> {
    pub(crate) async fn into_owned_with(
        self,
        work: &mut impl crate::work::Work<AnalysisError>,
    ) -> AnalysisResult<PreparedCharFilter<'static>> {
        Ok(match self {
            Self::HTMLStrip => PreparedCharFilter::HTMLStrip,
            Self::CJKWidth => PreparedCharFilter::CJKWidth,
            #[cfg(feature = "kuromoji")]
            Self::KuromojiIterationMark {
                normalize_kanji,
                normalize_kana,
            } => PreparedCharFilter::KuromojiIterationMark {
                normalize_kanji,
                normalize_kana,
            },
            Self::Mapping(mapping) => PreparedCharFilter::Mapping(mapping),
            Self::PatternReplace {
                expression,
                replacement,
            } => PreparedCharFilter::PatternReplace {
                expression,
                replacement: replacement.into_owned_with(work).await?,
            },
        })
    }

    pub(crate) fn filter_mapped<'a>(
        &self,
        text: FilteredText<'a>,
    ) -> AnalysisResult<FilteredText<'a>> {
        let budget = text.unbounded_budget();
        self.filter_mapped_budgeted(text, &budget, &mut || Ok(()))
    }

    pub(crate) fn filter_mapped_budgeted<'a>(
        &self,
        mut text: FilteredText<'a>,
        budget: &MemoryBudget,
        poll: &mut dyn FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<FilteredText<'a>> {
        poll()?;
        match self {
            Self::CJKWidth => super::width::replace_width(&mut text, budget, poll)?,
            #[cfg(feature = "kuromoji")]
            Self::KuromojiIterationMark {
                normalize_kanji,
                normalize_kana,
            } => super::iteration::replace(
                &mut text,
                *normalize_kanji,
                *normalize_kana,
                budget,
                poll,
            )?,
            Self::HTMLStrip => {
                replace_html(&mut text, budget, poll)?;
                replace_entities(&mut text, budget, poll)?;
            }
            Self::Mapping(mapping) => mapping.replace(&mut text, budget, poll)?,
            Self::PatternReplace {
                expression,
                replacement,
            } => replace_pattern(&mut text, expression, replacement, budget, poll)?,
        }
        text.prepare_coordinates(budget, poll)?;
        poll()?;
        Ok(text)
    }
}
