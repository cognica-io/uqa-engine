//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Porter (1980) stemming algorithm.
//!
//! Reference: M. F. Porter, "An Algorithm for Suffix Stripping", *Program*
//! 14(3), 1980. Note that this is the original 1980 algorithm, not Porter2
//! (Snowball English) — they differ on edge cases such as `agreed` and
//! `feedeing`. The output is intentionally identical to the upstream
//! UQA stemmer contract so that BM25 doc frequencies match across
//! engines.

mod algorithm;
mod allocation;
mod word;

#[cfg(test)]
pub(crate) use allocation::stem_utf16;
pub use allocation::{stem, stem_budgeted, stem_term_budgeted};

// Scalars and isolated surrogate units remain distinct algorithm elements.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Character(u32);

impl Character {
    fn is_one_of(self, characters: &[char]) -> bool {
        characters.iter().any(|&character| self == character)
    }
}

impl From<char> for Character {
    fn from(value: char) -> Self {
        Self(u32::from(value))
    }
}

impl PartialEq<char> for Character {
    fn eq(&self, other: &char) -> bool {
        self.0 == u32::from(*other)
    }
}

#[cfg(test)]
mod tests;
