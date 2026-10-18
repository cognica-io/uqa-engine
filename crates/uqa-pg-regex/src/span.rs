//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Match spans in the layout of `PostgreSQL`'s `regmatch_t`.

use std::ffi::c_long;
use std::ops::Range;

/// One `regmatch_t` slot: the code-point span of the whole match (slot 0) or of a subexpression (slot `n`).
///
/// Offsets count code points from the start of the input, not from the search start. A slot is unset, `None`, when its subexpression did not participate in the match, when the expression has fewer subexpressions than slots, or when the expression was compiled with `REG_NOSUB` (except slot 0).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MatchSpan {
    rm_so: c_long,
    rm_eo: c_long,
}

impl MatchSpan {
    /// A slot with no span, as `pg_regexec` marks one with `-1`.
    pub const UNSET: Self = Self {
        rm_so: -1,
        rm_eo: -1,
    };

    /// The start offset, if the slot is set.
    pub fn start(&self) -> Option<usize> {
        usize::try_from(self.rm_so).ok()
    }

    /// The end offset (exclusive), if the slot is set.
    pub fn end(&self) -> Option<usize> {
        usize::try_from(self.rm_eo).ok()
    }

    /// The span, if the slot is set.
    pub fn range(&self) -> Option<Range<usize>> {
        Some(self.start()?..self.end()?)
    }
}

impl Default for MatchSpan {
    fn default() -> Self {
        Self::UNSET
    }
}
