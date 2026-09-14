//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Prepared character stages share the existing ordered source-edit semantics.

use regex::Regex;
use uqa_core::memory::MemoryBudget;

use super::replacement::Replacement;
use super::stream::{replace_html, replace_literal, replace_pattern};
use super::{mapping_longest_first, CharFilter, HTML_ENTITIES};
use crate::cooperative_regex::CooperativeRegex;
use crate::{AnalysisError, AnalysisResult, FilteredText};

#[derive(Debug)]
pub(crate) enum PreparedCharFilter<'a> {
    HTMLStrip,
    Mapping(Vec<(String, String)>),
    PatternReplace {
        expression: Box<CooperativeRegex>,
        replacement: Replacement<'a>,
    },
}

impl CharFilter {
    pub(crate) fn prepare(&self) -> AnalysisResult<PreparedCharFilter<'_>> {
        Ok(match self {
            Self::HTMLStrip => PreparedCharFilter::HTMLStrip,
            Self::Mapping { mapping } => {
                PreparedCharFilter::Mapping(mapping_longest_first(mapping))
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
                let replacement = Replacement::prepare(replacement, &expression);
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
    pub(crate) fn into_owned(self) -> PreparedCharFilter<'static> {
        match self {
            Self::HTMLStrip => PreparedCharFilter::HTMLStrip,
            Self::Mapping(mapping) => PreparedCharFilter::Mapping(mapping),
            Self::PatternReplace {
                expression,
                replacement,
            } => PreparedCharFilter::PatternReplace {
                expression,
                replacement: replacement.into_owned(),
            },
        }
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
            Self::HTMLStrip => {
                replace_html(&mut text, budget, poll)?;
                for (entity, replacement) in HTML_ENTITIES {
                    replace_literal(&mut text, entity, replacement, budget, poll)?;
                }
            }
            Self::Mapping(mapping) => {
                for (old, new) in mapping {
                    replace_literal(&mut text, old, new, budget, poll)?;
                }
            }
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
