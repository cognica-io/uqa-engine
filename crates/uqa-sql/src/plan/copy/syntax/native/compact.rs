//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A small stored statement can use its original direct allowance. The field projections measure at most one regular work window of nodes and collection slots, and one UTF-8 chunk across all selected text.

use uqa_core::memory::ProductionString;

pub(crate) struct Window {
    slots: usize,
    bytes: usize,
}

impl Window {
    pub(crate) const STEPS: usize = 32;

    pub(crate) const fn new() -> Self {
        Self {
            slots: Self::STEPS,
            bytes: ProductionString::CHUNK_BYTES,
        }
    }

    pub(crate) fn slots(&mut self, count: usize) -> bool {
        let Some(remaining) = self.slots.checked_sub(count) else {
            return false;
        };
        self.slots = remaining;
        true
    }

    pub(crate) fn text(&mut self, text: &str) -> bool {
        let Some(remaining) = self.bytes.checked_sub(text.len()) else {
            return false;
        };
        self.bytes = remaining;
        true
    }
}
