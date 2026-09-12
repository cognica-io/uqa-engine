//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Errors produced while executing an analysis pipeline.

use regex::Error as RegexError;

use crate::token_filter::SynonymFileError;

/// An invalid analyzer is an execution error, never an empty token stream.
#[derive(Debug, thiserror::Error)]
pub enum AnalysisError {
    #[error("{coordinate} offset {offset} is not a Unicode scalar boundary within text of length {length}")]
    InvalidTextOffset {
        coordinate: &'static str,
        offset: usize,
        length: usize,
    },
    #[error("text span start {start} exceeds its end {end}")]
    InvalidTextSpan { start: usize, end: usize },
    #[error("character-filter edits overlap or are out of order")]
    OverlappingTextEdits,
    #[error("invalid {component} regular expression `{pattern}`: {source}")]
    InvalidRegex {
        component: &'static str,
        pattern: String,
        #[source]
        source: RegexError,
    },
    #[error("failed to initialize built-in {component} regular expression: {message}")]
    BuiltInRegex {
        component: &'static str,
        message: String,
    },
    #[error(
        "invalid {component} gram bounds: min_gram must be at least 1 and max_gram must be greater than or equal to min_gram (got {min_gram}..={max_gram})"
    )]
    InvalidGramBounds {
        component: &'static str,
        min_gram: usize,
        max_gram: usize,
    },
    #[error(transparent)]
    SynonymFile(#[from] SynonymFileError),
}

pub type AnalysisResult<T> = std::result::Result<T, AnalysisError>;
