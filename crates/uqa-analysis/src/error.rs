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
    #[cfg(feature = "nori")]
    #[error("this pipeline has no Korean normalization profile")]
    NormalizationUnavailable,
    #[error("invalid analyzer descriptor: {0}")]
    Descriptor(&'static str),
    #[error("analyzer {component} revision {actual} is unavailable; expected {expected}")]
    DescriptorRevision {
        component: &'static str,
        expected: u32,
        actual: u32,
    },
    #[error("analyzer fingerprint mismatch: expected {expected}, received {actual}")]
    DescriptorFingerprint {
        expected: crate::AnalyzerFingerprint,
        actual: crate::AnalyzerFingerprint,
    },
    #[error("analysis needs {required} {resource}, exceeding limit {limit}")]
    ResourceLimit {
        resource: &'static str,
        required: usize,
        limit: usize,
    },
    #[error("invalid analyzer JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("analysis cancelled")]
    Cancelled,
    #[error("token contains unpaired UTF-16 surrogate {unit:#06x}; use its lossless term units")]
    UnpairedTokenSurrogate { unit: u16 },
    #[error("analysis expected {expected_utf16} UTF-16 input units, but received {actual_utf16}")]
    MismatchedAnalysisInput {
        expected_utf16: usize,
        actual_utf16: usize,
    },
    #[cfg(feature = "nori")]
    #[error(transparent)]
    Dictionary(#[from] crate::nori::DictionaryError),
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
    #[error("highlighting requires original source offsets for every matching token")]
    MissingTokenOffsets,
    #[error("token graphs require a positive first increment and positive position lengths")]
    InvalidTokenPosition,
    #[error("token position exceeds the u32 position format")]
    TokenPositionOverflow,
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
