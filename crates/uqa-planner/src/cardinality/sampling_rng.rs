//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Deterministic PRNG used by graph cardinality sampling.

use super::Cell;

/// Tiny xorshift64 PRNG used by the random-walk sampler. We avoid
/// pulling in `rand` for a single 100-sample loop.
pub(super) struct XorShiftRng {
    state: Cell<u64>,
}

impl XorShiftRng {
    pub(super) fn new(seed: u64) -> Self {
        Self {
            state: Cell::new(seed.max(1)),
        }
    }

    fn next_u64(&self) -> u64 {
        let mut s = self.state.get();
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        self.state.set(s);
        s
    }

    /// Uniform random index in `0..bound` (`bound` must be >= 1).
    pub(super) fn bounded(&self, bound: usize) -> Option<usize> {
        if bound <= 1 {
            return Some(0);
        }
        let bound = u64::try_from(bound).ok()?;
        usize::try_from(self.next_u64() % bound).ok()
    }
}
