//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Language-independent token graph edges and corrected original-source coordinates.

use serde::{Deserialize, Serialize};

/// Original-source ranges. UTF-8 covers complete scalars; UTF-16 may split a surrogate pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TokenOffsets {
    pub start_utf8: u64,
    pub end_utf8: u64,
    pub start_utf16: u64,
    pub end_utf16: u64,
}

impl TokenOffsets {
    pub fn validate(&self) -> Result<(), TokenOccurrenceError> {
        if self.start_utf8 > self.end_utf8 || self.start_utf16 > self.end_utf16 {
            return Err(TokenOccurrenceError::ReversedOffsets);
        }
        Ok(())
    }
}

/// One occurrence of one term, including its graph edge and optional source spans.
///
/// Repeated equal occurrences remain distinct and contribute separately to term frequency. A collection is ordered by position with ties retained in emission order; source offsets need not increase. Validate values received from external code or persistence before using them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TokenOccurrence {
    pub position: u32,
    pub position_length: u32,
    pub offsets: Option<TokenOffsets>,
}

impl TokenOccurrence {
    pub fn validate(&self) -> Result<(), TokenOccurrenceError> {
        self.end_position()?;
        if let Some(offsets) = self.offsets {
            offsets.validate()?;
        }
        Ok(())
    }

    pub fn end_position(&self) -> Result<u32, TokenOccurrenceError> {
        if self.position_length == 0 {
            return Err(TokenOccurrenceError::EmptyEdge);
        }
        self.position
            .checked_add(self.position_length)
            .ok_or(TokenOccurrenceError::PositionOverflow)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TokenOccurrenceError {
    #[error("token occurrence has zero position length")]
    EmptyEdge,
    #[error("token occurrence end position exceeds u32")]
    PositionOverflow,
    #[error("token occurrence source offsets are reversed")]
    ReversedOffsets,
}
