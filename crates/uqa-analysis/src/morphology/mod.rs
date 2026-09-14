//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Language-independent dictionary encoding, lookup, and connection costs.
//!
//! Language modules own bundle schemas, word attributes, character classes, and public errors.

pub(crate) mod error;
pub(crate) mod frame;
pub(crate) mod io;
pub(crate) mod lexicon;
pub(crate) mod limits;
pub(crate) mod manifest;
pub(crate) mod matrix;
pub(crate) mod strings;
pub(crate) mod surfaces;
pub(crate) mod unicode;

use error::DictionaryError;
type DictionaryResult<T> = Result<T, DictionaryError>;

#[cfg(feature = "nori-tools")]
pub(crate) mod neutral;
