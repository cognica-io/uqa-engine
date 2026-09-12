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

#[path = "analysis/highlight.rs"]
mod highlight;

#[path = "analysis/source_offsets.rs"]
mod source_offsets;

#[path = "analysis/lossless_terms.rs"]
mod lossless_terms;
#[path = "analysis/rich_tokens.rs"]
mod rich_tokens;

#[cfg(feature = "nori-tools")]
#[path = "analysis/nori_model.rs"]
mod nori_model;

#[cfg(feature = "nori")]
#[path = "analysis/nori_resources.rs"]
mod nori_resources;

#[cfg(feature = "nori")]
#[path = "analysis/nori_users.rs"]
mod nori_users;

#[cfg(feature = "nori")]
#[path = "analysis/nori_tokenizer.rs"]
mod nori_tokenizer;

#[cfg(feature = "nori")]
#[path = "analysis/nori_analysis.rs"]
mod nori_analysis;

#[cfg(feature = "nori")]
#[path = "analysis/nori_numbers.rs"]
mod nori_numbers;

#[cfg(feature = "nori")]
#[path = "analysis/nori_bridge.rs"]
mod nori_bridge;

#[cfg(feature = "nori")]
#[path = "analysis/nori_shared.rs"]
mod nori_shared;

#[cfg(feature = "nori")]
#[path = "analysis/nori_resolvers.rs"]
mod nori_resolvers;

#[path = "analysis/compiled.rs"]
mod compiled;

#[path = "analysis/descriptor.rs"]
mod descriptor;

#[cfg(feature = "nori")]
#[path = "analysis/nori_compiled.rs"]
mod nori_compiled;
