//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Validated immutable resources and standalone native Korean tokenization.
//!
//! With the `nori` feature and a supplied `uqa-nori-data` bundle:
//!
//! ```
//! use uqa_analysis::nori::{
//!     DictionaryLimits, KoreanTokenizer, NoriDictionary, NoriOptions,
//!     UserDictionary, UserDictionaryLimits,
//! };
//!
//! let model = NoriDictionary::from_bytes(uqa_nori_data::BUNDLE, DictionaryLimits::default())?;
//! let user = UserDictionary::compile("세종시 세종 시", &model, UserDictionaryLimits::default())?;
//! let tokenizer = KoreanTokenizer::new(model, user, NoriOptions::default())?;
//! let output = tokenizer.tokenize("세종시")?;
//! let terms: Result<Vec<_>, _> = output.tokens.iter()
//!     .map(|token| String::from_utf16(&token.term_utf16)).collect();
//! assert_eq!(terms?, ["세종", "시"]);
//! assert_eq!(output.final_offset_utf16, 3);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! Some accepted user rules produce unpaired surrogate units, so UTF-8 conversion is fallible.
//! This API has no catalog effects and does not register a generic or SQL analyzer.

mod dictionary;
mod error;
mod frame;
mod io;
mod lexicon;
mod morphology;
mod tokenizer;
mod unicode;
mod user_dictionary;

#[cfg(feature = "nori-tools")]
pub mod pack;

pub use dictionary::{DictionaryLimits, NoriDictionary, SurfaceWords};
pub use error::{DictionaryError, DictionaryResult};
pub use frame::DictionaryId;
pub use morphology::{DictionaryWord, MorphemeRef, POSTag, POSType};
pub use unicode::UnicodeProperties;

#[cfg(test)]
mod tests;

pub use user_dictionary::{UserDictionary, UserDictionaryLimits, UserEntry};

pub use tokenizer::{
    DecompoundMode, KoreanTokenizer, NoriLimits, NoriMorpheme, NoriOptions, NoriOrigin, NoriOutput,
    NoriToken,
};
