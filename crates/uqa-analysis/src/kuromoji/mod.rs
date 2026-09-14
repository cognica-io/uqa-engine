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
mod dictionary;
mod error;
mod frame;
mod morphology;
mod provenance;
mod tables;
mod user_dictionary;

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
pub use user_dictionary::{UserDictionary, UserDictionaryLimits, UserEntry, UserMatch, UserWord};
