//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::{read_control::StorageReadControl, StorageBackendResult};

#[derive(Debug, Clone, Copy)]
pub(super) struct SplitMix64(pub u64);

impl SplitMix64 {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut value = self.0;
        value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        value ^ (value >> 31)
    }

    pub fn below(&mut self, bound: u64, control: &StorageReadControl) -> StorageBackendResult<u64> {
        debug_assert_ne!(bound, 0);
        let threshold = bound.wrapping_neg() % bound;
        loop {
            control.check()?;
            let value = self.next();
            if value >= threshold {
                return Ok(value % bound);
            }
        }
    }
}
