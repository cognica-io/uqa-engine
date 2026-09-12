//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Text analysis pipeline: char filters, tokenizers, token filters,
//! composable [`Analyzer`], and a global named-analyzer registry.

pub mod analyzer;
mod cache;
pub mod char_filter;
pub mod descriptor;
pub mod error;
pub mod highlight;
#[cfg(feature = "nori")]
pub mod nori;
pub mod porter;
pub mod registry;
pub mod resources;
pub mod source;
pub mod term;
pub mod token;
pub mod token_filter;
pub mod tokenizer;

pub use analyzer::{
    keyword_analyzer, standard_analyzer, standard_cjk_analyzer, whitespace_analyzer, Analyzer,
    CompiledAnalyzer,
};
pub use char_filter::CharFilter;
pub use descriptor::{AnalyzerDescriptor, AnalyzerFingerprint, AnalyzerLimits, TokenLengthPolicy};
pub use error::{AnalysisError, AnalysisResult};
pub use highlight::{highlight, highlight_compiled, HighlightOptions};
pub use registry::{
    drop_analyzer, get_analyzer, list_analyzers, register_analyzer, DEFAULT_ANALYZER_NAME,
};
pub use resources::{AnalyzerCacheStats, AnalyzerResources};
pub use source::{FilteredText, SourceOffsets, TextCoordinates};
pub use term::TokenTerm;
pub use token::{AnalysisToken, AnalyzedText};
pub use token_filter::{SynonymFileError, TokenFilter};
pub use tokenizer::Tokenizer;
