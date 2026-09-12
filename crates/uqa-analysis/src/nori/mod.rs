//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Validated immutable resources, native Korean analysis, and separate normalization.
//!
//! With the `nori` feature and a supplied `uqa-nori-data` bundle:
//!
//! ```
//! use uqa_analysis::nori::{
//!     DecompoundMode, DictionaryLimits, KoreanAnalyzer, KoreanFilter, KoreanTokenizer,
//!     NoriDictionary, NoriOptions, POSTag,
//!     UserDictionary, UserDictionaryLimits,
//! };
//!
//! let model = NoriDictionary::from_bytes(uqa_nori_data::BUNDLE, DictionaryLimits::default())?;
//! let user = UserDictionary::compile("세종시 세종 시", &model, UserDictionaryLimits::default())?;
//! let tokenizer = KoreanTokenizer::new(model.clone(), user, NoriOptions::default())?;
//! let output = tokenizer.tokenize("세종시")?;
//! let terms: Result<Vec<_>, _> = output.tokens.iter()
//!     .map(|token| String::from_utf16(&token.term_utf16)).collect();
//! assert_eq!(terms?, ["세종", "시"]);
//! assert_eq!(output.final_offset_utf16, 3);
//!
//! let analyzer = KoreanAnalyzer::new(model.clone(), None, NoriOptions::default())?;
//! let output = analyzer.analyze("나물은")?;
//! assert_eq!(String::from_utf16(&output.tokens[0].term_utf16)?, "나물");
//! assert_eq!(output.final_position_increment, 1);
//! assert_eq!(analyzer.normalize("喜悲哀歡 İ UQA")?, "喜悲哀歡 i uqa");
//!
//! let numbers = KoreanAnalyzer::with_filters(model, None, NoriOptions {
//!     decompound_mode: DecompoundMode::None,
//!     discard_punctuation: false,
//!     ..NoriOptions::default()
//! }, &[
//!     KoreanFilter::PartOfSpeech { stop_tags: Some(vec![POSTag::SP]) },
//!     KoreanFilter::Number,
//! ])?;
//! let output = numbers.analyze("３．２천 원 15,7")?;
//! let terms: Result<Vec<_>, _> = output.tokens.iter()
//!     .map(|token| String::from_utf16(&token.term_utf16)).collect();
//! assert_eq!(terms?, ["3200", "원", "157"]);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! Some accepted user rules produce unpaired surrogate units, so UTF-8 conversion is fallible.
//! This API has no catalog effects and does not register a generic or SQL analyzer.

mod analyzer;
mod attributes;
mod config;
mod dictionary;
mod error;
pub(crate) mod filters;
mod frame;
mod io;
mod lexicon;
mod morphology;
mod number;
pub(crate) mod pipeline;
mod resources;
mod tokenizer;
mod unicode;
mod user_dictionary;

#[cfg(feature = "nori-tools")]
pub mod pack;

pub use analyzer::KoreanAnalyzer;
pub use attributes::KoreanMorphology;
pub use config::{
    nori_analyzer, EmptyFilterConfig, NoriPOSConfig, NoriTokenizerConfig, SimpleLowercaseConfig,
};
pub use dictionary::{DictionaryLimits, NoriDictionary, SurfaceWords};
pub use error::{DictionaryError, DictionaryResult};
pub use filters::{KoreanFilter, DEFAULT_STOP_TAGS};
pub use frame::DictionaryId;
pub use morphology::{DictionaryWord, MorphemeRef, POSTag, POSType};
pub use number::{normalize_number, normalize_number_utf16};
pub use resources::{
    DictionaryArtifact, DictionaryBytes, DictionaryRequest, DictionaryResolver, NoriResources,
    ResolvedDictionary, ResolvedUserDictionary, ResourceCacheStats, ResourceHash, ResourceLimits,
    DEFAULT_NORI_DICTIONARY,
};
pub use unicode::UnicodeProperties;

#[cfg(test)]
mod tests;

pub use user_dictionary::{UserDictionary, UserDictionaryLimits, UserEntry};

pub use tokenizer::{
    DecompoundMode, KoreanTokenizer, NoriLimits, NoriMorpheme, NoriOptions, NoriOrigin, NoriOutput,
    NoriToken,
};
