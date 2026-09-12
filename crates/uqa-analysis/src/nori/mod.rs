//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Validated immutable resources for Korean morphological analysis.

mod dictionary;
mod error;
mod frame;
mod io;
mod lexicon;
mod morphology;
mod unicode;

#[cfg(feature = "nori-tools")]
pub mod pack;

pub use dictionary::{DictionaryLimits, NoriDictionary, SurfaceWords};
pub use error::{DictionaryError, DictionaryResult};
pub use frame::DictionaryId;
pub use morphology::{DictionaryWord, MorphemeRef, POSTag, POSType};
pub use unicode::UnicodeProperties;

#[cfg(test)]
mod tests;
