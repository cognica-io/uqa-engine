//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native Japanese tokenization over validated immutable dictionaries and pinned morphology.
//!
//! The `kuromoji` feature supplies the portable dictionary and its checked loader:
//!
//! ```
//! use uqa_analysis::kuromoji::{DictionaryLimits, KuromojiDictionary};
//!
//! let dictionary = KuromojiDictionary::from_bytes(
//!     uqa_kuromoji_data::BUNDLE,
//!     DictionaryLimits::default(),
//! )?;
//! let surface = dictionary.lookup("令和").unwrap();
//! assert!(surface.word_ids.into_iter().any(|id| {
//!     dictionary.word(id).unwrap().reading() == Some("レイワ")
//! }));
//! assert_eq!(dictionary.id().to_string(), uqa_kuromoji_data::DICTIONARY_ID);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! Loading publishes one immutable model after validation and does not resolve external resources. [`JapaneseTokenizer`] consumes that model with optional user rules and explicit modes and limits. Common analyzer registration and filtering are separate from the standalone tokenizer API.
//!
//! Japanese user rules compile against the selected model with explicit preparation bounds:
//!
//! ```
//! use uqa_analysis::kuromoji::{DictionaryLimits, KuromojiDictionary, UserDictionary, UserDictionaryLimits};
//! let model = KuromojiDictionary::from_bytes(uqa_kuromoji_data::BUNDLE, DictionaryLimits::default())?;
//! let user = UserDictionary::compile(
//!     "東京大学,東京 大学,トウキョウ ダイガク,名詞",
//!     &model,
//!     UserDictionaryLimits::default(),
//! )?.unwrap();
//! let phrase = user.lookup("東京大学").unwrap();
//! let entry = user.entry(phrase).unwrap();
//! assert_eq!(entry.segment_lengths(), [2, 2]);
//! assert_eq!(user.word(entry.word_base()).unwrap().reading()?, "トウキョウ");
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

mod analysis;
mod analyzer;
pub use analyzer::JapaneseAnalyzer;
mod attributes;
pub(crate) use attributes::AttributeErrors;
mod completion;
mod config;
pub use completion::{
    romanize_completion_utf16, romanize_completion_utf16_budgeted, CompletionMode,
};
pub use config::KuromojiTokenizerConfig;
mod dictionary;
mod error;
pub(crate) mod filters;
mod frame;
pub use filters::JapaneseFilter;
mod morphology;
pub(crate) mod normalization;
mod number;
pub(crate) mod pipeline;
mod provenance;
mod resources;
mod tables;
mod tokenizer;
mod user_dictionary;

#[cfg(feature = "kuromoji-tools")]
pub mod pack;

pub use crate::morphology::limits::DictionaryLimits;
pub use crate::morphology::surfaces::SurfaceWords;
pub use crate::morphology::unicode::UnicodeProperties;

#[cfg(test)]
mod tests;
pub use analysis::CompletionMapping;
pub use attributes::JapaneseMorphology;
pub use dictionary::KuromojiDictionary;
pub use error::{DictionaryError, DictionaryResult};
pub use frame::DictionaryId;
pub use morphology::DictionaryWord;
pub use number::{
    normalize_number, normalize_number_budgeted, normalize_number_utf16,
    normalize_number_utf16_budgeted,
};
pub use tokenizer::{
    JapaneseTokenizer, KuromojiLimits, KuromojiMode, KuromojiOptions, KuromojiOrigin,
    KuromojiOutput, KuromojiToken,
};
pub use user_dictionary::{UserDictionary, UserDictionaryLimits, UserEntry, UserMatch, UserWord};

pub use resources::{
    DictionaryArtifact, DictionaryBytes, DictionaryRequest, DictionaryResolver, KuromojiResources,
    ResolvedDictionary, ResolvedUserDictionary, ResourceCacheStats, ResourceHash, ResourceLimits,
    DEFAULT_KUROMOJI_DICTIONARY,
};
