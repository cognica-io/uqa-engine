//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Text analysis pipeline: char filters, tokenizers, token filters,
//! composable [`Analyzer`], and a global named-analyzer registry.

pub mod analyzer;
pub mod char_filter;
pub mod highlight;
pub mod porter;
pub mod registry;
pub mod token_filter;
pub mod tokenizer;

pub use analyzer::Analyzer;
pub use char_filter::CharFilter;
pub use highlight::{highlight, HighlightOptions};
pub use registry::{
    drop_analyzer, get_analyzer, list_analyzers, register_analyzer, DEFAULT_ANALYZER_NAME,
};
pub use token_filter::TokenFilter;
pub use tokenizer::Tokenizer;
