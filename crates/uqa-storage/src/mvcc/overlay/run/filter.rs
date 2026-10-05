//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A Bloom filter over the keys of one run, which answers most lookups of keys the run lacks without reading it.

use uqa_core::memory::{BudgetedVec, MemoryBudget};

const BITS_PER_KEY: u64 = 10;
const HASHES: u64 = 7;

/// A Bloom filter of `HASHES` probes over `BITS_PER_KEY` bits for each expected key, which errs only by reporting an absent key as possibly present.
pub(super) struct KeyFilter {
    words: BudgetedVec<u64>,
    bits: u64,
}

impl KeyFilter {
    /// A filter for `keys` keys, or `None` when it would occupy more than a quarter of the available workspace needed by the run index and readers. Omitting it makes lookups slower but exact.
    pub(super) fn with_capacity(keys: u64, memory: &MemoryBudget) -> Option<Self> {
        let bits = keys
            .checked_mul(BITS_PER_KEY)?
            .max(64)
            .checked_next_multiple_of(64)?;
        let words = usize::try_from(bits / 64).ok()?;
        let maximum = memory.available() / 4;
        if words.checked_mul(size_of::<u64>())? > maximum {
            return None;
        }
        let mut storage = BudgetedVec::new(memory);
        storage.reserve(words).ok()?;
        if storage.capacity().checked_mul(size_of::<u64>())? > maximum {
            return None;
        }
        for _ in 0..words {
            storage.push(0).ok()?;
        }
        Some(Self {
            words: storage,
            bits,
        })
    }

    pub(super) fn insert(&mut self, key: &[u8]) {
        let (first, second) = probes(key);
        for probe in 0..HASHES {
            let bit = first.wrapping_add(probe.wrapping_mul(second)) % self.bits;
            self.words[(bit / 64) as usize] |= 1 << (bit % 64);
        }
    }

    pub(super) fn may_contain(&self, key: &[u8]) -> bool {
        let (first, second) = probes(key);
        (0..HASHES).all(|probe| {
            let bit = first.wrapping_add(probe.wrapping_mul(second)) % self.bits;
            self.words[(bit / 64) as usize] & (1 << (bit % 64)) != 0
        })
    }
}

/// Two independent hashes of `key` for double hashing; the second is odd, so the probes cover the filter.
fn probes(key: &[u8]) -> (u64, u64) {
    (
        hash(key, 0xcbf2_9ce4_8422_2325),
        hash(key, 0x8422_2325_cbf2_9ce4) | 1,
    )
}

/// FNV-1a from `seed`, finished with the `MurmurHash3` 64-bit mixer so that every input bit affects every output bit.
fn hash(key: &[u8], seed: u64) -> u64 {
    let mut state = seed;
    for byte in key {
        state ^= u64::from(*byte);
        state = state.wrapping_mul(0x0000_0100_0000_01b3);
    }
    state ^= state >> 33;
    state = state.wrapping_mul(0xff51_afd7_ed55_8ccd);
    state ^= state >> 33;
    state = state.wrapping_mul(0xc4ce_b9fe_1a85_ec53);
    state ^ (state >> 33)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn optional_filter_leaves_shared_ancestor_workspace_for_the_run_consumer() {
        let memory = MemoryBudget::new(64 << 10);
        let retained = memory.reserve(48 << 10).unwrap();
        let reader = memory.child(256 << 10);
        assert!(KeyFilter::with_capacity(8_192, &reader).is_none());
        assert_eq!(memory.used(), retained.bytes());

        let mut filter = KeyFilter::with_capacity(128, &reader).unwrap();
        for key in 0_u64..128 {
            filter.insert(&key.to_be_bytes());
        }
        for key in 0_u64..128 {
            assert!(filter.may_contain(&key.to_be_bytes()));
        }
        assert!(reader.used() <= (16 << 10) / 4);
        drop((filter, retained));
        assert_eq!(memory.used(), 0);
    }
}
