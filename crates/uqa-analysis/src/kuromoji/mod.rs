//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Validated immutable Japanese dictionaries and their complete pinned morphology.
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
//! Loading publishes one immutable model after validation and does not resolve external resources.
//! Japanese tokenization and analyzer registration are separate from this dictionary API.

mod analysis;
mod dictionary;
mod error;
mod frame;
mod morphology;
mod provenance;
mod tables;

#[cfg(feature = "kuromoji-tools")]
pub mod pack;

pub use crate::morphology::limits::DictionaryLimits;
pub use crate::morphology::surfaces::SurfaceWords;
pub use crate::morphology::unicode::UnicodeProperties;

#[cfg(test)]
mod tests;
pub use analysis::CompletionMapping;
pub use dictionary::KuromojiDictionary;
pub use error::{DictionaryError, DictionaryResult};
pub use frame::DictionaryId;
pub use morphology::DictionaryWord;
