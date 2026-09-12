//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Prepared character stages share the existing ordered source-edit semantics.

use regex::Regex;
use std::borrow::Cow;

use super::{
    html_tag_re, mapping_longest_first, replace_literal, replace_pattern, CharFilter, HTML_ENTITIES,
};
use crate::{AnalysisError, AnalysisResult, FilteredText};

#[derive(Debug)]
pub(crate) enum PreparedCharFilter<'a> {
    HTMLStrip(&'static Regex),
    Mapping(Vec<(String, String)>),
    PatternReplace {
        expression: Regex,
        replacement: Cow<'a, str>,
    },
}

impl CharFilter {
    pub(crate) fn prepare(&self) -> AnalysisResult<PreparedCharFilter<'_>> {
        Ok(match self {
            Self::HTMLStrip => PreparedCharFilter::HTMLStrip(html_tag_re()?),
            Self::Mapping { mapping } => {
                PreparedCharFilter::Mapping(mapping_longest_first(mapping))
            }
            Self::PatternReplace {
                pattern,
                replacement,
            } => PreparedCharFilter::PatternReplace {
                expression: Regex::new(pattern).map_err(|source| AnalysisError::InvalidRegex {
                    component: "pattern-replace character filter",
                    pattern: pattern.clone(),
                    source,
                })?,
                replacement: Cow::Borrowed(replacement),
            },
        })
    }
}

impl PreparedCharFilter<'_> {
    pub(crate) fn into_owned(self) -> PreparedCharFilter<'static> {
        match self {
            Self::HTMLStrip(expression) => PreparedCharFilter::HTMLStrip(expression),
            Self::Mapping(mapping) => PreparedCharFilter::Mapping(mapping),
            Self::PatternReplace {
                expression,
                replacement,
            } => PreparedCharFilter::PatternReplace {
                expression,
                replacement: Cow::Owned(replacement.into_owned()),
            },
        }
    }

    pub(crate) fn filter_mapped<'a>(
        &self,
        mut text: FilteredText<'a>,
    ) -> AnalysisResult<FilteredText<'a>> {
        match self {
            Self::HTMLStrip(expression) => {
                replace_pattern(&mut text, expression, " ")?;
                for (entity, replacement) in HTML_ENTITIES {
                    replace_literal(&mut text, entity, replacement)?;
                }
            }
            Self::Mapping(mapping) => {
                for (old, new) in mapping {
                    replace_literal(&mut text, old, new)?;
                }
            }
            Self::PatternReplace {
                expression,
                replacement,
            } => replace_pattern(&mut text, expression, replacement)?,
        }
        Ok(text)
    }
}
