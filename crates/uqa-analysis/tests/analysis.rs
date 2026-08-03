//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Coverage for the Lucene-style text-analysis pipeline: tokenizers, token
//! filters, character filters,
//! analyzer composition, serialization round-trips, and the named
//! analyzer registry.

use std::collections::BTreeMap;

use uqa_analysis::{
    keyword_analyzer, standard_analyzer, standard_cjk_analyzer, whitespace_analyzer, AnalysisError,
    Analyzer, CharFilter, SynonymFileError, TokenFilter, Tokenizer,
};

#[path = "analysis/tokenizers.rs"]
mod tokenizers;

#[path = "analysis/token_filters.rs"]
mod token_filters;

#[path = "analysis/char_filters.rs"]
mod char_filters;

#[path = "analysis/analyzers.rs"]
mod analyzers;

#[path = "analysis/synonym_file.rs"]
mod synonym_file;

#[path = "analysis/validation.rs"]
mod validation;
